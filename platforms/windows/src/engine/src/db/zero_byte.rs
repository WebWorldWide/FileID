#![allow(dead_code)]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

pub const ZERO_BYTE_REASON: &str = "File is zero bytes and has no indexable content.";

#[derive(Debug, Clone)]
pub struct ZeroByteObservation {
    pub path: PathBuf,
    pub file_ref: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct ValidatedZeroByteObservation {
    path: PathBuf,
    created_unix: Option<f64>,
    modified_unix: Option<f64>,
    file_ref: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct ZeroByteValidation {
    pub observations: Vec<ValidatedZeroByteObservation>,
    pub changed_since_observation: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ZeroByteSummary {
    pub applied: u64,
    pub missing_rows: u64,
    pub changed_since_observation: u64,
}

pub struct ZeroByteMutation {
    summary: ZeroByteSummary,
    crop_ids: Vec<i64>,
}

pub fn validate_zero_byte_files(observations: &[ZeroByteObservation]) -> ZeroByteValidation {
    let mut validation = ZeroByteValidation {
        observations: Vec::with_capacity(observations.len()),
        changed_since_observation: 0,
    };
    for observation in observations {
        let fs_path = crate::util::path_safety::to_extended_length(&observation.path);
        let metadata = match std::fs::symlink_metadata(&fs_path) {
            Ok(metadata) if metadata.file_type().is_file() && metadata.len() == 0 => metadata,
            _ => {
                validation.changed_since_observation += 1;
                continue;
            }
        };
        let current_ref = crate::platform::file_ref(&fs_path);
        if observation.file_ref.is_some() && observation.file_ref != current_ref {
            validation.changed_since_observation += 1;
            continue;
        }
        validation.observations.push(ValidatedZeroByteObservation {
            path: observation.path.clone(),
            created_unix: metadata.created().ok().and_then(system_time_to_unix),
            // None (→ SQL NULL → COALESCE preserves the stored value) when the
            // FS modified-time is unreadable, rather than fabricating `now` and
            // silently discarding the true timestamp (pre-1970 / FAT media).
            modified_unix: metadata.modified().ok().and_then(system_time_to_unix),
            file_ref: current_ref,
        });
    }
    validation
}

pub fn apply_validated_zero_byte_files(
    conn: &Connection,
    observations: &[ValidatedZeroByteObservation],
) -> Result<ZeroByteMutation> {
    let mut summary = ZeroByteSummary::default();
    let mut crop_ids = Vec::new();
    let mut find_file = conn
        .prepare_cached("SELECT id FROM files WHERE path_text = ?1")
        .context("preparing zero-byte row lookup")?;
    let mut find_faces = conn
        .prepare_cached("SELECT id, person_id FROM face_prints WHERE file_id = ?1")
        .context("preparing zero-byte face lookup")?;
    let mut update_file = conn
        .prepare_cached(
            "UPDATE files SET \
             size_bytes = 0, created_at = COALESCE(?2, created_at), modified_at = COALESCE(?3, modified_at), \
             scanned_at = ?4, file_ref = ?5, failed = 1, error_message = ?6, \
             phash = NULL, aesthetic = NULL, has_faces = 0, has_text = 0, \
             camera_model = NULL, location_lat = NULL, location_lon = NULL, \
             content_hash = NULL \
             WHERE id = ?1",
        )
        .context("preparing zero-byte row update")?;
    let mut delete_tags = conn
        .prepare_cached("DELETE FROM tags WHERE file_id = ?1 AND source IN ('auto', 'vlm')")
        .context("preparing zero-byte tag cleanup")?;
    let mut delete_faces = conn
        .prepare_cached("DELETE FROM face_prints WHERE file_id = ?1")
        .context("preparing zero-byte face cleanup")?;
    let mut delete_ocr = conn
        .prepare_cached("DELETE FROM ocr_text WHERE file_id = ?1")
        .context("preparing zero-byte OCR cleanup")?;
    let mut delete_doc = conn
        .prepare_cached("DELETE FROM doc_text WHERE file_id = ?1")
        .context("preparing zero-byte document cleanup")?;
    let mut delete_clip = conn
        .prepare_cached("DELETE FROM clip_embeddings WHERE file_id = ?1")
        .context("preparing zero-byte CLIP cleanup")?;
    let mut delete_text_embedding = conn
        .prepare_cached("DELETE FROM text_embeddings WHERE file_id = ?1")
        .context("preparing zero-byte text-embedding cleanup")?;
    let mut refresh_person = conn
        .prepare_cached(
            "UPDATE persons SET \
             file_count = (SELECT COUNT(DISTINCT file_id) FROM face_prints WHERE person_id = ?1), \
             representative_face_id = (SELECT id FROM face_prints \
                 WHERE person_id = ?1 ORDER BY COALESCE(face_quality, 0) DESC, id LIMIT 1) \
             WHERE id = ?1",
        )
        .context("preparing zero-byte person refresh")?;

    for observation in observations {
        let path_text = observation.path.to_string_lossy();
        let file_id = find_file
            .query_row(params![path_text.as_ref()], |row| row.get::<_, i64>(0))
            .optional()
            .with_context(|| {
                format!(
                    "looking up zero-byte row for {}",
                    crate::platform::redact_path_for_log(&observation.path)
                )
            })?;
        let Some(file_id) = file_id else {
            summary.missing_rows += 1;
            continue;
        };
        let faces = find_faces
            .query_map(params![file_id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("reading zero-byte face ownership")?;
        let mut affected_people = BTreeSet::new();
        for (face_id, person_id) in faces {
            crop_ids.push(face_id);
            if let Some(person_id) = person_id {
                affected_people.insert(person_id);
            }
        }

        update_file
            .execute(params![
                file_id,
                observation.created_unix,
                observation.modified_unix,
                now_unix(),
                observation.file_ref.map(|value| value as i64),
                ZERO_BYTE_REASON,
            ])
            .context("updating zero-byte catalog row")?;
        delete_tags.execute(params![file_id])?;
        delete_faces.execute(params![file_id])?;
        delete_ocr.execute(params![file_id])?;
        delete_doc.execute(params![file_id])?;
        delete_clip.execute(params![file_id])?;
        delete_text_embedding.execute(params![file_id])?;
        for person_id in affected_people {
            refresh_person.execute(params![person_id])?;
        }
        summary.applied += 1;
    }
    Ok(ZeroByteMutation { summary, crop_ids })
}

pub fn finish_zero_byte_mutation(mutation: ZeroByteMutation) -> ZeroByteSummary {
    for face_id in mutation.crop_ids {
        crate::pipeline::dbwriter::remove_face_crop(face_id);
    }
    mutation.summary
}

pub fn deactivate_validated_zero_byte_files(
    conn: &Connection,
    observations: &[ValidatedZeroByteObservation],
) -> Result<ZeroByteMutation> {
    let tx = conn
        .unchecked_transaction()
        .context("opening zero-byte transition")?;
    let mutation = apply_validated_zero_byte_files(&tx, observations)?;
    tx.commit().context("committing zero-byte transitions")?;
    Ok(mutation)
}

#[cfg(test)]
fn deactivate_zero_byte_files(
    conn: &Connection,
    observations: &[ZeroByteObservation],
) -> Result<ZeroByteSummary> {
    let validation = validate_zero_byte_files(observations);
    let mutation = deactivate_validated_zero_byte_files(conn, &validation.observations)?;
    let mut summary = finish_zero_byte_mutation(mutation);
    summary.changed_since_observation = validation.changed_since_observation;
    Ok(summary)
}

fn now_unix() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}

fn system_time_to_unix(time: SystemTime) -> Option<f64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    fn fixture(name: &str) -> (PathBuf, PathBuf, Connection) {
        let root = std::env::temp_dir().join(format!(
            "fileid-zero-byte-{name}-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("file.txt");
        std::fs::write(&path, b"").unwrap();
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        (root, path, conn)
    }

    #[test]
    fn zero_byte_transition_preserves_identity_and_user_tags_but_clears_derivatives() {
        let (root, path, conn) = fixture("cleanup");
        let path_text = path.to_string_lossy().into_owned();
        conn.execute(
            "INSERT INTO files (id, path_text, path_hash, size_bytes, scanned_at, kind, extension, \
             phash, aesthetic, has_faces, has_text, camera_model, location_lat, location_lon, failed, \
             content_hash, file_ref) \
             VALUES (7, ?1, 1, 99, 1, 'doc', 'txt', 2, 3, 1, 1, 'camera', 4, 5, 0, x'01', 9)",
            params![path_text],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tags(file_id, tag, source) VALUES \
             (7, 'automatic', 'auto'), (7, 'caption-tag', 'vlm'), (7, 'keep-me', 'user')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO persons(id, name, representative_face_id, file_count, created_at) \
             VALUES (3, 'Person', 11, 1, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO face_prints(id, file_id, person_id, print_data, bbox, face_quality) \
             VALUES (11, 7, 3, x'00', '0,0,1,1', 0.5)",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO ocr_text(file_id, text) VALUES (7, 'old ocr')", [])
            .unwrap();
        conn.execute("INSERT INTO doc_text(file_id, text) VALUES (7, 'old document')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO clip_embeddings(file_id, embedding, model) VALUES (7, x'00', 'clip')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO text_embeddings(file_id, embedding, model) VALUES (7, x'00', 'text')",
            [],
        )
        .unwrap();

        let summary = deactivate_zero_byte_files(
            &conn,
            &[ZeroByteObservation {
                path: path.clone(),
                file_ref: crate::platform::file_ref(&path),
            }],
        )
        .unwrap();
        assert_eq!(summary.applied, 1);
        let state: (i64, i64, i64, Option<i64>, Option<Vec<u8>>) = conn
            .query_row(
                "SELECT id, size_bytes, failed, phash, content_hash \
                 FROM files WHERE path_text = ?1",
                params![path_text],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .unwrap();
        assert_eq!(state, (7, 0, 1, None, None));
        let completion: (Option<String>, Option<f64>) = conn
            .query_row(
                "SELECT vlm_model, vlm_analyzed_at FROM files WHERE id=7",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(completion, (None, None));
        let surviving_tags: Vec<(String, String)> = conn
            .prepare("SELECT tag, source FROM tags WHERE file_id = 7 ORDER BY tag")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(surviving_tags, vec![("keep-me".to_string(), "user".to_string())]);
        for table in ["face_prints", "ocr_text", "doc_text", "clip_embeddings", "text_embeddings"] {
            let count: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table} WHERE file_id = 7"), [], |row| row.get(0))
                .unwrap();
            assert_eq!(count, 0, "{table}");
        }
        for (fts, term) in [("ocr_fts", "old"), ("doc_fts", "document")] {
            let hits: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {fts} WHERE {fts} MATCH ?1"), [term], |row| row.get(0))
                .unwrap();
            assert_eq!(hits, 0, "{fts}");
        }
        let person: (i64, Option<i64>) = conn
            .query_row("SELECT file_count, representative_face_id FROM persons WHERE id = 3", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(person, (0, None));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn zero_byte_transition_never_inserts_and_revalidates_identity_and_size() {
        let (root, path, conn) = fixture("revalidation");
        let missing = deactivate_zero_byte_files(
            &conn,
            &[ZeroByteObservation {
                path: path.clone(),
                file_ref: crate::platform::file_ref(&path),
            }],
        )
        .unwrap();
        assert_eq!(missing.missing_rows, 1);
        let rows: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0)).unwrap();
        assert_eq!(rows, 0);

        conn.execute(
            "INSERT INTO files(path_text, path_hash, size_bytes, scanned_at, kind, extension) \
             VALUES (?1, 1, 10, 1, 'doc', 'txt')",
            params![path.to_string_lossy().as_ref()],
        )
        .unwrap();
        let observed_ref = crate::platform::file_ref(&path);
        std::fs::write(&path, b"not empty").unwrap();
        let changed = deactivate_zero_byte_files(
            &conn,
            &[ZeroByteObservation {
                path: path.clone(),
                file_ref: observed_ref,
            }],
        )
        .unwrap();
        assert_eq!(changed.changed_since_observation, 1);
        let state: (i64, i64) = conn
            .query_row("SELECT size_bytes, failed FROM files", [], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap();
        assert_eq!(state, (10, 0));
        std::fs::remove_dir_all(root).unwrap();
    }
}
