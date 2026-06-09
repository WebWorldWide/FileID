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

/// Max images per batched inference. With ~3 GB VRAM headroom on a 6 GB
/// RTX 2060 (baseline measurement: 2.8/6 GB peak), 8 amortizes the
/// DirectML per-call dispatch overhead better than 4 without pushing
/// peak VRAM into the danger zone. Tunable via `FILEID_CLIP_BATCH_SIZE`
/// — drop to 4 if a smaller GPU OOMs under sustained scan load.
const DEFAULT_BATCH_SIZE: usize = 8;

/// Hard ceiling on how long the coordinator waits for batch fill once it has at
/// least one request. Tunable via `FILEID_CLIP_BATCH_TIMEOUT_MS`.
///
/// HW-4 finding (RTX 2060, 2026-06-01): widening this 20 ms → 75 ms did NOT
/// help — batch avg rose only 1.5 → ~2.0 and throughput stayed ~5 files/s while
/// per-file CLIP latency grew 190 → ~250 ms. The root cause is upstream/
/// downstream, NOT this window: only ~2 files are ever concurrently in the CLIP
/// stage even though there are 9 tagging workers, so the coordinator never has
/// more than ~2 requests queued no matter how long it waits. Per-stage
/// instrumentation (`ramplus_us`/`vision_wait_us` in [STATS]) then PINNED the
/// real bottleneck — NOT DBWriter, NOT this window: RAM++ Swin-L @384 costs
/// ~670 ms/file and runs on only pool_size=2 sessions (VRAM-clamped on the 6 GB
/// card), so workers spend ~680 ms just WAITING for the vision-sem/RAM++ pool;
/// CLIP (~190 ms) sits downstream and is starved. The real lever is RAM++
/// throughput (a CUDA-specific larger pool, or a batched RAM++ ONNX re-export).
/// Kept at 20 ms (a longer wait only adds latency). See NEXT.md.
const DEFAULT_BATCH_TIMEOUT_MS: u64 = 20;

/// Channel depth — backs up tagging workers if CLIP is the bottleneck
/// instead of letting decode race ahead and balloon memory. Benign on any tier:
/// each `ClipRequest` carries only the pre-resized 224×224 buffer (~150 KB), so
/// a full 256-deep queue is ~38 MB even though the value is 6 GB-tuned. (Unlike
/// the RAM++ batch channel, which carries FULL frames — see ram_plus_batch.rs.)
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
        // Don't panic mid-scan if the OS refuses a new thread (handle/memory
        // pressure on a very large library). On failure the closure — and with
        // it `receiver` — is dropped, disconnecting the channel; `embed()` then
        // returns its graceful "channel closed" Err per request and the CLIP
        // stage degrades (no per-file embeddings) instead of aborting the engine.
        match std::thread::Builder::new()
            .name("fileid-clip-batch".to_string())
            .spawn(move || run_coordinator(&mut model, receiver, batch_size, batch_timeout_ms))
        {
            Ok(_) => tracing::info!(
                batch_size,
                batch_timeout_ms,
                "[CLIP-BATCH] coordinator spawned"
            ),
            Err(e) => tracing::error!(
                "[CLIP-BATCH] coordinator thread failed to spawn ({e}); CLIP embeddings disabled for this scan"
            ),
        }
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

        // Move the 224×224 buffers out instead of cloning them — the batch is
        // fully consumed below either way. Senders ride alongside in order.
        let mut imgs: Vec<Vec<u8>> = Vec::with_capacity(batch.len());
        let mut senders: Vec<oneshot::Sender<Result<Vec<f32>>>> = Vec::with_capacity(batch.len());
        for req in batch {
            imgs.push(req.rgb_256);
            senders.push(req.response);
        }
        match model.embed_batch(&imgs) {
            Ok(mut embeddings) if embeddings.len() == senders.len() => {
                for (sender, emb) in senders.into_iter().zip(embeddings.drain(..)) {
                    let _ = sender.send(Ok(emb));
                }
            }
            Ok(_) => {
                // Pathological: model returned a wrong-shaped output.
                for sender in senders {
                    let _ = sender.send(Err(anyhow!(
                        "MobileCLIP embed_batch returned wrong-sized embedding vec"
                    )));
                }
            }
            Err(err) => {
                let err_str = format!("{:#}", err);
                tracing::warn!(?err, "[CLIP-BATCH] embed_batch failed; failing whole batch");
                for sender in senders {
                    let _ = sender.send(Err(anyhow!(err_str.clone())));
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
