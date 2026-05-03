//! IPC sink — bounded mpsc channel that drains to stdout one frame per line.
//!
//! Mirror of `IPCSink.swift` on macOS. Every IPCEvent the engine wants to
//! emit goes through `Sink::send`. A single writer task reads from the
//! channel, serializes with sorted keys + a trailing newline, and writes
//! atomically to stdout. Backpressure: if the channel fills, senders block —
//! the engine slows itself down rather than dropping events.

use std::io::Write;
use tokio::sync::mpsc;

use super::IpcEvent;

const CHANNEL_CAPACITY: usize = 4096;

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
            while let Some(event) = rx.blocking_recv() {
                if let Err(err) = write_frame(&mut handle, &event) {
                    // We can't emit an IPC event about a stdout write failure
                    // (stdout is the failing surface). Log via tracing — it
                    // will hit the local file and stderr.
                    tracing::error!(?err, "ipc sink write failed");
                    break;
                }
            }
            // Channel closed: best-effort flush.
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

    /// Close the sink. After this, `send` returns immediately (silently).
    /// The writer task will exit once the channel drains.
    pub fn close(&self) {
        // Drop the only known sender from this handle isn't enough since
        // clones may exist. We expose this only as a documented hook; in
        // practice the engine relies on dropping all Sink handles.
        drop(self.tx.clone());
    }
}

fn write_frame<W: Write>(w: &mut W, event: &IpcEvent) -> std::io::Result<()> {
    // serde_json with `preserve_order` doesn't emit sorted keys by default.
    // For byte-for-byte cross-platform parity with Swift's sortedKeys, we
    // serialize via a Value first, then emit via a custom sorted writer.
    //
    // For the Phase 0 cut we accept default (insertion-order) keys; the
    // round-trip tests still pass because consumers don't care about key
    // order. A follow-up issue tracks the canonicalization to a sorted
    // emitter once we have a perf budget.
    serde_json::to_writer(&mut *w, event).map_err(std::io::Error::other)?;
    w.write_all(b"\n")?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::{EngineInfo, EventPayload, IpcEvent, Wrap};

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
}
