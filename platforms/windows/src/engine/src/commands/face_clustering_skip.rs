//! Skips a clustering pass when nothing it reads has changed since the last
//! pass that finished. The pass re-runs after every scan; on an unchanged
//! library it would redo minutes of work to arrive where it started.
//!
//! The fingerprint is a SHA-256 over every row the pass reads (faces with
//! their vectors and file revisions, people, verdicts, constraints,
//! dismissals and the review queues), the face settings in the environment
//! and the model space. It is taken after a pass commits and stored beside
//! the catalog with that pass's summary; an equal fingerprint before the next
//! pass means the catalog is exactly as that pass left it.

use crate::ipc::FaceClusteringResult;
use rusqlite::{types::ValueRef, Connection};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

const VERSION: &str = "face-clustering-fingerprint-1";

const TABLES: [&str; 9] = [
    "SELECT fp.id,fp.file_id,fp.person_id,fp.bbox,fp.face_quality,fp.excluded,fp.arcface_embedding,fp.embedding_model,\
            fp.processing_version,fp.source_revision,f.path_text,f.size_bytes,f.modified_at,f.file_ref,f.failed,f.content_hash,r.revision \
     FROM face_prints fp LEFT JOIN files f ON f.id=fp.file_id LEFT JOIN catalog_revisions r ON r.file_id=fp.file_id ORDER BY fp.id",
    "SELECT * FROM persons ORDER BY id",
    "SELECT * FROM face_verifications ORDER BY person_a,person_b",
    "SELECT * FROM face_identity_constraints ORDER BY id",
    "SELECT * FROM face_queue_dismissals ORDER BY id",
    "SELECT * FROM face_review_candidates ORDER BY face_id",
    "SELECT * FROM face_assignment_assessed ORDER BY face_id",
    "SELECT * FROM face_merge_suggestions ORDER BY person_id,other_person_id",
    "SELECT * FROM face_split_suggestions ORDER BY person_id",
];

#[derive(serde::Serialize, serde::Deserialize)]
struct Remembered {
    fingerprint: String,
    result: FaceClusteringResult,
}

fn put(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

pub(super) fn fingerprint(conn: &Connection) -> rusqlite::Result<String> {
    let tx = conn.unchecked_transaction()?;
    let mut hasher = Sha256::new();
    put(&mut hasher, VERSION.as_bytes());
    put(&mut hasher, env!("CARGO_PKG_VERSION").as_bytes());
    put(&mut hasher, crate::commands::face_refresh::processing().as_bytes());
    put(&mut hasher, crate::commands::face_refresh::current_model().unwrap_or_default().as_bytes());
    let mut settings: Vec<(String, String)> = std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.to_string_lossy().into_owned())))
        .filter(|(key, _)| key.starts_with("FILEID_FACE"))
        .collect();
    settings.sort();
    for (key, value) in settings {
        put(&mut hasher, key.as_bytes());
        put(&mut hasher, value.as_bytes());
    }
    for sql in TABLES {
        let mut statement = tx.prepare(sql)?;
        let columns = statement.column_count();
        let mut rows = statement.query([])?;
        let mut count = 0u64;
        while let Some(row) = rows.next()? {
            for column in 0..columns {
                match row.get_ref(column)? {
                    ValueRef::Null => hasher.update([0u8]),
                    ValueRef::Integer(value) => {
                        hasher.update([1u8]);
                        hasher.update(value.to_le_bytes());
                    }
                    ValueRef::Real(value) => {
                        hasher.update([2u8]);
                        hasher.update(value.to_bits().to_le_bytes());
                    }
                    ValueRef::Text(value) => {
                        hasher.update([3u8]);
                        put(&mut hasher, value);
                    }
                    ValueRef::Blob(value) => {
                        hasher.update([4u8]);
                        put(&mut hasher, value);
                    }
                }
            }
            count += 1;
        }
        hasher.update([5u8]);
        hasher.update(count.to_le_bytes());
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Runs `work` on a read-only connection so hashing the catalog does not
/// hold the writer; an in-memory catalog only has its writer.
pub(super) fn with_reader<R>(writer: &parking_lot::Mutex<Connection>, work: impl FnOnce(&Connection) -> R) -> R {
    match crate::db::reader_for(writer) {
        Ok(Some(reader)) => work(&reader),
        _ => work(&writer.lock()),
    }
}

fn state_path(conn: &Connection) -> Option<PathBuf> {
    let path = conn.path().filter(|path| !path.is_empty() && !path.starts_with(':'))?;
    Some(PathBuf::from(path).with_extension("face-clustering.json"))
}

/// The last finished pass's summary when the catalog is exactly as it left it.
pub(super) fn unchanged(conn: &Connection) -> Option<FaceClusteringResult> {
    let path = state_path(conn)?;
    let metadata = std::fs::metadata(&path).ok()?;
    if !metadata.is_file() || metadata.len() > 65_536 {
        return None;
    }
    let remembered: Remembered = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
    (fingerprint(conn).ok()? == remembered.fingerprint).then_some(remembered.result)
}

/// Remembers a pass that finished without error. A pass that deferred faces
/// is not remembered: their files may come back (a drive plugged in again)
/// without the catalog changing.
pub(super) fn remember(conn: &Connection, result: &FaceClusteringResult) {
    let Some(path) = state_path(conn) else { return };
    if result.deferred_faces.unwrap_or(0) > 0 {
        forget(conn);
        return;
    }
    let written = (|| -> anyhow::Result<()> {
        crate::util::read_only::require_writable(&path)?;
        let remembered = Remembered { fingerprint: fingerprint(conn)?, result: result.clone() };
        let staging = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
        std::fs::write(&staging, serde_json::to_vec(&remembered)?)?;
        if let Err(error) = std::fs::rename(&staging, &path) {
            let _ = std::fs::remove_file(&staging);
            return Err(error.into());
        }
        Ok(())
    })();
    if let Err(error) = written {
        tracing::debug!(%error, "face clustering state was not saved; the next pass runs in full");
    }
}

pub(super) fn forget(conn: &Connection) {
    if let Some(path) = state_path(conn) {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        directory: PathBuf,
        conn: Connection,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }
    fn fixture() -> Fixture {
        let directory = std::env::temp_dir().join(format!("fileid-face-skip-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let conn = crate::db::open_writer(&directory.join("catalog.sqlite")).unwrap();
        conn.execute_batch(
            "INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension) VALUES
               (1,'/a.jpg',1,100,10,0,'image','jpg'),(2,'/b.jpg',2,100,10,0,'image','jpg');
             INSERT INTO persons(id,name,created_at) VALUES(1,'Ann',1);
             INSERT INTO face_prints(id,file_id,person_id,print_data,bbox,face_quality) VALUES
               (10,1,1,x'00','0.1,0.1,0.2,0.2',0.9),(20,2,NULL,x'00','0.3,0.3,0.2,0.2',0.8);",
        )
        .unwrap();
        Fixture { directory, conn }
    }
    fn summary() -> FaceClusteringResult {
        FaceClusteringResult { person_count: 1, face_count: 2, unmatched_faces: 1, duration_seconds: 3.0, deferred_faces: None, review_queued: Some(0) }
    }

    #[test]
    fn an_unchanged_catalog_returns_the_remembered_summary() {
        let fixture = fixture();
        assert!(unchanged(&fixture.conn).is_none(), "nothing remembered yet");
        remember(&fixture.conn, &summary());
        let again = unchanged(&fixture.conn).expect("unchanged");
        assert_eq!((again.person_count, again.face_count, again.unmatched_faces, again.review_queued), (1, 2, 1, Some(0)));
        fixture.conn.execute("UPDATE files SET scanned_at=99 WHERE id=1", []).unwrap();
        fixture.conn.execute("INSERT INTO scan_sessions(id,root_path,started_at,status) VALUES('s','/',1,'completed')", []).unwrap();
        assert!(unchanged(&fixture.conn).is_some(), "a scan that found nothing new does not force a pass");
    }

    #[test]
    fn every_kind_of_change_the_pass_reads_forces_a_new_pass() {
        let changes = [
            "INSERT INTO face_prints(id,file_id,print_data,bbox) VALUES(30,2,x'00','0.5,0.5,0.1,0.1')",
            "DELETE FROM face_prints WHERE id=20",
            "UPDATE face_prints SET person_id=1 WHERE id=20",
            "UPDATE face_prints SET excluded=1 WHERE id=20",
            "UPDATE face_prints SET face_quality=0.2 WHERE id=20",
            "UPDATE face_prints SET arcface_embedding=zeroblob(512),embedding_model='m',processing_version='p',source_revision='r' WHERE id=20",
            "UPDATE persons SET name='Anna' WHERE id=1",
            "UPDATE persons SET is_unknown=1 WHERE id=1",
            "INSERT INTO persons(id,created_at) VALUES(2,1)",
            "UPDATE files SET size_bytes=101 WHERE id=1",
            "UPDATE files SET failed=1 WHERE id=2",
            "INSERT INTO face_verifications(person_a,person_b,same_person,confidence,vlm_model,verified_at) VALUES(1,2,0,1,'user-verified',0)",
            "INSERT INTO face_identity_constraints(file_id,bbox,anchor_file_id,anchor_bbox,source,created_at) VALUES(1,'a',2,'b','review',0)",
            "INSERT INTO face_queue_dismissals(kind,file_id,bbox,created_at) VALUES('split',1,'a',0)",
            "INSERT INTO face_review_candidates(face_id,person_id,score,reason,namespace,created_at) VALUES(20,1,0.5,'suggested','m|p',0)",
            "INSERT INTO face_assignment_assessed(face_id,namespace,outcome,assessed_at) VALUES(20,'m|p','rejected',0)",
        ];
        for change in changes {
            let fixture = fixture();
            remember(&fixture.conn, &summary());
            assert!(unchanged(&fixture.conn).is_some());
            fixture.conn.execute(change, []).unwrap();
            assert!(unchanged(&fixture.conn).is_none(), "{change}");
        }
    }

    #[test]
    fn deferred_faces_forgetting_and_memory_catalogs_never_skip() {
        let fixture = fixture();
        remember(&fixture.conn, &summary());
        let mut deferred = summary();
        deferred.deferred_faces = Some(4);
        remember(&fixture.conn, &deferred);
        assert!(unchanged(&fixture.conn).is_none(), "a pass that deferred faces is not remembered");
        remember(&fixture.conn, &summary());
        forget(&fixture.conn);
        assert!(unchanged(&fixture.conn).is_none());
        remember(&fixture.conn, &summary());
        std::fs::write(state_path(&fixture.conn).unwrap(), b"{not json").unwrap();
        assert!(unchanged(&fixture.conn).is_none(), "a damaged state file is ignored");

        let memory = crate::db::test_support::memory_db();
        remember(&memory, &summary());
        assert!(unchanged(&memory).is_none());
    }
}
