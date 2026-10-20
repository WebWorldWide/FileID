// Engine client — the app side of the two-binary IPC design
// (`shared/docs/ARCHITECTURE.md`). Mirrors macOS `EngineClient` + Windows
// `EngineClient.cs`:
//
//   * spawns the shared Rust engine as a child process,
//   * sends `IpcCommand`s as newline-delimited JSON on its stdin — reusing the
//     engine crate's own serde types so the wire contract can never drift,
//   * parses bounded `IpcEvent` frames off a reader thread and fans them out to
//     every UI subscriber on the GTK main context through bounded channels,
//   * proves readiness with a per-spawn health-check nonce, and respawns the
//     engine with capped backoff if it dies.
//
// File listing is NOT an IPC command — the engine is the single DB *writer*;
// the app reads file rows directly from the same SQLite WAL DB on one
// latest-request-wins worker, exactly like macOS / Windows `ReadStore`.
//
// Thumbnails are decoded by a bounded fixed-size worker pool; only pixel
// buffers cross back to the main thread, where textures are built.

use anyhow::{Context, Result};
use async_channel::{Receiver, Sender};
use fileid_engine::ipc::{
    CancelPrewarmPayload, CommandPayload, EventPayload, HealthCheckPayload, IpcCommand, IpcEvent,
    ModelDownloadProgress, PrewarmModelPayload, StartScanPayload,
};
use gtk::prelude::*;
use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::rc::Rc;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub enum EngineState {
    CatalogResponse(fileid_engine::ipc::CatalogResponse),
    ToolResponse(fileid_engine::ipc::ToolResponse),
    ChatResponse(fileid_engine::ipc::ChatResponse),
    ClipTextEmbedding(fileid_engine::ipc::ClipTextEmbedding),
    Progress(fileid_engine::ipc::ScanProgress),
    ScanPhase(fileid_engine::ipc::ScanPhase),
    RestructurePlan(fileid_engine::ipc::RestructurePlan),
    RestructureApplyResult(fileid_engine::ipc::RestructureApplyResult),
    DeepAnalyzeStarting(fileid_engine::ipc::DeepAnalyzeStarting),
    DeepAnalyzeProgress(fileid_engine::ipc::DeepAnalyzeProgress),
    DeepAnalyzeFileDone(fileid_engine::ipc::DeepAnalyzeFileDone),
    DeepAnalyzeComplete(fileid_engine::ipc::DeepAnalyzeComplete),
    Spawning,
    /// The engine of the current spawn generation answered our health probe.
    /// `is_ready` is already true when this is delivered.
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
    ModelCatalog(fileid_engine::ipc::ModelCatalog),
    BenchmarkProgress(fileid_engine::ipc::BenchmarkProgress),
    BenchmarkResult(fileid_engine::ipc::BenchmarkResult),
    Error { kind: String, message: String, model_kind: Option<String> },
    Failed(String),
    /// The engine process exited; a respawn follows unless the app is closing.
    Exited,
}

pub type EngineEvent = EngineState;

/// What the reader thread hands the main-context pump. Engine `ready` and
/// health answers are consumed by the pump; subscribers only ever see a
/// `Ready` that the live generation has proven.
// At most RAW_EVENT_CAP of these exist at once; boxing would buy nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
enum RawEvent {
    State(EngineState),
    Announced,
    Health { request_id: String, pid: i32 },
}

/// Each caller keeps its own latest-wins slot, so the Library grid, the
/// preview and the File Tools picker never cancel one another.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum QueryChannel {
    #[default]
    Library,
    Preview,
    Tools,
}

impl QueryChannel {
    const COUNT: usize = 3;

    fn slot(self) -> usize {
        match self {
            QueryChannel::Library => 0,
            QueryChannel::Preview => 1,
            QueryChannel::Tools => 2,
        }
    }
}

/// The most rows one read returns (the preview's full-library list).
pub const QUERY_MAX_ROWS: i64 = 20_000;

#[derive(Debug, Clone, Default)]
pub struct QuerySpec {
    pub search: String,
    pub kind: Option<String>,
    pub limit: i64,
    /// Rows to skip; the Library pages in as the grid nears its end.
    pub offset: i64,
    pub channel: QueryChannel,
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
    )
    .upcast()
}

type QueryReply = Sender<Result<(Vec<FileRow>, i64)>>;

struct QueryJob {
    spec: QuerySpec,
    reply: QueryReply,
}

#[derive(Default)]
struct QuerySlots {
    pending: [Option<QueryJob>; QueryChannel::COUNT],
    next: usize,
}

impl QuerySlots {
    fn is_empty(&self) -> bool {
        self.pending.iter().all(Option::is_none)
    }

    /// Round-robin across channels so a chatty Library cannot starve the rest.
    fn take_next(&mut self) -> Option<QueryJob> {
        for step in 0..QueryChannel::COUNT {
            let index = (self.next + step) % QueryChannel::COUNT;
            if let Some(job) = self.pending[index].take() {
                self.next = (index + 1) % QueryChannel::COUNT;
                return Some(job);
            }
        }
        None
    }
}

struct QueryWorkerState {
    latest: Mutex<QuerySlots>,
    ready: Condvar,
    stopping: AtomicBool,
}

impl QueryWorkerState {
    fn new() -> Self {
        Self {
            latest: Mutex::new(QuerySlots::default()),
            ready: Condvar::new(),
            stopping: AtomicBool::new(false),
        }
    }

    fn submit(&self, job: QueryJob) {
        if self.stopping.load(Ordering::Acquire) {
            return;
        }
        let slot = job.spec.channel.slot();
        self.latest.lock().expect("query queue poisoned").pending[slot] = Some(job);
        self.ready.notify_one();
    }

    fn stop(&self) {
        self.stopping.store(true, Ordering::Release);
        *self.latest.lock().expect("query queue poisoned") = QuerySlots::default();
        self.ready.notify_all();
    }
}

fn spawn_query_worker() -> Arc<QueryWorkerState> {
    let state = Arc::new(QueryWorkerState::new());
    let worker_state = state.clone();
    thread::spawn(move || loop {
        let job = {
            let mut latest = worker_state.latest.lock().expect("query queue poisoned");
            while latest.is_empty() && !worker_state.stopping.load(Ordering::Acquire) {
                latest = worker_state.ready.wait(latest).expect("query queue poisoned");
            }
            if worker_state.stopping.load(Ordering::Acquire) {
                return;
            }
            latest.take_next()
        };
        let Some(job) = job else { continue };
        let _ = job.reply.send_blocking(query_files_from_db(&job.spec));
    });
    state
}

const RESPAWN_DELAYS: [Duration; 3] = [Duration::from_secs(1), Duration::from_secs(4), Duration::from_secs(16)];
const RESPAWN_WINDOW: Duration = Duration::from_secs(60);
const STABLE_READY: Duration = Duration::from_secs(30);

/// macOS `EngineClient` respawn budget: three restarts per 60 s, backing off
/// 1/4/16 s. Only 30 s of continuous Ready refills it; clearing it on every
/// Ready would let a ready-then-crash engine loop forever.
#[derive(Debug, Default)]
struct RespawnBudget {
    attempts: Vec<Instant>,
    last_ready_at: Option<Instant>,
}

impl RespawnBudget {
    fn record_ready(&mut self, now: Instant) {
        self.last_ready_at = Some(now);
    }

    fn next_delay(&mut self, now: Instant) -> Option<Duration> {
        if self
            .last_ready_at
            .is_some_and(|ready| now.saturating_duration_since(ready) >= STABLE_READY)
        {
            self.attempts.clear();
        }
        self.last_ready_at = None;
        self.attempts
            .retain(|at| now.saturating_duration_since(*at) < RESPAWN_WINDOW);
        let delay = *RESPAWN_DELAYS.get(self.attempts.len())?;
        self.attempts.push(now);
        Some(delay)
    }
}

const THUMB_QUEUE_CAP: usize = 64;
const THUMB_WORKERS: usize = 4;
const RAW_EVENT_CAP: usize = 8;
const SUBSCRIBER_EVENT_CAP: usize = 4;
const MAX_ENGINE_FRAME_BYTES: usize = 64 * 1024 * 1024;
const MAX_ENGINE_LOG_BYTES: usize = 1024 * 1024;
/// A stray multi-GB file must not balloon memory; larger files keep the icon.
const THUMB_MAX_BYTES: u64 = 48 * 1024 * 1024;

pub struct EngineClient {
    child: Option<Child>,
    stdin: Option<Arc<Mutex<ChildStdin>>>,
    raw_tx: Sender<RawEvent>,
    raw_rx: Option<Receiver<RawEvent>>,
    subscribers: Vec<Sender<EngineState>>,
    thumbs: Arc<ThumbQueue>,
    preview: Arc<PreviewSlot>,
    query_worker: Arc<QueryWorkerState>,
    next_id: u64,
    respawn_budget: RespawnBudget,
    ready: bool,
    /// Bumped on every spawn; a health answer counts only for the live one.
    generation: u64,
    pending_health: Option<(String, u64)>,
    /// Set when the app closes or asks the engine to shut down, so the
    /// resulting exit does not respawn it.
    shutting_down: Arc<AtomicBool>,
}

fn prune_closed_subscribers(subscribers: &mut Vec<Sender<EngineState>>) {
    subscribers.retain(|subscriber| !subscriber.is_closed());
}

impl EngineClient {
    pub fn new() -> Self {
        let (raw_tx, raw_rx) = async_channel::bounded(RAW_EVENT_CAP);
        Self {
            child: None,
            stdin: None,
            raw_tx,
            raw_rx: Some(raw_rx),
            subscribers: Vec::new(),
            thumbs: Arc::new(ThumbQueue::new()),
            preview: Arc::new(PreviewSlot::new()),
            query_worker: spawn_query_worker(),
            next_id: 0,
            respawn_budget: RespawnBudget::default(),
            ready: false,
            generation: 0,
            pending_health: None,
            shutting_down: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Each call returns a receiver that sees every event from now on.
    pub fn subscribe(&mut self) -> Receiver<EngineState> {
        let (tx, rx) = async_channel::bounded(SUBSCRIBER_EVENT_CAP);
        self.subscribers.push(tx);
        rx
    }

    #[cfg(test)]
    pub(crate) fn publish_test_event(&self, event: EngineState) {
        for subscriber in &self.subscribers {
            deliver_from_main(subscriber, event.clone());
        }
    }

    /// True only once a health probe minted for the current spawn generation
    /// has been answered with its own nonce; a bare `ready` is not enough.
    pub fn is_ready(&self) -> bool {
        self.ready
    }

    fn begin_health_probe(&mut self) -> Option<(String, u64)> {
        if self
            .pending_health
            .as_ref()
            .is_some_and(|(_, generation)| *generation == self.generation)
        {
            return None;
        }
        self.next_id = self.next_id.wrapping_add(1);
        let request_id = format!("health-{}-{}", self.generation, self.next_id);
        self.pending_health = Some((request_id.clone(), self.generation));
        Some((request_id, self.generation))
    }

    /// Returns true when this answer made the client ready.
    fn settle_health_probe(&mut self, request_id: &str, pid: i32) -> bool {
        let matches = self
            .pending_health
            .as_ref()
            .is_some_and(|(id, generation)| id == request_id && *generation == self.generation);
        if !matches {
            tracing::debug!(target: "engine", "discarded health answer {request_id} from pid {pid}");
            return false;
        }
        self.pending_health = None;
        self.ready = true;
        tracing::debug!(target: "engine", "engine generation {} ready (pid {pid})", self.generation);
        true
    }

    fn abandon_health_probe(&mut self, request_id: &str, generation: u64) {
        if self
            .pending_health
            .as_ref()
            .is_some_and(|(id, gen)| id == request_id && *gen == generation)
        {
            self.pending_health = None;
        }
    }

    /// Close stdin so the engine checkpoints and exits on EOF; kill it only
    /// if it is still running well after its own shutdown budget.
    pub fn shutdown(&mut self) {
        if self.shutting_down.swap(true, Ordering::AcqRel) {
            return;
        }
        self.ready = false;
        self.pending_health = None;
        self.subscribers.clear();
        self.thumbs.stop();
        self.preview.stop();
        self.query_worker.stop();
        self.stdin.take();
        if let Some(mut child) = self.child.take() {
            thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(36);
                loop {
                    match child.try_wait() {
                        Ok(Some(_)) => return,
                        Ok(None) if Instant::now() < deadline => {
                            thread::sleep(Duration::from_millis(50));
                        }
                        Ok(None) | Err(_) => break,
                    }
                }
                let _ = child.kill();
                let _ = child.wait();
            });
        }
    }

    /// Boot the engine and thumbnail workers and start the main-context
    /// fan-out pump, which also drives health probes and respawns.
    pub fn start(this: &Rc<RefCell<Self>>) {
        let raw_rx = {
            let mut client = this.borrow_mut();
            for _ in 0..THUMB_WORKERS {
                let queue = client.thumbs.clone();
                thread::spawn(move || thumbnail_worker(&queue));
            }
            let preview = client.preview.clone();
            thread::spawn(move || preview_worker(&preview));
            client.raw_rx.take().expect("EngineClient::start called twice")
        };
        spawn_process(this);

        let weak = Rc::downgrade(this);
        glib::MainContext::default().spawn_local(async move {
            while let Ok(raw) = raw_rx.recv().await {
                let Some(client) = weak.upgrade() else { break };
                let event = match raw {
                    RawEvent::Announced => {
                        // A `ready` can race in from the previous generation
                        // during a respawn; only our own nonce settles it.
                        let probe = {
                            client.borrow_mut().begin_health_probe()
                        };
                        if let Some((request_id, generation)) = probe {
                            let mut client = client.borrow_mut();
                            let sent = client.send(CommandPayload::HealthCheck(HealthCheckPayload {
                                request_id: request_id.clone(),
                            }));
                            if sent.is_err() {
                                client.abandon_health_probe(&request_id, generation);
                            }
                        }
                        continue;
                    }
                    RawEvent::Health { request_id, pid } => {
                        let mut settled = client.borrow_mut();
                        if !settled.settle_health_probe(&request_id, pid) {
                            continue;
                        }
                        settled.respawn_budget.record_ready(Instant::now());
                        drop(settled);
                        EngineState::Ready
                    }
                    RawEvent::State(state) => state,
                };
                match &event {
                    EngineState::Spawning => {
                        let mut client = client.borrow_mut();
                        client.ready = false;
                        client.pending_health = None;
                        client.generation = client.generation.wrapping_add(1);
                    }
                    EngineState::Exited => {
                        let mut client = client.borrow_mut();
                        client.ready = false;
                        client.pending_health = None;
                    }
                    _ => {}
                }
                let subscribers = client.borrow().subscribers.clone();
                drop(client);
                for subscriber in &subscribers {
                    let _ = subscriber.send(event.clone()).await;
                }
                let Some(client) = weak.upgrade() else { break };
                prune_closed_subscribers(&mut client.borrow_mut().subscribers);
                if matches!(event, EngineState::Exited)
                    && !client.borrow().shutting_down.load(Ordering::Relaxed)
                {
                    retire_exited_child(&client);
                    schedule_respawn(&client);
                }
            }
        });
    }

    pub fn start_scan(&mut self, root_path: &str) -> Result<()> {
        self.send(CommandPayload::StartScan(StartScanPayload {
            root_path: root_path.to_owned(),
            root_bookmark: None,
            root_display: None,
            rescan: false,
            excluded_paths: None,
        }))?;
        emit_from_main(&self.raw_tx, RawEvent::State(EngineState::Scanning));
        Ok(())
    }

    pub fn prewarm_model(&mut self, model_kind: &str) -> Result<()> {
        self.send(CommandPayload::PrewarmModel(PrewarmModelPayload {
            model_kind: model_kind.to_owned(),
        }))
    }

    pub fn list_models(&mut self, request_id: &str, role: Option<&str>) -> Result<()> {
        self.send(CommandPayload::ListModels(fileid_engine::ipc::ListModelsPayload {
            request_id: request_id.to_owned(),
            role: role.map(str::to_owned),
        }))
    }

    pub fn benchmark_model(&mut self, request_id: &str, model_kind: &str, tune: bool) -> Result<()> {
        self.send(CommandPayload::BenchmarkModel(fileid_engine::ipc::BenchmarkModelPayload {
            request_id: request_id.to_owned(),
            model_kind: model_kind.to_owned(),
            tune,
        }))
    }

    pub fn cancel_benchmark(&mut self) -> Result<()> {
        self.send(CommandPayload::CancelBenchmark(fileid_engine::ipc::Empty {}))
    }

    pub fn cancel_prewarm(&mut self, model_kind: &str) -> Result<()> {
        self.send(CommandPayload::CancelPrewarm(CancelPrewarmPayload {
            model_kind: Some(model_kind.to_owned()),
        }))
    }

    pub fn send(&mut self, payload: CommandPayload) -> Result<()> {
        let shutdown = matches!(payload, CommandPayload::Shutdown(_));
        self.next_id = self.next_id.wrapping_add(1);
        let command = IpcCommand {
            id: format!("linux-{}", self.next_id),
            payload,
        };
        let line = serde_json::to_string(&command)? + "\n";
        let stdin = self.stdin.as_ref().context("engine not spawned")?.clone();
        let mut guard = stdin.lock().expect("engine stdin poisoned");
        guard.write_all(line.as_bytes())?;
        guard.flush()?;
        if shutdown {
            self.shutting_down.store(true, Ordering::Release);
        }
        Ok(())
    }

    /// Queue a read on the query worker. Within one `QueryChannel` the latest
    /// request wins and a superseded receiver closes without a result.
    pub fn query_files(&self, spec: QuerySpec) -> Receiver<Result<(Vec<FileRow>, i64)>> {
        let (reply, rx) = async_channel::bounded(1);
        self.query_worker.submit(QueryJob { spec, reply });
        rx
    }

    /// First-in-first-out decode on the shared bounded workers.
    pub fn request_scaled_thumbnail(&self, path: String, dim: i32) -> Receiver<Option<DecodedImage>> {
        self.background_plain(ThumbWork::Image { path, dim })
    }

    pub fn request_video_thumbnail(&self, path: String, dim: i32) -> Receiver<Option<DecodedImage>> {
        self.background_plain(ThumbWork::VideoKeyframe { path, dim })
    }

    fn background_plain(&self, work: ThumbWork) -> Receiver<Option<DecodedImage>> {
        let (reply, rx) = async_channel::bounded(1);
        self.thumbs.push_background(ThumbJob { work, reply: ReplyTo::Plain(reply), cancel: None });
        rx
    }

    /// A grid tile's thumbnail: newest request first, the oldest evicted with
    /// `Dropped` when the queue is full, and skipped once `cancel` is set.
    pub fn request_tile(&self, path: String, dim: i32, video: bool, cancel: CancelToken) -> Receiver<ThumbReply> {
        let work = if video { ThumbWork::VideoKeyframe { path, dim } } else { ThumbWork::Image { path, dim } };
        let (reply, rx) = async_channel::bounded(1);
        self.thumbs.push_tile(ThumbJob { work, reply: ReplyTo::Full(reply), cancel: Some(cancel) });
        rx
    }

    /// Any bounded decode (face crops) on the shared workers, in order.
    pub fn request_decode(
        &self,
        decode: impl FnOnce() -> Option<DecodedImage> + Send + 'static,
        cancel: CancelToken,
    ) -> Receiver<ThumbReply> {
        let (reply, rx) = async_channel::bounded(1);
        self.thumbs.push_background(ThumbJob {
            work: ThumbWork::Custom(Box::new(decode)),
            reply: ReplyTo::Full(reply),
            cancel: Some(cancel),
        });
        rx
    }

    /// A preview image on its own latest-wins worker, never queued behind
    /// grid tiles; a newer request supersedes an unstarted one.
    pub fn request_preview(&self, path: String, dim: i32, video: bool) -> Receiver<ThumbReply> {
        let work = if video { ThumbWork::VideoKeyframe { path, dim } } else { ThumbWork::Image { path, dim } };
        let (reply, rx) = async_channel::bounded(1);
        self.preview.submit(ThumbJob { work, reply: ReplyTo::Full(reply), cancel: None });
        rx
    }
}

impl Drop for EngineClient {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The pump drains these channels on the main thread, so the main thread must
/// never park on a full one: retry from the idle loop instead.
fn emit_from_main(raw_tx: &Sender<RawEvent>, event: RawEvent) {
    match raw_tx.try_send(event) {
        Ok(()) | Err(async_channel::TrySendError::Closed(_)) => {}
        Err(async_channel::TrySendError::Full(event)) => {
            let raw_tx = raw_tx.clone();
            glib::idle_add_local_once(move || emit_from_main(&raw_tx, event));
        }
    }
}

#[cfg(test)]
fn deliver_from_main(subscriber: &Sender<EngineState>, event: EngineState) {
    match subscriber.try_send(event) {
        Ok(()) | Err(async_channel::TrySendError::Closed(_)) => {}
        Err(async_channel::TrySendError::Full(event)) => {
            let subscriber = subscriber.clone();
            glib::idle_add_local_once(move || deliver_from_main(&subscriber, event));
        }
    }
}

fn spawn_process(this: &Rc<RefCell<EngineClient>>) {
    let raw_tx = this.borrow().raw_tx.clone();
    emit_from_main(&raw_tx, RawEvent::State(EngineState::Spawning));
    let exe = match locate_engine_binary() {
        Ok(path) => path,
        Err(err) => {
            emit_from_main(
                &raw_tx,
                RawEvent::State(EngineState::Failed(format!("engine binary not found: {err}"))),
            );
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
                let mut client = this.borrow_mut();
                client.stdin = Some(Arc::new(Mutex::new(stdin)));
                client.child = Some(child);
            }
            thread::spawn(move || drain_engine_stdout(BufReader::new(stdout), raw_tx));
            // A chatty engine must never block on a full stderr pipe.
            if let Some(stderr) = stderr {
                thread::spawn(move || drain_stderr(stderr));
            }
        }
        Err(err) => {
            emit_from_main(&raw_tx, RawEvent::State(EngineState::Failed(format!("spawn failed: {err}"))));
            schedule_respawn(this);
        }
    }
}

/// Dropping a `Child` does not wait on Unix; reap the dead engine off the GTK
/// thread so every crash does not leave a zombie.
fn retire_exited_child(this: &Rc<RefCell<EngineClient>>) {
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

fn schedule_respawn(this: &Rc<RefCell<EngineClient>>) {
    let delay = this.borrow_mut().respawn_budget.next_delay(Instant::now());
    let Some(delay) = delay else {
        let raw_tx = this.borrow().raw_tx.clone();
        emit_from_main(
            &raw_tx,
            RawEvent::State(EngineState::Failed(
                "Engine exited 3 times within 60 s; relaunch the app.".into(),
            )),
        );
        return;
    };
    let weak = Rc::downgrade(this);
    glib::timeout_add_local_once(delay, move || {
        if let Some(client) = weak.upgrade() {
            if !client.borrow().shutting_down.load(Ordering::Relaxed) {
                spawn_process(&client);
            }
        }
    });
}

/// Locate the engine binary. Search order:
///   1. `$FILEID_ENGINE`
///   2. `FileIDEngine` or `fileid-engine` alongside the app executable
///   3. system install dirs (.deb / Flatpak)
fn locate_engine_binary() -> Result<PathBuf> {
    if let Some(value) = std::env::var_os("FILEID_ENGINE").filter(|value| !value.is_empty()) {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Ok(path);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for candidate in ["FileIDEngine", "fileid-engine"] {
                let path = dir.join(candidate);
                if path.is_file() {
                    return Ok(path);
                }
            }
        }
    }
    for system in [
        "/usr/lib/FileID/FileIDEngine",
        "/usr/libexec/FileID/FileIDEngine",
        "/usr/lib/FileID/fileid-engine",
    ] {
        let path = PathBuf::from(system);
        if path.is_file() {
            return Ok(path);
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
        let take = newline.unwrap_or(available.len());
        if draining || buffer.len().saturating_add(take) > max_bytes {
            draining = true;
        } else {
            buffer.extend_from_slice(&available[..take]);
        }
        reader.consume(take + usize::from(newline.is_some()));
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

fn drain_engine_stdout(mut reader: impl BufRead, tx: Sender<RawEvent>) {
    let mut frame = Vec::with_capacity(8 * 1024);
    loop {
        match read_bounded_line(&mut reader, &mut frame, MAX_ENGINE_FRAME_BYTES) {
            Ok(BoundedLine::Eof) => break,
            Err(err) => {
                let _ = tx.send_blocking(RawEvent::State(EngineState::Failed(format!(
                    "engine output error: {err}"
                ))));
                break;
            }
            Ok(BoundedLine::Oversize) => {
                let message = format!(
                    "engine response exceeded the {} MiB safety limit and was discarded",
                    MAX_ENGINE_FRAME_BYTES / (1024 * 1024)
                );
                if tx
                    .send_blocking(RawEvent::State(EngineState::Error {
                        kind: "oversized_frame".into(),
                        message,
                        model_kind: None,
                    }))
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
        if let Some(raw) = map_engine_payload(event.payload) {
            if tx.send_blocking(raw).is_err() {
                return;
            }
        }
    }
    let _ = tx.send_blocking(RawEvent::State(EngineState::Exited));
}

fn map_engine_payload(payload: EventPayload) -> Option<RawEvent> {
    let state = match payload {
        EventPayload::Ready(_) => return Some(RawEvent::Announced),
        EventPayload::HealthCheckResult(r) => {
            return Some(RawEvent::Health {
                request_id: r.inner.request_id,
                pid: r.inner.pid,
            })
        }
        EventPayload::CatalogResponse(r) => EngineState::CatalogResponse(r.inner),
        EventPayload::ToolResponse(r) => EngineState::ToolResponse(r.inner),
        EventPayload::ChatResponse(r) => EngineState::ChatResponse(r.inner),
        EventPayload::ClipTextEmbedding(r) => EngineState::ClipTextEmbedding(r.inner),
        EventPayload::Progress(r) => EngineState::Progress(r.inner),
        EventPayload::PhaseChanged(r) => EngineState::ScanPhase(r.inner),
        EventPayload::RestructurePlan(r) => EngineState::RestructurePlan(r.inner),
        EventPayload::RestructureApplyResult(r) => EngineState::RestructureApplyResult(r.inner),
        EventPayload::DeepAnalyzeStarting(r) => EngineState::DeepAnalyzeStarting(r.inner),
        EventPayload::DeepAnalyzeProgress(r) => EngineState::DeepAnalyzeProgress(r.inner),
        EventPayload::DeepAnalyzeFileDone(r) => EngineState::DeepAnalyzeFileDone(r.inner),
        EventPayload::DeepAnalyzeComplete(r) => EngineState::DeepAnalyzeComplete(r.inner),
        EventPayload::BatchSummary(batch) => EngineState::BatchLanded(batch.inner.processed_total),
        EventPayload::ScanComplete(result) => EngineState::ScanComplete(result.inner.processed_files),
        EventPayload::FaceClusteringComplete(result) => EngineState::FaceClusteringComplete(result.inner),
        EventPayload::BulkActionResult(result) => EngineState::BulkActionResult(result.inner),
        EventPayload::MergeSuggestions(result) => EngineState::MergeSuggestions(result.inner),
        EventPayload::ModelDownloadProgress(progress) => EngineState::ModelDownloadProgress(progress.inner),
        EventPayload::ModelCatalog(catalog) => EngineState::ModelCatalog(catalog.inner),
        EventPayload::BenchmarkProgress(progress) => EngineState::BenchmarkProgress(progress.inner),
        EventPayload::BenchmarkResult(result) => EngineState::BenchmarkResult(result.inner),
        EventPayload::Error(error) => match error.inner.kind.as_str() {
            "face_clustering_failed" => EngineState::FaceClusteringFailed(error.inner.message),
            "face_clustering_busy" => EngineState::FaceClusteringBusy(error.inner.message),
            _ => EngineState::Error {
                kind: error.inner.kind,
                message: error.inner.message,
                model_kind: error.inner.model_kind,
            },
        },
        _ => return None,
    };
    Some(RawEvent::State(state))
}

/// Engine diagnostics go to the local debug log only. Never transmits.
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
                tracing::debug!(target: "engine_stderr", "{}", String::from_utf8_lossy(&frame));
            }
        }
    }
}

fn query_files_from_db(spec: &QuerySpec) -> Result<(Vec<FileRow>, i64)> {
    let path = fileid_engine::paths::db_path()?;
    if !path.exists() {
        return Ok((Vec::new(), 0));
    }
    query_files_at_path(&path, spec)
}

const QUERY_FILTER: &str = "f.failed = 0 \
    AND (:has_search = 0 \
         OR COALESCE(f.path_search, f.path_text) LIKE :like ESCAPE '\\' \
         OR EXISTS (SELECT 1 FROM tags t WHERE t.file_id = f.id AND t.tag LIKE :like ESCAPE '\\') \
         OR (:has_fts = 1 AND ( \
                f.id IN (SELECT rowid FROM ocr_fts WHERE ocr_fts MATCH :fts) \
                OR f.id IN (SELECT rowid FROM doc_fts WHERE doc_fts MATCH :fts)))) \
    AND (:kind IS NULL OR f.kind = :kind)";

const FILE_COLUMNS: &str = "f.id, f.path_text, f.kind, f.extension, f.size_bytes, f.created_at, \
     f.modified_at, f.has_faces, f.has_text, f.file_ref, f.content_hash, f.vlm_description, f.vlm_proposed_name";

fn file_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileRow> {
    let path: String = row.get(1)?;
    let name = Path::new(&path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.clone());
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
}

fn file_row_at_path(path: &Path, id: i64) -> Result<Option<FileRow>> {
    use rusqlite::OptionalExtension;
    let conn = fileid_engine::db::open_read(path)?;
    Ok(conn
        .query_row(&format!("SELECT {FILE_COLUMNS} FROM files f WHERE f.id = ?1 AND f.failed = 0"), [id], file_row)
        .optional()?)
}

/// One file's current row, read off the main loop (the preview refreshes
/// what it shows after a scan).
pub fn read_file_row(id: i64) -> Receiver<Option<FileRow>> {
    let (tx, rx) = async_channel::bounded(1);
    thread::spawn(move || {
        let row = fileid_engine::paths::db_path()
            .ok()
            .filter(|path| path.exists())
            .and_then(|path| file_row_at_path(&path, id).ok().flatten());
        let _ = tx.send_blocking(row);
    });
    rx
}

fn query_files_at_path(path: &Path, spec: &QuerySpec) -> Result<(Vec<FileRow>, i64)> {
    let conn = fileid_engine::db::open_read(path)?;
    let search = spec.search.trim();
    let has_search = !search.is_empty();
    let like = format!("%{}%", escape_like(search));
    let fts = fts_match(search);
    let has_fts = !fts.is_empty();
    let kind = spec.kind.as_deref().filter(|kind| !kind.is_empty());
    let limit = if spec.limit > 0 { spec.limit.min(QUERY_MAX_ROWS) } else { 500 };
    let mut stmt = conn.prepare(&format!(
        "SELECT {FILE_COLUMNS} FROM files f WHERE {QUERY_FILTER} \
         ORDER BY f.scanned_at DESC, f.id DESC LIMIT :limit OFFSET :offset"
    ))?;
    let rows = stmt
        .query_map(
            rusqlite::named_params! {
                ":has_search": has_search,
                ":like": like,
                ":has_fts": has_fts,
                ":fts": fts,
                ":kind": kind,
                ":limit": limit,
                ":offset": spec.offset.max(0),
            },
            file_row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // The common small-library reload never fills the limit, so it skips the
    // second scan for an exact count.
    let total = if spec.offset <= 0 && (rows.len() as i64) < limit {
        rows.len() as i64
    } else {
        conn.query_row(
            &format!("SELECT COUNT(*) FROM files f WHERE {QUERY_FILTER}"),
            rusqlite::named_params! {
                ":has_search": has_search,
                ":like": like,
                ":has_fts": has_fts,
                ":fts": fts,
                ":kind": kind,
            },
            |row| row.get(0),
        )?
    };
    Ok((rows, total))
}

fn escape_like(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

/// Quote each alphanumeric token so user punctuation cannot produce a
/// malformed MATCH expression. Empty when nothing usable remains.
fn fts_match(text: &str) -> String {
    text.split_whitespace()
        .map(|token| token.chars().filter(|c| c.is_alphanumeric()).collect::<String>())
        .filter(|token| !token.is_empty())
        .map(|token| format!("\"{token}\""))
        .collect::<Vec<_>>()
        .join(" ")
}

pub type CancelToken = Arc<AtomicBool>;

#[derive(Debug)]
pub enum ThumbReply {
    Image(DecodedImage),
    Failed,
    /// Evicted, cancelled or superseded before it was decoded.
    Dropped,
}

impl ThumbReply {
    pub fn into_image(self) -> Option<DecodedImage> {
        match self {
            ThumbReply::Image(image) => Some(image),
            ThumbReply::Failed | ThumbReply::Dropped => None,
        }
    }
}

enum ThumbWork {
    Image { path: String, dim: i32 },
    VideoKeyframe { path: String, dim: i32 },
    Custom(Box<dyn FnOnce() -> Option<DecodedImage> + Send>),
}

impl ThumbWork {
    fn run(self) -> Option<DecodedImage> {
        match self {
            ThumbWork::Image { path, dim } => image_thumbnail(Path::new(&path), dim),
            ThumbWork::VideoKeyframe { path, dim } => video_thumbnail(Path::new(&path), dim),
            ThumbWork::Custom(decode) => decode(),
        }
    }
}

enum ReplyTo {
    Plain(Sender<Option<DecodedImage>>),
    Full(Sender<ThumbReply>),
}

impl ReplyTo {
    fn send(&self, reply: ThumbReply) {
        match self {
            ReplyTo::Plain(tx) => {
                let _ = tx.try_send(reply.into_image());
            }
            ReplyTo::Full(tx) => {
                let _ = tx.try_send(reply);
            }
        }
    }

    fn is_closed(&self) -> bool {
        match self {
            ReplyTo::Plain(tx) => tx.is_closed(),
            ReplyTo::Full(tx) => tx.is_closed(),
        }
    }
}

struct ThumbJob {
    work: ThumbWork,
    reply: ReplyTo,
    cancel: Option<CancelToken>,
}

impl ThumbJob {
    fn abandoned(&self) -> bool {
        self.reply.is_closed() || self.cancel.as_ref().is_some_and(|cancel| cancel.load(Ordering::Acquire))
    }

    fn finish(self) {
        if self.abandoned() {
            self.reply.send(ThumbReply::Dropped);
            return;
        }
        let reply = match self.work.run() {
            Some(image) => ThumbReply::Image(image),
            None => ThumbReply::Failed,
        };
        self.reply.send(reply);
    }
}

#[derive(Default)]
struct ThumbQueues {
    tiles: VecDeque<ThumbJob>,
    background: VecDeque<ThumbJob>,
}

struct ThumbQueue {
    queues: Mutex<ThumbQueues>,
    ready: Condvar,
    stopping: AtomicBool,
}

impl ThumbQueue {
    fn new() -> Self {
        Self { queues: Mutex::new(ThumbQueues::default()), ready: Condvar::new(), stopping: AtomicBool::new(false) }
    }

    fn push_tile(&self, job: ThumbJob) {
        if self.stopping.load(Ordering::Acquire) {
            job.reply.send(ThumbReply::Dropped);
            return;
        }
        let evicted = {
            let mut queues = self.queues.lock().expect("thumbnail queue poisoned");
            queues.tiles.push_back(job);
            if queues.tiles.len() > THUMB_QUEUE_CAP {
                queues.tiles.pop_front()
            } else {
                None
            }
        };
        if let Some(evicted) = evicted {
            evicted.reply.send(ThumbReply::Dropped);
        }
        self.ready.notify_one();
    }

    fn push_background(&self, job: ThumbJob) {
        if self.stopping.load(Ordering::Acquire) {
            job.reply.send(ThumbReply::Dropped);
            return;
        }
        self.queues.lock().expect("thumbnail queue poisoned").background.push_back(job);
        self.ready.notify_one();
    }

    /// Tiles newest-first (what is on screen now), then background work.
    fn next(&self) -> Option<ThumbJob> {
        let mut queues = self.queues.lock().expect("thumbnail queue poisoned");
        loop {
            if self.stopping.load(Ordering::Acquire) {
                return None;
            }
            if let Some(job) = queues.tiles.pop_back().or_else(|| queues.background.pop_front()) {
                return Some(job);
            }
            queues = self.ready.wait(queues).expect("thumbnail queue poisoned");
        }
    }

    fn stop(&self) {
        self.stopping.store(true, Ordering::Release);
        let drained = std::mem::take(&mut *self.queues.lock().expect("thumbnail queue poisoned"));
        for job in drained.tiles.into_iter().chain(drained.background) {
            job.reply.send(ThumbReply::Dropped);
        }
        self.ready.notify_all();
    }
}

fn thumbnail_worker(queue: &ThumbQueue) {
    while let Some(job) = queue.next() {
        job.finish();
    }
}

struct PreviewSlot {
    latest: Mutex<Option<ThumbJob>>,
    ready: Condvar,
    stopping: AtomicBool,
}

impl PreviewSlot {
    fn new() -> Self {
        Self { latest: Mutex::new(None), ready: Condvar::new(), stopping: AtomicBool::new(false) }
    }

    fn submit(&self, job: ThumbJob) {
        if self.stopping.load(Ordering::Acquire) {
            job.reply.send(ThumbReply::Dropped);
            return;
        }
        let superseded = self.latest.lock().expect("preview slot poisoned").replace(job);
        if let Some(superseded) = superseded {
            superseded.reply.send(ThumbReply::Dropped);
        }
        self.ready.notify_one();
    }

    fn next(&self) -> Option<ThumbJob> {
        let mut latest = self.latest.lock().expect("preview slot poisoned");
        loop {
            if self.stopping.load(Ordering::Acquire) {
                return None;
            }
            if let Some(job) = latest.take() {
                return Some(job);
            }
            latest = self.ready.wait(latest).expect("preview slot poisoned");
        }
    }

    fn stop(&self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(job) = self.latest.lock().expect("preview slot poisoned").take() {
            job.reply.send(ThumbReply::Dropped);
        }
        self.ready.notify_all();
    }
}

fn preview_worker(slot: &PreviewSlot) {
    while let Some(job) = slot.next() {
        job.finish();
    }
}

/// Upright thumbnails through the engine: RAW embedded previews with their
/// container orientation, EXIF orientation, scaled JPEG decode and HEIC.
/// RAW files are memory-mapped and only their preview is decoded, so the
/// byte cap applies to everything else.
fn image_thumbnail(path: &Path, dim: i32) -> Option<DecodedImage> {
    let raw = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(fileid_engine::pipeline::raw_preview::is_raw_extension);
    if !raw && std::fs::metadata(path).ok()?.len() > THUMB_MAX_BYTES {
        return None;
    }
    fileid_engine::shell::thumbnail::render_oriented(path, dim).ok()
}

fn video_thumbnail(path: &Path, dim: i32) -> Option<DecodedImage> {
    let frame = fileid_engine::shell::video::keyframe_25pct(path).ok()?;
    fileid_engine::shell::thumbnail::from_rgb(frame.width, frame.height, frame.rgb, dim).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fileid_engine::ipc::{EngineError, EngineInfo, ScanComplete, Wrap};
    use std::io::Cursor;
    use std::sync::atomic::AtomicUsize;

    fn drain(stream: String) -> Vec<RawEvent> {
        let (tx, rx) = async_channel::unbounded();
        drain_engine_stdout(Cursor::new(stream), tx);
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    #[test]
    fn a_ready_then_crash_flap_exhausts_the_respawn_budget() {
        let mut budget = RespawnBudget::default();
        let start = Instant::now();
        let mut delays = Vec::new();
        for cycle in 0..4u64 {
            let at = start + Duration::from_secs(cycle * 5);
            budget.record_ready(at);
            delays.push(budget.next_delay(at + Duration::from_secs(1)));
        }
        assert_eq!(
            delays,
            [Some(Duration::from_secs(1)), Some(Duration::from_secs(4)), Some(Duration::from_secs(16)), None]
        );
    }

    #[test]
    fn a_stable_engine_refills_the_budget_and_old_attempts_age_out() {
        let mut budget = RespawnBudget::default();
        let start = Instant::now();
        for _ in 0..3 {
            assert!(budget.next_delay(start).is_some());
        }
        assert!(budget.next_delay(start).is_none());
        budget.record_ready(start + Duration::from_secs(10));
        assert_eq!(budget.next_delay(start + Duration::from_secs(41)), Some(Duration::from_secs(1)));
        let mut aging = RespawnBudget::default();
        for _ in 0..3 {
            aging.next_delay(start);
        }
        assert_eq!(aging.next_delay(start + Duration::from_secs(61)), Some(Duration::from_secs(1)));
    }

    #[test]
    fn ready_alone_does_not_make_the_client_ready() {
        let mut client = EngineClient::new();
        let (request_id, generation) = client.begin_health_probe().unwrap();
        assert_eq!(generation, client.generation);
        assert!(!client.is_ready());
        assert!(client.settle_health_probe(&request_id, 4242));
        assert!(client.is_ready());
    }

    #[test]
    fn health_probe_from_a_retired_generation_is_ignored() {
        let mut client = EngineClient::new();
        let (stale, _) = client.begin_health_probe().unwrap();
        client.generation = client.generation.wrapping_add(1);
        client.pending_health = None;
        let (fresh, _) = client.begin_health_probe().unwrap();
        assert!(!client.settle_health_probe(&stale, 4242));
        assert!(!client.is_ready());
        assert!(client.settle_health_probe(&fresh, 4343));
    }

    #[test]
    fn a_foreign_nonce_never_satisfies_readiness() {
        let mut client = EngineClient::new();
        let _ = client.begin_health_probe().unwrap();
        assert!(!client.settle_health_probe("health-999-999", 4242));
        assert!(!client.is_ready());
    }

    #[test]
    fn duplicate_ready_does_not_invalidate_the_probe_in_flight() {
        let mut client = EngineClient::new();
        let (first, _) = client.begin_health_probe().unwrap();
        assert!(client.begin_health_probe().is_none());
        assert!(client.settle_health_probe(&first, 4242));
    }

    #[test]
    fn abandoning_a_failed_send_clears_only_its_own_probe() {
        let mut client = EngineClient::new();
        let (first, generation) = client.begin_health_probe().unwrap();
        client.abandon_health_probe("health-0-999", generation);
        assert!(client.pending_health.is_some());
        client.abandon_health_probe(&first, generation);
        assert!(client.pending_health.is_none());
        assert!(!client.is_ready());
    }

    #[cfg(unix)]
    #[test]
    fn shutdown_allows_engine_to_exit_after_stdin_eof() {
        let marker = std::env::temp_dir().join(format!("fileid-linux-graceful-shutdown-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let mut child = Command::new("sh")
            .args(["-c", "cat >/dev/null; printf graceful > \"$FILEID_MARKER\""])
            .env("FILEID_MARKER", &marker)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut client = EngineClient::new();
        client.stdin = Some(Arc::new(Mutex::new(child.stdin.take().unwrap())));
        client.child = Some(child);
        client.shutdown();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !marker.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(marker.exists(), "shutdown killed the engine before graceful EOF handling");
        let _ = std::fs::remove_file(marker);
    }

    #[cfg(unix)]
    #[test]
    fn requested_shutdown_suppresses_respawn() {
        let mut child = Command::new("cat").stdin(Stdio::piped()).stdout(Stdio::null()).spawn().unwrap();
        let mut client = EngineClient::new();
        client.stdin = Some(Arc::new(Mutex::new(child.stdin.take().unwrap())));
        client.child = Some(child);
        client.send(CommandPayload::Shutdown(fileid_engine::ipc::Empty {})).unwrap();
        assert!(client.shutting_down.load(Ordering::Acquire));
    }

    #[cfg(unix)]
    #[test]
    fn selected_folder_is_sent_to_child_as_an_engine_scan_frame() {
        let root = std::env::temp_dir().join(format!("fileid-ipc-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.to_str().unwrap();
        let legacy = serde_json::json!({"cmd": "startScan", "id": "scan-1", "rootPath": root});
        assert!(serde_json::from_value::<IpcCommand>(legacy).is_err());

        let mut child = Command::new("cat").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut client = EngineClient::new();
        client.stdin = Some(Arc::new(Mutex::new(child.stdin.take().unwrap())));
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
    fn reader_maps_events_and_never_forwards_an_unproven_ready() {
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
        let progress = IpcEvent::now(EventPayload::ModelDownloadProgress(Wrap::new(ModelDownloadProgress {
            model_kind: "arcface".to_owned(),
            fraction: 0.4,
            message: "Downloading face models".to_owned(),
            bytes_done: Some(40),
            total_bytes: Some(100),
        })));
        let error = IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
            kind: "models_not_installed".to_owned(),
            message: "Install models before scanning".to_owned(),
            path: None,
            model_kind: None,
        })));
        let busy = IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
            kind: "face_clustering_busy".to_owned(),
            message: "busy".to_owned(),
            path: None,
            model_kind: None,
        })));
        let stream = format!(
            "{{\"t\":\"2026-01-01T00:00:00Z\",\"payload\":{{\"futureEvent\":{{\"message\":\"ready\"}}}}}}\n\n{}\n{}\n{}\n{}\n{}\n",
            serde_json::to_string(&ready).unwrap(),
            serde_json::to_string(&done).unwrap(),
            serde_json::to_string(&progress).unwrap(),
            serde_json::to_string(&error).unwrap(),
            serde_json::to_string(&busy).unwrap(),
        );
        let events = drain(stream);
        assert!(matches!(events[0], RawEvent::Announced));
        assert!(matches!(events[1], RawEvent::State(EngineState::ScanComplete(13))));
        assert!(matches!(&events[2], RawEvent::State(EngineState::ModelDownloadProgress(p))
            if p.model_kind == "arcface" && p.bytes_done == Some(40) && p.total_bytes == Some(100)));
        assert!(matches!(&events[3], RawEvent::State(EngineState::Error { kind, model_kind, .. })
            if kind == "models_not_installed" && model_kind.is_none()));
        assert!(matches!(&events[4], RawEvent::State(EngineState::FaceClusteringBusy(m)) if m == "busy"));
        assert!(matches!(events[5], RawEvent::State(EngineState::Exited)));
        assert_eq!(events.len(), 6);
    }

    #[test]
    fn workbench_responses_keep_request_and_conversation_ids() {
        use fileid_engine::ipc::{CatalogResponse, ChatResponse, ToolResponse};
        let events = [
            IpcEvent::now(EventPayload::CatalogResponse(Wrap::new(CatalogResponse {
                request_id: "catalog-1".into(),
                status: "ok".into(),
                message: None,
                hits: vec![],
                chapters: vec![],
                jobs: vec![],
                events: None,
                takes: None,
                recommendation: None,
                suggested_take_groups: None,
            }))),
            IpcEvent::now(EventPayload::ToolResponse(Wrap::new(ToolResponse {
                request_id: "tools-2".into(),
                status: "error".into(),
                message: "Unavailable".into(),
                operation_id: None,
                outputs: vec![],
                capabilities: vec![],
            }))),
            IpcEvent::now(EventPayload::ChatResponse(Wrap::new(ChatResponse {
                request_id: "chat-3".into(),
                conversation_id: "conversation".into(),
                status: "completed".into(),
                message: "Retrieved".into(),
                messages: vec![],
                hits: vec![],
            }))),
        ];
        let stream = events.iter().map(|e| serde_json::to_string(e).unwrap()).collect::<Vec<_>>().join("\n");
        let events = drain(stream);
        assert!(matches!(&events[0], RawEvent::State(EngineState::CatalogResponse(r)) if r.request_id == "catalog-1"));
        assert!(matches!(&events[1], RawEvent::State(EngineState::ToolResponse(r)) if r.request_id == "tools-2"));
        assert!(matches!(&events[2], RawEvent::State(EngineState::ChatResponse(r))
            if r.request_id == "chat-3" && r.conversation_id == "conversation"));
        assert!(matches!(events[3], RawEvent::State(EngineState::Exited)));
    }

    #[test]
    fn health_answers_carry_their_nonce_and_pid() {
        let answer = IpcEvent::now(EventPayload::HealthCheckResult(Wrap::new(
            fileid_engine::ipc::HealthCheckResult { request_id: "health-1-2".into(), pid: 77 },
        )));
        let events = drain(serde_json::to_string(&answer).unwrap());
        assert!(matches!(&events[0], RawEvent::Health { request_id, pid } if request_id == "health-1-2" && *pid == 77));
    }

    #[test]
    fn engine_frame_reader_rejects_oversize_and_resynchronizes() {
        let mut reader = Cursor::new(b"123456\nok\n");
        let mut frame = Vec::new();
        assert!(matches!(read_bounded_line(&mut reader, &mut frame, 4).unwrap(), BoundedLine::Oversize));
        assert!(matches!(read_bounded_line(&mut reader, &mut frame, 4).unwrap(), BoundedLine::Line));
        assert_eq!(frame, b"ok");
    }

    #[test]
    fn engine_frame_reader_bounds_unterminated_input() {
        let mut reader = Cursor::new(vec![b'x'; 128]);
        let mut frame = Vec::new();
        assert!(matches!(read_bounded_line(&mut reader, &mut frame, 16).unwrap(), BoundedLine::Oversize));
        assert!(frame.is_empty());
    }

    #[test]
    fn query_queue_is_bounded_and_latest_request_wins() {
        let state = QueryWorkerState::new();
        let (first_reply, first_rx) = async_channel::bounded(1);
        state.submit(QueryJob { spec: QuerySpec { search: "first".into(), ..QuerySpec::default() }, reply: first_reply });
        let (second_reply, _second_rx) = async_channel::bounded(1);
        state.submit(QueryJob { spec: QuerySpec { search: "second".into(), ..QuerySpec::default() }, reply: second_reply });
        assert!(matches!(first_rx.try_recv(), Err(async_channel::TryRecvError::Closed)));
        assert_eq!(state.latest.lock().unwrap().pending[0].as_ref().unwrap().spec.search, "second");
    }

    #[test]
    fn query_channels_do_not_cancel_each_other_and_take_turns() {
        let state = QueryWorkerState::new();
        let submit = |search: &str, channel| {
            let (reply, rx) = async_channel::bounded(1);
            state.submit(QueryJob { spec: QuerySpec { search: search.into(), channel, ..QuerySpec::default() }, reply });
            rx
        };
        let tools = submit("picker", QueryChannel::Tools);
        let _library = submit("grid", QueryChannel::Library);
        let _preview = submit("siblings", QueryChannel::Preview);
        assert!(!tools.is_closed());
        let mut slots = state.latest.lock().unwrap();
        let order: Vec<_> = std::iter::from_fn(|| slots.take_next()).map(|job| job.spec.search).collect();
        assert_eq!(order, ["grid", "siblings", "picker"]);
        assert!(slots.is_empty());
    }

    #[test]
    fn closed_one_shot_subscribers_are_pruned() {
        let (closed_sender, closed_receiver) = async_channel::bounded(1);
        let (live_sender, _live_receiver) = async_channel::bounded(1);
        drop(closed_receiver);
        let mut subscribers = vec![closed_sender, live_sender];
        prune_closed_subscribers(&mut subscribers);
        assert_eq!(subscribers.len(), 1);
        assert!(!subscribers[0].is_closed());
    }

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn engine_event_channels_are_bounded() {
        assert!(RAW_EVENT_CAP > 0 && RAW_EVENT_CAP <= 8);
        assert!(SUBSCRIBER_EVENT_CAP > 0 && SUBSCRIBER_EVENT_CAP <= 4);
    }

    fn probe_job(run: impl FnOnce() -> Option<DecodedImage> + Send + 'static) -> (ThumbJob, Receiver<ThumbReply>, CancelToken) {
        let (reply, rx) = async_channel::bounded(1);
        let cancel = CancelToken::default();
        (ThumbJob { work: ThumbWork::Custom(Box::new(run)), reply: ReplyTo::Full(reply), cancel: Some(cancel.clone()) }, rx, cancel)
    }

    #[test]
    fn a_full_tile_queue_evicts_the_oldest_and_serves_the_newest_first() {
        let queue = ThumbQueue::new();
        let mut replies = Vec::new();
        for _ in 0..=THUMB_QUEUE_CAP {
            let (job, rx, _) = probe_job(|| None);
            queue.push_tile(job);
            replies.push(rx);
        }
        assert!(matches!(replies[0].try_recv(), Ok(ThumbReply::Dropped)));
        assert_eq!(queue.queues.lock().unwrap().tiles.len(), THUMB_QUEUE_CAP);
        let newest = queue.next().unwrap();
        newest.finish();
        assert!(matches!(replies[THUMB_QUEUE_CAP].try_recv(), Ok(ThumbReply::Failed)));
    }

    #[test]
    fn a_still_bound_tile_is_not_starved_by_a_fling() {
        let queue = ThumbQueue::new();
        for _ in 0..(THUMB_QUEUE_CAP * 3) {
            let (job, _rx, cancel) = probe_job(|| None);
            cancel.store(true, Ordering::Release);
            queue.push_tile(job);
        }
        let ran = Arc::new(AtomicBool::new(false));
        let flag = ran.clone();
        let (wanted, rx, _) = probe_job(move || {
            flag.store(true, Ordering::SeqCst);
            None
        });
        queue.push_tile(wanted);
        queue.next().unwrap().finish();
        assert!(ran.load(Ordering::SeqCst));
        assert!(matches!(rx.try_recv(), Ok(ThumbReply::Failed)));
    }

    #[test]
    fn cancelled_and_superseded_work_is_skipped_without_decoding() {
        let queue = ThumbQueue::new();
        let (job, rx, cancel) = probe_job(|| panic!("cancelled work must not decode"));
        queue.push_background(job);
        cancel.store(true, Ordering::Release);
        queue.next().unwrap().finish();
        assert!(matches!(rx.try_recv(), Ok(ThumbReply::Dropped)));

        let slot = PreviewSlot::new();
        let (first, first_rx, _) = probe_job(|| panic!("superseded preview must not decode"));
        let (second, _second_rx, _) = probe_job(|| None);
        slot.submit(first);
        slot.submit(second);
        assert!(matches!(first_rx.try_recv(), Ok(ThumbReply::Dropped)));
    }

    #[test]
    fn thumbnail_decode_concurrency_never_exceeds_worker_count() {
        let queue = Arc::new(ThumbQueue::new());
        let workers: Vec<_> = (0..THUMB_WORKERS)
            .map(|_| {
                let queue = queue.clone();
                thread::spawn(move || thumbnail_worker(&queue))
            })
            .collect();
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut replies = Vec::new();
        for _ in 0..(THUMB_WORKERS * 4) {
            let (active, peak) = (active.clone(), peak.clone());
            let (job, rx, _) = probe_job(move || {
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(10));
                active.fetch_sub(1, Ordering::SeqCst);
                None
            });
            queue.push_background(job);
            replies.push(rx);
        }
        for reply in replies {
            assert!(matches!(reply.recv_blocking().unwrap(), ThumbReply::Failed));
        }
        queue.stop();
        for worker in workers {
            worker.join().unwrap();
        }
        assert!(peak.load(Ordering::SeqCst) <= THUMB_WORKERS);
    }

    #[test]
    fn oversized_sparse_image_is_not_decoded() {
        let path = std::env::temp_dir().join(format!("fileid-linux-thumb-cap-{}.png", std::process::id()));
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(THUMB_MAX_BYTES + 1).unwrap();
        drop(file);
        assert!(image_thumbnail(&path, 64).is_none());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn raw_files_skip_the_byte_cap_and_still_fail_cleanly_without_a_preview() {
        let path = std::env::temp_dir().join(format!("fileid-linux-thumb-raw-{}.ARW", std::process::id()));
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(THUMB_MAX_BYTES + 1).unwrap();
        drop(file);
        assert!(image_thumbnail(&path, 64).is_none());
        let _ = std::fs::remove_file(path);
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
                )
                .unwrap();
            }
            conn.execute(
                "INSERT INTO files (path_text,path_hash,size_bytes,scanned_at,kind,extension,failed) VALUES (?1,4,17,4,'image','png',1)",
                [fixture.join("failed.png").to_str().unwrap()],
            )
            .unwrap();
            conn.execute("INSERT INTO tags (file_id,tag,source) VALUES (1,'urgent','manual')", []).unwrap();
            conn.execute("INSERT INTO ocr_text (file_id,text) VALUES (3,'needle in OCR')", []).unwrap();
        }
        let query = |search: &str, kind: Option<&str>, limit: i64| {
            query_files_at_path(
                &db_path,
                &QuerySpec { search: search.to_owned(), kind: kind.map(str::to_owned), limit, ..QuerySpec::default() },
            )
            .unwrap()
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
        assert!(query_files_at_path(&fixture.join("missing.sqlite"), &QuerySpec { limit: 10, ..QuerySpec::default() }).is_err());
        assert_eq!(file_row_at_path(&db_path, 2).unwrap().unwrap().name, "image.png");
        assert!(file_row_at_path(&db_path, 4).unwrap().is_none());
        assert!(file_row_at_path(&db_path, 99).unwrap().is_none());
        std::fs::remove_dir_all(fixture).unwrap();
    }
}
