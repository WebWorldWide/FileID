// Batched MobileCLIP inference coordinator.
//
// One Session, batched inputs. A dedicated OS thread owns the
// `MobileClipImage`. Tagging workers submit (rgb_256, oneshot) requests
// through a crossbeam channel. The coordinator drains up to BATCH_SIZE
// requests (or BATCH_TIMEOUT_MS, whichever first), packs them into one
// (N, 3, 256, 256) tensor, runs `embed_batch` once, and fans the
// embeddings back through the oneshots. Per-batch GPU dispatch is one
// wake instead of N, and VRAM stays at single-session footprint.
//
// Alternative was N-session pool, which exhausted VRAM on a 6 GB RTX
// 2060 and wedged the DirectML driver (full system hang).
//
// Failure mode: if `embed_batch` errors, every request in that batch
// receives the error — caller decides whether to skip the file or retry.
// The coordinator never panics on a single bad input.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use crossbeam_channel::{bounded, Receiver, Sender, TryRecvError};
use tokio::sync::oneshot;

use crate::models::mobileclip::MobileClipImage;

/// Max images per batched inference. DirectML's per-call dispatch
/// overhead amortizes well up to ~4; beyond that the linear cost of
/// the batched run dominates and per-file latency suffers without
/// further throughput gain. Tunable via `FILEID_CLIP_BATCH_SIZE` for
/// experimentation.
const DEFAULT_BATCH_SIZE: usize = 4;

/// Hard ceiling on how long the coordinator waits for batch fill once
/// it has at least one request. Tunable via `FILEID_CLIP_BATCH_TIMEOUT_MS`.
const DEFAULT_BATCH_TIMEOUT_MS: u64 = 20;

/// Channel depth — backs up tagging workers if CLIP is the bottleneck
/// instead of letting decode race ahead and balloon memory.
const REQUEST_CHANNEL_CAP: usize = 256;

/// Stats: lets `[STATS]` log average batch size so we can verify
/// batching is actually engaging. `batch_count` is the number of
/// inference calls; `batch_size_sum` divided by it = avg batch size.
pub static STATS_BATCH_COUNT: AtomicU64 = AtomicU64::new(0);
pub static STATS_BATCH_SIZE_SUM: AtomicU64 = AtomicU64::new(0);

struct ClipRequest {
    rgb_256: Vec<u8>,
    response: oneshot::Sender<Result<Vec<f32>>>,
}

/// Handle that tagging workers use to submit embed requests. Cheap to
/// clone (`Arc`-wrapped sender).
#[derive(Clone)]
pub struct ClipBatchCoordinator {
    sender: Sender<ClipRequest>,
}

impl ClipBatchCoordinator {
    /// Spawn the coordinator on a dedicated OS thread that owns
    /// `model` for the rest of the process. The thread exits when all
    /// senders are dropped (i.e. the Tagger has finished and the
    /// scan-session arc tree is being released).
    pub fn spawn(mut model: MobileClipImage) -> Arc<Self> {
        let batch_size = read_env_usize("FILEID_CLIP_BATCH_SIZE", DEFAULT_BATCH_SIZE).max(1);
        let batch_timeout_ms = read_env_usize("FILEID_CLIP_BATCH_TIMEOUT_MS", DEFAULT_BATCH_TIMEOUT_MS as usize) as u64;
        let (sender, receiver) = bounded::<ClipRequest>(REQUEST_CHANNEL_CAP);
        std::thread::Builder::new()
            .name("fileid-clip-batch".to_string())
            .spawn(move || run_coordinator(&mut model, receiver, batch_size, batch_timeout_ms))
            .expect("spawn fileid-clip-batch thread");
        tracing::info!(
            batch_size,
            batch_timeout_ms,
            "[CLIP-BATCH] coordinator spawned"
        );
        Arc::new(Self { sender })
    }

    /// Submit one image, await its embedding. Cheap-error path: returns
    /// Err if the coordinator thread has exited (process shutting down)
    /// or the underlying embed_batch failed for this batch.
    pub async fn embed(&self, rgb_256: Vec<u8>) -> Result<Vec<f32>> {
        let (tx, rx) = oneshot::channel();
        let req = ClipRequest { rgb_256, response: tx };
        // Use blocking send via a tiny spawn_blocking — crossbeam's send is
        // sync. The bounded channel can apply backpressure when CLIP is the
        // pipeline bottleneck.
        let sender = self.sender.clone();
        tokio::task::spawn_blocking(move || sender.send(req))
            .await
            .map_err(|e| anyhow!("clip coordinator join: {e}"))?
            .map_err(|_| anyhow!("clip coordinator channel closed"))?;
        rx.await
            .map_err(|_| anyhow!("clip coordinator dropped response sender"))?
    }
}

fn run_coordinator(
    model: &mut MobileClipImage,
    receiver: Receiver<ClipRequest>,
    batch_size: usize,
    batch_timeout_ms: u64,
) {
    loop {
        // Block until the first request of a new batch arrives. recv()
        // returns Err only when every sender is dropped → coordinator
        // shuts down cleanly.
        let first = match receiver.recv() {
            Ok(r) => r,
            Err(_) => {
                tracing::info!("[CLIP-BATCH] coordinator exiting (channel closed)");
                return;
            }
        };
        let mut batch: Vec<ClipRequest> = Vec::with_capacity(batch_size);
        batch.push(first);

        // Greedy: pull any already-pending requests without waiting.
        while batch.len() < batch_size {
            match receiver.try_recv() {
                Ok(r) => batch.push(r),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }

        // If still under cap, wait up to batch_timeout_ms for more to
        // arrive. Use a poll-and-sleep loop instead of recv_timeout —
        // crossbeam offers recv_timeout but we want to stay tight to
        // the deadline regardless of channel granularity.
        if batch.len() < batch_size && batch_timeout_ms > 0 {
            let deadline = Instant::now() + Duration::from_millis(batch_timeout_ms);
            while batch.len() < batch_size {
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                let remaining = deadline - now;
                match receiver.recv_timeout(remaining) {
                    Ok(r) => batch.push(r),
                    Err(_) => break,
                }
            }
        }

        STATS_BATCH_COUNT.fetch_add(1, Ordering::Relaxed);
        STATS_BATCH_SIZE_SUM.fetch_add(batch.len() as u64, Ordering::Relaxed);

        let imgs: Vec<Vec<u8>> = batch.iter().map(|r| r.rgb_256.clone()).collect();
        match model.embed_batch(&imgs) {
            Ok(embeddings) if embeddings.len() == batch.len() => {
                for (i, req) in batch.into_iter().enumerate() {
                    let _ = req.response.send(Ok(embeddings[i].clone()));
                }
            }
            Ok(_) => {
                // Pathological: model returned a wrong-shaped output.
                for req in batch {
                    let _ = req.response.send(Err(anyhow!(
                        "MobileCLIP embed_batch returned wrong-sized embedding vec"
                    )));
                }
            }
            Err(err) => {
                let err_str = format!("{:#}", err);
                tracing::warn!(?err, "[CLIP-BATCH] embed_batch failed; failing whole batch");
                for req in batch {
                    let _ = req.response.send(Err(anyhow!(err_str.clone())));
                }
            }
        }
    }
}

fn read_env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}
