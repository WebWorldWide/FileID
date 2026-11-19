// Failure mode: if `embed_batch` errors, every request in that batch receives the error and the caller decides whether to skip or retry; the coordinator never panics on a single bad input.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use crossbeam_channel::{bounded, Receiver, Sender, TryRecvError, TrySendError};
use tokio::sync::oneshot;

use crate::models::mobileclip::MobileClipImage;

/// Max images per batched inference: 8 gives tract's multithreaded matmul enough rows to split across the performance cores. `FILEID_CLIP_BATCH_SIZE` overrides.
const DEFAULT_BATCH_SIZE: usize = 8;

/// Hard ceiling on waiting for a batch to fill once it has one request (`FILEID_CLIP_BATCH_TIMEOUT_MS`). Widening it did not raise batch size: CLIP is starved by the RAM++ stage upstream.
const DEFAULT_BATCH_TIMEOUT_MS: u64 = 20;

const REQUEST_CHANNEL_CAP: usize = 256;

pub static STATS_BATCH_COUNT: AtomicU64 = AtomicU64::new(0);
pub static STATS_BATCH_SIZE_SUM: AtomicU64 = AtomicU64::new(0);

struct ClipRequest {
    rgb: Vec<u8>,
    response: oneshot::Sender<Result<Vec<f32>>>,
}

#[derive(Clone)]
pub struct ClipBatchCoordinator {
    sender: Sender<ClipRequest>,
}

impl ClipBatchCoordinator {
    /// Runs the coordinator on a dedicated OS thread that owns `model`; it exits when all senders are dropped.
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

    pub async fn embed(&self, rgb: Vec<u8>) -> Result<Vec<f32>> {
        if crate::coordinator::process_gpu_device_removed() {
            return Err(anyhow!(crate::models::runtime::GPU_DEVICE_REMOVED_MARKER));
        }
        let (tx, rx) = oneshot::channel();
        let req = ClipRequest { rgb, response: tx };
        // The queue almost always has room, so submit in place. Only when it is
        // full (CLIP is the bottleneck and applies backpressure) does the
        // blocking send move to a blocking thread.
        match self.sender.try_send(req) {
            Ok(()) => {}
            Err(TrySendError::Disconnected(_)) => return Err(anyhow!("clip coordinator channel closed")),
            Err(TrySendError::Full(req)) => {
                let sender = self.sender.clone();
                tokio::task::spawn_blocking(move || sender.send(req))
                    .await
                    .map_err(|e| anyhow!("clip coordinator join: {e}"))?
                    .map_err(|_| anyhow!("clip coordinator channel closed"))?;
            }
        }
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
        let first = match receiver.recv() {
            Ok(r) => r,
            Err(_) => {
                tracing::info!("[CLIP-BATCH] coordinator exiting (channel closed)");
                return;
            }
        };
        let mut batch: Vec<ClipRequest> = Vec::with_capacity(batch_size);
        batch.push(first);

        while batch.len() < batch_size {
            match receiver.try_recv() {
                Ok(r) => batch.push(r),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }

    // Poll-and-sleep rather than recv_timeout: keeps the deadline tight regardless of channel granularity.
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

        let mut imgs: Vec<Vec<u8>> = Vec::with_capacity(batch.len());
        let mut senders: Vec<oneshot::Sender<Result<Vec<f32>>>> = Vec::with_capacity(batch.len());
        for req in batch {
            imgs.push(req.rgb);
            senders.push(req.response);
        }
        if crate::coordinator::process_gpu_device_removed() {
            for sender in senders {
                let _ = sender.send(Err(anyhow!(
                    crate::models::runtime::GPU_DEVICE_REMOVED_MARKER
                )));
            }
            continue;
        }
        match model.embed_batch(&imgs) {
            Ok(mut embeddings) if embeddings.len() == senders.len() => {
                for (sender, emb) in senders.into_iter().zip(embeddings.drain(..)) {
                    let _ = sender.send(Ok(emb));
                }
            }
            Ok(_) => {
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
