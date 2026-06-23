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
// Thumbnails are produced client-side and fully off the GTK main loop: a worker
// thread reads raw image bytes, the call site hands those to a short-lived
// decode thread (`decode_scaled` / the off-main-thread decode section below),
// and only the decoded pixel `glib::Bytes` (which IS `Send`) cross back to the
// main thread, where a `gdk::MemoryTexture` is built (GTK objects are
// main-thread-only).

use anyhow::{Context, Result};
use async_channel::{Receiver, Sender};
use std::io::{BufRead, BufReader, Write};
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
    /// Set on drop so the reader thread's EOF doesn't trigger a respawn.
    shutting_down: Arc<AtomicBool>,
}

impl EngineClient {
    pub fn new() -> Self {
        let (raw_tx, raw_rx) = async_channel::unbounded::<EngineEvent>();
        Self {
            child: None,
            stdin: None,
            raw_tx,
            raw_rx: Some(raw_rx),
            subscribers: Vec::new(),
            thumb_tx: None,
            next_id: 0,
            respawns: 0,
            shutting_down: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Register a UI subscriber. Each call returns a fresh receiver that will
    /// see every event from now on. Call before or after `start` — late
    /// subscribers still receive future events.
    pub fn subscribe(&mut self) -> Receiver<EngineEvent> {
        let (tx, rx) = async_channel::unbounded::<EngineEvent>();
        self.subscribers.push(tx);
        rx
    }

    /// Boot the engine + the thumbnail worker, and start the main-context
    /// fan-out pump. Takes the shared `Rc<RefCell<Self>>` so the pump can drive
    /// respawns on its own. Idempotent-ish: call once after construction.
    pub fn start(this: &std::rc::Rc<std::cell::RefCell<Self>>) {
        // Thumbnail worker.
        let (thumb_tx, thumb_rx) = async_channel::unbounded::<ThumbJob>();
        thread::spawn(move || thumbnail_worker(thumb_rx));

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
        let this_pump = this.clone();
        glib::MainContext::default().spawn_local(async move {
            while let Ok(ev) = raw_rx.recv().await {
                // Snapshot subscribers WITHOUT holding the RefCell borrow across
                // the await below (a held borrow + await = a latent panic).
                let subs: Vec<Sender<EngineEvent>> =
                    this_pump.borrow().subscribers.clone();
                for sub in &subs {
                    let _ = sub.send(ev.clone()).await;
                }
                if let EngineEvent::Ready = ev {
                    // A confirmed healthy (re)start clears the crash budget, so the
                    // cap means "5 consecutive failed respawns", not "5 for the
                    // whole session" — otherwise five crashes hours apart, each
                    // recovered, would permanently give up on the engine.
                    this_pump.borrow_mut().respawns = 0;
                }
                if let EngineEvent::Exited = ev {
                    let shutting = this_pump.borrow().shutting_down.load(Ordering::Relaxed);
                    if !shutting {
                        schedule_respawn(&this_pump);
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
        });
        self.send(payload)
    }

    /// Serialize + write an arbitrary command as one NDJSON line.
    pub fn send(&mut self, payload: CommandPayload) -> Result<()> {
        self.next_id += 1;
        let cmd = IpcCommand {
            id: format!("lin-{}", self.next_id),
            payload,
        };
        let line = serde_json::to_string(&cmd)? + "\n";
        let stdin = self
            .stdin
            .as_ref()
            .context("engine not spawned")?
            .clone();
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

    /// Request raw file bytes for a thumbnail (read off the main loop). The
    /// caller decodes them off-thread via `decode_scaled` and builds the texture
    /// on the main thread (see `tabs::library`). Returns `None` if the file
    /// can't be read.
    pub fn request_thumbnail(&self, path: String) -> Receiver<Option<Vec<u8>>> {
        let (tx, rx) = async_channel::bounded::<Option<Vec<u8>>>(1);
        if let Some(thumb_tx) = &self.thumb_tx {
            let _ = thumb_tx.send_blocking(ThumbJob { path, reply: tx });
        } else {
            let _ = tx.send_blocking(None);
        }
        rx
    }
}

impl Drop for EngineClient {
    fn drop(&mut self) {
        self.shutting_down.store(true, Ordering::Relaxed);
        self.stdin.take(); // EOF → engine exits cleanly
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
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
        }
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
        let _ = this
            .borrow()
            .raw_tx
            .send_blocking(EngineEvent::Error(
                "engine crashed repeatedly — giving up. Restart the app.".into(),
            ));
        return;
    }
    let delay = std::time::Duration::from_millis(400 * n as u64);
    let this2 = this.clone();
    glib::timeout_add_local_once(delay, move || spawn_process(&this2));
}

/// Locate the engine binary. Search order mirrors the scaffold:
///   1. `$FILEID_ENGINE`
///   2. next to the app binary (dev / staged)
///   3. system install dirs (.deb / Flatpak)
fn locate_engine_binary() -> Result<PathBuf> {
    if let Ok(s) = std::env::var("FILEID_ENGINE") {
        let p = PathBuf::from(s);
        if p.exists() {
            return Ok(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for candidate in ["FileIDEngine", "fileid-engine"] {
                let p = dir.join(candidate);
                if p.exists() {
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
        if p.exists() {
            return Ok(p);
        }
    }
    anyhow::bail!("set FILEID_ENGINE or place the engine beside the app binary")
}

/// Reader thread: parse NDJSON `IpcEvent`s into `EngineEvent`s. Tolerant — a
/// line that doesn't parse is ignored (the engine also emits human log lines).
fn drain_stdout(stdout: std::process::ChildStdout, tx: Sender<EngineEvent>) {
    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<IpcEvent>(&line) else {
            continue;
        };
        let mapped = match event.payload {
            EventPayload::Ready(_) => Some(EngineEvent::Ready),
            EventPayload::Progress(w) => Some(EngineEvent::Progress(w.inner)),
            EventPayload::BatchSummary(w) => Some(EngineEvent::BatchLanded(w.inner.processed_total)),
            EventPayload::ScanComplete(w) => {
                Some(EngineEvent::ScanComplete(w.inner.processed_files))
            }
            EventPayload::Error(w) => Some(EngineEvent::Error(w.inner.message)),
            EventPayload::DeepAnalyzeStarting(w) => {
                Some(EngineEvent::DeepAnalyzeStarting(w.inner))
            }
            EventPayload::DeepAnalyzeProgress(w) => {
                Some(EngineEvent::DeepAnalyzeProgress(w.inner))
            }
            EventPayload::DeepAnalyzeFileDone(w) => {
                Some(EngineEvent::DeepAnalyzeFileDone(w.inner))
            }
            EventPayload::DeepAnalyzeComplete(w) => {
                Some(EngineEvent::DeepAnalyzeComplete(w.inner))
            }
            EventPayload::ModelDownloadProgress(w) => {
                Some(EngineEvent::ModelDownloadProgress(w.inner))
            }
            EventPayload::RestructurePlan(w) => Some(EngineEvent::RestructurePlan(w.inner)),
            EventPayload::RestructureApplyResult(w) => {
                Some(EngineEvent::RestructureApplyResult(w.inner))
            }
            _ => None,
        };
        if let Some(ev) = mapped {
            if tx.send_blocking(ev).is_err() {
                return;
            }
        }
    }
    // stdout closed → engine exited.
    let _ = tx.send_blocking(EngineEvent::Exited);
}

/// Drain the engine's stderr to the local debug log. Never transmits.
fn drain_stderr(stderr: std::process::ChildStderr) {
    let reader = BufReader::new(stderr);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        tracing::debug!(target: "engine_stderr", "{line}");
    }
}

// ─── DB reads (mirror of macOS/Windows ReadStore) ────────────────────────────

const SELECT_COLS: &str = "id, path_text, size_bytes, created_at, modified_at, kind, extension, \
    has_faces, has_text, vlm_proposed_name, vlm_description";

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
         WHERE ( :has_search = 0 \
                 OR f.path_text LIKE :like ESCAPE '\\' \
                 OR EXISTS (SELECT 1 FROM tags t WHERE t.file_id = f.id AND t.tag LIKE :like ESCAPE '\\') \
                 OR ( :has_fts = 1 AND f.id IN (SELECT rowid FROM ocr_fts WHERE ocr_fts MATCH :fts) ) ) \
           AND ( :kind IS NULL OR f.kind = :kind ) \
         ORDER BY f.scanned_at DESC \
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

struct ThumbJob {
    path: String,
    reply: Sender<Option<Vec<u8>>>,
}

/// Cap the bytes we'll read for a thumbnail so a stray multi-GB file can't
/// balloon memory. Anything larger falls back to the icon placeholder.
const THUMB_MAX_BYTES: u64 = 48 * 1024 * 1024;

fn thumbnail_worker(rx: Receiver<ThumbJob>) {
    while let Ok(job) = rx.recv_blocking() {
        let bytes = read_capped(&job.path);
        let _ = job.reply.send_blocking(bytes);
    }
}

fn read_capped(path: &str) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > THUMB_MAX_BYTES {
        return None;
    }
    std::fs::read(path).ok()
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
pub fn decode_scaled(file_bytes: &[u8], max_px: i32) -> Option<DecodedImage> {
    let gbytes = glib::Bytes::from(file_bytes);
    let stream = gio::MemoryInputStream::from_bytes(&gbytes);
    let pixbuf = gtk::gdk_pixbuf::Pixbuf::from_stream_at_scale(
        &stream,
        max_px,
        max_px,
        true,
        gio::Cancellable::NONE,
    )
    .ok()?;
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
