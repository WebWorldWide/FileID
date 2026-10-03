// Engine client — spawns the shared Rust engine and tracks scan status
// over its newline-delimited IPC protocol.

use anyhow::{Context, Result};
use async_channel::{Receiver, Sender};
use fileid_engine::ipc::{
    CancelPrewarmPayload, CommandPayload, EventPayload, IpcCommand, IpcEvent,
    ModelDownloadProgress, PrewarmModelPayload, StartScanPayload,
};
use gtk::prelude::*;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

#[derive(Debug, Clone)]
pub enum EngineState {
    Spawning,
    Ready,
    Scanning,
    ScanComplete(u64),
    BatchLanded(u64),
    FaceClusteringComplete(fileid_engine::ipc::FaceClusteringResult),
    FaceClusteringFailed(String),
    FaceClusteringBusy(String),
    BulkActionResult(fileid_engine::ipc::BulkActionResult),
    MergeSuggestions(fileid_engine::ipc::MergeSuggestions),
    ModelDownloadProgress(ModelDownloadProgress),
    Error { kind: String, message: String, model_kind: Option<String> },
    Failed(String),
    Exited,
}

pub type EngineEvent = EngineState;

pub struct QuerySpec {
    pub search: String,
    pub kind: Option<String>,
    pub limit: i64,
}

#[derive(Debug, Clone)]
pub struct FileRow {
    pub id: i64,
    pub path: String,
    pub name: String,
    pub kind: String,
    pub extension: String,
    pub size_bytes: i64,
    pub created_at: Option<f64>,
    pub modified_at: Option<f64>,
    pub has_faces: bool,
    pub has_text: bool,
    pub file_ref: Option<i64>,
    pub content_hash: Option<Vec<u8>>,
    pub description: Option<String>,
    pub proposed_name: Option<String>,
}

pub type DecodedImage = fileid_engine::shell::thumbnail::Thumbnail;

pub fn texture_from_decoded(decoded: DecodedImage) -> gtk::gdk::Texture {
    let width = decoded.width as i32;
    let height = decoded.height as i32;
    let pixels = glib::Bytes::from_owned(decoded.rgba);
    gtk::gdk::MemoryTexture::new(
        width,
        height,
        gtk::gdk::MemoryFormat::R8g8b8a8,
        &pixels,
        width as usize * 4,
    ).upcast()
}

pub struct EngineClient {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    subscribers: Arc<Mutex<Vec<Sender<EngineState>>>>,
    next_command_id: u64,
}

impl EngineClient {
    pub fn new() -> Self {
        Self {
            child: None,
            stdin: None,
            subscribers: Arc::new(Mutex::new(Vec::new())),
            next_command_id: 0,
        }
    }

    pub fn subscribe(&mut self) -> Receiver<EngineState> {
        let (tx, rx) = async_channel::unbounded();
        self.subscribers.lock().expect("subscriber lock poisoned").push(tx);
        rx
    }
    pub fn is_ready(&self) -> bool {
        self.child.is_some()
    }

    pub fn spawn(&mut self) -> Receiver<EngineState> {
        let rx = self.subscribe();
        let exe = locate_engine_binary();

        match exe {
            Ok(path) => {
                publish(&self.subscribers, EngineState::Spawning);
            match Command::new(&path)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(mut child) => {
                    self.stdin = child.stdin.take();
                    let stdout = child.stdout.take().expect("piped stdout should be present");
                    let subscribers = Arc::clone(&self.subscribers);
                    thread::spawn(move || drain_engine_stdout(BufReader::new(stdout), subscribers));
                    self.child = Some(child);
                }
                Err(err) => publish(&self.subscribers, EngineState::Failed(format!("spawn failed: {err}"))),
            }
                }
            Err(err) => publish(&self.subscribers, EngineState::Failed(format!("engine binary not found: {err}"))),
        }
        rx
    }

    /// Send a scan command for the folder selected in the file dialog.
    pub fn start_scan(&mut self, root_path: &str) -> Result<()> {
        self.send(CommandPayload::StartScan(StartScanPayload {
            root_path: root_path.to_owned(),
            root_display: None,
            rescan: false,
            excluded_paths: None,
        }))?;
        publish(&self.subscribers, EngineState::Scanning);
        Ok(())
    }

    pub fn prewarm_model(&mut self, model_kind: &str) -> Result<()> {
        self.send(CommandPayload::PrewarmModel(PrewarmModelPayload {
            model_kind: model_kind.to_owned(),
        }))
    }

    pub fn cancel_prewarm(&mut self, model_kind: &str) -> Result<()> {
        self.send(CommandPayload::CancelPrewarm(CancelPrewarmPayload {
            model_kind: Some(model_kind.to_owned()),
        }))
    }

    pub fn send(&mut self, payload: CommandPayload) -> Result<()> {
        let stdin = self.stdin.as_mut().context("engine not spawned")?;
        self.next_command_id = self.next_command_id.wrapping_add(1);
        let cmd = IpcCommand { id: format!("linux-{}", self.next_command_id), payload };
        serde_json::to_writer(&mut *stdin, &cmd)?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;
        Ok(())
    }

    pub fn query_files(&self, spec: QuerySpec) -> Receiver<Result<(Vec<FileRow>, i64)>> {
        let (tx, rx) = async_channel::bounded(1);
        thread::spawn(move || {
            let _ = tx.send_blocking(query_files_from_db(&spec));
        });
        rx
    }

    pub fn request_scaled_thumbnail(&self, path: String, dim: i32) -> Receiver<Option<DecodedImage>> {
        let (tx, rx) = async_channel::bounded(1);
        thread::spawn(move || {
            let image = fileid_engine::shell::thumbnail::render_at(Path::new(&path), dim).ok();
            let _ = tx.send_blocking(image);
        });
        rx
    }

    pub fn request_video_thumbnail(&self, path: String, dim: i32) -> Receiver<Option<DecodedImage>> {
        let (tx, rx) = async_channel::bounded(1);
        thread::spawn(move || {
            let image = fileid_engine::shell::video::keyframe_25pct(Path::new(&path)).ok()
                .filter(|frame| frame.width <= dim as u32 && frame.height <= dim as u32)
                .map(|frame| {
                    let mut rgba = Vec::with_capacity(frame.rgb.len() / 3 * 4);
                    for pixel in frame.rgb.chunks_exact(3) {
                        rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
                    }
                    DecodedImage { width: frame.width, height: frame.height, rgba }
                });
            let _ = tx.send_blocking(image);
        });
        rx
    }
}

fn publish(subscribers: &Arc<Mutex<Vec<Sender<EngineState>>>>, state: EngineState) {
    subscribers.lock().expect("subscriber lock poisoned")
        .retain(|subscriber| subscriber.send_blocking(state.clone()).is_ok());
}

fn query_files_from_db(spec: &QuerySpec) -> Result<(Vec<FileRow>, i64)> {
    let path = fileid_engine::paths::db_path()?;
    query_files_at_path(&path, spec)
}

fn query_files_at_path(path: &Path, spec: &QuerySpec) -> Result<(Vec<FileRow>, i64)> {
    let conn = fileid_engine::db::open_read(path)?;
    let search = spec.search.trim();
    let mut pattern = String::new();
    if !search.is_empty() {
        pattern.reserve(search.len() + 2);
        pattern.push('%');
        for ch in search.chars() {
            if matches!(ch, '%' | '_' | '\\') {
                pattern.push('\\');
            }
            pattern.push(ch);
        }
        pattern.push('%');
    }
    let kind = spec.kind.as_deref().unwrap_or("");
    let filter = "f.failed = 0 AND (?1 = '' OR \
        COALESCE(f.path_search, f.path_text) LIKE ?1 ESCAPE '\\' OR \
        EXISTS (SELECT 1 FROM tags t WHERE t.file_id = f.id AND t.tag LIKE ?1 ESCAPE '\\') OR \
        EXISTS (SELECT 1 FROM ocr_text o WHERE o.file_id = f.id AND o.text LIKE ?1 ESCAPE '\\') OR \
        EXISTS (SELECT 1 FROM doc_text d WHERE d.file_id = f.id AND d.text LIKE ?1 ESCAPE '\\')) \
        AND (?2 = '' OR f.kind = ?2)";
    let total: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM files f WHERE {filter}"),
        (pattern.as_str(), kind),
        |row| row.get(0),
    )?;
    let mut stmt = conn.prepare(&format!(
        "SELECT f.id, f.path_text, f.kind, f.extension, f.size_bytes, f.created_at, \
         f.modified_at, f.has_faces, f.has_text, f.file_ref, f.content_hash, \
         f.vlm_description, f.vlm_proposed_name \
         FROM files f WHERE {filter} ORDER BY f.scanned_at DESC, f.id DESC LIMIT ?3"
    ))?;
    let rows = stmt.query_map((pattern.as_str(), kind, spec.limit.clamp(1, 1000)), |row| {
        let path: String = row.get(1)?;
        let name = Path::new(&path).file_name().unwrap_or_default().to_string_lossy().into_owned();
        Ok(FileRow {
            id: row.get(0)?,
            path,
            name,
            kind: row.get(2)?,
            extension: row.get(3)?,
            size_bytes: row.get(4)?,
            created_at: row.get(5)?,
            modified_at: row.get(6)?,
            has_faces: row.get(7)?,
            has_text: row.get(8)?,
            file_ref: row.get(9)?,
            content_hash: row.get(10)?,
            description: row.get(11)?,
            proposed_name: row.get(12)?,
        })
    })?.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok((rows, total))
}


impl Drop for EngineClient {
    fn drop(&mut self) {
        self.stdin.take();
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Locate the engine binary. Search order:
///   1. `$FILEID_ENGINE` environment variable
///   2. `FileIDEngine` or `fileid-engine` alongside the app executable
///   3. `/usr/lib/FileID/FileIDEngine` (installed via .deb / Flatpak)
fn locate_engine_binary() -> Result<PathBuf> {
    if let Ok(s) = std::env::var("FILEID_ENGINE") {
        let p = PathBuf::from(s);
        if p.exists() { return Ok(p); }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for candidate in ["FileIDEngine", "fileid-engine"] {
                let p = dir.join(candidate);
                if p.exists() { return Ok(p); }
            }
        }
    }
    for sys in ["/usr/lib/FileID/FileIDEngine", "/usr/libexec/FileID/FileIDEngine"] {
        let p = PathBuf::from(sys);
        if p.exists() { return Ok(p); }
    }
    anyhow::bail!("engine binary not found (set FILEID_ENGINE or place beside the app exe)")
}

fn drain_engine_stdout(reader: impl BufRead, subscribers: Arc<Mutex<Vec<Sender<EngineState>>>>) {
    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                publish(&subscribers, EngineState::Failed(format!("engine output error: {err}")));
                publish(&subscribers, EngineState::Exited);
                return;
            }
        };
        let Ok(event) = serde_json::from_str::<IpcEvent>(&line) else {
            continue;
        };
        let state = match event.payload {
            EventPayload::Ready(_) => EngineState::Ready,
            EventPayload::BatchSummary(batch) => EngineState::BatchLanded(batch.inner.processed_total),
            EventPayload::ScanComplete(result) => EngineState::ScanComplete(result.inner.processed_files),
            EventPayload::FaceClusteringComplete(result) => EngineState::FaceClusteringComplete(result.inner),
            EventPayload::BulkActionResult(result) => EngineState::BulkActionResult(result.inner),
            EventPayload::MergeSuggestions(result) => EngineState::MergeSuggestions(result.inner),
            EventPayload::ModelDownloadProgress(progress) =>
                EngineState::ModelDownloadProgress(progress.inner),
            EventPayload::Error(error) => match error.inner.kind.as_str() {
                "face_clustering_failed" => EngineState::FaceClusteringFailed(error.inner.message),
                "face_clustering_busy" => EngineState::FaceClusteringBusy(error.inner.message),
                _ => EngineState::Error {
                    kind: error.inner.kind,
                    message: error.inner.message,
                    model_kind: error.inner.model_kind,
                },
            },
            _ => continue,
        };
        publish(&subscribers, state);
    }
    publish(&subscribers, EngineState::Failed("engine exited".to_owned()));
    publish(&subscribers, EngineState::Exited);
}

#[cfg(test)]
mod tests {
    use super::*;
    use fileid_engine::ipc::{EngineError, EngineInfo, ScanComplete, Wrap};
    use std::io::Cursor;

    #[cfg(unix)]
    #[test]
    fn selected_folder_is_sent_to_child_as_an_engine_scan_frame() {
        let root = std::env::temp_dir().join(format!("fileid-ipc-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.to_str().unwrap();
        let legacy = serde_json::json!({"cmd": "startScan", "id": "scan-1", "rootPath": root});
        assert!(serde_json::from_value::<IpcCommand>(legacy).is_err());

        let mut child = Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut client = EngineClient::new();
        client.stdin = child.stdin.take();
        client.child = Some(child);
        client.start_scan(root).unwrap();
        client.stdin.take();
        let mut lines = BufReader::new(stdout).lines();
        let frame = lines.next().unwrap().unwrap();
        assert!(lines.next().is_none());
        let command: IpcCommand = serde_json::from_str(&frame).unwrap();
        let CommandPayload::StartScan(scan) = command.payload else {
            panic!("engine did not receive a startScan command");
        };
        assert_eq!(scan.root_path, root);
        assert!(!scan.rescan);
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn reader_dispatches_actual_scan_count_and_error_without_false_ready() {
        let ready = IpcEvent::now(EventPayload::Ready(Wrap::new(EngineInfo {
            version: "0.1.0".to_owned(),
            pid: 42,
            worker_cap: 2,
            physical_memory_gb: 8.0,
            hardware: None,
        })));
        let done = IpcEvent::now(EventPayload::ScanComplete(Wrap::new(ScanComplete {
            session_id: "test-session".to_owned(),
            total_files: 20,
            processed_files: 13,
            failed_files: 7,
            total_seconds: 1.0,
        })));
        let progress = IpcEvent::now(EventPayload::ModelDownloadProgress(Wrap::new(
            ModelDownloadProgress {
                model_kind: "arcface".to_owned(),
                fraction: 0.4,
                message: "Downloading face models".to_owned(),
                bytes_done: Some(40),
                total_bytes: Some(100),
            },
        )));
        let error = IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
            kind: "models_not_installed".to_owned(),
            message: "Install models before scanning".to_owned(),
            path: None,
            model_kind: None,
        })));
        let cancelled = IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
            kind: "prewarm_cancelled".to_owned(),
            message: "Face model download cancelled".to_owned(),
            path: None,
            model_kind: Some("arcface".to_owned()),
        })));
        let stream = format!(
            "{{\"t\":\"2026-01-01T00:00:00Z\",\"payload\":{{\"futureEvent\":{{\"message\":\"ready\"}}}}}}\n{}\n{}\n{}\n{}\n{}\n",
            serde_json::to_string(&ready).unwrap(),
            serde_json::to_string(&done).unwrap(),
            serde_json::to_string(&progress).unwrap(),
            serde_json::to_string(&error).unwrap(),
            serde_json::to_string(&cancelled).unwrap()
        );
        let (tx, rx) = async_channel::unbounded();
        let (tx_other, rx_other) = async_channel::unbounded();
        drain_engine_stdout(Cursor::new(stream), Arc::new(Mutex::new(vec![tx, tx_other])));
        assert!(matches!(rx.recv_blocking().unwrap(), EngineState::Ready));
        assert!(matches!(rx.recv_blocking().unwrap(), EngineState::ScanComplete(13)));
        assert!(matches!(rx.recv_blocking().unwrap(), EngineState::ModelDownloadProgress(progress)
            if progress.model_kind == "arcface" && progress.fraction == 0.4
                && progress.bytes_done == Some(40)));
        assert!(matches!(rx.recv_blocking().unwrap(), EngineState::Error { kind, message, model_kind }
            if kind == "models_not_installed" && message == "Install models before scanning"
                && model_kind.is_none()));
        assert!(matches!(rx.recv_blocking().unwrap(), EngineState::Error { kind, model_kind, .. }
            if kind == "prewarm_cancelled" && model_kind.as_deref() == Some("arcface")));
        assert!(matches!(rx.recv_blocking().unwrap(), EngineState::Failed(message) if message == "engine exited"));
        assert!(matches!(rx_other.recv_blocking().unwrap(), EngineState::Ready));
        assert!(matches!(rx_other.recv_blocking().unwrap(), EngineState::ScanComplete(13)));
        assert!(matches!(rx_other.recv_blocking().unwrap(), EngineState::ModelDownloadProgress(progress)
            if progress.model_kind == "arcface" && progress.total_bytes == Some(100)));
    }

    #[test]
    fn library_query_filters_indexed_files_without_treating_wildcards_as_searches() {
        let fixture = std::env::temp_dir().join(format!(
            "fileid-library-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&fixture).unwrap();
        let db_path = fixture.join("fileid.sqlite");
        {
            let conn = fileid_engine::db::open_writer(&db_path).unwrap();
            for (hash, name, kind, extension, scanned) in [
                (1i64, "draft_100%.txt", "doc", "txt", 1f64),
                (2, "image.png", "image", "png", 3f64),
                (3, "notes.pdf", "pdf", "pdf", 2f64),
            ] {
                let path = fixture.join(name);
                std::fs::write(&path, b"generated fixture").unwrap();
                conn.execute(
                    "INSERT INTO files (path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES (?1,?2,17,?3,?4,?5)",
                    (path.to_str().unwrap(), hash, scanned, kind, extension),
                ).unwrap();
            }
            conn.execute(
                "INSERT INTO files (path_text,path_hash,size_bytes,scanned_at,kind,extension,failed) VALUES (?1,4,17,4,'image','png',1)",
                [fixture.join("failed.png").to_str().unwrap()],
            ).unwrap();
            conn.execute("INSERT INTO tags (file_id,tag,source) VALUES (1,'urgent','manual')", []).unwrap();
            conn.execute("INSERT INTO ocr_text (file_id,text) VALUES (3,'needle in OCR')", []).unwrap();
        }
        let query = |search: &str, kind: Option<&str>, limit: i64| {
            query_files_at_path(&db_path, &QuerySpec {
                search: search.to_owned(),
                kind: kind.map(str::to_owned),
                limit,
            }).unwrap()
        };
        let (first, total) = query("", None, 2);
        assert_eq!(total, 3);
        assert_eq!(first.iter().map(|file| file.name.as_str()).collect::<Vec<_>>(), ["image.png", "notes.pdf"]);
        let (literal, literal_total) = query("100%", None, 10);
        assert_eq!(literal_total, 1);
        assert_eq!(literal[0].name, "draft_100%.txt");
        assert_eq!(query("urgent", None, 10).1, 1);
        assert_eq!(query("needle", None, 10).0[0].kind, "pdf");
        assert_eq!(query("", Some("image"), 10).1, 1);
        assert!(query_files_at_path(&fixture.join("missing.sqlite"), &QuerySpec {
            search: String::new(), kind: None, limit: 10,
        }).is_err());
        std::fs::remove_dir_all(fixture).unwrap();
    }
}
