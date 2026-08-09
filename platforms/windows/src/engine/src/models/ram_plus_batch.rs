// Batched RAM++ inference coordinator — OPT-IN, OFF by default.
//
// One Session, batched inputs. A dedicated OS thread owns the `RamPlusTagger`;
// tagging workers submit (rgb, w, h, oneshot) requests through a crossbeam
// channel. The coordinator drains up to BATCH_SIZE requests (or
// BATCH_TIMEOUT_MS, whichever first), runs `tag_batch` ONCE, and fans the
// per-image tag lists back through the oneshots — one GPU dispatch instead of N.
//
// MEASURED CAVEAT (2026-06-01, RTX 2060): batching does NOT help and in fact
// REGRESSES on this card. A single-path scan profiled GPU util at p50=87% /
// p90=97% with VRAM 90% full (5348/5955 MB) — Swin-L @384 already saturates the
// SMs at batch=1, and the single-image *pool* overlaps inference for free. With
// no idle compute to fill and no spare VRAM to grow into, an A/B (same ONNX,
// same corpus) measured batched=4 at 1.6 files/s vs single-pool 2.1 files/s
// (~23% slower); the pool also beats it on the production fp16 model (6.2 f/s).
// So RAM++ is compute/VRAM-bound here, NOT latency-bound — see DECISIONS.md.
//
// This path is retained as an opt-in tuning knob for GPUs that do NOT saturate
// at batch=1 (high-SM-count / high-VRAM cards) per the all-vendor HW-accel
// roadmap — RE-VALIDATE per card before enabling. It REQUIRES a RAM++ ONNX
// exported with a dynamic batch axis (`export_ram_plus_onnx.py --dynamic-batch`)
// and is only spawned when `FILEID_RAMPLUS_BATCH_SIZE > 1` (see
// `ModelStack::load_default`); otherwise the single-image pool path is used.
//
// Failure mode: if `tag_batch` errors, every request in that batch receives the
// error — the caller logs + skips the file's tags. The coordinator never panics
// on a single bad input.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use crossbeam_channel::{bounded, Receiver, Sender, TryRecvError};
use tokio::sync::oneshot;

use crate::models::ram_plus::RamPlusTagger;

/// Max images per batched RAM++ forward. Tunable via `FILEID_RAMPLUS_BATCH_SIZE`
/// (also the enable switch — unset/<=1 keeps the single-image pool path).
const DEFAULT_BATCH_SIZE: usize = 8;

/// Hard ceiling on how long the coordinator waits for batch fill once it has at
/// least one request. RAM++ inference is ~670 ms at batch=1, so a generous fill
/// window is cheap relative to the per-batch GPU time and lets stragglers join.
/// Tunable via `FILEID_RAMPLUS_BATCH_TIMEOUT_MS`.
const DEFAULT_BATCH_TIMEOUT_MS: u64 = 200;

/// Channel depth — applies backpressure to tagging workers if RAM++ is the
/// bottleneck instead of letting decode race ahead and balloon memory. 6 GB-
/// tuned. UNLIKE the CLIP batch channel, each `RamPlusRequest` carries a FULL
/// decoded frame (no pre-resize), so a deep queue can pin many full frames; this
/// path is opt-in (`FILEID_RAMPLUS_BATCH_SIZE > 1`) and clamped under Low tier in
/// `spawn`.
const REQUEST_CHANNEL_CAP: usize = 256;

/// Shallower channel under memory pressure: full frames here are large, so on a
/// low-RAM box bound the in-flight read-ahead far tighter than the 6 GB default.
const REQUEST_CHANNEL_CAP_LOW: usize = 16;

/// `[STATS]`-style counters so the average RAM++ batch size is observable.
pub static STATS_BATCH_COUNT: AtomicU64 = AtomicU64::new(0);
pub static STATS_BATCH_SIZE_SUM: AtomicU64 = AtomicU64::new(0);

type TagResult = Result<Vec<(String, f32)>>;

struct RamPlusRequest {
    rgb: Vec<u8>,
    width: u32,
    height: u32,
    response: oneshot::Sender<TagResult>,
}

/// Handle tagging workers use to submit RAM++ tag requests. Cheap to clone.
#[derive(Clone)]
pub struct RamPlusBatchCoordinator {
    sender: Sender<RamPlusRequest>,
}

impl RamPlusBatchCoordinator {
    /// Read the batch size from `FILEID_RAMPLUS_BATCH_SIZE`. `> 1` enables the
    /// coordinator; unset / `0` / `1` means single-image (caller uses the pool).
    pub fn configured_batch_size() -> usize {
        read_env_usize("FILEID_RAMPLUS_BATCH_SIZE", 0)
    }

    /// Spawn the coordinator on a dedicated OS thread that owns `tagger` for the
    /// rest of the process. The thread exits when all senders drop.
    pub fn spawn(mut tagger: RamPlusTagger) -> Option<Arc<Self>> {
        // The enable gate (ModelStack) already checked configured_batch_size() > 1;
        // re-read here defaulting to DEFAULT_BATCH_SIZE so a future auto-detect
        // caller that spawns without the env still gets a sane batch size.
        let batch_size = read_env_usize("FILEID_RAMPLUS_BATCH_SIZE", DEFAULT_BATCH_SIZE).max(2);
        let batch_timeout_ms = read_env_usize(
            "FILEID_RAMPLUS_BATCH_TIMEOUT_MS",
            DEFAULT_BATCH_TIMEOUT_MS as usize,
        ) as u64;
        // Bound the full-frame read-ahead far tighter on a low-RAM box, but never
        // below the batch size (a channel shallower than one batch could stall the
        // coordinator's greedy fill on a single producer).
        let channel_cap = match crate::platform::memory_tier() {
            crate::platform::MemoryTier::Low => REQUEST_CHANNEL_CAP_LOW.max(batch_size),
            _ => REQUEST_CHANNEL_CAP,
        };
        let (sender, receiver) = bounded::<RamPlusRequest>(channel_cap);
        // Degrade gracefully on spawn failure (mirrors the sibling model-load
        // sites) — batch tagging becomes unavailable and the caller falls back
        // to the single-image pool, rather than panicking the engine. (F-C1-016)
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

    /// Submit one image, await its tag list. Errs if the coordinator thread has
    /// exited (shutdown) or `tag_batch` failed for this batch.
    pub async fn tag(&self, rgb: Vec<u8>, width: u32, height: u32) -> TagResult {
        let (tx, rx) = oneshot::channel();
        let req = RamPlusRequest { rgb, width, height, response: tx };
        let sender = self.sender.clone();
        tokio::task::spawn_blocking(move || sender.send(req))
            .await
            .map_err(|e| anyhow!("ram++ coordinator join: {e}"))?
            .map_err(|_| anyhow!("ram++ coordinator channel closed"))?;
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

        // Greedy: pull any already-pending requests without waiting.
        while batch.len() < batch_size {
            match receiver.try_recv() {
                Ok(r) => batch.push(r),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
        // Under cap → wait up to the timeout for more to arrive.
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

        // Borrow each frame — tag_batch only reads the slices, and `batch` is
        // consumed for responses below, so no second full-frame clone is needed.
        let imgs: Vec<(&[u8], u32, u32)> = batch
            .iter()
            .map(|r| (r.rgb.as_slice(), r.width, r.height))
            .collect();
        match tagger.tag_batch(&imgs) {
            Ok(results) if results.len() == batch.len() => {
                for (req, tags) in batch.into_iter().zip(results.into_iter()) {
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
