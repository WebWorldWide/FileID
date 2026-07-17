// Engine client — the app side of the two-binary IPC design
// (`shared/docs/ARCHITECTURE.md`). Mirrors macOS `EngineClient` + Windows
// `EngineClient.cs`:
//
//   * spawns the shared Rust engine (`fileid-engine`) as a child process,
//   * sends `IpcCommand`s as newline-delimited JSON on its stdin — reusing the
//     engine crate's own serde types so the wire contract can never drift,
//   * parses `IpcEvent`s off a reader thread and fans them out to every UI
//     subscriber on the GTK main context (async-channel → glib),
//   * respawns the engine with backoff if it dies.
//
// File listing is NOT an IPC command — the engine is the single DB *writer*;
// the app reads file rows directly from the same SQLite WAL DB, exactly like
// macOS `ReadStore` / Windows `ReadStore`. We reuse the engine crate's
// `db::open_read` + `paths::db_path` so the schema + location can't drift.
//
// Thumbnails are produced client-side and fully off the GTK main loop by a
// bounded fixed-size worker pool. Jobs retain only paths/decoder closures while
// queued; encoded and decoded buffers exist only in active workers. Only the
// decoded pixel `glib::Bytes` (which IS `Send`) cross back to the main thread,
// where a `gdk::MemoryTexture` is built (GTK objects are main-thread-only).

use anyhow::{Context, Result};
use async_channel::{Receiver, Sender};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use fileid_engine::ipc::{
    CommandPayload, EventPayload, IpcCommand, IpcEvent, ScanProgress, StartScanPayload,
};

// ─── Public event surface ────────────────────────────────────────────────────

/// The slice of `IpcEvent` the UI cares about, already unwrapped from the wire
/// envelope. Delivered on the GTK main context.
#[derive(Debug, Clone)]
pub enum EngineEvent {
    Spawning,
    Ready,
    Progress(ScanProgress),
    /// A batch landed — carries the running processed-file total. The Library
    /// uses this to throttle live grid reloads during a scan.
    BatchLanded(u64),
    /// Terminal: scan finished with this many processed files.
    ScanComplete(u64),
    Error(String),
    ModelDownloadFailed {
        model_kind: String,
        message: String,
    },
    /// The engine process exited (crash or clean EOF). Triggers a respawn.
    Exited,

    // ── Deep Analyze lifecycle (consumed by the Deep Analyze tab) ────────────
    DeepAnalyzeStarting(fileid_engine::ipc::DeepAnalyzeStarting),
    DeepAnalyzeProgress(fileid_engine::ipc::DeepAnalyzeProgress),
    DeepAnalyzeFileDone(fileid_engine::ipc::DeepAnalyzeFileDone),
    DeepAnalyzeComplete(fileid_engine::ipc::DeepAnalyzeComplete),
    /// VLM / model-weight download progress (Deep Analyze + Settings).
    ModelDownloadProgress(fileid_engine::ipc::ModelDownloadProgress),

    // ── Restructure (consumed by the Restructure tab) ────────────────────────
    /// Authoritative plan from `planRestructure`.
    RestructurePlan(fileid_engine::ipc::RestructurePlan),
    /// Result of `applyRestructure` (applied / failed counts, privilege error).
    RestructureApplyResult(fileid_engine::ipc::RestructureApplyResult),
    BulkActionResult(fileid_engine::ipc::BulkActionResult),
}

/// A file row read from the DB. The app-side mirror of macOS `FileRow` /
/// Windows `FileRow`, populated from the engine's `files` table.
#[derive(Debug, Clone)]
pub struct FileRow {
    pub id: i64,
    pub path: String,
    pub name: String,
    pub size_bytes: i64,
    pub kind: String,
    pub extension: String,
    pub created_at: Option<f64>,
    pub modified_at: Option<f64>,
    pub file_ref: Option<i64>,
    pub content_hash: Option<Vec<u8>>,
    pub has_faces: bool,
    pub has_text: bool,
    pub proposed_name: Option<String>,
    pub description: Option<String>,
}

/// Parameters for a Library query. `search` empty = browse-all.
#[derive(Debug, Clone, Default)]
pub struct QuerySpec {
    pub search: String,
    pub kind: Option<String>,
    pub limit: i64,
}

const RESPAWN_CAP: u32 = 5;
const THUMB_QUEUE_CAP: usize = 64;
const THUMB_WORKERS: usize = 4;
const RAW_EVENT_CAP: usize = 8;
const SUBSCRIBER_EVENT_CAP: usize = 4;
const MAX_ENGINE_FRAME_BYTES: usize = 8 * 1024 * 1024;
const MAX_ENGINE_LOG_BYTES: usize = 1024 * 1024;

// ─── Engine client ───────────────────────────────────────────────────────────

pub struct EngineClient {
    child: Option<Child>,
    stdin: Option<Arc<Mutex<ChildStdin>>>,
    /// Raw events from the reader thread(s) → the main-context fan-out pump.
    raw_tx: Sender<EngineEvent>,
    raw_rx: Option<Receiver<EngineEvent>>,
    /// One Sender per UI subscriber; the pump clones each event to all of them.
    subscribers: Vec<Sender<EngineEvent>>,
    /// Thumbnail worker request channel.
    thumb_tx: Option<Sender<ThumbJob>>,
    next_id: u64,
    respawns: u32,
    models_busy: bool,
    /// Set on drop so the reader thread's EOF doesn't trigger a respawn.
    shutting_down: Arc<AtomicBool>,
}

impl EngineClient {
    pub fn new() -> Self {
        let (raw_tx, raw_rx) = async_channel::bounded::<EngineEvent>(RAW_EVENT_CAP);
        Self {
            child: None,
            stdin: None,
            raw_tx,
            raw_rx: Some(raw_rx),
            subscribers: Vec::new(),
            thumb_tx: None,
            next_id: 0,
            respawns: 0,
            models_busy: false,
            shutting_down: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn models_busy(&self) -> bool {
        self.models_busy
    }

    pub fn shutdown(&mut self) {
        if self.shutting_down.swap(true, Ordering::AcqRel) {
            return;
        }
        self.models_busy = false;
        self.subscribers.clear();
        self.thumb_tx.take();
        self.stdin.take();
        if let Some(mut child) = self.child.take() {
            thread::spawn(move || {
                let _ = child.kill();
                let _ = child.wait();
            });
        }
    }

    /// Register a UI subscriber. Each call returns a fresh receiver that will
    /// see every event from now on. Call before or after `start` — late
    /// subscribers still receive future events.
    pub fn subscribe(&mut self) -> Receiver<EngineEvent> {
        let (tx, rx) = async_channel::bounded::<EngineEvent>(SUBSCRIBER_EVENT_CAP);
        self.subscribers.push(tx);
        rx
    }

    /// Boot the engine + the thumbnail worker, and start the main-context
    /// fan-out pump. Takes the shared `Rc<RefCell<Self>>` so the pump can drive
    /// respawns on its own. Idempotent-ish: call once after construction.
    pub fn start(this: &std::rc::Rc<std::cell::RefCell<Self>>) {
        let (thumb_tx, thumb_rx) = async_channel::bounded::<ThumbJob>(THUMB_QUEUE_CAP);
        for _ in 0..THUMB_WORKERS {
            let worker_rx = thumb_rx.clone();
            thread::spawn(move || thumbnail_worker(worker_rx));
        }

        // Reader side: take the raw receiver out for the pump, keep a tx clone.
        let raw_rx = {
            let mut e = this.borrow_mut();
            e.thumb_tx = Some(thumb_tx);
            e.raw_rx.take().expect("EngineClient::start called twice")
        };

        // Spawn the engine process for the first time.
        spawn_process(this);

        // Fan-out pump on the GTK main context: raw → all subscribers, and
        // drive respawn on Exited.
        let this_pump = std::rc::Rc::downgrade(this);
        glib::MainContext::default().spawn_local(async move {
            while let Ok(ev) = raw_rx.recv().await {
                let Some(client) = this_pump.upgrade() else {
                    break;
                };
                match &ev {
                    EngineEvent::Progress(_)
                    | EngineEvent::BatchLanded(_)
                    | EngineEvent::DeepAnalyzeStarting(_) => {
                        client.borrow_mut().models_busy = true;
                    }
                    EngineEvent::ScanComplete(_)
                    | EngineEvent::DeepAnalyzeComplete(_)
                    | EngineEvent::Exited => {
                        client.borrow_mut().models_busy = false;
                    }
                    _ => {}
                }
                let subs: Vec<Sender<EngineEvent>> = client.borrow().subscribers.clone();
                drop(client);
                for sub in &subs {
                    let _ = sub.send(ev.clone()).await;
                }
                let Some(client) = this_pump.upgrade() else {
                    break;
                };
                if let EngineEvent::Ready = ev {
                    client.borrow_mut().respawns = 0;
                }
                if let EngineEvent::Exited = ev {
                    let shutting = client.borrow().shutting_down.load(Ordering::Relaxed);
                    if !shutting {
                        retire_exited_child(&client);
                        schedule_respawn(&client);
                    }
                }
            }
        });
    }

    /// Send `startScan` to the engine using the real `IpcCommand` types so the
    /// wire shape matches the engine byte-for-byte.
    pub fn start_scan(&mut self, root_path: &str, rescan: bool) -> Result<()> {
        let payload = CommandPayload::StartScan(StartScanPayload {
            root_path: root_path.to_string(),
            root_display: None,
            rescan,
            excluded_paths: None,
        });
        let result = self.send(payload);
        if result.is_ok() {
            self.models_busy = true;
        }
        result
    }

    /// Serialize + write an arbitrary command as one NDJSON line.
    pub fn send(&mut self, payload: CommandPayload) -> Result<()> {
        self.next_id += 1;
        let cmd = IpcCommand {
            id: format!("lin-{}", self.next_id),
            payload,
        };
        let line = serde_json::to_string(&cmd)? + "\n";
        let stdin = self.stdin.as_ref().context("engine not spawned")?.clone();
        let mut guard = stdin.lock().expect("engine stdin poisoned");
        guard.write_all(line.as_bytes())?;
        guard.flush()?;
        Ok(())
    }

    /// Run a Library query off the main loop. Returns a oneshot receiver the
    /// caller awaits via `spawn_local`. Each query opens a fresh read-only
    /// connection (cheap, WAL-safe, and tolerant of the DB not existing yet).
    pub fn query_files(&self, spec: QuerySpec) -> Receiver<Vec<FileRow>> {
        let (tx, rx) = async_channel::bounded::<Vec<FileRow>>(1);
        thread::spawn(move || {
            let rows = run_query(&spec).unwrap_or_default();
            let _ = tx.send_blocking(rows);
        });
        rx
    }

    pub fn request_scaled_thumbnail(
        &self,
        path: String,
        max_px: i32,
    ) -> Receiver<Option<DecodedImage>> {
        self.request_thumbnail_with(path, move |bytes| decode_scaled(bytes, max_px))
    }

    pub fn request_thumbnail_with<F>(
        &self,
        path: String,
        decoder: F,
    ) -> Receiver<Option<DecodedImage>>
    where
        F: FnOnce(Vec<u8>) -> Option<DecodedImage> + Send + 'static,
    {
        let (reply, rx) = async_channel::bounded::<Option<DecodedImage>>(1);
        let job = ThumbJob {
            path,
            decoder: Box::new(decoder),
            reply,
        };
        match &self.thumb_tx {
            Some(sender) => match sender.try_send(job) {
                Ok(()) => {}
                Err(async_channel::TrySendError::Full(job))
                | Err(async_channel::TrySendError::Closed(job)) => {
                    let _ = job.reply.try_send(None);
                }
            },
            None => {
                let _ = job.reply.try_send(None);
            }
        }
        rx
    }
}

impl Drop for EngineClient {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ─── Process lifecycle ───────────────────────────────────────────────────────

/// Spawn (or respawn) the engine child and wire its stdout to a reader thread
/// feeding `raw_tx`. Updates `child` / `stdin` on the shared client.
fn spawn_process(this: &std::rc::Rc<std::cell::RefCell<EngineClient>>) {
    let raw_tx = this.borrow().raw_tx.clone();
    let _ = raw_tx.send_blocking(EngineEvent::Spawning);

    let exe = match locate_engine_binary() {
        Ok(p) => p,
        Err(err) => {
            let _ = raw_tx.send_blocking(EngineEvent::Error(format!(
                "engine binary not found: {err}"
            )));
            schedule_respawn(this);
            return;
        }
    };

    match Command::new(&exe)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(mut child) => {
            let stdin = child.stdin.take().expect("piped stdin present");
            let stdout = child.stdout.take().expect("piped stdout present");
            let stderr = child.stderr.take();
            {
                let mut e = this.borrow_mut();
                e.stdin = Some(Arc::new(Mutex::new(stdin)));
                e.child = Some(child);
            }
            let tx_reader = raw_tx.clone();
            thread::spawn(move || drain_stdout(stdout, tx_reader));
            // Drain stderr so a chatty engine can't fill the pipe and block on
            // a write. Engine diagnostics land in the local debug log only.
            if let Some(stderr) = stderr {
                thread::spawn(move || drain_stderr(stderr));
            }
        }
        Err(err) => {
            let _ = raw_tx.send_blocking(EngineEvent::Error(format!("spawn failed: {err}")));
            schedule_respawn(this);
        }
    }
}

/// Close the dead process' stdin and reap it off the GTK thread. Dropping a
/// `std::process::Child` does not wait on Unix, so overwriting the slot during a
/// respawn leaked one zombie per crash.
fn retire_exited_child(this: &std::rc::Rc<std::cell::RefCell<EngineClient>>) {
    let child = {
        let mut client = this.borrow_mut();
        client.stdin.take();
        client.child.take()
    };
    if let Some(mut child) = child {
        thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

/// Respawn after exit with a capped, linearly-backed-off delay.
fn schedule_respawn(this: &std::rc::Rc<std::cell::RefCell<EngineClient>>) {
    let n = {
        let mut e = this.borrow_mut();
        e.respawns += 1;
        e.respawns
    };
    if n > RESPAWN_CAP {
        let _ = this.borrow().raw_tx.send_blocking(EngineEvent::Error(
            "engine crashed repeatedly — giving up. Restart the app.".into(),
        ));
        return;
    }
    let delay = std::time::Duration::from_millis(400 * n as u64);
    let weak = std::rc::Rc::downgrade(this);
    glib::timeout_add_local_once(delay, move || {
        if let Some(client) = weak.upgrade() {
            let shutting = client.borrow().shutting_down.load(Ordering::Relaxed);
            if !shutting {
                spawn_process(&client);
            }
        }
    });
}

/// Locate the engine binary. Search order mirrors the scaffold:
///   1. `$FILEID_ENGINE`
///   2. next to the app binary (dev / staged)
///   3. system install dirs (.deb / Flatpak)
fn locate_engine_binary() -> Result<PathBuf> {
    if let Some(s) = std::env::var_os("FILEID_ENGINE").filter(|value| !value.is_empty()) {
        let p = PathBuf::from(s);
        if p.is_file() {
            return Ok(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for candidate in ["FileIDEngine", "fileid-engine"] {
                let p = dir.join(candidate);
                if p.is_file() {
                    return Ok(p);
                }
            }
        }
    }
    for sys in [
        "/usr/lib/FileID/FileIDEngine",
        "/usr/libexec/FileID/FileIDEngine",
        "/usr/lib/FileID/fileid-engine",
    ] {
        let p = PathBuf::from(sys);
        if p.is_file() {
            return Ok(p);
        }
    }
    anyhow::bail!("set FILEID_ENGINE or place the engine beside the app binary")
}

enum BoundedLine {
    Eof,
    Line,
    Oversize,
}

fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    max_bytes: usize,
) -> std::io::Result<BoundedLine> {
    buffer.clear();
    let mut draining = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(if draining {
                BoundedLine::Oversize
            } else if buffer.is_empty() {
                BoundedLine::Eof
            } else {
                BoundedLine::Line
            });
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(available.len(), |index| index);
        if draining || buffer.len().saturating_add(take) > max_bytes {
            draining = true;
        } else {
            buffer.extend_from_slice(&available[..take]);
        }
        let consumed = take + usize::from(newline.is_some());
        reader.consume(consumed);
        if newline.is_some() {
            if draining {
                buffer.clear();
                return Ok(BoundedLine::Oversize);
            }
            if buffer.last() == Some(&b'\r') {
                buffer.pop();
            }
            return Ok(BoundedLine::Line);
        }
    }
}

/// Reader thread: parse bounded NDJSON `IpcEvent`s into `EngineEvent`s.
fn drain_stdout(stdout: std::process::ChildStdout, tx: Sender<EngineEvent>) {
    let mut reader = BufReader::new(stdout);
    let mut frame = Vec::with_capacity(8 * 1024);
    loop {
        match read_bounded_line(&mut reader, &mut frame, MAX_ENGINE_FRAME_BYTES) {
            Ok(BoundedLine::Eof) | Err(_) => break,
            Ok(BoundedLine::Oversize) => {
                if tx
                    .send_blocking(EngineEvent::Error(format!(
                        "engine response exceeded the {} MiB safety limit and was discarded",
                        MAX_ENGINE_FRAME_BYTES / (1024 * 1024)
                    )))
                    .is_err()
                {
                    return;
                }
                continue;
            }
            Ok(BoundedLine::Line) => {}
        }
        if frame.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let Ok(event) = serde_json::from_slice::<IpcEvent>(&frame) else {
            continue;
        };
        let mapped = match event.payload {
            EventPayload::Ready(_) => Some(EngineEvent::Ready),
            EventPayload::Progress(w) => Some(EngineEvent::Progress(w.inner)),
            EventPayload::BatchSummary(w) => {
                Some(EngineEvent::BatchLanded(w.inner.processed_total))
            }
            EventPayload::ScanComplete(w) => {
                Some(EngineEvent::ScanComplete(w.inner.processed_files))
            }
            EventPayload::Error(w) => match w.inner.model_kind {
                Some(model_kind) => Some(EngineEvent::ModelDownloadFailed {
                    model_kind,
                    message: w.inner.message,
                }),
                None => Some(EngineEvent::Error(w.inner.message)),
            },
            EventPayload::DeepAnalyzeStarting(w) => Some(EngineEvent::DeepAnalyzeStarting(w.inner)),
            EventPayload::DeepAnalyzeProgress(w) => Some(EngineEvent::DeepAnalyzeProgress(w.inner)),
            EventPayload::DeepAnalyzeFileDone(w) => Some(EngineEvent::DeepAnalyzeFileDone(w.inner)),
            EventPayload::DeepAnalyzeComplete(w) => Some(EngineEvent::DeepAnalyzeComplete(w.inner)),
            EventPayload::ModelDownloadProgress(w) => {
                Some(EngineEvent::ModelDownloadProgress(w.inner))
            }
            EventPayload::RestructurePlan(w) => Some(EngineEvent::RestructurePlan(w.inner)),
            EventPayload::RestructureApplyResult(w) => {
                Some(EngineEvent::RestructureApplyResult(w.inner))
            }
            EventPayload::BulkActionResult(w) => Some(EngineEvent::BulkActionResult(w.inner)),
            _ => None,
        };
        if let Some(ev) = mapped {
            if tx.send_blocking(ev).is_err() {
                return;
            }
        }
    }
    let _ = tx.send_blocking(EngineEvent::Exited);
}

/// Drain bounded engine stderr lines to the local debug log. Never transmits.
fn drain_stderr(stderr: std::process::ChildStderr) {
    let mut reader = BufReader::new(stderr);
    let mut frame = Vec::with_capacity(4 * 1024);
    loop {
        match read_bounded_line(&mut reader, &mut frame, MAX_ENGINE_LOG_BYTES) {
            Ok(BoundedLine::Eof) | Err(_) => break,
            Ok(BoundedLine::Oversize) => {
                tracing::warn!(target: "engine_stderr", "oversized engine log line discarded");
            }
            Ok(BoundedLine::Line) => {
                let line = String::from_utf8_lossy(&frame);
                tracing::debug!(target: "engine_stderr", "{line}");
            }
        }
    }
}

// ─── DB reads (mirror of macOS/Windows ReadStore) ────────────────────────────

const SELECT_COLS: &str = "id, path_text, size_bytes, created_at, modified_at, kind, extension, \
    has_faces, has_text, vlm_proposed_name, vlm_description, file_ref, content_hash";

fn run_query(spec: &QuerySpec) -> Result<Vec<FileRow>> {
    let db_path = fileid_engine::paths::db_path()?;
    if !db_path.exists() {
        return Ok(Vec::new()); // no scan yet
    }
    let conn = fileid_engine::db::open_read(&db_path)?;

    let trimmed = spec.search.trim();
    let has_search = !trimmed.is_empty();
    let like = format!("%{}%", escape_like(trimmed));
    let fts = fts_match(trimmed);
    let has_fts = !fts.is_empty();
    let limit = if spec.limit > 0 { spec.limit } else { 500 };

    let sql = format!(
        "SELECT {cols} FROM files f \
         WHERE f.failed = 0 \
           AND ( :has_search = 0 \
                 OR COALESCE(f.path_search, f.path_text) LIKE :like ESCAPE '\\' \
                 OR EXISTS (SELECT 1 FROM tags t WHERE t.file_id = f.id AND t.tag LIKE :like ESCAPE '\\') \
                 OR ( :has_fts = 1 AND ( \
                        f.id IN (SELECT rowid FROM ocr_fts WHERE ocr_fts MATCH :fts) \
                        OR f.id IN (SELECT rowid FROM doc_fts WHERE doc_fts MATCH :fts) \
                    ) ) ) \
           AND ( :kind IS NULL OR f.kind = :kind ) \
         ORDER BY f.scanned_at DESC, f.id DESC \
         LIMIT :limit",
        cols = SELECT_COLS
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(
            rusqlite::named_params! {
                ":has_search": has_search as i64,
                ":like": like,
                ":has_fts": has_fts as i64,
                ":fts": fts,
                ":kind": spec.kind,
                ":limit": limit,
            },
            map_row,
        )?
        .collect::<rusqlite::Result<Vec<FileRow>>>()?;
    Ok(rows)
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileRow> {
    let path: String = row.get(1)?;
    let name = std::path::Path::new(&path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.clone());
    Ok(FileRow {
        id: row.get(0)?,
        path,
        name,
        size_bytes: row.get(2)?,
        created_at: row.get(3)?,
        modified_at: row.get(4)?,
        kind: row.get(5)?,
        extension: row.get(6)?,
        has_faces: row.get::<_, i64>(7)? != 0,
        has_text: row.get::<_, i64>(8)? != 0,
        proposed_name: row.get(9)?,
        description: row.get(10)?,
        file_ref: row.get(11)?,
        content_hash: row.get(12)?,
    })
}

/// Escape `%`, `_` and `\` for a `LIKE … ESCAPE '\'` pattern.
fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '%' || c == '_' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Build a safe FTS5 MATCH expression: quote each alphanumeric token so user
/// punctuation can't produce a malformed-MATCH error. Empty when no usable
/// tokens (the caller then skips the FTS subquery).
fn fts_match(s: &str) -> String {
    let tokens: Vec<String> = s
        .split_whitespace()
        .map(|t| {
            t.chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
        })
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{t}\""))
        .collect();
    tokens.join(" ")
}

// ─── Thumbnail worker ────────────────────────────────────────────────────────

type ThumbnailDecoder = Box<dyn FnOnce(Vec<u8>) -> Option<DecodedImage> + Send + 'static>;

struct ThumbJob {
    path: String,
    decoder: ThumbnailDecoder,
    reply: Sender<Option<DecodedImage>>,
}

/// Cap the bytes we'll read for a thumbnail so a stray multi-GB file can't
/// balloon memory. Anything larger falls back to the icon placeholder.
const THUMB_MAX_BYTES: u64 = 48 * 1024 * 1024;

fn thumbnail_worker(rx: Receiver<ThumbJob>) {
    while let Ok(job) = rx.recv_blocking() {
        let decoded = read_capped(&job.path).and_then(job.decoder);
        let _ = job.reply.send_blocking(decoded);
    }
}

fn read_capped(path: &str) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    if length > THUMB_MAX_BYTES {
        return None;
    }
    let mut bytes = Vec::with_capacity(length as usize);
    file.take(THUMB_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= THUMB_MAX_BYTES).then_some(bytes)
}

// ─── Off-main-thread image decode ────────────────────────────────────────────
//
// `gdk_pixbuf::Pixbuf` and `gdk::Texture` are not `Send`, so the decode runs
// entirely on a worker thread (gdk-pixbuf + gio memory streams are thread-safe
// and need no GTK init) and only the raw pixel `glib::Bytes` — which IS `Send` —
// plus its geometry cross back to the main thread, where a `gdk::MemoryTexture`
// is built. This keeps JPEG/PNG decode (the expensive part) off the GTK main
// loop so the grid never stutters while thumbnails stream in.

/// Decoded pixels, safe to move across threads (`glib::Bytes` is `Send + Sync`).
pub struct DecodedImage {
    bytes: glib::Bytes,
    width: i32,
    height: i32,
    rowstride: i32,
    has_alpha: bool,
}

impl DecodedImage {
    /// Snapshot a decoded pixbuf's pixels. Callable from any thread. For a
    /// loaded (mutable) pixbuf, `read_pixel_bytes` returns a self-contained,
    /// read-only copy that outlives the pixbuf — so the result is safe to send.
    pub fn from_pixbuf(pb: &gtk::gdk_pixbuf::Pixbuf) -> Self {
        DecodedImage {
            width: pb.width(),
            height: pb.height(),
            rowstride: pb.rowstride(),
            has_alpha: pb.has_alpha(),
            bytes: pb.read_pixel_bytes(),
        }
    }
}

/// Decode + scale image bytes to fit `max_px` on the longest side. Designed to
/// run on a worker thread (creates no widgets, touches no main-loop state).
pub fn decode_scaled(file_bytes: Vec<u8>, max_px: i32) -> Option<DecodedImage> {
    let gbytes = glib::Bytes::from_owned(file_bytes);
    let stream = gio::MemoryInputStream::from_bytes(&gbytes);
    let pixbuf = gtk::gdk_pixbuf::Pixbuf::from_stream_at_scale(
        &stream,
        max_px,
        max_px,
        true,
        gio::Cancellable::NONE,
    )
    .ok()?;
    // Apply the EXIF orientation tag so portrait photos aren't shown sideways
    // (from_stream_at_scale ignores it). Falls back to the raw pixbuf if there's
    // no orientation to apply.
    let pixbuf = pixbuf.apply_embedded_orientation().unwrap_or(pixbuf);
    Some(DecodedImage::from_pixbuf(&pixbuf))
}

/// Build a GPU texture from decoded pixels. MAIN THREAD ONLY (GTK object).
/// GdkPixbuf decodes to packed 8-bit RGB / RGBA in byte order, matching
/// `R8g8b8` / `R8g8b8a8`; the pixbuf `rowstride` is honored so padded rows are
/// safe.
pub fn texture_from_decoded(d: &DecodedImage) -> gtk::gdk::MemoryTexture {
    let format = if d.has_alpha {
        gtk::gdk::MemoryFormat::R8g8b8a8
    } else {
        gtk::gdk::MemoryFormat::R8g8b8
    };
    gtk::gdk::MemoryTexture::new(d.width, d.height, format, &d.bytes, d.rowstride as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn no_op_job(path: String) -> (ThumbJob, Receiver<Option<DecodedImage>>) {
        let (reply, rx) = async_channel::bounded(1);
        (
            ThumbJob {
                path,
                decoder: Box::new(|_| None),
                reply,
            },
            rx,
        )
    }

    #[test]
    fn engine_frame_reader_rejects_oversize_and_resynchronizes() {
        let input = b"123456\nok\n";
        let mut reader = std::io::Cursor::new(input);
        let mut frame = Vec::new();
        assert!(matches!(
            read_bounded_line(&mut reader, &mut frame, 4).unwrap(),
            BoundedLine::Oversize
        ));
        assert!(matches!(
            read_bounded_line(&mut reader, &mut frame, 4).unwrap(),
            BoundedLine::Line
        ));
        assert_eq!(frame, b"ok");
    }

    #[test]
    fn engine_frame_reader_bounds_unterminated_input() {
        let mut reader = std::io::Cursor::new(vec![b'x'; 128]);
        let mut frame = Vec::new();
        assert!(matches!(
            read_bounded_line(&mut reader, &mut frame, 16).unwrap(),
            BoundedLine::Oversize
        ));
        assert!(frame.is_empty());
    }

    #[test]
    #[allow(clippy::assertions_on_constants)] // intentional compile-time bound checks
    fn engine_event_channels_are_bounded() {
        assert!(RAW_EVENT_CAP > 0 && RAW_EVENT_CAP <= 8);
        assert!(SUBSCRIBER_EVENT_CAP > 0 && SUBSCRIBER_EVENT_CAP <= 4);
    }

    #[test]
    fn thumbnail_queue_has_a_hard_admission_bound() {
        let (sender, _receiver) = async_channel::bounded(THUMB_QUEUE_CAP);
        for index in 0..THUMB_QUEUE_CAP {
            let (job, _reply) = no_op_job(index.to_string());
            assert!(sender.try_send(job).is_ok());
        }
        let (overflow, _reply) = no_op_job("overflow".into());
        assert!(matches!(
            sender.try_send(overflow),
            Err(async_channel::TrySendError::Full(_))
        ));
    }

    #[test]
    fn thumbnail_decode_concurrency_never_exceeds_worker_count() {
        let path = std::env::temp_dir().join(format!(
            "fileid-linux-thumb-pool-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, b"fixture").unwrap();
        let (sender, receiver) = async_channel::bounded(THUMB_QUEUE_CAP);
        let workers: Vec<_> = (0..THUMB_WORKERS)
            .map(|_| {
                let receiver = receiver.clone();
                thread::spawn(move || thumbnail_worker(receiver))
            })
            .collect();
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut replies = Vec::new();
        for _ in 0..(THUMB_WORKERS * 4) {
            let active = active.clone();
            let peak = peak.clone();
            let (reply, rx) = async_channel::bounded(1);
            replies.push(rx);
            sender
                .send_blocking(ThumbJob {
                    path: path.to_string_lossy().into_owned(),
                    decoder: Box::new(move |_| {
                        let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        thread::sleep(Duration::from_millis(10));
                        active.fetch_sub(1, Ordering::SeqCst);
                        None
                    }),
                    reply,
                })
                .unwrap();
        }
        drop(sender);
        for reply in replies {
            assert!(reply.recv_blocking().unwrap().is_none());
        }
        for worker in workers {
            worker.join().unwrap();
        }
        assert!(peak.load(Ordering::SeqCst) <= THUMB_WORKERS);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn capped_reader_rejects_oversized_sparse_file() {
        let path = std::env::temp_dir().join(format!(
            "fileid-linux-thumb-cap-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(THUMB_MAX_BYTES + 1).unwrap();
        drop(file);
        assert!(read_capped(path.to_str().unwrap()).is_none());
        let _ = std::fs::remove_file(path);
    }
}
