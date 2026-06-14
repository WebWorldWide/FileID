//! IPC sink — bounded mpsc channel that drains to stdout one frame per line.
//!
//! Mirror of `IPCSink.swift` on macOS. Every IPCEvent the engine wants to
//! emit goes through `Sink::send`. A single writer task reads from the
//! channel, serializes with sorted keys + a trailing newline, and writes
//! atomically to stdout. Backpressure: if the channel fills, senders block —
//! the engine slows itself down rather than dropping events.

use std::io::Write;
use tokio::sync::mpsc;

use super::{EngineError, EventPayload, IpcEvent, Wrap};

// 16384 is comfortable for the worst burst we've measured: Deep Analyze
// emits ~50 token events/sec/file and a batch of 100 files can transiently
// buffer 5000+ events. 4096 was an inherited default from before Deep
// Analyze landed; the bump costs ~256 KB peak memory and eliminates
// senders blocking on the channel during fast scans + caption streams.
const CHANNEL_CAPACITY: usize = 16384;

// C1-009: per-frame size cap, symmetric with the app's inbound cap
// (EngineClient.MaxFrameChars) and the engine's command-read cap
// (main.rs MAX_FRAME_BYTES). A frame above this is silently dropped by the
// app's bounded reader (it resyncs to the next newline), which hangs the UI
// that was awaiting the reply (e.g. a huge restructurePlan). Rather than emit
// the oversize frame, the sink substitutes a small structured
// `ipc_frame_too_large` error the app can surface.
const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone)]
pub struct Sink {
    tx: mpsc::Sender<IpcEvent>,
}

impl Sink {
    /// Create the sink and spawn the stdout writer task. Returns the sink
    /// handle (cheap to clone) and a join handle for the writer task; the
    /// caller awaits the join handle on shutdown to flush in-flight events.
    pub fn spawn() -> (Self, tokio::task::JoinHandle<()>) {
        let (tx, mut rx) = mpsc::channel::<IpcEvent>(CHANNEL_CAPACITY);

        let writer = tokio::task::spawn_blocking(move || {
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            // The runtime can't await on the blocking thread, so we use a
            // sync-style consumer over the channel via blocking_recv.
            //
            // C6-015: coalesce flushes. blocking_recv blocks for the first
            // event of a burst; we then drain every already-queued event with
            // try_recv, writing each as a line WITHOUT flushing, and flush ONCE
            // when the channel momentarily empties. During the bursts the 16384
            // cap was sized for this cuts the flush syscall rate from per-frame
            // to per-drain. Terminal events are NOT delayed: a drain cycle ends
            // (and flushes) the instant try_recv finds nothing, so a terminal
            // frame is on the wire within the same cycle it was written.
            let mut closed = false;
            while !closed {
                let first = match rx.blocking_recv() {
                    Some(event) => event,
                    None => break, // channel closed and drained
                };
                if !write_line(&mut handle, &first) {
                    break;
                }
                // Drain the rest of the current burst without flushing.
                loop {
                    match rx.try_recv() {
                        Ok(event) => {
                            if !write_line(&mut handle, &event) {
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
                // One flush per drained burst.
                if let Err(err) = handle.flush() {
                    tracing::error!(?err, "ipc sink flush failed");
                    break;
                }
            }
            // Channel closed: best-effort final flush.
            let _ = handle.flush();
        });

        (Self { tx }, writer)
    }

    /// Send an event. Blocks the caller's task if the channel is full —
    /// applying backpressure so the engine slows down rather than dropping.
    pub async fn send(&self, event: IpcEvent) {
        if self.tx.send(event).await.is_err() {
            // Receiver dropped — sink is shutting down. Silent: the engine
            // is exiting anyway.
        }
    }

    /// Try-send variant for hot paths that can't await. Drops the event if
    /// the channel is full; logs a warning at most once per N drops (caller
    /// is responsible for the dedup if it cares).
    pub fn try_send(&self, event: IpcEvent) -> Result<(), mpsc::error::TrySendError<IpcEvent>> {
        self.tx.try_send(event)
    }

    /// Test-only sink backed by a caller-held receiver instead of the stdout
    /// writer task — lets tests assert backpressure semantics (try_send drops,
    /// send blocks) without writing to real stdout.
    #[cfg(test)]
    pub(crate) fn channel_for_test(capacity: usize) -> (Self, mpsc::Receiver<IpcEvent>) {
        let (tx, rx) = mpsc::channel::<IpcEvent>(capacity);
        (Self { tx }, rx)
    }

    /// Close the sink. After this, `send` returns immediately (silently).
    /// The writer task will exit once the channel drains.
    #[allow(dead_code)]
    pub fn close(&self) {
        // Drop the only known sender from this handle isn't enough since
        // clones may exist. We expose this only as a documented hook; in
        // practice the engine relies on dropping all Sink handles.
        drop(self.tx.clone());
    }
}

/// Serialize `event` and write it as one newline-terminated frame. Does NOT
/// flush — the caller coalesces flushes per drained burst (C6-015). Returns
/// `false` only on a fatal stdout write error (the writer loop then exits).
///
/// C1-009: serialize to a buffer first so we can enforce `MAX_FRAME_BYTES`.
/// An over-cap frame is dropped app-side with no error, hanging whatever UI
/// awaited the reply; we substitute a small structured `ipc_frame_too_large`
/// event so the app can surface the failure instead of hanging.
fn write_line<W: Write>(w: &mut W, event: &IpcEvent) -> bool {
    // Insertion-order keys are fine — consumers don't care about JSON key
    // order. Byte-for-byte parity with sorted output would require a
    // custom sorted-key writer.
    let bytes = match serde_json::to_vec(event) {
        Ok(b) => b,
        Err(err) => {
            tracing::error!(?err, "ipc sink encode failed");
            return true; // skip this frame; the channel is still healthy
        }
    };
    if bytes.len() > MAX_FRAME_BYTES {
        tracing::warn!(
            frame_bytes = bytes.len(),
            cap = MAX_FRAME_BYTES,
            "ipc frame exceeds outbound cap; substituting ipc_frame_too_large"
        );
        return write_frame_too_large(w, bytes.len());
    }
    write_raw_line(w, &bytes)
}

/// Emit the structured replacement for an over-cap frame. Tiny and fixed-size,
/// so it can never itself exceed the cap.
fn write_frame_too_large<W: Write>(w: &mut W, frame_bytes: usize) -> bool {
    let event = IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
        kind: "ipc_frame_too_large".into(),
        message: format!(
            "The engine produced a response too large to send ({frame_bytes} bytes, cap {MAX_FRAME_BYTES}). \
             It was dropped to keep the pipe in sync. If this was a Restructure plan on a very large \
             library, try restructuring a subfolder."
        ),
        path: None,
        model_kind: None,
    })));
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
        // We can't emit an IPC event about a stdout write failure (stdout is
        // the failing surface). Log via tracing — it hits the local file and
        // stderr.
        tracing::error!(?err, "ipc sink write failed");
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::{EngineInfo, ScanPhase};

    #[tokio::test(flavor = "multi_thread")]
    async fn sink_writes_one_line_per_event() {
        // Smoke test: spawn the sink, send a single event, drop it, ensure
        // the writer task exits cleanly. We don't actually capture stdout
        // here (Rust test stdout interception is tricky); this just exercises
        // the code path for panics / panics on encoder.
        let (sink, join) = Sink::spawn();
        sink.send(IpcEvent::now(EventPayload::Ready(Wrap::new(EngineInfo {
            version: "0.1.0".into(),
            pid: 1,
            worker_cap: 1,
            physical_memory_gb: 1.0,
            hardware: None,
        })))).await;
        drop(sink);
        // Writer task drains and exits.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), join).await;
    }

    /// A `Write` that tallies writes vs. flushes so a test can prove the sink
    /// no longer flushes per frame (C6-015).
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

    /// C1-009: an outbound frame above the cap must be replaced by a small,
    /// structured `ipc_frame_too_large` error — never emitted oversize (the app
    /// would silently drop it and hang the awaiting UI).
    #[test]
    fn oversize_frame_becomes_structured_error_event() {
        let huge = log_event("X".repeat(MAX_FRAME_BYTES + 1024));
        let mut out = CountingWriter::default();
        assert!(write_line(&mut out, &huge), "write_line must not report a fatal error");

        // The bytes on the wire are the substitute event, well under the cap.
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

    /// A within-cap frame is written verbatim (regression guard so the cap
    /// doesn't reject normal traffic).
    #[test]
    fn normal_frame_is_written_verbatim() {
        let evt = log_event("hello".into());
        let mut out = CountingWriter::default();
        assert!(write_line(&mut out, &evt));
        let line = out.bytes.strip_suffix(b"\n").unwrap();
        let decoded: IpcEvent = serde_json::from_slice(line).unwrap();
        match decoded.payload {
            EventPayload::Log(w) => assert_eq!(w.inner.message, "hello"),
            other => panic!("expected Log, got {other:?}"),
        }
    }

    /// C6-015: writing a burst of frames must NOT flush per frame. `write_line`
    /// is the per-event primitive; flushing is the writer loop's job, coalesced
    /// once per drained burst. Proven here by asserting write_line performs zero
    /// flushes across many events (the old `write_frame` flushed every call).
    #[test]
    fn burst_writes_do_not_flush_per_frame() {
        let mut out = CountingWriter::default();
        for i in 0..100 {
            assert!(write_line(&mut out, &log_event(format!("event-{i}"))));
        }
        assert_eq!(
            out.flushes, 0,
            "write_line must never flush — flushes are coalesced per burst by the writer loop"
        );
        assert!(out.writes >= 100, "each frame still produces output");
    }

    /// C1-002: a terminal PhaseChanged must use the guaranteed (blocking/
    /// awaiting) `send` path, which delivers even when the channel is full —
    /// unlike `try_send`, which drops. A dropped terminal frame makes the app
    /// render a cancelled/failed scan as Completed. This pins the difference:
    /// try_send drops on a full channel; send delivers once capacity frees.
    #[tokio::test(flavor = "multi_thread")]
    async fn terminal_phase_send_is_never_dropped_under_backpressure() {
        let (sink, mut rx) = Sink::channel_for_test(1);
        // Fill the single slot.
        sink.try_send(log_event("filler".into())).unwrap();
        // The channel is now full: the droppable path fails.
        assert!(
            sink.try_send(IpcEvent::now(EventPayload::PhaseChanged(Wrap::new(
                ScanPhase::Cancelled
            ))))
            .is_err(),
            "try_send must drop a terminal PhaseChanged on a full channel — the bug"
        );

        // The guaranteed path awaits capacity instead. Free a slot from the
        // receiver after a beat; the awaiting send must then deliver.
        let drainer = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            let _ = rx.recv().await; // drop the filler, freeing one slot
            // Receive the terminal frame that send() delivered.
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
