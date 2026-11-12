use super::*;
use crate::db::test_support::memory_db;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

const MODEL: &str = "sface128-sha256:test";
const PROCESSING: &str = "macos-landmark-overlap-v3-orientation";

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tempdir() -> TempDir {
    let path = std::env::temp_dir().join(format!("fileid-face-extract-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    TempDir(path.canonicalize().unwrap())
}

fn unit(axis: usize) -> Vec<f32> {
    let mut v = vec![0.0f32; 128];
    v[axis] = 1.0;
    v
}

/// A photo on disk with `faces` boxes, stamped like a fresh Swift scan (no vectors).
fn photo(conn: &Connection, dir: &Path, id: i64, faces: &[i64]) -> PathBuf {
    let path = dir.join(format!("{id}.jpg"));
    std::fs::write(&path, format!("photo {id}")).unwrap();
    let metadata = std::fs::metadata(&path).unwrap();
    let modified = crate::commands::face_clustering_mac::foundation_mtime(&metadata).unwrap();
    conn.execute(
        "INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension) VALUES(?1,?2,?1,?3,?4,0,'image','jpg')",
        params![id, path.to_string_lossy(), metadata.len() as i64, modified],
    )
    .unwrap();
    for face in faces {
        conn.execute(
            "INSERT INTO face_prints(id,file_id,print_data,bbox,face_quality) VALUES(?1,?2,X'00',?3,0.8)",
            params![face, id, format!("0.1,0.1,0.2,0.{face}")],
        )
        .unwrap();
    }
    path
}

struct Fake {
    calls: AtomicUsize,
    fail_box: Option<String>,
}

impl Extractor for Fake {
    fn model(&self) -> &str {
        MODEL
    }

    fn extract(&self, _path: &Path, boxes: &[String]) -> Vec<Option<Embedded>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        boxes
            .iter()
            .enumerate()
            .map(|(index, bbox)| {
                (self.fail_box.as_deref() != Some(bbox.as_str())).then(|| Embedded { embedding: unit(index), jpeg: Some(vec![0xFF, 0xD8, index as u8]) })
            })
            .collect()
    }
}

fn database(conn: Connection) -> Arc<Mutex<Connection>> {
    Arc::new(Mutex::new(conn))
}

#[test]
fn pending_lists_faces_without_a_current_vector() {
    let dir = tempdir();
    let conn = memory_db();
    photo(&conn, &dir.0, 1, &[1, 2]);
    photo(&conn, &dir.0, 2, &[3]);
    let all = pending(&conn, MODEL, PROCESSING, 10, 0, 0.0).unwrap();
    assert_eq!(all.iter().map(|i| i.id).collect::<Vec<_>>(), [1, 2, 3]);
    assert_eq!(pending(&conn, MODEL, PROCESSING, 2, 0, 0.0).unwrap().len(), 2);
    assert_eq!(pending(&conn, MODEL, PROCESSING, 10, 2, 0.0).unwrap().iter().map(|i| i.id).collect::<Vec<_>>(), [3]);
    assert!(persist(&conn, &all[0], &unit(3), MODEL, PROCESSING, 1.0).unwrap());
    assert_eq!(pending(&conn, MODEL, PROCESSING, 10, 0, 0.0).unwrap().iter().map(|i| i.id).collect::<Vec<_>>(), [2, 3]);
    assert_eq!(pending(&conn, "sface128-sha256:other", PROCESSING, 10, 0, 0.0).unwrap().len(), 3, "another model needs every face");
    assert_eq!(pending(&conn, MODEL, "macos-bbox-crop-v3-orientation", 10, 0, 0.0).unwrap().len(), 3);
    conn.execute("UPDATE files SET modified_at=modified_at+1 WHERE id=1", []).unwrap();
    assert_eq!(pending(&conn, MODEL, PROCESSING, 10, 0, 0.0).unwrap().len(), 3, "a changed photo needs its faces again");
}

#[test]
fn persist_refuses_stale_rows_and_bad_vectors() {
    let dir = tempdir();
    let conn = memory_db();
    photo(&conn, &dir.0, 1, &[1]);
    let input = pending(&conn, MODEL, PROCESSING, 10, 0, 0.0).unwrap().remove(0);
    assert!(!persist(&conn, &input, &[0.5; 128], MODEL, PROCESSING, 1.0).unwrap(), "not a unit vector");
    assert!(!persist(&conn, &input, &unit(0), "", PROCESSING, 1.0).unwrap());
    let mut moved = input.clone();
    moved.bbox = "0.9,0.9,0.05,0.05".into();
    assert!(!persist(&conn, &moved, &unit(0), MODEL, PROCESSING, 1.0).unwrap());
    let mut resized = input.clone();
    resized.size += 1;
    assert!(!persist(&conn, &resized, &unit(0), MODEL, PROCESSING, 1.0).unwrap());
    record_failure(&conn, &input, MODEL, PROCESSING, "no_embedding", 10.0).unwrap();
    assert!(persist(&conn, &input, &unit(0), MODEL, PROCESSING, 1.0).unwrap());
    let (model, processing, revision): (String, String, String) = conn
        .query_row("SELECT embedding_model,processing_version,source_revision FROM face_prints WHERE id=1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap();
    assert_eq!((model.as_str(), processing.as_str(), revision.clone()), (MODEL, PROCESSING, input.revision()));
    let catalog: String = conn.query_row("SELECT revision FROM catalog_revisions WHERE file_id=1", [], |r| r.get(0)).unwrap();
    assert_eq!(catalog, revision);
    let failures: i64 = conn.query_row("SELECT COUNT(*) FROM face_refresh_failures", [], |r| r.get(0)).unwrap();
    assert_eq!(failures, 0, "a saved vector clears the face's failure");
}

#[test]
fn failures_back_off_and_double() {
    let dir = tempdir();
    let conn = memory_db();
    photo(&conn, &dir.0, 1, &[1]);
    let input = pending(&conn, MODEL, PROCESSING, 10, 0, 0.0).unwrap().remove(0);
    record_failure(&conn, &input, MODEL, PROCESSING, "no_embedding", 100.0).unwrap();
    assert!(pending(&conn, MODEL, PROCESSING, 10, 0, 150.0).unwrap().is_empty(), "inside the first minute");
    assert_eq!(pending(&conn, MODEL, PROCESSING, 10, 0, 161.0).unwrap().len(), 1);
    assert_eq!(pending(&conn, "sface128-sha256:other", PROCESSING, 10, 0, 150.0).unwrap().len(), 1, "another model is not backed off");
    record_failure(&conn, &input, MODEL, PROCESSING, "no_embedding", 200.0).unwrap();
    let (attempts, retry): (i64, f64) = conn.query_row("SELECT attempts,retry_after FROM face_refresh_failures WHERE face_id=1", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!((attempts, retry), (2, 320.0));
    record_failure(&conn, &input, MODEL, PROCESSING, "made up", 300.0).unwrap();
    let attempts: i64 = conn.query_row("SELECT attempts FROM face_refresh_failures WHERE face_id=1", [], |r| r.get(0)).unwrap();
    assert_eq!(attempts, 2, "unknown reasons are not recorded");
}

#[test]
fn extract_pending_saves_vectors_crops_and_failures() {
    let dir = tempdir();
    let conn = memory_db();
    for file in 1..=6 {
        photo(&conn, &dir.0, file, &[file * 10, file * 10 + 1]);
    }
    let gone = photo(&conn, &dir.0, 7, &[70]);
    std::fs::remove_file(gone).unwrap();
    let db = database(conn);
    let crops = dir.0.join("face_crops");
    let fake = Fake { calls: AtomicUsize::new(0), fail_box: Some("0.1,0.1,0.2,0.21".into()) };
    let skip: HashSet<i64> = [60, 61].into_iter().collect();
    let summary = extract_pending(&db, &fake, PROCESSING, &skip, Some(&crops), &|| false).unwrap();
    assert_eq!(summary, Summary { pending: 11, extracted: 9, failed: 2, files: 6 });
    assert_eq!(fake.calls.load(Ordering::SeqCst), 5, "one call per readable photo; the missing one is not read");
    let conn = db.lock();
    let saved: i64 = conn.query_row("SELECT COUNT(*) FROM face_prints WHERE length(arcface_embedding)=512 AND processing_version=?1", [PROCESSING], |r| r.get(0)).unwrap();
    assert_eq!(saved, 9);
    let reasons: Vec<(i64, String)> = conn
        .prepare("SELECT face_id,reason FROM face_refresh_failures ORDER BY face_id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(reasons, [(21, "no_embedding".to_string()), (70, "source_unavailable_or_changed".to_string())]);
    assert!(crops.join("10.jpg").is_file() && !crops.join("21.jpg").exists() && !crops.join("60.jpg").exists());
    assert!(pending(&conn, MODEL, PROCESSING, 100, 0, super::super::face_review::now()).unwrap().iter().all(|i| i.id == 60 || i.id == 61));
}

#[test]
fn extract_pending_stops_when_cancelled() {
    let dir = tempdir();
    let conn = memory_db();
    photo(&conn, &dir.0, 1, &[1]);
    let db = database(conn);
    let fake = Fake { calls: AtomicUsize::new(0), fail_box: None };
    let summary = extract_pending(&db, &fake, PROCESSING, &HashSet::new(), None, &|| true).unwrap();
    assert_eq!(summary, Summary::default());
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
    let failures: i64 = db.lock().query_row("SELECT COUNT(*) FROM face_refresh_failures", [], |r| r.get(0)).unwrap();
    assert_eq!(failures, 0);
}
