// Tagging — N parallel workers consume DiscoveredFile from Discovery,
// run the per-file ML pipeline, and emit TaggedFile rows that DBWriter
// batches into SQLite.
//
// Mirror of macOS engine/Sources/FileIDEngine/Pipeline/Tagging.swift.
// Worker count: physical_cores * 1.7 (matches macOS's 14-on-M1Pro
// heuristic). ANE-style semaphores cap concurrent ORT inferences to
// prevent VRAM thrash on the GPU EP — 4 for vision models, 2 for CLIP.
//
// V14.3: per-file body fans out per-kind into the real handlers —
// EXIF + dHash + SCRFD/ArcFace + MobileCLIP for images. Video/PDF/OCR
// land as their shell helpers fill in. Models are loaded into a
// ModelStack at scan start; missing models gracefully degrade
// (the pipeline emits TaggedFile with the missing fields = None).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;
use tokio::sync::{mpsc, Semaphore};

use crate::coordinator::ScanCoordinator;
use crate::models::runtime::error_has_device_removed_marker;
use crate::models::{arcface::ArcFace, mobileclip::MobileClipImage, scrfd::{self, Scrfd}};
use crate::pipeline::batch_clip::ClipBatchCoordinator;
use crate::pipeline::discovery::{DiscoveredFile, FileKind};
use crate::shell;

/// V14.9-W stats rollup. Each worker adds its per-file µs into these
/// atomics; every STATS_PERIOD files we emit one info-level [STATS] line
/// to `engine.jsonl` summarising avg stage timings. Lets us see whether
/// each new optimization iteration actually shrunk what it claimed.
static STATS_FILES: AtomicU64 = AtomicU64::new(0);
static STATS_DECODE_US: AtomicU64 = AtomicU64::new(0);
static STATS_EXIF_US: AtomicU64 = AtomicU64::new(0);
static STATS_DHASH_US: AtomicU64 = AtomicU64::new(0);
static STATS_VISION_US: AtomicU64 = AtomicU64::new(0);
static STATS_CLIP_US: AtomicU64 = AtomicU64::new(0);
static STATS_OCR_US: AtomicU64 = AtomicU64::new(0);
static STATS_OCR_RAN: AtomicU64 = AtomicU64::new(0);
static STATS_TOTAL_US: AtomicU64 = AtomicU64::new(0);
const STATS_PERIOD: u64 = 100;

fn record_stage(stage: &AtomicU64, started: Instant) {
    stage.fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
}

fn maybe_emit_stats() {
    // fetch_add returns the PRIOR value; +1 lets the modulo land cleanly
    // on every STATS_PERIOD-th file.
    let n = STATS_FILES.fetch_add(1, Ordering::Relaxed) + 1;
    if n % STATS_PERIOD != 0 {
        return;
    }
    let decode = STATS_DECODE_US.load(Ordering::Relaxed) / n;
    let exif = STATS_EXIF_US.load(Ordering::Relaxed) / n;
    let dhash = STATS_DHASH_US.load(Ordering::Relaxed) / n;
    let vision = STATS_VISION_US.load(Ordering::Relaxed) / n;
    let clip = STATS_CLIP_US.load(Ordering::Relaxed) / n;
    let total = STATS_TOTAL_US.load(Ordering::Relaxed) / n;
    let ocr_ran = STATS_OCR_RAN.load(Ordering::Relaxed);
    let ocr_avg = if ocr_ran > 0 {
        STATS_OCR_US.load(Ordering::Relaxed) / ocr_ran
    } else {
        0
    };
    let batch_count = crate::pipeline::batch_clip::STATS_BATCH_COUNT.load(Ordering::Relaxed);
    let batch_sum = crate::pipeline::batch_clip::STATS_BATCH_SIZE_SUM.load(Ordering::Relaxed);
    let avg_batch = if batch_count > 0 {
        (batch_sum * 10) / batch_count // ×10 so we can see 4.2 as "42"
    } else {
        0
    };
    tracing::info!(
        target: "FileIDEngine::stats",
        processed = n,
        decode_us = decode,
        exif_us = exif,
        dhash_us = dhash,
        vision_us = vision,
        clip_us = clip,
        ocr_us = ocr_avg,
        ocr_ran = ocr_ran,
        total_us = total,
        clip_batches = batch_count,
        clip_avg_batch_x10 = avg_batch,
        "[STATS] per-file avg microseconds"
    );
}

/// Bounded channel capacity, Tagging → DBWriter. Matches macOS's 256 —
/// one transaction worth + slack so workers don't stall on a slow flush.
pub const TAGGING_CHANNEL_CAP: usize = 256;

/// Cap concurrent ORT vision-model inferences.
/// V14.9-Y: reverted 8→4. V14.9-V's bump put more pressure on the
/// DirectML command queue, which under load contributed to the 2 s
/// TDR deadline being missed and the GPU device getting removed. The
/// throughput cost of 4 vs 8 is ~10 % at the upper end; the safety
/// cost of TDR is a full system hang.
const VISION_CONCURRENCY: usize = 4;

/// Cap concurrent CLIP image embeds.
/// V14.9-Y: reverted 4→2 for the same TDR-pressure reason.
const CLIP_CONCURRENCY: usize = 2;

/// Padding fraction added to the SCRFD bbox before cropping. ArcFace
/// trains on tightly-cropped faces with ~25% slack; over-tight crops
/// degrade embedding quality.
const FACE_CROP_PAD: f32 = 0.25;

/// One per file post-tagging. The DBWriter batches these into a single
/// transaction. Embeddings are L2-normalized float32 vectors stored as
/// raw little-endian bytes (matches macOS GRDB layout).
#[derive(Debug, Clone)]
pub struct TaggedFile {
    pub path: PathBuf,
    pub kind: FileKind,
    pub size_bytes: u64,
    pub modified_unix: f64,
    pub scanned_unix: f64,

    pub has_faces: bool,
    pub faces: Vec<DetectedFace>,

    pub has_text: bool,
    pub ocr_text: Option<String>,

    pub phash: Option<i64>,
    pub clip_embedding: Option<Vec<f32>>,

    pub camera_model: Option<String>,
    pub location_lat: Option<f64>,
    pub location_lon: Option<f64>,

    pub vision_ms: f64,
    pub clip_ms: f64,
    pub total_ms: f64,

    pub failed: bool,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct DetectedFace {
    pub bbox: [f32; 4],          // x, y, w, h in image pixels
    pub landmarks: [[f32; 2]; 5],
    pub embedding: Vec<f32>,
    pub roll: f32,
    pub yaw: f32,
    pub pitch: f32,
    pub quality: f32,
    /// Tightly-packed RGB8 of the 112×112 ArcFace crop. Written by the
    /// dbwriter to face_crops/<face_id>.jpg so the People tab cards can
    /// render real faces instead of gray circles. Cleared after persist
    /// to avoid holding ~37 KB per face in memory.
    pub crop_rgb_112: Option<Vec<u8>>,
}

/// Loaded ML weights shared across tagging workers. Each model is
/// optional — missing weights at scan start cause the corresponding
/// stage to no-op so a partial install doesn't fail the whole scan.
///
/// V14.9-W: each model is now a POOL of independent Sessions instead of
/// a single `Mutex<T>`. Workers index into the pool by `worker_idx %
/// pool.len()` so multiple inferences can run in parallel against the
/// GPU's command queue. Pool size capped at `MODEL_POOL_SIZE` to keep
/// VRAM usage bounded; on machines with fewer workers the pool shrinks
/// to match.
pub struct ModelStack {
    pub arcface: Option<Vec<Mutex<ArcFace>>>,
    pub scrfd: Option<Vec<Mutex<Scrfd>>>,
    /// V14.9-X: MobileCLIP carries TWO alternate paths:
    /// - `mobileclip_pool` (default) — N-Session pool, same shape as
    ///   ArcFace/SCRFD. VRAM-clamped via `resolve_pool_size`. Empirically
    ///   the throughput winner for MobileCLIP-S2 on DirectML.
    /// - `mobileclip_batch` — single Session behind `ClipBatchCoordinator`.
    ///   Opt-in via `FILEID_CLIP_USE_BATCH=1`. Kept for future experiments
    ///   with CUDA EP or larger models where batching DOES amortize.
    pub mobileclip_pool: Option<Vec<Mutex<MobileClipImage>>>,
    pub mobileclip_batch: Option<Arc<ClipBatchCoordinator>>,
}

/// Aspirational pool size — the actual cap is `min(this, vram_cap, worker_count)`
/// computed in `resolve_pool_size`. The VRAM gate is the safety belt that
/// prevents V14.9-W's system-hang regression (pool=4 on a 6 GB RTX 2060
/// exhausted VRAM and wedged the DirectML driver). On a 6 GB card the gate
/// clamps to 2; on 12 GB cards it allows up to ~7. Tunable per-user via
/// `FILEID_MODEL_POOL_SIZE`, but the env value is ALSO clamped by the gate.
const MODEL_POOL_SIZE: usize = 4;

/// Estimated VRAM headroom per pooled-Session of (ArcFace + SCRFD +
/// MobileCLIP combined): weights + DirectML allocator + intermediate
/// tensors. Conservative upper bound — used to clamp pool size to fit
/// available `DedicatedVideoMemory`. Real per-session residency varies
/// by model and EP; treat this as a ceiling, not a measurement.
const VRAM_PER_POOL_INSTANCE_MB: u64 = 1500;

/// Always-reserved VRAM headroom (Windows desktop compositor + other
/// apps). Subtracted from the dedicated total before dividing by
/// VRAM_PER_POOL_INSTANCE_MB. On a 6 GB card this leaves ~4.5 GB
/// available for our pool, capping it at 3 — but combined with the
/// MODEL_POOL_SIZE default of 1, the gate just makes pool=1 the
/// near-universal result.
const VRAM_RESERVED_MB: u64 = 1500;

fn resolve_pool_size(worker_count: usize) -> usize {
    let env_override = std::env::var("FILEID_MODEL_POOL_SIZE")
        .ok()
        .and_then(|s| s.parse::<usize>().ok());
    let mut cap = env_override.unwrap_or(MODEL_POOL_SIZE);
    if let Some(vram_mb) = crate::platform::dedicated_vram_mb() {
        let usable = vram_mb.saturating_sub(VRAM_RESERVED_MB);
        let vram_cap = ((usable / VRAM_PER_POOL_INSTANCE_MB).max(1)) as usize;
        if cap > vram_cap {
            tracing::warn!(
                requested = cap,
                vram_cap,
                vram_mb,
                "clamping ML pool to fit VRAM"
            );
            cap = vram_cap;
        }
    }
    cap.max(1).min(worker_count.max(1))
}

impl ModelStack {
    /// Load whatever model files are installed at the canonical paths.
    /// Each present model gets loaded `pool_size` times so workers can
    /// run inference in parallel without serializing on a single Mutex.
    pub fn load_default(worker_count: usize) -> Self {
        let pool_size = resolve_pool_size(worker_count);
        let arcface = load_pool("ArcFace", pool_size, crate::models::arcface::default_weights_path(), |p| {
            ArcFace::load(p)
        });
        let scrfd = load_pool("SCRFD", pool_size, scrfd::default_weights_path(), Scrfd::load);

        let use_batch = std::env::var("FILEID_CLIP_USE_BATCH")
            .ok()
            .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        let (mobileclip_pool, mobileclip_batch) = if use_batch {
            // Batch-coordinator path (opt-in, experimental).
            let coord = match crate::models::mobileclip::default_weights_path() {
                Ok(p) if p.exists() => match MobileClipImage::load(p.clone()) {
                    Ok(model) => {
                        tracing::info!(model = "MobileCLIP", path = %p.display(), "model loaded (batch-coordinator mode)");
                        Some(ClipBatchCoordinator::spawn(model))
                    }
                    Err(err) => {
                        tracing::warn!(model = "MobileCLIP", ?err, "model load failed; stage will skip");
                        None
                    }
                },
                Ok(p) => {
                    tracing::info!(model = "MobileCLIP", path = %p.display(), "model not installed; stage will skip");
                    None
                }
                Err(err) => {
                    tracing::warn!(model = "MobileCLIP", ?err, "model path unresolved");
                    None
                }
            };
            (None, coord)
        } else {
            // Pool path (default — empirically faster for MobileCLIP-S2 on DirectML).
            let pool = load_pool(
                "MobileCLIP",
                pool_size,
                crate::models::mobileclip::default_weights_path(),
                |p| MobileClipImage::load(p),
            );
            (pool, None)
        };

        Self { arcface, scrfd, mobileclip_pool, mobileclip_batch }
    }

    #[allow(dead_code)]
    pub fn empty() -> Self {
        Self {
            arcface: None,
            scrfd: None,
            mobileclip_pool: None,
            mobileclip_batch: None,
        }
    }
}

fn load_pool<T, F>(label: &str, pool_size: usize, path: anyhow::Result<PathBuf>, loader: F) -> Option<Vec<Mutex<T>>>
where
    F: Fn(PathBuf) -> anyhow::Result<T>,
{
    let p = match path {
        Ok(p) => p,
        Err(err) => {
            tracing::warn!(model = label, ?err, "model path unresolved");
            return None;
        }
    };
    if !p.exists() {
        tracing::info!(model = label, path = %p.display(), "model not installed; stage will skip");
        return None;
    }
    let mut pool = Vec::with_capacity(pool_size);
    for idx in 0..pool_size {
        // V15.0 Phase A: stagger each Session allocation by 250 ms so a
        // 6-session pool (2 × 3 models) doesn't burst DirectML's command
        // queue at engine startup. Empirically the riskiest TDR window.
        // First iteration (idx=0) doesn't sleep; subsequent slots wait.
        if idx > 0 {
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        match loader(p.clone()) {
            Ok(model) => pool.push(Mutex::new(model)),
            Err(err) => {
                // V15.0 Phase A: if any slot's load (which now includes a
                // warmup inference) hit device-removed, the marker is on
                // the error. Bail the whole stack rather than try further
                // slots that will hit the same dead GPU.
                use crate::models::runtime::error_has_device_removed_marker;
                if error_has_device_removed_marker(&err) {
                    tracing::error!(model = label, slot = idx, ?err, "[STARTUP-TDR] device-removed during warmup; aborting pool load");
                    return None;
                }
                tracing::warn!(model = label, slot = idx, ?err, "model pool load failed; stage will skip if pool empty");
                break;
            }
        }
    }
    if pool.is_empty() {
        None
    } else {
        tracing::info!(model = label, path = %p.display(), pool_size = pool.len(), "model pool loaded");
        Some(pool)
    }
}

pub struct Tagger {
    coordinator: ScanCoordinator,
    worker_count: usize,
    models: Arc<ModelStack>,
}

impl Tagger {
    pub fn new(coordinator: ScanCoordinator, worker_count: usize, models: Arc<ModelStack>) -> Self {
        Self {
            coordinator,
            worker_count: worker_count.max(1),
            models,
        }
    }

    /// Wire Discovery → N tagging workers → DBWriter. Spawns `worker_count`
    /// tasks that share the input receiver via async-channel (mpsc has only
    /// one consumer; we want fan-out to N workers from the same queue).
    pub fn spawn(self, mut input: mpsc::Receiver<DiscoveredFile>) -> mpsc::Receiver<TaggedFile> {
        let (out_tx, out_rx) = mpsc::channel(TAGGING_CHANNEL_CAP);

        // Convert mpsc::Receiver → async-channel for N-consumer fan-out.
        let (fan_tx, fan_rx) = async_channel::bounded::<DiscoveredFile>(TAGGING_CHANNEL_CAP);
        let coordinator_pump = self.coordinator.clone();
        tokio::spawn(async move {
            while let Some(file) = input.recv().await {
                if coordinator_pump.is_cancelled() {
                    break;
                }
                if fan_tx.send(file).await.is_err() {
                    break;
                }
            }
        });

        let vision_sem = Arc::new(Semaphore::new(VISION_CONCURRENCY));
        let clip_sem = Arc::new(Semaphore::new(CLIP_CONCURRENCY));

        for worker_idx in 0..self.worker_count {
            let rx = fan_rx.clone();
            let tx = out_tx.clone();
            let coord = self.coordinator.clone();
            let vision_sem = vision_sem.clone();
            let clip_sem = clip_sem.clone();
            let models = self.models.clone();

            tokio::spawn(async move {
                // V15.0 Phase H: drop the per-worker thread priority to
                // LOWEST so foreground apps (File Explorer, browser)
                // stay snappy during scans. tokio worker threads inherit
                // the parent process priority by default; we explicitly
                // demote ourselves.
                crate::platform::set_worker_background_priority();
                // V15.0 Phase E: yield cadence. After every YIELD_AFTER
                // files this worker processes, sleep briefly to give
                // foreground apps + DWM breathing room. Costs <1 % at
                // 50 ms × 1/500 files, but lets multi-hour scans stay
                // friendly to a desktop being actively used.
                const YIELD_AFTER: u64 = 500;
                let mut files_done: u64 = 0;
                while let Ok(file) = rx.recv().await {
                    if coord.check().await.is_err() {
                        break;
                    }
                    // V15.0 Phase C: per-file timeout. Image decoders or
                    // network UNC reads can hang indefinitely; a 60-second
                    // ceiling lets the worker abandon and move on.
                    let fut = process_file(&file, &models, &vision_sem, &clip_sem, worker_idx, &coord);
                    let tagged = match tokio::time::timeout(
                        std::time::Duration::from_secs(60),
                        fut,
                    )
                    .await
                    {
                        Ok(t) => t,
                        Err(_elapsed) => {
                            tracing::warn!(
                                path = %crate::platform::redact_path_for_log(&file.path),
                                "per-file timeout after 60s; marking failed and continuing"
                            );
                            // Emit a minimally-filled TaggedFile so DBWriter sees the file.
                            TaggedFile {
                                path: file.path.clone(),
                                kind: file.kind,
                                size_bytes: file.size_bytes,
                                modified_unix: file.modified_unix,
                                scanned_unix: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs_f64(),
                                has_faces: false,
                                faces: Vec::new(),
                                has_text: false,
                                ocr_text: None,
                                phash: None,
                                clip_embedding: None,
                                camera_model: None,
                                location_lat: None,
                                location_lon: None,
                                vision_ms: 0.0,
                                clip_ms: 0.0,
                                total_ms: 60000.0,
                                failed: true,
                                error_message: Some("per-file timeout after 60s".into()),
                            }
                        }
                    };
                    if tx.send(tagged).await.is_err() {
                        break;
                    }
                    files_done += 1;
                    if files_done % YIELD_AFTER == 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    }
                }
            });
        }

        drop(out_tx);
        out_rx
    }
}

/// Per-file ML body. Loads the image, runs face detect → embed,
/// CLIP embed, dHash, EXIF — each gated on its semaphore + the model
/// being installed. Failure of any single stage is non-fatal: the row
/// gets emitted with that field = None and `failed=0` (only image
/// decode failure marks the row failed).
async fn process_file(
    file: &DiscoveredFile,
    models: &Arc<ModelStack>,
    vision_sem: &Arc<Semaphore>,
    clip_sem: &Arc<Semaphore>,
    worker_idx: usize,
    coord: &ScanCoordinator,
) -> TaggedFile {
    let started = Instant::now();
    let scanned_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();

    let mut tagged = TaggedFile {
        path: file.path.clone(),
        kind: file.kind,
        size_bytes: file.size_bytes,
        modified_unix: file.modified_unix,
        scanned_unix,
        has_faces: false,
        faces: Vec::new(),
        has_text: false,
        ocr_text: None,
        phash: None,
        clip_embedding: None,
        camera_model: None,
        location_lat: None,
        location_lon: None,
        vision_ms: 0.0,
        clip_ms: 0.0,
        total_ms: 0.0,
        failed: false,
        error_message: None,
    };

    // V15.0 Phase F: face pipeline gate. SCRFD needs the full-resolution
    // frame for accurate face detection. CLIP / dhash / OCR work fine on
    // a 512×512 shell thumbnail. Force-disable via FILEID_FORCE_THUMBNAIL=1
    // to trade face accuracy for ~30 % CPU savings.
    let force_thumb = std::env::var("FILEID_FORCE_THUMBNAIL")
        .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let face_pipeline_active =
        models.scrfd.is_some() && models.arcface.is_some() && !force_thumb;

    let decode_started = Instant::now();
    let image_source: Option<(Vec<u8>, u32, u32)> = match file.kind {
        FileKind::Image => {
            if face_pipeline_active {
                match load_image_rgb(&file.path).await {
                    Ok(t) => Some(t),
                    Err(err) => {
                        tracing::warn!(?err, path = %crate::platform::redact_path_for_log(&file.path), "image decode failed");
                        tagged.failed = true;
                        tagged.error_message = Some(format!("image decode: {err:#}"));
                        None
                    }
                }
            } else {
                // Thumbnail fast path: ask the Windows shell for the
                // pre-cached 512×512 RGBA8. Falls back to full decode
                // on Err (e.g. file Explorer never indexed it).
                match try_shell_thumbnail(file.path.clone()).await {
                    Ok(t) => Some(t),
                    Err(_) => match load_image_rgb(&file.path).await {
                        Ok(t) => Some(t),
                        Err(err) => {
                            tracing::warn!(?err, path = %crate::platform::redact_path_for_log(&file.path), "image decode failed (thumbnail fallback)");
                            tagged.failed = true;
                            tagged.error_message = Some(format!("image decode: {err:#}"));
                            None
                        }
                    }
                }
            }
        },
        FileKind::Video => extract_video_keyframe_blocking(file.path.clone()).await.ok(),
        _ => None,
    };
    record_stage(&STATS_DECODE_US, decode_started);

    if let Some((rgb, w, h)) = image_source {
            if matches!(file.kind, FileKind::Image) {
                let exif_started = Instant::now();
                if let Some((cam, lat, lon)) = parse_exif_blocking(file.path.clone()).await {
                    tagged.camera_model = cam;
                    tagged.location_lat = lat;
                    tagged.location_lon = lon;
                }
                record_stage(&STATS_EXIF_US, exif_started);
            }

                let dhash_started = Instant::now();
                tagged.phash = Some(compute_dhash(&rgb, w as usize, h as usize));
                record_stage(&STATS_DHASH_US, dhash_started);

                // V14.9-Y: short-circuit GPU stages if a prior file
                // already detected device-removed. Don't submit any new
                // work against the dead GPU device — that's what wedges
                // the system when TDR fires.
                let gpu_alive = !coord.is_gpu_dead();
                if gpu_alive {
                if let (Some(scrfd_pool), Some(arcface_pool)) = (&models.scrfd, &models.arcface) {
                    let permit = vision_sem.acquire().await;
                    let vision_started = Instant::now();
                    if permit.is_ok() {
                        let scrfd_mu = &scrfd_pool[worker_idx % scrfd_pool.len()];
                        let arcface_mu = &arcface_pool[worker_idx % arcface_pool.len()];
                        let detections = {
                            let mut s = scrfd_mu.lock();
                            s.detect(&rgb, w, h)
                        };
                        match detections {
                            Ok(dets) => {
                                for det in dets {
                                    if coord.is_gpu_dead() { break; }
                                    if let Some(crop) = crop_and_resize_face(&rgb, w, h, &det.bbox) {
                                        let embed_result = {
                                            let mut a = arcface_mu.lock();
                                            a.embed(&crop)
                                        };
                                        match embed_result {
                                            Ok(emb) => {
                                                let pose = scrfd::estimate_pose(&det.landmarks);
                                                tagged.faces.push(DetectedFace {
                                                    bbox: det.bbox,
                                                    landmarks: det.landmarks,
                                                    embedding: emb,
                                                    roll: pose.roll,
                                                    yaw: pose.yaw,
                                                    pitch: pose.pitch,
                                                    quality: det.score,
                                                    crop_rgb_112: Some(crop),
                                                });
                                            }
                                            Err(err) => {
                                                if error_has_device_removed_marker(&err) {
                                                    if coord.mark_gpu_dead() {
                                                        tracing::error!(?err, "[GPU-TDR] ArcFace device-removed; cancelling scan");
                                                    }
                                                    break;
                                                }
                                                tracing::warn!(?err, "ArcFace embed failed");
                                            }
                                        }
                                    }
                                }
                            }
                            Err(err) => {
                                if error_has_device_removed_marker(&err) {
                                    if coord.mark_gpu_dead() {
                                        tracing::error!(?err, "[GPU-TDR] SCRFD device-removed; cancelling scan");
                                    }
                                } else {
                                    tracing::warn!(?err, "SCRFD detect failed");
                                }
                            }
                        }
                    }
                    tagged.vision_ms = vision_started.elapsed().as_secs_f64() * 1000.0;
                    STATS_VISION_US.fetch_add(vision_started.elapsed().as_micros() as u64, Ordering::Relaxed);
                }
                }
                tagged.has_faces = !tagged.faces.is_empty();

                if !coord.is_gpu_dead() {
                // Pool path: locked Session, single-image inference.
                if let Some(clip_pool) = &models.mobileclip_pool {
                    let permit = clip_sem.acquire().await;
                    let clip_started = Instant::now();
                    if permit.is_ok() {
                        let clip_mu = &clip_pool[worker_idx % clip_pool.len()];
                        let resized = resize_rgb_nearest(&rgb, w as usize, h as usize, 256, 256);
                        let embed_result = {
                            let mut c = clip_mu.lock();
                            c.embed(&resized)
                        };
                        match embed_result {
                            Ok(emb) => tagged.clip_embedding = Some(emb),
                            Err(err) => {
                                if error_has_device_removed_marker(&err) {
                                    if coord.mark_gpu_dead() {
                                        tracing::error!(?err, "[GPU-TDR] MobileCLIP device-removed; cancelling scan");
                                    }
                                } else {
                                    tracing::warn!(?err, "MobileCLIP embed failed");
                                }
                            }
                        }
                    }
                    tagged.clip_ms = clip_started.elapsed().as_secs_f64() * 1000.0;
                    STATS_CLIP_US.fetch_add(clip_started.elapsed().as_micros() as u64, Ordering::Relaxed);
                } else if let Some(clip_coord) = &models.mobileclip_batch {
                    // Opt-in batch path: workers submit to coordinator,
                    // get batched embedding back via oneshot.
                    let clip_started = Instant::now();
                    let resized = resize_rgb_nearest(&rgb, w as usize, h as usize, 256, 256);
                    match clip_coord.embed(resized).await {
                        Ok(emb) => tagged.clip_embedding = Some(emb),
                        Err(err) => {
                            if error_has_device_removed_marker(&err) {
                                if coord.mark_gpu_dead() {
                                    tracing::error!(?err, "[GPU-TDR] MobileCLIP (batch) device-removed; cancelling scan");
                                }
                            } else {
                                tracing::warn!(?err, "MobileCLIP embed (batched) failed");
                            }
                        }
                    }
                    tagged.clip_ms = clip_started.elapsed().as_secs_f64() * 1000.0;
                    STATS_CLIP_US.fetch_add(clip_started.elapsed().as_micros() as u64, Ordering::Relaxed);
                }
                }

                // V14.9-W gate: OCR is the biggest CPU per file (~30-50 ms).
                // For camera photos (EXIF camera_model present) it returns
                // nothing useful, so skip. Document-like files (screenshots,
                // scans, PNG over a quality threshold) still run OCR.
                // V15.0 Phase D: belt-and-suspenders GPU-dead short-circuit
                // for OCR. Windows.Media.Ocr usually fails-soft on driver
                // issues, but skipping the call if we already know the GPU
                // is dead saves CPU + avoids any chance of a recursive
                // device-init that could complicate driver recovery.
                if matches!(file.kind, FileKind::Image) && !coord.is_gpu_dead() && should_run_ocr(&file.path, &tagged, file.size_bytes) {
                    let ocr_started = Instant::now();
                    if let Ok(Some(ocr)) = run_ocr_blocking(rgb.clone(), w, h).await {
                        if !ocr.text.trim().is_empty() {
                            tagged.has_text = true;
                            tagged.ocr_text = Some(ocr.text);
                        }
                    }
                    STATS_OCR_RAN.fetch_add(1, Ordering::Relaxed);
                    record_stage(&STATS_OCR_US, ocr_started);
                }
    }

    tagged.total_ms = started.elapsed().as_secs_f64() * 1000.0;
    record_stage(&STATS_TOTAL_US, started);
    maybe_emit_stats();
    tagged
}

/// V14.9-W: heuristic OCR gate. Most photo libraries are 99% camera
/// snapshots where OCR is wasted CPU. Skip in that case; keep running
/// for document/screenshot-shaped inputs.
fn should_run_ocr(path: &std::path::Path, tagged: &TaggedFile, _size_bytes: u64) -> bool {
    if tagged.camera_model.is_some() {
        return false;
    }
    // Filenames that strongly suggest documents/screenshots.
    let lower = path
        .file_name()
        .and_then(|f| f.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    if lower.contains("screenshot")
        || lower.contains("screen shot")
        || lower.contains("scan")
        || lower.contains("receipt")
        || lower.contains("invoice")
        || lower.contains("document")
    {
        return true;
    }
    // PNG without a camera model is often a screenshot or document — run OCR.
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        if ext.eq_ignore_ascii_case("png") {
            return true;
        }
    }
    // Default: skip. Settings → "Always run OCR" could re-enable later
    // when we wire a per-scan policy flag.
    false
}

/// V15.0 Phase C: hard cap on image dimensions. A 100 KB JPEG can
/// decode to a 4 GB raw buffer; reject before decode to avoid OOM-ing
/// a worker. 50 megapixels = ~150 MB raw RGB8, well within budget but
/// big enough for legitimate photo + scanner output.
const MAX_DECODED_PIXELS: u64 = 50_000_000;

/// V15.0 Phase F: thumbnail fast path. Uses Windows IThumbnailProvider
/// (via `shell::thumbnail::render`) to pull the pre-cached 512×512 RGBA8
/// File Explorer has already indexed for almost every image in the user's
/// Pictures library. Cache hit ≈ 1 ms vs ~30 ms for full JPEG decode.
/// Converts RGBA8 → RGB8 to match the rest of the pipeline's expected
/// format.
async fn try_shell_thumbnail(path: PathBuf) -> anyhow::Result<(Vec<u8>, u32, u32)> {
    tokio::task::spawn_blocking(move || -> anyhow::Result<(Vec<u8>, u32, u32)> {
        let thumb = crate::shell::thumbnail::render(&path)?;
        // RGBA8 → RGB8 (strip alpha).
        let pixel_count = (thumb.width as usize) * (thumb.height as usize);
        let mut rgb = Vec::with_capacity(pixel_count * 3);
        for px in thumb.rgba.chunks_exact(4) {
            rgb.extend_from_slice(&px[..3]);
        }
        Ok((rgb, thumb.width, thumb.height))
    })
    .await?
}

/// Load an image from disk and return its RGB8 bytes + dimensions.
/// Done on a blocking thread to keep the tokio reactor free.
///
/// V15.0 Phase C: peeks header to reject pathological dimensions BEFORE
/// decode, and wraps the decode body in `catch_unwind` so a panicking
/// image-crate codec (e.g. malformed JPEG) propagates as Err instead of
/// crashing the worker.
async fn load_image_rgb(path: &std::path::Path) -> anyhow::Result<(Vec<u8>, u32, u32)> {
    let p = path.to_path_buf();
    tokio::task::spawn_blocking(move || -> anyhow::Result<(Vec<u8>, u32, u32)> {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> anyhow::Result<(Vec<u8>, u32, u32)> {
            // V15.2 perf #1: mmap the file once, drive both the dimension peek
            // and the full decode from the same memory region. The old
            // double-open path cost ~100 µs per file (50 ms wasted per 500
            // files; ~5 s on a 50k library; worse on slow disks).
            use std::io::Cursor;
            let file = std::fs::File::open(&p)
                .map_err(|e| anyhow::anyhow!("open: {e}"))?;
            let mmap = unsafe {
                memmap2::Mmap::map(&file)
                    .map_err(|e| anyhow::anyhow!("mmap: {e}"))?
            };
            let bytes: &[u8] = &mmap;

            let peek = image::ImageReader::new(Cursor::new(bytes))
                .with_guessed_format()
                .map_err(|e| anyhow::anyhow!("guess format (peek): {e}"))?;
            let (pw, ph) = peek
                .into_dimensions()
                .map_err(|e| anyhow::anyhow!("dimensions: {e}"))?;
            let pixels = pw as u64 * ph as u64;
            if pixels > MAX_DECODED_PIXELS {
                anyhow::bail!(
                    "image dimensions {}×{} ({} pixels) exceed cap of {} — refusing to decode",
                    pw, ph, pixels, MAX_DECODED_PIXELS
                );
            }
            let reader = image::ImageReader::new(Cursor::new(bytes))
                .with_guessed_format()
                .map_err(|e| anyhow::anyhow!("guess format (decode): {e}"))?;
            let dyn_img = reader.decode().map_err(|e| anyhow::anyhow!("decode: {e}"))?;
            let rgb = dyn_img.to_rgb8();
            let (w, h) = rgb.dimensions();
            Ok((rgb.into_raw(), w, h))
        }));
        match result {
            Ok(r) => r,
            Err(panic_payload) => {
                let msg = if let Some(s) = panic_payload.downcast_ref::<&'static str>() {
                    (*s).to_string()
                } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "non-string panic payload".to_string()
                };
                anyhow::bail!("image decoder panicked (adversarial input?): {msg}");
            }
        }
    })
    .await?
}

/// Pull a 25%-duration keyframe from a video via Media Foundation. Heavy
/// (codec init + decode) — runs on a blocking thread.
async fn extract_video_keyframe_blocking(path: PathBuf) -> anyhow::Result<(Vec<u8>, u32, u32)> {
    tokio::task::spawn_blocking(move || -> anyhow::Result<(Vec<u8>, u32, u32)> {
        let frame = shell::video::keyframe_25pct(&path)?;
        Ok((frame.rgb, frame.width, frame.height))
    })
    .await?
}

/// Run OCR on an RGB buffer. Returns None if no text or OCR isn't
/// available on this system.
/// Arc-shared OCR variant — avoids cloning a multi-MB RGB buffer when
/// the caller still holds the original. The shared Arc is dropped on
/// the worker thread when OCR returns; the original Vec stays untouched.
#[allow(dead_code)]
async fn run_ocr_blocking_arc(rgb: std::sync::Arc<Vec<u8>>, w: u32, h: u32) -> anyhow::Result<Option<shell::ocr::OcrResult>> {
    tokio::task::spawn_blocking(move || -> anyhow::Result<Option<shell::ocr::OcrResult>> {
        match shell::ocr::recognize(&rgb, w, h) {
            Ok(r) => Ok(Some(r)),
            Err(err) => {
                tracing::debug!(?err, "OCR skipped");
                Ok(None)
            }
        }
    })
    .await?
}

async fn run_ocr_blocking(rgb: Vec<u8>, w: u32, h: u32) -> anyhow::Result<Option<shell::ocr::OcrResult>> {
    tokio::task::spawn_blocking(move || -> anyhow::Result<Option<shell::ocr::OcrResult>> {
        match shell::ocr::recognize(&rgb, w, h) {
            Ok(r) => Ok(Some(r)),
            Err(err) => {
                tracing::debug!(?err, "OCR skipped");
                Ok(None)
            }
        }
    })
    .await?
}

/// Parse camera model + GPS from EXIF if present. Best-effort, never
/// fails — returns None on any error.
async fn parse_exif_blocking(path: PathBuf) -> Option<(Option<String>, Option<f64>, Option<f64>)> {
    tokio::task::spawn_blocking(move || -> Option<(Option<String>, Option<f64>, Option<f64>)> {
        let file = std::fs::File::open(&path).ok()?;
        let mut reader = std::io::BufReader::new(file);
        let exif = exif::Reader::new().read_from_container(&mut reader).ok()?;

        let camera_model = exif
            .get_field(exif::Tag::Model, exif::In::PRIMARY)
            .map(|f| f.display_value().with_unit(&exif).to_string().trim_matches('"').to_string())
            .filter(|s| !s.is_empty());

        let lat = read_gps_coord(&exif, exif::Tag::GPSLatitude, exif::Tag::GPSLatitudeRef);
        let lon = read_gps_coord(&exif, exif::Tag::GPSLongitude, exif::Tag::GPSLongitudeRef);

        Some((camera_model, lat, lon))
    })
    .await
    .ok()
    .flatten()
}

fn read_gps_coord(exif: &exif::Exif, value_tag: exif::Tag, ref_tag: exif::Tag) -> Option<f64> {
    let value_field = exif.get_field(value_tag, exif::In::PRIMARY)?;
    let ref_field = exif.get_field(ref_tag, exif::In::PRIMARY)?;

    let dms = match &value_field.value {
        exif::Value::Rational(r) if r.len() >= 3 => [
            r[0].to_f64(),
            r[1].to_f64(),
            r[2].to_f64(),
        ],
        _ => return None,
    };
    let mut decimal = dms[0] + dms[1] / 60.0 + dms[2] / 3600.0;

    let ref_str = ref_field.display_value().to_string();
    if ref_str.starts_with('S') || ref_str.starts_with('W') {
        decimal = -decimal;
    }
    Some(decimal)
}

/// Crop a face region from the source RGB image with `FACE_CROP_PAD`
/// slack, then nearest-resize to 112×112. Returns None if the bbox is
/// degenerate or outside the image.
fn crop_and_resize_face(
    rgb: &[u8],
    img_w: u32,
    img_h: u32,
    bbox: &[f32; 4],
) -> Option<Vec<u8>> {
    let pad_w = bbox[2] * FACE_CROP_PAD;
    let pad_h = bbox[3] * FACE_CROP_PAD;
    let x1 = (bbox[0] - pad_w).max(0.0).round() as i32;
    let y1 = (bbox[1] - pad_h).max(0.0).round() as i32;
    let x2 = (bbox[0] + bbox[2] + pad_w).min(img_w as f32).round() as i32;
    let y2 = (bbox[1] + bbox[3] + pad_h).min(img_h as f32).round() as i32;

    let crop_w = (x2 - x1).max(1) as usize;
    let crop_h = (y2 - y1).max(1) as usize;
    if crop_w < 16 || crop_h < 16 {
        return None;
    }

    let mut crop = vec![0u8; crop_w * crop_h * 3];
    for cy in 0..crop_h {
        let sy = (y1 as usize) + cy;
        if sy >= img_h as usize { continue; }
        for cx in 0..crop_w {
            let sx = (x1 as usize) + cx;
            if sx >= img_w as usize { continue; }
            let s = (sy * img_w as usize + sx) * 3;
            let d = (cy * crop_w + cx) * 3;
            crop[d] = rgb[s];
            crop[d + 1] = rgb[s + 1];
            crop[d + 2] = rgb[s + 2];
        }
    }

    Some(resize_rgb_nearest(&crop, crop_w, crop_h, 112, 112))
}

/// Nearest-neighbor resize for interleaved RGB. Fast, fine for ML
/// preprocessing where the model is robust to interpolation choice.
/// Bilinear/Lanczos via `fast-image-resize` is a Phase 2.6 polish.
pub fn resize_rgb_nearest(
    rgb: &[u8],
    src_w: usize,
    src_h: usize,
    dst_w: usize,
    dst_h: usize,
) -> Vec<u8> {
    let mut out = vec![0u8; dst_w * dst_h * 3];
    if src_w == 0 || src_h == 0 {
        return out;
    }
    let sx_step = src_w as f32 / dst_w as f32;
    let sy_step = src_h as f32 / dst_h as f32;
    for dy in 0..dst_h {
        let sy = ((dy as f32 + 0.5) * sy_step) as usize;
        let sy = sy.min(src_h - 1);
        for dx in 0..dst_w {
            let sx = ((dx as f32 + 0.5) * sx_step) as usize;
            let sx = sx.min(src_w - 1);
            let s = (sy * src_w + sx) * 3;
            let d = (dy * dst_w + dx) * 3;
            out[d] = rgb[s];
            out[d + 1] = rgb[s + 1];
            out[d + 2] = rgb[s + 2];
        }
    }
    out
}

/// Difference-hash perceptual fingerprint. 9×8 grayscale → 64 bits, one
/// bit per horizontal-adjacent-pixel comparison. Matches macOS dHash.
pub fn compute_dhash(rgb: &[u8], width: usize, height: usize) -> i64 {
    if width == 0 || height == 0 || rgb.len() < width * height * 3 {
        return 0;
    }
    let small = resize_rgb_nearest(rgb, width, height, 9, 8);
    let mut bits: u64 = 0;
    for y in 0..8 {
        for x in 0..8 {
            let l = grayscale(&small, 9, x, y);
            let r = grayscale(&small, 9, x + 1, y);
            if l > r {
                bits |= 1u64 << (y * 8 + x);
            }
        }
    }
    bits as i64
}

fn grayscale(rgb: &[u8], stride: usize, x: usize, y: usize) -> u8 {
    let i = (y * stride + x) * 3;
    let r = rgb[i] as u32;
    let g = rgb[i + 1] as u32;
    let b = rgb[i + 2] as u32;
    // Rec. 601 luma — same coefficients macOS uses.
    ((299 * r + 587 * g + 114 * b) / 1000) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn tagger_passes_discovered_through_to_tagged() {
        let coord = ScanCoordinator::new();
        let (tx, rx) = mpsc::channel(8);
        let models = Arc::new(ModelStack::empty());
        let tagger = Tagger::new(coord, 2, models);
        let mut out = tagger.spawn(rx);

        tx.send(DiscoveredFile {
            path: PathBuf::from("C:/tmp/a.jpg"),
            kind: FileKind::Image,
            size_bytes: 1024,
            modified_unix: 1.0,
        })
        .await
        .unwrap();
        drop(tx);

        let got = tokio::time::timeout(Duration::from_secs(2), out.recv())
            .await
            .expect("worker emitted")
            .expect("channel not closed");
        // No models installed → image stage skipped, but row still flows.
        assert_eq!(got.size_bytes, 1024);
        assert_eq!(got.kind, FileKind::Image);
        // File doesn't exist, decode failed → marked failed.
        assert!(got.failed);
    }

    #[test]
    fn detected_face_has_crop_field() {
        // Sanity: the field exists + defaults to None when constructed without it.
        let f = DetectedFace {
            bbox: [0.0; 4],
            landmarks: [[0.0; 2]; 5],
            embedding: vec![0.0; 512],
            roll: 0.0,
            yaw: 0.0,
            pitch: 0.0,
            quality: 0.0,
            crop_rgb_112: None,
        };
        assert!(f.crop_rgb_112.is_none());
    }

    #[test]
    fn dhash_solid_color_is_zero() {
        let rgb = vec![128u8; 64 * 64 * 3];
        assert_eq!(compute_dhash(&rgb, 64, 64), 0);
    }

    #[test]
    fn dhash_changes_with_horizontal_gradient() {
        // Decreasing gradient — left > right at every step, so every bit
        // gets set. Result must be non-zero (and in fact -1 in i64 = all 1s).
        let mut rgb = vec![0u8; 64 * 64 * 3];
        for y in 0..64 {
            for x in 0..64 {
                let v = (252 - x * 4) as u8;
                let i = (y * 64 + x) * 3;
                rgb[i] = v; rgb[i + 1] = v; rgb[i + 2] = v;
            }
        }
        let h = compute_dhash(&rgb, 64, 64);
        assert_ne!(h, 0);
    }

    #[test]
    fn resize_nearest_preserves_extent() {
        let rgb = vec![200u8; 16 * 16 * 3];
        let out = resize_rgb_nearest(&rgb, 16, 16, 4, 4);
        assert_eq!(out.len(), 4 * 4 * 3);
        assert!(out.iter().all(|&v| v == 200));
    }

    #[test]
    fn crop_face_returns_112x112() {
        let rgb = vec![100u8; 200 * 200 * 3];
        let bbox = [50.0, 60.0, 80.0, 80.0];
        let crop = crop_and_resize_face(&rgb, 200, 200, &bbox).expect("crop");
        assert_eq!(crop.len(), 112 * 112 * 3);
    }

    #[test]
    fn crop_face_rejects_tiny_bbox() {
        let rgb = vec![100u8; 200 * 200 * 3];
        let bbox = [50.0, 50.0, 4.0, 4.0];
        assert!(crop_and_resize_face(&rgb, 200, 200, &bbox).is_none());
    }
}
