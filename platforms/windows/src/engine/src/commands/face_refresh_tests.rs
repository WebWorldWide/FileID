use super::*;
use crate::db::test_support::memory_db;

struct TempDir(PathBuf);

impl TempDir {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tempdir() -> TempDir {
    let path = std::env::temp_dir().join(format!("fileid-face-refresh-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    TempDir(path.canonicalize().unwrap())
}

struct Fake {
    model: String,
    calls: usize,
}

impl Backend for Fake {
    fn model(&self) -> &str {
        &self.model
    }

    fn embed(&mut self, _path: &Path, boxes: &[String]) -> Vec<std::result::Result<Vec<f32>, &'static str>> {
        self.calls += 1;
        boxes
            .iter()
            .map(|bbox| {
                if bbox.starts_with("bad") {
                    return Err("no_embedding");
                }
                let mut vector = vec![0.0_f32; 128];
                vector[0] = 1.0;
                Ok(vector)
            })
            .collect()
    }
}

fn stamp(path: &Path) -> (i64, f64) {
    let metadata = std::fs::metadata(path).unwrap();
    let modified = metadata.modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64();
    (metadata.len() as i64, modified)
}

fn file(conn: &Connection, id: i64, path: &Path, exists: bool) {
    let (size, modified) = if exists { stamp(path) } else { (100, 10.0) };
    conn.execute(
        "INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension) VALUES(?1,?2,?1,?3,?4,0,'image','jpg')",
        params![id, path.to_string_lossy(), size, modified],
    )
    .unwrap();
    conn.execute("INSERT INTO catalog_revisions(file_id,revision,processing_version,updated_at) VALUES(?1,'rev',?2,0)", params![id, "file-stat-v1"]).unwrap();
}

fn face(conn: &Connection, id: i64, file: i64, bbox: &str, model: Option<&str>) {
    conn.execute(
        "INSERT INTO face_prints(id,file_id,print_data,bbox,arcface_embedding,embedding_model,processing_version,source_revision) \
         VALUES(?1,?2,X'00',?3,?4,?5,'p','rev')",
        params![id, file, bbox, model.map(|_| vec![0_u8; 512]), model],
    )
    .unwrap();
}

struct Library {
    conn: Connection,
    _dir: TempDir,
}

/// Faces 1–2 in a present photo (2 fails), 3 in a photo deleted from a present
/// location, 4 on an unplugged location, 5 already current.
fn library() -> Library {
    let dir = tempdir();
    let root = dir.path().join("library");
    std::fs::create_dir_all(&root).unwrap();
    let present = root.join("a.jpg");
    std::fs::write(&present, b"jpeg").unwrap();
    let conn = memory_db();
    conn.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    conn.execute("INSERT INTO library_roots(path,display,added_at) VALUES(?1,'library',0)", [root.to_string_lossy()]).unwrap();
    let gone = dir.path().join("gone");
    conn.execute("INSERT INTO library_roots(path,display,added_at) VALUES(?1,'gone',0)", [gone.to_string_lossy()]).unwrap();
    file(&conn, 1, &present, true);
    file(&conn, 2, &root.join("deleted.jpg"), false);
    file(&conn, 3, &gone.join("b.jpg"), false);
    face(&conn, 1, 1, "ok-1", Some("old"));
    face(&conn, 2, 1, "bad-2", None);
    face(&conn, 3, 2, "ok-3", Some("old"));
    face(&conn, 4, 3, "ok-4", Some("old"));
    face(&conn, 5, 1, "ok-5", Some("m"));
    Library { conn, _dir: dir }
}

fn states(conn: &Connection) -> Vec<(i64, String)> {
    let mut statement = conn.prepare("SELECT face_id, state FROM face_refresh_backlog ORDER BY face_id").unwrap();
    statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap()
}

fn start(conn: &Connection) {
    seed(conn, "m", "p", 1.0).unwrap();
    conn.execute("UPDATE catalog_jobs SET state='running' WHERE id=?1", [JOB_ID]).unwrap();
}

#[test]
fn seeding_queues_outdated_faces_of_one_namespace() {
    let library = library();
    let conn = &library.conn;
    assert_eq!(outdated(conn, "m", "p").unwrap(), 4);
    seed(conn, "m", "p", 1.0).unwrap();
    assert_eq!(states(conn).iter().map(|(face, _)| *face).collect::<Vec<_>>(), vec![1, 2, 3, 4]);
    let job: (String, String) = conn.query_row("SELECT kind, state FROM catalog_jobs WHERE id=?1", [JOB_ID], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(job, (JOB_KIND.to_string(), "queued".to_string()));
    let status = status(conn, Some("m"), "p").unwrap();
    assert_eq!((status.total, status.pending, status.outdated, status.state.as_deref()), (4, 4, 4, Some("queued")));
    let other = super::status(conn, Some("n"), "p").unwrap();
    assert_eq!(other.total, 0, "another model has its own backlog");
}

#[test]
fn draining_records_done_missing_offline_and_failed_and_retries_on_the_next_seed() {
    let library = library();
    let conn = &library.conn;
    start(conn);
    let mut backend = Fake { model: "m".into(), calls: 0 };
    drain(conn, &mut backend).unwrap();
    assert_eq!(
        states(conn),
        vec![(1, "done".into()), (2, "failed".into()), (3, "missing".into()), (4, "offline".into())]
    );
    assert_eq!(backend.calls, 1, "one decode per present photo");
    let saved: (String, String, usize) = conn
        .query_row("SELECT embedding_model, processing_version, length(arcface_embedding) FROM face_prints WHERE id=1", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    assert_eq!(saved, ("m".into(), "p".into(), 512));
    let progress: f64 = conn.query_row("SELECT progress FROM catalog_jobs WHERE id=?1", [JOB_ID], |r| r.get(0)).unwrap();
    assert!((progress - 1.0).abs() < 1e-9);
    let attempts: i64 = conn.query_row("SELECT attempts FROM face_refresh_backlog WHERE face_id=2", [], |r| r.get(0)).unwrap();
    assert_eq!(attempts, 1);
    seed(conn, "m", "p", 2.0).unwrap();
    assert_eq!(
        states(conn),
        vec![(1, "done".into()), (2, "pending".into()), (3, "pending".into()), (4, "pending".into())]
    );
}

#[test]
fn a_paused_job_stops_between_batches_and_keeps_the_rest_pending() {
    let library = library();
    let conn = &library.conn;
    seed(conn, "m", "p", 1.0).unwrap();
    conn.execute("UPDATE catalog_jobs SET state='paused' WHERE id=?1", [JOB_ID]).unwrap();
    let error = drain(conn, &mut Fake { model: "m".into(), calls: 0 }).unwrap_err();
    assert!(error.is::<Interrupted>());
    assert!(states(conn).iter().all(|(_, state)| state == "pending"));
}

#[test]
fn a_different_model_fails_the_job_instead_of_mixing_spaces() {
    let library = library();
    let conn = &library.conn;
    start(conn);
    let error = drain(conn, &mut Fake { model: "other".into(), calls: 0 }).unwrap_err();
    assert_eq!(error.to_string(), MODEL_CHANGED);
}

#[test]
fn a_changed_photo_fails_instead_of_saving_a_stale_vector() {
    let library = library();
    let conn = &library.conn;
    start(conn);
    conn.execute("UPDATE files SET size_bytes=size_bytes+1 WHERE id=1", []).unwrap();
    drain(conn, &mut Fake { model: "m".into(), calls: 0 }).unwrap();
    let reason: (String, Option<String>) =
        conn.query_row("SELECT state, reason FROM face_refresh_backlog WHERE face_id=1", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(reason, ("failed".into(), Some("source_changed".into())));
    let model: Option<String> = conn.query_row("SELECT embedding_model FROM face_prints WHERE id=1", [], |r| r.get(0)).unwrap();
    assert_eq!(model.as_deref(), Some("old"));
}

#[test]
fn an_interrupted_job_comes_back_paused() {
    let library = library();
    let conn = &library.conn;
    start(conn);
    recover(conn).unwrap();
    let job: (String, String) = conn.query_row("SELECT state, error FROM catalog_jobs WHERE id=?1", [JOB_ID], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(job, ("paused".into(), INTERRUPTED.into()));
    assert!(is_job(conn, JOB_ID).unwrap());
}

#[test]
fn enqueue_needs_a_face_model() {
    let library = library();
    let request: CatalogRequest = serde_json::from_value(serde_json::json!({"requestID": "r", "action": "enqueueFaceRefresh"})).unwrap();
    assert_eq!(enqueue(&library.conn, &request, None).unwrap_err().to_string(), NO_MODEL);
    if !refresh_supported() {
        assert_eq!(enqueue(&library.conn, &request, Some("m")).unwrap_err().to_string(), REPORT_ONLY);
        return;
    }
    let response = enqueue(&library.conn, &request, Some("m")).unwrap();
    assert_eq!(response.jobs.len(), 1);
}

#[test]
fn the_platform_names_the_face_space() {
    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    {
        assert_eq!(processing(), MAC_PROCESSING);
        assert!(!refresh_supported());
    }
    #[cfg(not(all(target_os = "macos", feature = "apple-adapters")))]
    {
        assert_eq!(processing(), PORTABLE_PROCESSING);
        assert!(refresh_supported());
    }
}

#[test]
fn classify_tells_missing_photos_from_unreachable_locations() {
    let dir = tempdir();
    let root = dir.path().to_path_buf();
    let roots = vec![root.clone(), dir.path().join("unplugged")];
    std::fs::write(root.join("here.jpg"), b"x").unwrap();
    assert_eq!(classify(&root.join("here.jpg"), &roots), Source::Available);
    assert_eq!(classify(&root.join("gone.jpg"), &roots), Source::Missing);
    assert_eq!(classify(&dir.path().join("unplugged/a.jpg"), &roots), Source::Offline);
    assert_eq!(classify(Path::new("/Volumes/FileID-test-volume-absent/a.jpg"), &[]), Source::Offline);
    assert_eq!(classify(&root.join("x/y.jpg"), &[]), Source::Missing);
}

#[test]
fn pixel_boxes_follow_both_engines_box_formats() {
    let json = r#"{"x":10,"y":20,"w":30,"h":40,"coordinateSpace":"pixel-top-left","sourceWidth":100,"sourceHeight":200}"#;
    assert_eq!(pixel_box(json, 100, 200), Ok([10.0, 20.0, 30.0, 40.0]));
    assert_eq!(pixel_box(json, 50, 100), Err("dimensions_changed"));
    let mac = pixel_box("0.1,0.5,0.2,0.25", 100, 200).unwrap();
    assert!((mac[0] - 10.0).abs() < 1e-4 && (mac[1] - 50.0).abs() < 1e-4 && (mac[2] - 20.0).abs() < 1e-4 && (mac[3] - 50.0).abs() < 1e-4);
    assert_eq!(pixel_box("nonsense", 1, 1), Err("bad_box"));
}

#[test]
fn status_page_reads_the_backlog() {
    let library = library();
    let page = status_page(&library.conn, "r").unwrap();
    assert_eq!((page.status.as_str(), page.queue.as_deref()), ("ok", Some("refresh")));
    assert_eq!(page.refresh.unwrap().processing, processing());
}
