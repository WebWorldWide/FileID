//! Bounded mpsc channel that drains to stdout one frame per line (mirror of `IPCSink.swift`). A single writer task
//! serializes and writes atomically; when the channel fills, senders block so the engine slows down rather than
//! dropping events.

use std::io::Write;
use tokio::sync::mpsc;

use super::{EngineError, IpcEvent};

// 16384 covers the worst measured burst (Deep Analyze ~50 token events/sec/file, 100-file batches buffer 5000+ events)
// for ~256 KB peak.
const CHANNEL_CAPACITY: usize = 16384;

// Symmetric with the app's inbound cap (EngineClient.MaxFrameBytes) and the engine's command-read cap (main.rs
// MAX_FRAME_BYTES), sized for a whole-library plan. The app silently drops a larger frame and resyncs, hanging the UI
// awaiting the reply (e.g. a huge restructurePlan), so the sink substitutes a small `ipc_frame_too_large` error.
const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// When not stdout: the macOS app-hosted engine writes events on fd 2, like the Swift engine (see
/// [`route_events_to_stderr`]).
static EVENT_WIRE: std::sync::OnceLock<std::fs::File> = std::sync::OnceLock::new();

#[cfg(unix)]
const STDERR_LOG_ROTATE_BYTES: u64 = 32 * 1024 * 1024;

/// The macOS app reads events from the engine's stderr. fd 2 is duplicated for the sink's exclusive use, then pointed
/// at `<logs>/engine-stderr.log`, so tracing, panics and native library chatter can never splice into an event line.
/// Call once at process start before any thread writes to stderr; if the log can't be opened events still go to fd 2.
#[cfg(unix)]
pub fn route_events_to_stderr(logs_dir: &std::path::Path) -> std::io::Result<()> {
    use std::os::fd::{AsRawFd, FromRawFd};

    // SAFETY: dup/fcntl on the process's own stderr descriptor; the new
    // descriptor is owned by the File below and never closed elsewhere.
    let wire = unsafe {
        let saved = libc::dup(libc::STDERR_FILENO);
        if saved < 0 {
            return Err(std::io::Error::last_os_error());
        }
        libc::fcntl(saved, libc::F_SETFD, libc::FD_CLOEXEC);
        std::fs::File::from_raw_fd(saved)
    };
    let log = (|| {
        crate::util::read_only::require_writable(logs_dir).map_err(std::io::Error::other)?;
        std::fs::create_dir_all(logs_dir)?;
        let path = logs_dir.join("engine-stderr.log");
        if std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() > STDERR_LOG_ROTATE_BYTES) {
            let _ = std::fs::rename(&path, logs_dir.join("engine-stderr.log.1"));
        }
        std::fs::OpenOptions::new().create(true).append(true).open(path)
    })();
    match log {
        // SAFETY: both descriptors are open; dup2 atomically repoints fd 2.
        Ok(log) => unsafe {
            libc::dup2(log.as_raw_fd(), libc::STDERR_FILENO);
        },
        Err(error) => {
            let _ = EVENT_WIRE.set(wire);
            return Err(error);
        }
    }
    EVENT_WIRE
        .set(wire)
        .map_err(|_| std::io::Error::other("the IPC event wire is already set"))
}

#[derive(Clone)]
pub struct Sink {
    tx: mpsc::Sender<IpcEvent>,
    command_id: Option<std::sync::Arc<str>>,
    before_terminal: Option<(fn(&super::EventPayload) -> bool, std::sync::Arc<dyn Fn() + Send + Sync>)>,
}

impl Sink {
    /// Returns the sink handle and the writer's join handle, which the caller awaits on shutdown to flush in-flight
    /// events.
    pub fn spawn() -> (Self, tokio::task::JoinHandle<()>) {
        let (tx, mut rx) = mpsc::channel::<IpcEvent>(CHANNEL_CAPACITY);

        let writer = tokio::task::spawn_blocking(move || match EVENT_WIRE.get() {
            Some(wire) => {
                let mut handle = std::io::BufWriter::with_capacity(64 * 1024, wire);
                drain_to(&mut handle, &mut rx);
            }
            None => {
                // Stdout is line-buffered, so unwrapped every frame is its own write call; flushed whenever the queue
                // runs empty (see `drain_to`).
                let stdout = std::io::stdout();
                let mut handle = std::io::BufWriter::with_capacity(64 * 1024, stdout.lock());
                drain_to(&mut handle, &mut rx);
            }
        });

        (Self { tx, command_id: None, before_terminal: None }, writer)
    }

    /// Blocks while the channel is full (backpressure).
    pub async fn send(&self, event: IpcEvent) {
        let event = self.stamped(event);
        if self.tx.send(event).await.is_err() {
            // Receiver dropped: the sink is shutting down.
        }
    }

    /// For hot paths that can't await: drops the event if the channel is full.
    pub fn try_send(&self, event: IpcEvent) -> Result<(), mpsc::error::TrySendError<IpcEvent>> {
        self.tx.try_send(self.stamped(event))
    }

    /// Stamps `command_id` on every `BulkActionResult` it sends so each reply from a spawned task carries the id of the
    /// command it answers (schema 1.28.0).
    pub fn for_command(&self, id: &str) -> Self {
        Self { command_id: Some(id.into()), ..self.clone() }
    }

    /// Runs `release` before queuing a job's terminal event: a client sends its next command as soon as it reads that
    /// event, possibly before the sending task has returned.
    pub(crate) fn releasing_before(
        &self,
        is_terminal: fn(&super::EventPayload) -> bool,
        release: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        Self { before_terminal: Some((is_terminal, std::sync::Arc::new(release))), ..self.clone() }
    }

    fn stamped(&self, mut event: IpcEvent) -> IpcEvent {
        if let (Some(id), super::EventPayload::BulkActionResult(result)) = (&self.command_id, &mut event.payload) {
            result.inner.command_id.get_or_insert_with(|| id.to_string());
        }
        if let Some((is_terminal, release)) = &self.before_terminal
            && is_terminal(&event.payload)
        {
            release();
        }
        event
    }

    /// Backed by a caller-held receiver so tests can assert backpressure (try_send drops, send blocks) without real
    /// stdout.
    #[cfg(test)]
    pub(crate) fn channel_for_test(capacity: usize) -> (Self, mpsc::Receiver<IpcEvent>) {
        let (tx, rx) = mpsc::channel::<IpcEvent>(capacity);
        (Self { tx, command_id: None, before_terminal: None }, rx)
    }
}

fn drain_to<W: Write>(handle: &mut W, rx: &mut mpsc::Receiver<IpcEvent>) {
    let mut scratch: Vec<u8> = Vec::new();
    // blocking_recv blocks for the first event of a burst; then every already-queued event is written with try_recv
    // WITHOUT flushing, and flushed ONCE when the channel empties, cutting flush syscalls from per-frame to per-drain.
    // Terminal events are not delayed: the cycle flushes as soon as try_recv finds nothing.
    let mut closed = false;
    while !closed {
        let first = match rx.blocking_recv() {
            Some(event) => event,
            None => break,
        };
        if !write_line(handle, &mut scratch, &first) {
            break;
        }
        loop {
            match rx.try_recv() {
                Ok(event) => {
                    if !write_line(handle, &mut scratch, &event) {
                        closed = true;
                        break;
                    }
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    closed = true;
                    break;
                }
            }
        }
        if let Err(err) = handle.flush() {
            tracing::error!(?err, "ipc sink flush failed");
            break;
        }
    }
    let _ = handle.flush();
}

/// Does NOT flush (the caller coalesces per burst); `false` only on a fatal write error. Serializes to a buffer first
/// to enforce `MAX_FRAME_BYTES`: an over-cap frame is dropped app-side with no error, so a small `ipc_frame_too_large`
/// event is substituted.
fn write_line<W: Write>(w: &mut W, buf: &mut Vec<u8>, event: &IpcEvent) -> bool {
    // Insertion-order keys are fine; consumers don't care about JSON key order. `buf` is reused so the drain loop
    // allocates no Vec per event.
    buf.clear();
    if let Err(err) = serde_json::to_writer(&mut *buf, event) {
        tracing::error!(?err, "ipc sink encode failed");
        return true;
    }
    if buf.len() > MAX_FRAME_BYTES {
        tracing::warn!(
            frame_bytes = buf.len(),
            cap = MAX_FRAME_BYTES,
            "ipc frame exceeds outbound cap; substituting ipc_frame_too_large"
        );
        return write_frame_too_large(w, buf.len());
    }
    write_raw_line(w, buf)
}

/// Tiny and fixed-size, so it can never itself exceed the cap.
fn write_frame_too_large<W: Write>(w: &mut W, frame_bytes: usize) -> bool {
    let event = EngineError::event(
        "ipc_frame_too_large",
        format!(
            "The engine produced a response too large to send ({frame_bytes} bytes, cap {MAX_FRAME_BYTES}). \
             It was dropped to keep the pipe in sync. If this was a Restructure plan on a very large \
             library, try restructuring a subfolder."
        ),
    );
    match serde_json::to_vec(&event) {
        Ok(bytes) => write_raw_line(w, &bytes),
        Err(err) => {
            tracing::error!(?err, "ipc_frame_too_large encode failed");
            true
        }
    }
}

fn write_raw_line<W: Write>(w: &mut W, bytes: &[u8]) -> bool {
    if let Err(err) = w.write_all(bytes).and_then(|_| w.write_all(b"\n")) {
        // stdout is the failing surface, so log via tracing (local file and stderr) instead of an IPC event.
        tracing::error!(?err, "ipc sink write failed");
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::{EngineInfo, EventPayload, ScanPhase, Wrap};

    #[tokio::test(flavor = "multi_thread")]
    async fn sink_writes_one_line_per_event() {
        // Smoke test: the writer task exits cleanly after one event (stdout is not captured).
        let (sink, join) = Sink::spawn();
        sink.send(IpcEvent::now(EventPayload::Ready(Wrap::new(EngineInfo {
            version: "0.1.0".into(),
            pid: 1,
            worker_cap: 1,
            physical_memory_gb: 1.0,
            engine: None,
            hardware: None,
        })))).await;
        drop(sink);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), join).await;
    }

    #[derive(Default)]
    struct CountingWriter {
        bytes: Vec<u8>,
        writes: usize,
        flushes: usize,
    }
    impl Write for CountingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.writes += 1;
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    fn log_event(message: String) -> IpcEvent {
        IpcEvent::now(EventPayload::Log(Wrap::new(crate::ipc::LogLine {
            level: crate::ipc::LogLevel::Info,
            message,
        })))
    }

    /// An oversize frame becomes a small `ipc_frame_too_large` error, never emitted oversize (the app would drop it and
    /// hang the UI).
    #[test]
    fn oversize_frame_becomes_structured_error_event() {
        let huge = log_event("X".repeat(MAX_FRAME_BYTES + 1024));
        let mut out = CountingWriter::default();
        let mut buf = Vec::new();
        assert!(write_line(&mut out, &mut buf, &huge), "write_line must not report a fatal error");

        assert!(
            out.bytes.len() < MAX_FRAME_BYTES,
            "substitute frame must be small, got {} bytes",
            out.bytes.len()
        );
        let line = out.bytes.strip_suffix(b"\n").expect("one trailing newline");
        let decoded: IpcEvent = serde_json::from_slice(line).expect("substitute is valid JSON");
        match decoded.payload {
            EventPayload::Error(w) => {
                assert_eq!(w.inner.kind, "ipc_frame_too_large");
            }
            other => panic!("expected ipc_frame_too_large Error, got {other:?}"),
        }
    }

    /// A within-cap frame is written verbatim.
    #[test]
    fn normal_frame_is_written_verbatim() {
        let evt = log_event("hello".into());
        let mut out = CountingWriter::default();
        let mut buf = Vec::new();
        assert!(write_line(&mut out, &mut buf, &evt));
        let line = out.bytes.strip_suffix(b"\n").unwrap();
        let decoded: IpcEvent = serde_json::from_slice(line).unwrap();
        match decoded.payload {
            EventPayload::Log(w) => assert_eq!(w.inner.message, "hello"),
            other => panic!("expected Log, got {other:?}"),
        }
    }

    /// `write_line` performs zero flushes across many events; flushing is the writer loop's job, once per drained
    /// burst.
    #[test]
    fn a_buffered_burst_reaches_the_stream_in_one_write_and_a_lone_event_is_not_held_back() {
        #[derive(Clone, Default)]
        struct Shared(std::sync::Arc<std::sync::Mutex<CountingWriter>>);
        impl Write for Shared {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().write(buf)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.0.lock().unwrap().flush()
            }
        }
        let stream = Shared::default();
        let (tx, mut rx) = mpsc::channel::<IpcEvent>(256);
        for i in 0..100 {
            tx.try_send(log_event(format!("event-{i}"))).unwrap();
        }
        let writer = {
            let stream = stream.clone();
            std::thread::spawn(move || {
                let mut handle = std::io::BufWriter::with_capacity(64 * 1024, stream);
                drain_to(&mut handle, &mut rx);
            })
        };
        let wait_for = |lines: usize| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let state = stream.0.lock().unwrap();
                if state.bytes.split(|b| *b == b'\n').count() > lines && state.flushes > 0 {
                    return (state.writes, state.flushes);
                }
                drop(state);
                assert!(std::time::Instant::now() < deadline, "events were held in the buffer");
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        };
        let (writes, _) = wait_for(100);
        assert!(writes < 100, "a burst is not one write per frame (got {writes})");
        tx.try_send(IpcEvent::now(EventPayload::PhaseChanged(Wrap::new(ScanPhase::Cancelled)))).unwrap();
        wait_for(101);
        drop(tx);
        writer.join().unwrap();
    }

    #[test]
    fn closing_the_channel_writes_and_flushes_every_buffered_event_including_the_terminal_one() {
        #[derive(Clone, Default)]
        struct Shared(std::sync::Arc<std::sync::Mutex<CountingWriter>>);
        impl Write for Shared {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().write(buf)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.0.lock().unwrap().flush()
            }
        }
        let stream = Shared::default();
        let (tx, mut rx) = mpsc::channel::<IpcEvent>(256);
        for i in 0..50 {
            tx.try_send(log_event(format!("event-{i}"))).unwrap();
        }
        tx.try_send(IpcEvent::now(EventPayload::PhaseChanged(Wrap::new(ScanPhase::Cancelled)))).unwrap();
        drop(tx);
        let mut handle = std::io::BufWriter::with_capacity(1 << 20, stream.clone());
        drain_to(&mut handle, &mut rx);
        let written = String::from_utf8(stream.0.lock().unwrap().bytes.clone()).unwrap();
        let lines: Vec<&str> = written.lines().collect();
        assert_eq!(lines.len(), 51, "nothing stays in the buffer when the sink closes");
        assert!(lines[50].contains("Cancelled") || lines[50].contains("cancelled"), "the terminal event is last: {}", lines[50]);
    }

    #[test]
    fn burst_writes_do_not_flush_per_frame() {
        let mut out = CountingWriter::default();
        let mut buf = Vec::new();
        for i in 0..100 {
            assert!(write_line(&mut out, &mut buf, &log_event(format!("event-{i}"))));
        }
        assert_eq!(
            out.flushes, 0,
            "write_line must never flush — flushes are coalesced per burst by the writer loop"
        );
        assert!(out.writes >= 100, "each frame still produces output");
    }

    /// A terminal PhaseChanged must use the awaiting `send` path, which delivers when the channel is full; `try_send`
    /// drops, and a dropped terminal frame renders a cancelled/failed scan as Completed.
    #[tokio::test(flavor = "multi_thread")]
    async fn terminal_phase_send_is_never_dropped_under_backpressure() {
        let (sink, mut rx) = Sink::channel_for_test(1);
        sink.try_send(log_event("filler".into())).unwrap();
        assert!(
            sink.try_send(IpcEvent::now(EventPayload::PhaseChanged(Wrap::new(
                ScanPhase::Cancelled
            ))))
            .is_err(),
            "try_send must drop a terminal PhaseChanged on a full channel — the bug"
        );

        let drainer = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            let _ = rx.recv().await;
            rx.recv().await
        });

        sink.send(IpcEvent::now(EventPayload::PhaseChanged(Wrap::new(
            ScanPhase::Cancelled,
        ))))
        .await;

        let delivered = tokio::time::timeout(std::time::Duration::from_secs(2), drainer)
            .await
            .expect("drainer did not hang")
            .expect("drainer task ok");
        match delivered {
            Some(evt) => match evt.payload {
                EventPayload::PhaseChanged(w) => assert_eq!(w.inner, ScanPhase::Cancelled),
                other => panic!("expected terminal PhaseChanged(Cancelled), got {other:?}"),
            },
            None => panic!("terminal PhaseChanged was dropped — guaranteed send failed"),
        }
    }
}
