// Batched RAM++ coordinator, opt-in (FILEID_RAMPLUS_BATCH_SIZE > 1; needs an ONNX exported with `--dynamic-batch`). One
// thread owns the RamPlusTagger; workers submit frames over a channel and up to BATCH_SIZE requests (or
// BATCH_TIMEOUT_MS) run as one tag_batch. Measured slower than the single-image pool on an RTX 2060 where batch=1
// already saturates the SMs, so re-validate per device. A tag_batch error fails every request in that batch; the
// coordinator never panics on one bad input.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use crossbeam_channel::{bounded, Receiver, Sender, TryRecvError};
use tokio::sync::oneshot;

use crate::models::ram_plus::RamPlusTagger;

/// Also the enable switch: unset or <=1 keeps the single-image pool path.
const DEFAULT_BATCH_SIZE: usize = 8;

/// Max wait for batch fill once one request is queued; RAM++ takes ~670 ms at batch=1, so a generous window is cheap.
const DEFAULT_BATCH_TIMEOUT_MS: u64 = 200;

/// Backpressure on tagging workers. Unlike the CLIP batch channel each request carries a FULL decoded frame, so a deep
/// queue pins many frames; clamped under the Low tier in `spawn`.
const REQUEST_CHANNEL_CAP: usize = 256;

const REQUEST_CHANNEL_CAP_LOW: usize = 16;

pub static STATS_BATCH_COUNT: AtomicU64 = AtomicU64::new(0);
pub static STATS_BATCH_SIZE_SUM: AtomicU64 = AtomicU64::new(0);

type TagResult = Result<Vec<(String, f32)>>;

struct RamPlusRequest {
    rgb: Vec<u8>,
    width: u32,
    height: u32,
    response: oneshot::Sender<TagResult>,
}

#[derive(Clone)]
pub struct RamPlusBatchCoordinator {
    sender: Sender<RamPlusRequest>,
}

impl RamPlusBatchCoordinator {
    pub fn configured_batch_size() -> usize {
        read_env_usize("FILEID_RAMPLUS_BATCH_SIZE", 0)
    }

    pub fn spawn(mut tagger: RamPlusTagger) -> Option<Arc<Self>> {
        let batch_size = read_env_usize("FILEID_RAMPLUS_BATCH_SIZE", DEFAULT_BATCH_SIZE).max(2);
        let batch_timeout_ms = read_env_usize(
            "FILEID_RAMPLUS_BATCH_TIMEOUT_MS",
            DEFAULT_BATCH_TIMEOUT_MS as usize,
        ) as u64;
        // Never below the batch size: a shallower channel could stall the greedy fill on a single producer.
        let channel_cap = match crate::platform::memory_tier() {
            crate::platform::MemoryTier::Low => REQUEST_CHANNEL_CAP_LOW.max(batch_size),
            _ => REQUEST_CHANNEL_CAP,
        };
        let (sender, receiver) = bounded::<RamPlusRequest>(channel_cap);
        // On spawn failure batch tagging is unavailable and the caller falls back to the single-image pool.
        if let Err(err) = std::thread::Builder::new()
            .name("fileid-ramplus-batch".to_string())
            .spawn(move || run_coordinator(&mut tagger, receiver, batch_size, batch_timeout_ms))
        {
            tracing::warn!(%err, "[RAMPLUS-BATCH] coordinator spawn failed; falling back to single-image tagging");
            return None;
        }
        tracing::info!(batch_size, batch_timeout_ms, "[RAMPLUS-BATCH] coordinator spawned");
        Some(Arc::new(Self { sender }))
    }

    pub async fn tag(&self, rgb: Vec<u8>, width: u32, height: u32) -> TagResult {
        if crate::coordinator::process_gpu_device_removed() {
            return Err(anyhow!(crate::models::runtime::GPU_DEVICE_REMOVED_MARKER));
        }
        let (tx, rx) = oneshot::channel();
        let req = RamPlusRequest { rgb, width, height, response: tx };
        match self.sender.try_send(req) {
            Ok(()) => {}
            Err(crossbeam_channel::TrySendError::Disconnected(_)) => return Err(anyhow!("ram++ coordinator channel closed")),
            Err(crossbeam_channel::TrySendError::Full(req)) => {
                let sender = self.sender.clone();
                tokio::task::spawn_blocking(move || sender.send(req))
                    .await
                    .map_err(|e| anyhow!("ram++ coordinator join: {e}"))?
                    .map_err(|_| anyhow!("ram++ coordinator channel closed"))?;
            }
        }
        rx.await
            .map_err(|_| anyhow!("ram++ coordinator dropped response sender"))?
    }
}

fn run_coordinator(
    tagger: &mut RamPlusTagger,
    receiver: Receiver<RamPlusRequest>,
    batch_size: usize,
    batch_timeout_ms: u64,
) {
    loop {
        let first = match receiver.recv() {
            Ok(r) => r,
            Err(_) => {
                tracing::info!("[RAMPLUS-BATCH] coordinator exiting (channel closed)");
                return;
            }
        };
        let mut batch: Vec<RamPlusRequest> = Vec::with_capacity(batch_size);
        batch.push(first);

        while batch.len() < batch_size {
            match receiver.try_recv() {
                Ok(r) => batch.push(r),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
        if batch.len() < batch_size && batch_timeout_ms > 0 {
            let deadline = Instant::now() + Duration::from_millis(batch_timeout_ms);
            while batch.len() < batch_size {
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                match receiver.recv_timeout(deadline - now) {
                    Ok(r) => batch.push(r),
                    Err(_) => break,
                }
            }
        }

        STATS_BATCH_COUNT.fetch_add(1, Ordering::Relaxed);
        STATS_BATCH_SIZE_SUM.fetch_add(batch.len() as u64, Ordering::Relaxed);

        // `tag_batch` only reads the slices; `batch` is consumed for responses below, so borrow rather than clone
        // frames.
        let imgs: Vec<(&[u8], u32, u32)> = batch
            .iter()
            .map(|r| (r.rgb.as_slice(), r.width, r.height))
            .collect();
        if crate::coordinator::process_gpu_device_removed() {
            for req in batch {
                let _ = req.response.send(Err(anyhow!(
                    crate::models::runtime::GPU_DEVICE_REMOVED_MARKER
                )));
            }
            continue;
        }
        match tagger.tag_batch(&imgs) {
            Ok(results) if results.len() == batch.len() => {
                for (req, tags) in batch.into_iter().zip(results) {
                    let _ = req.response.send(Ok(tags));
                }
            }
            Ok(_) => {
                for req in batch {
                    let _ = req
                        .response
                        .send(Err(anyhow!("RAM++ tag_batch returned wrong result count")));
                }
            }
            Err(err) => {
                let err_str = format!("{err:#}");
                tracing::warn!(?err, "[RAMPLUS-BATCH] tag_batch failed; failing whole batch");
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
