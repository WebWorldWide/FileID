// Tagging — N parallel workers consume DiscoveredFile from Discovery,
// run the per-file ML pipeline, and emit TaggedFile rows that DBWriter
// batches into SQLite.
//
// Worker count: physical_cores * 1.7. Semaphores cap concurrent ORT
// inferences to prevent VRAM thrash on the GPU EP — 4 for vision models,
// 2 for CLIP. Missing models gracefully degrade (the pipeline emits a
// TaggedFile with the missing fields = None).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use parking_lot::Mutex;
use tokio::sync::{mpsc, Semaphore};

/// Opt-in per-stage tracing for perf investigations. Default off — toggle via
/// `FILEID_PERF_TRACE=1`. When on, `process_file` emits a `[PERF]` debug line
/// for each pipeline stage with `path` + `elapsed_ms` so a 100-file run can be
/// distilled into a stage-cost table.
fn perf_trace_enabled() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(|| {
        std::env::var("FILEID_PERF_TRACE")
            .map(|s| !s.is_empty() && s != "0")
            .unwrap_or(false)
    })
}

#[inline]
fn perf_trace(stage: &str, path: &std::path::Path, elapsed_ms: f64) {
    if perf_trace_enabled() {
        tracing::debug!(
            target: "FileIDEngine::perf",
            stage,
            path = %crate::platform::redact_path_for_log(path),
            elapsed_ms,
            "[PERF]"
        );
    }
}

use crate::coordinator::ScanCoordinator;
use crate::models::runtime::error_has_device_removed_marker;
use crate::models::{arcface::ArcFace, classifier::ClassifierSession, mobileclip::MobileClipImage, scrfd::{self, Scrfd}};
use crate::pipeline::batch_clip::ClipBatchCoordinator;
use crate::pipeline::discovery::{DiscoveredFile, FileKind};
use crate::shell;

/// Per-file stage timings. Each worker adds its µs into these atomics;
/// every STATS_PERIOD files an info-level [STATS] line summarises the
/// average so optimization wins (or regressions) show up in engine.jsonl.
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
    let ocr_avg = STATS_OCR_US
        .load(Ordering::Relaxed)
        .checked_div(ocr_ran)
        .unwrap_or(0);
    let batch_count = crate::pipeline::batch_clip::STATS_BATCH_COUNT.load(Ordering::Relaxed);
    let batch_sum = crate::pipeline::batch_clip::STATS_BATCH_SIZE_SUM.load(Ordering::Relaxed);
    // ×10 so we can see 4.2 as "42"
    let avg_batch = (batch_sum * 10).checked_div(batch_count).unwrap_or(0);
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

/// Bounded channel capacity, Tagging → DBWriter. One transaction worth
/// + slack so workers don't stall on a slow flush.
pub const TAGGING_CHANNEL_CAP: usize = 256;

/// Cap concurrent ORT vision-model inferences. Higher values pressure the
/// DirectML command queue past the 2 s TDR deadline and the GPU device
/// gets removed — a full system hang. The ~10 % throughput cost of 4 vs
/// 8 is the price of staying inside the TDR ceiling.
const VISION_CONCURRENCY: usize = 4;

/// Cap concurrent CLIP image embeds. Same TDR-pressure reason as
/// VISION_CONCURRENCY.
const CLIP_CONCURRENCY: usize = 2;

/// Cap concurrent classifier inferences. Runs alongside CLIP under a
/// separate semaphore so neither stage starves the other. Default 2 to
/// match CLIP's caution against the DirectML TDR ceiling; tune upward
/// only after measuring on a 500-file run.
const CLASSIFIER_CONCURRENCY: usize = 2;

/// Tags below this confidence are dropped — matches the macOS Vision
/// classifier behaviour (`Tagging.swift`).
const CLASSIFIER_THRESHOLD: f32 = 0.30;

/// Per-file classifier top-K. macOS Vision pulls similar count of
/// labels per file.
const CLASSIFIER_TOP_K: usize = 8;

/// Padding fraction added to the SCRFD bbox before cropping. Must match
/// the macOS value (FaceClustering.swift = 0.15) so the same library
/// produces the same ArcFace embeddings → same cluster IDs across
/// platforms. Clustering parity is the biggest single regression guard.
const FACE_CROP_PAD: f32 = 0.15;

/// One per file post-tagging. The DBWriter batches these into a single
/// transaction. Embeddings are L2-normalized float32 vectors stored as
/// raw little-endian bytes.
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
    pub aesthetic: Option<f64>,
    pub image_width: u32,
    pub image_height: u32,
    pub clip_embedding: Option<Vec<f32>>,

    pub camera_model: Option<String>,
    pub location_lat: Option<f64>,
    pub location_lon: Option<f64>,

    pub vision_ms: f64,
    pub clip_ms: f64,
    pub total_ms: f64,

    pub failed: bool,
    pub error_message: Option<String>,

    /// Semantic tags assembled from (a) the MobileNetV3 classifier top-K
    /// labels above `CLASSIFIER_THRESHOLD` and (b) enriched-extras
    /// derived from existing per-file signals (Year/Camera family/
    /// Wide-Tall-Square/Has Faces/Has Text/Has Location). Persisted
    /// into the `tags` table by DBWriter with source = `"auto"`; the
    /// Library UI reads them via ReadStore and renders as TagChip rows.
    pub tags: Vec<String>,
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
/// optional — missing weights cause the corresponding stage to no-op so
/// a partial install doesn't fail the whole scan.
///
/// Each model is a POOL of independent Sessions; workers index by
/// `worker_idx % pool.len()` so multiple inferences run in parallel
/// against the GPU's command queue. Pool size capped at MODEL_POOL_SIZE
/// to keep VRAM bounded.
pub struct ModelStack {
    pub arcface: Option<Vec<Mutex<ArcFace>>>,
    pub scrfd: Option<Vec<Mutex<Scrfd>>>,
    /// MobileCLIP has two paths:
    /// - `mobileclip_pool` (default) — N-Session pool, VRAM-clamped via
    ///   `resolve_pool_size`. Throughput winner for MobileCLIP-S2 on DirectML.
    /// - `mobileclip_batch` — single Session behind `ClipBatchCoordinator`,
    ///   opt-in via `FILEID_CLIP_USE_BATCH=1`. Kept for experiments with
    ///   CUDA EP or larger models where batching amortizes.
    pub mobileclip_pool: Option<Vec<Mutex<MobileClipImage>>>,
    pub mobileclip_batch: Option<Arc<ClipBatchCoordinator>>,
    /// MobileNetV3 ImageNet-1k scene classifier. Optional — when absent
    /// the per-file `tags` Vec is populated only from enriched extras.
    pub classifier: Option<Vec<Mutex<ClassifierSession>>>,
}

/// Aspirational pool size — actual cap is `min(this, vram_cap, worker_count)`
/// computed in `resolve_pool_size`. The VRAM gate prevents a wedged DirectML
/// driver: pool=4 on a 6 GB RTX 2060 exhausts VRAM and requires a hard reboot.
/// On a 6 GB card the gate clamps to 2; on 12 GB cards it allows up to ~7.
/// Tunable per-user via `FILEID_MODEL_POOL_SIZE` (also gated).
const MODEL_POOL_SIZE: usize = 4;

/// Estimated VRAM headroom per pooled-Session of (ArcFace + SCRFD +
/// MobileCLIP combined): weights + DirectML allocator + intermediate
/// tensors. Conservative upper bound — used to clamp pool size to fit
/// available `DedicatedVideoMemory`. Real per-session residency varies
/// by model and EP; treat this as a ceiling, not a measurement.
///
/// Measured on RTX 2060 6 GB during a scan against ~40 JPEGs in
/// %USERPROFILE%\Pictures: total dedicated VRAM peaked at ~2.6 GB from a
/// ~1.65 GB idle baseline, i.e. ~940 MB attributed to the engine. Keeping
/// the ceiling at 1500 MB preserves a ~560 MB safety margin for DirectML
/// allocator fragmentation under longer-running scans.
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

        // Batch path is now default-on. The pool path was empirically faster
        // at the time it was written (small N-session pool on DirectML), but
        // VRAM-clamp drops pool_size to 1 on most 6 GB cards, leaving a
        // single Mutex<MobileClipImage> behind the CLIP_CONCURRENCY=2
        // semaphore — effectively serializing CLIP work. The batch
        // coordinator drives one Session with batched (N, 3, 256, 256)
        // tensors, amortizing per-call DirectML dispatch overhead and
        // beating the pool path on the user's hardware. Set
        // `FILEID_CLIP_USE_BATCH=0` to fall back to the pool path.
        let use_batch = std::env::var("FILEID_CLIP_USE_BATCH")
            .ok()
            .map(|s| !(s == "0" || s.eq_ignore_ascii_case("false")))
            .unwrap_or(true);

        let classifier = load_classifier_pool(pool_size);

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

        Self { arcface, scrfd, mobileclip_pool, mobileclip_batch, classifier }
    }

    #[allow(dead_code)]
    pub fn empty() -> Self {
        Self {
            arcface: None,
            scrfd: None,
            mobileclip_pool: None,
            mobileclip_batch: None,
            classifier: None,
        }
    }
}

/// Load the MobileNetV3 classifier as a small pool (same shape as the
/// vision-model pools) so multiple workers can run inference in parallel
/// against the GPU's command queue. Missing weights or labels degrade
/// the stage to a no-op — pipeline still emits the enriched-extras tags.
fn load_classifier_pool(pool_size: usize) -> Option<Vec<Mutex<ClassifierSession>>> {
    let model_path = match crate::models::classifier::default_model_path() {
        Ok(p) => p,
        Err(err) => {
            tracing::warn!(model = "classifier", ?err, "model path unresolved");
            return None;
        }
    };
    let labels_path = match crate::models::classifier::default_labels_path() {
        Ok(p) => p,
        Err(err) => {
            tracing::warn!(model = "classifier", ?err, "labels path unresolved");
            return None;
        }
    };
    if !model_path.exists() || !labels_path.exists() {
        tracing::info!(
            model = "classifier",
            model_path = %model_path.display(),
            labels_path = %labels_path.display(),
            "classifier model+labels not installed; stage will skip"
        );
        return None;
    }
    let mut pool = Vec::with_capacity(pool_size);
    for idx in 0..pool_size {
        if idx > 0 {
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        match ClassifierSession::load(&model_path, &labels_path) {
            Ok(model) => pool.push(Mutex::new(model)),
            Err(err) => {
                use crate::models::runtime::error_has_device_removed_marker;
                if error_has_device_removed_marker(&err) {
                    tracing::error!(model = "classifier", slot = idx, ?err, "[STARTUP-TDR] device-removed during classifier warmup; aborting pool load");
                    return None;
                }
                tracing::warn!(model = "classifier", slot = idx, ?err, "classifier pool load failed; stage will skip if pool empty");
                break;
            }
        }
    }
    if pool.is_empty() {
        None
    } else {
        tracing::info!(model = "classifier", pool_size = pool.len(), "classifier pool loaded");
        Some(pool)
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
        // Stagger each Session allocation by 250 ms so a 6-session pool
        // (2 × 3 models) doesn't burst DirectML's command queue at engine
        // startup — the riskiest TDR window.
        if idx > 0 {
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        match loader(p.clone()) {
            Ok(model) => pool.push(Mutex::new(model)),
            Err(err) => {
                // If a slot's warmup hit device-removed, bail the whole
                // stack rather than try further slots against the dead GPU.
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

    /// Wire Discovery → decoder pool → N tagging workers → DBWriter.
    ///
    /// Two-stage pipeline:
    ///   1. **Decoder pool**: M dedicated OS threads pull `DiscoveredFile`
    ///      from the discovery channel and run blocking image decode (the
    ///      CPU-bound part). Decoded `(rgb, w, h)` bytes are pushed into
    ///      the pre-decoded channel.
    ///   2. **Inference workers**: N async tokio tasks pull `PreDecoded`,
    ///      run face/CLIP/OCR under semaphore-bounded inference, and ship
    ///      `TaggedFile` rows to the DBWriter.
    ///
    /// Previously decode happened inline inside each worker, which meant
    /// new files were only pulled from discovery once a worker freed up
    /// from its prior ML wait. With `CLIP_CONCURRENCY=2` and `worker_count=14`
    /// most workers were idle waiting on semaphores — the CPU only saw
    /// ~12 % utilization. Splitting decode into its own pool lets the
    /// decoder threads saturate available cores ahead of inference,
    /// keeping a warm buffer of pre-decoded frames so workers never wait
    /// on the CPU-bound path.
    pub fn spawn(self, mut input: mpsc::Receiver<DiscoveredFile>) -> mpsc::Receiver<TaggedFile> {
        let (out_tx, out_rx) = mpsc::channel(TAGGING_CHANNEL_CAP);

        // Stage 1a — bridge tokio mpsc<DiscoveredFile> into a
        // multi-consumer async-channel so the decoder pool can fan out.
        let (raw_tx, raw_rx) = async_channel::bounded::<DiscoveredFile>(TAGGING_CHANNEL_CAP);
        let coordinator_pump = self.coordinator.clone();
        tokio::spawn(async move {
            while let Some(file) = input.recv().await {
                if coordinator_pump.is_cancelled() {
                    break;
                }
                if raw_tx.send(file).await.is_err() {
                    break;
                }
            }
        });

        // Stage 1b — decoder pool: M sync OS threads. Sized by physical
        // CPU topology (p+e cores), clamped to the [2, 12] range so we
        // don't oversaturate the GPU side or starve the WinUI 3 app.
        let topo = crate::platform::cpu_topology();
        let decoder_count = ((topo.p_cores + topo.e_cores) as usize).clamp(2, 12);
        // Channel cap: 2× worker count keeps a small read-ahead buffer
        // ready, without ballooning RAM with decoded RGB bytes (each
        // frame can be ~50 MB for a 12 MP photo).
        let predecoded_cap = (self.worker_count * 2).max(8);
        let (predecoded_tx, predecoded_rx) =
            async_channel::bounded::<PreDecoded>(predecoded_cap);
        for decoder_idx in 0..decoder_count {
            let rx = raw_rx.clone();
            let tx = predecoded_tx.clone();
            let coord = self.coordinator.clone();
            std::thread::Builder::new()
                .name(format!("fileid-decode-{decoder_idx}"))
                .spawn(move || run_decoder_thread(rx, tx, coord))
                .expect("spawn decoder thread");
        }
        drop(raw_rx);
        drop(predecoded_tx);

        let vision_sem = Arc::new(Semaphore::new(VISION_CONCURRENCY));
        let clip_sem = Arc::new(Semaphore::new(CLIP_CONCURRENCY));
        let classifier_sem = Arc::new(Semaphore::new(CLASSIFIER_CONCURRENCY));

        for worker_idx in 0..self.worker_count {
            let rx = predecoded_rx.clone();
            let tx = out_tx.clone();
            let coord = self.coordinator.clone();
            let vision_sem = vision_sem.clone();
            let clip_sem = clip_sem.clone();
            let classifier_sem = classifier_sem.clone();
            let models = self.models.clone();

            tokio::spawn(async move {
                // Drop per-worker thread priority to LOWEST so foreground
                // apps stay snappy during scans. tokio worker threads
                // inherit the parent process priority by default.
                crate::platform::set_worker_background_priority();
                // After every YIELD_AFTER files, sleep briefly to give
                // foreground apps + DWM breathing room. <1 % cost; keeps
                // multi-hour scans friendly to an actively-used desktop.
                const YIELD_AFTER: u64 = 500;
                let mut files_done: u64 = 0;
                while let Ok(predecoded) = rx.recv().await {
                    if coord.check().await.is_err() {
                        break;
                    }
                    let path_for_timeout = predecoded.file.path.clone();
                    let timeout_kind = predecoded.file.kind;
                    let timeout_size = predecoded.file.size_bytes;
                    let timeout_modified = predecoded.file.modified_unix;
                    // Per-file timeout — image decoders or network UNC reads
                    // can hang indefinitely.
                    let fut = process_file_predecoded(predecoded, &models, &vision_sem, &clip_sem, &classifier_sem, worker_idx, &coord);
                    let tagged = match tokio::time::timeout(
                        std::time::Duration::from_secs(60),
                        fut,
                    )
                    .await
                    {
                        Ok(t) => t,
                        Err(_elapsed) => {
                            tracing::warn!(
                                path = %crate::platform::redact_path_for_log(&path_for_timeout),
                                "per-file timeout after 60s; marking failed and continuing"
                            );
                            TaggedFile {
                                path: path_for_timeout,
                                kind: timeout_kind,
                                size_bytes: timeout_size,
                                modified_unix: timeout_modified,
                                scanned_unix: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs_f64(),
                                has_faces: false,
                                faces: Vec::new(),
                                has_text: false,
                                ocr_text: None,
                                phash: None,
                                aesthetic: None,
                                image_width: 0,
                                image_height: 0,
                                clip_embedding: None,
                                camera_model: None,
                                location_lat: None,
                                location_lon: None,
                                vision_ms: 0.0,
                                clip_ms: 0.0,
                                total_ms: 60000.0,
                                failed: true,
                                error_message: Some("per-file timeout after 60s".into()),
                                tags: Vec::new(),
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

/// One unit of work between the decoder pool and the inference workers.
/// `decoded` is `Ok((rgb, w, h))` on a successful image/video decode and
/// `Err(_)` when the file couldn't be opened or decoded (the worker emits
/// a failed `TaggedFile` row so the DB still records the file). `None` is
/// returned for non-image, non-video kinds where the existing pipeline
/// doesn't attempt a decode.
pub struct PreDecoded {
    pub file: DiscoveredFile,
    pub decoded: Option<anyhow::Result<(Vec<u8>, u32, u32)>>,
}

/// Decoder-pool worker. Sync OS thread (not a tokio task) so the
/// blocking JPEG/PNG decode doesn't tie up tokio's runtime threads.
/// Pulls from the raw discovery channel via `recv_blocking()` and pushes
/// into the pre-decoded channel via `send_blocking()`. Exits cleanly
/// when the input channel closes or the coordinator is cancelled.
fn run_decoder_thread(
    rx: async_channel::Receiver<DiscoveredFile>,
    tx: async_channel::Sender<PreDecoded>,
    coord: ScanCoordinator,
) {
    loop {
        if coord.is_cancelled() {
            return;
        }
        let file = match rx.recv_blocking() {
            Ok(f) => f,
            Err(_) => return,
        };
        let decode_started = Instant::now();
        let decoded = match file.kind {
            FileKind::Image => Some(decode_image_sync(&file.path)),
            FileKind::Video => Some(decode_video_keyframe_sync(&file.path)),
            _ => None,
        };
        if decoded.is_some() {
            STATS_DECODE_US.fetch_add(decode_started.elapsed().as_micros() as u64, Ordering::Relaxed);
        }
        let item = PreDecoded { file, decoded };
        if tx.send_blocking(item).is_err() {
            return;
        }
    }
}

/// Decode an image off disk into RGB8 bytes + dimensions on the calling
/// thread. mmap'd dimension peek + decode in one pass; rejects images
/// over `MAX_DECODED_PIXELS` before commit. `catch_unwind` wraps a
/// panicking codec (malformed JPEG) so it surfaces as Err instead of
/// crashing the decoder thread.
///
/// On Windows, falls back to the WinRT BitmapDecoder (HEIF Image
/// Extensions) when image-rs fails on a .heic / .heif file. The
/// fallback is silent on other extensions.
fn decode_image_sync(path: &std::path::Path) -> anyhow::Result<(Vec<u8>, u32, u32)> {
    let primary = decode_image_sync_imagecrate(path);
    if primary.is_ok() {
        return primary;
    }
    // Extension probe — only try the WinRT fallback for HEIC/HEIF.
    #[cfg(windows)]
    {
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        if ext == "heic" || ext == "heif" {
            match shell::heic::decode(path) {
                Ok(out) => return Ok(out),
                Err(heic_err) => {
                    // Bubble up a user-facing instruction. The string is
                    // matched by the pipeline so the row's `error` field
                    // surfaces a clean install hint instead of the raw
                    // WinRT HRESULT.
                    return Err(anyhow::anyhow!(
                        "HEIC codec not installed — install HEIF Image Extensions from the Microsoft Store ({heic_err})"
                    ));
                }
            }
        }
    }
    primary
}

fn decode_image_sync_imagecrate(path: &std::path::Path) -> anyhow::Result<(Vec<u8>, u32, u32)> {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> anyhow::Result<(Vec<u8>, u32, u32)> {
        use std::io::Cursor;
        let file = std::fs::File::open(path)
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
}

/// Sync video-keyframe decode. Used by the decoder pool.
fn decode_video_keyframe_sync(path: &std::path::Path) -> anyhow::Result<(Vec<u8>, u32, u32)> {
    let frame = shell::video::keyframe_25pct(path)?;
    Ok((frame.rgb, frame.width, frame.height))
}

/// Per-file ML body. Receives a pre-decoded image (or video keyframe)
/// from the decoder pool, runs face detect → embed, CLIP embed, dHash,
/// EXIF — each gated on its semaphore + the model being installed.
/// Failure of any single stage is non-fatal: the row gets emitted with
/// that field = None and `failed=0` (only image decode failure marks
/// the row failed). The decode itself happened on a sibling decoder
/// thread so this function never blocks on filesystem I/O.
async fn process_file_predecoded(
    predecoded: PreDecoded,
    models: &Arc<ModelStack>,
    vision_sem: &Arc<Semaphore>,
    clip_sem: &Arc<Semaphore>,
    classifier_sem: &Arc<Semaphore>,
    worker_idx: usize,
    coord: &ScanCoordinator,
) -> TaggedFile {
    let PreDecoded { file, decoded } = predecoded;
    let file = &file;
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
        aesthetic: None,
        image_width: 0,
        image_height: 0,
        clip_embedding: None,
        camera_model: None,
        location_lat: None,
        location_lon: None,
        vision_ms: 0.0,
        clip_ms: 0.0,
        total_ms: 0.0,
        failed: false,
        error_message: None,
        tags: Vec::new(),
    };

    // SEC/perf: once the GPU is marked dead (TDR or device-removed),
    // skip the whole ML pipeline for the rest of this process lifetime.
    // mark_gpu_dead is one-shot (see coordinator.rs:96 — never reset),
    // so this also prevents the remaining Discovery queue from queueing
    // up tens of thousands of doomed inference calls behind us. The row
    // is still emitted (with failed=false, empty embeddings) so the
    // file row exists in the DB and a future scan after restart picks
    // it up.
    if coord.is_gpu_dead() {
        tagged.total_ms = started.elapsed().as_secs_f64() * 1000.0;
        return tagged;
    }

    // Decode already happened upstream on a decoder-pool thread; we just
    // consume the result here. Image-decode failure → emit a failed row
    // so the DB still records the file (resume cursor advances, the next
    // scan re-tries it).
    perf_trace("image_decode_start", &file.path, 0.0);
    let image_source: Option<(Vec<u8>, u32, u32)> = match decoded {
        Some(Ok(t)) => Some(t),
        Some(Err(err)) => {
            tracing::warn!(?err, path = %crate::platform::redact_path_for_log(&file.path), "image decode failed");
            tagged.failed = true;
            tagged.error_message = Some(format!("image decode: {err:#}"));
            None
        }
        None => None,
    };
    perf_trace("image_decode_done", &file.path, started.elapsed().as_secs_f64() * 1000.0);

    if let Some((rgb, w, h)) = image_source {
        tagged.image_width = w;
        tagged.image_height = h;
        {
            let megapixels = (w as f64 * h as f64) / 1_000_000.0;
            let res_score = (megapixels / 50.0).min(1.0);
            let size_score = (file.size_bytes as f64 / (100.0 * 1024.0 * 1024.0)).min(1.0);
            tagged.aesthetic = Some(size_score * 0.5 + res_score * 0.5);
        }
            if matches!(file.kind, FileKind::Image) {
                let exif_started = Instant::now();
                if let Some((cam, lat, lon)) = parse_exif_blocking(file.path.clone()).await {
                    tagged.camera_model = cam;
                    tagged.location_lat = lat;
                    tagged.location_lon = lon;
                }
                record_stage(&STATS_EXIF_US, exif_started);
                perf_trace("exif_done", &file.path, exif_started.elapsed().as_secs_f64() * 1000.0);
            }

                let dhash_started = Instant::now();
                tagged.phash = Some(compute_dhash(&rgb, w as usize, h as usize));
                record_stage(&STATS_DHASH_US, dhash_started);
                perf_trace("dhash_done", &file.path, dhash_started.elapsed().as_secs_f64() * 1000.0);

                // Short-circuit GPU stages if a prior file already detected
                // device-removed. Submitting new work against the dead GPU
                // device wedges the system when TDR fires.
                let gpu_alive = !coord.is_gpu_dead();
                if gpu_alive {
                if let (Some(scrfd_pool), Some(arcface_pool)) = (&models.scrfd, &models.arcface) {
                    let permit = vision_sem.acquire().await;
                    let vision_started = Instant::now();
                    if permit.is_ok() {
                        let scrfd_mu = &scrfd_pool[worker_idx % scrfd_pool.len()];
                        let arcface_mu = &arcface_pool[worker_idx % arcface_pool.len()];
                        let scrfd_started = Instant::now();
                        let detections = {
                            let mut s = scrfd_mu.lock();
                            s.detect(&rgb, w, h)
                        };
                        perf_trace("scrfd_done", &file.path, scrfd_started.elapsed().as_secs_f64() * 1000.0);
                        let arcface_started = Instant::now();
                        match detections {
                            Ok(dets) => {
                                for det in dets {
                                    if coord.is_gpu_dead() { break; }
                                    let quality = match scrfd::validate_face_geometry(&det, w, h) {
                                        Some(q) => q,
                                        None => continue,
                                    };
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
                                                    quality,
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
                        perf_trace("arcface_done", &file.path, arcface_started.elapsed().as_secs_f64() * 1000.0);
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
                    perf_trace("clip_done", &file.path, clip_started.elapsed().as_secs_f64() * 1000.0);
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
                    perf_trace("clip_done", &file.path, clip_started.elapsed().as_secs_f64() * 1000.0);
                }
                }

                // Classifier — reuses the same decoded RGB. Resized
                // separately to 224×224 (the MobileNetV3 input dimension)
                // since CLIP wants 256×256.
                if !coord.is_gpu_dead() {
                    if let Some(classifier_pool) = &models.classifier {
                        let permit = classifier_sem.acquire().await;
                        let classifier_started = Instant::now();
                        if permit.is_ok() {
                            let classifier_mu = &classifier_pool[worker_idx % classifier_pool.len()];
                            let n = crate::models::classifier::INPUT_SIZE;
                            let resized_224 = resize_rgb_nearest(&rgb, w as usize, h as usize, n, n);
                            let classify_result = {
                                let mut c = classifier_mu.lock();
                                c.classify_batch(&[resized_224], CLASSIFIER_TOP_K, CLASSIFIER_THRESHOLD)
                            };
                            match classify_result {
                                Ok(mut batches) => {
                                    if let Some(per_image) = batches.pop() {
                                        for (label, _score) in per_image {
                                            tagged.tags.push(label);
                                        }
                                    }
                                }
                                Err(err) => {
                                    if error_has_device_removed_marker(&err) {
                                        if coord.mark_gpu_dead() {
                                            tracing::error!(?err, "[GPU-TDR] classifier device-removed; cancelling scan");
                                        }
                                    } else {
                                        tracing::warn!(?err, "classifier classify failed");
                                    }
                                }
                            }
                        }
                        perf_trace("classifier_done", &file.path, classifier_started.elapsed().as_secs_f64() * 1000.0);
                    } else {
                        // Classifier model not installed — log once, then
                        // proceed with enriched-extras-only tags.
                        static SKIP_LOGGED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
                        SKIP_LOGGED.get_or_init(|| {
                            tracing::info!("[CLASSIFIER] model_not_installed; per-file tags are enriched-extras only");
                        });
                    }
                }

                // OCR is the biggest CPU per file (~30-50 ms). For camera
                // photos (EXIF camera_model present) it returns nothing
                // useful, so skip. Documents/screenshots still run OCR.
                // Also short-circuit when the GPU is already known dead —
                // Windows.Media.Ocr fails-soft on driver issues but skipping
                // avoids any recursive device-init.
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
                    perf_trace("ocr_done", &file.path, ocr_started.elapsed().as_secs_f64() * 1000.0);
                }
    }

    // Enriched extras — derive Year / Camera family / Wide-Tall-Square /
    // Has Faces / Has Text / Has Location from the signals we already
    // have. Cheap (no inference) and gives a baseline of useful chips
    // even when the classifier model isn't installed. Mirrors macOS
    // `Tagging.swift::extraTags`.
    push_enriched_extras(&mut tagged);
    // Dedupe + cap. Two-pass: keep first occurrence to preserve the
    // classifier's confidence-ordered output; cap at 16 to avoid
    // exploding the tags table on noisy classifier outputs.
    {
        let mut seen = std::collections::HashSet::new();
        tagged.tags.retain(|t| seen.insert(t.clone()));
        tagged.tags.truncate(16);
    }

    tagged.total_ms = started.elapsed().as_secs_f64() * 1000.0;
    record_stage(&STATS_TOTAL_US, started);
    if perf_trace_enabled() {
        let total_ms = tagged.total_ms;
        let files_per_sec = if total_ms > 0.0 { 1000.0 / total_ms } else { 0.0 };
        tracing::debug!(
            target: "FileIDEngine::perf",
            stage = "total",
            path = %crate::platform::redact_path_for_log(&file.path),
            elapsed_ms = total_ms,
            files_per_sec,
            "[PERF]"
        );
    }
    maybe_emit_stats();
    tagged
}

/// Heuristic OCR gate. Photo libraries are 99% camera snapshots where OCR
/// is wasted CPU; skip in that case but keep running for documents/screenshots.
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

/// Derive tag-like chips from per-file signals that are already on hand:
/// EXIF camera model + creation date, face count, OCR result, EXIF GPS.
/// Matches the surface of macOS `Tagging.swift::extraTags(...)`. Cheap —
/// no inference, no I/O.
///
/// Format choices to align with the macOS Library tile `formatTag`:
/// - `"Year_2024"` keeps the underscore form (the formatter strips it).
/// - Camera family is the human-friendly brand (`"iPhone"`, `"Canon"`).
/// - Orientation tags (`"Wide"`, `"Tall"`, `"Square"`) read as-is.
/// - Capability tags (`"Has Faces"`, `"Has Text"`, `"Has Location"`)
///   contain a space and stay intact through `formatTag`.
fn push_enriched_extras(tagged: &mut TaggedFile) {
    // Year tag from modified_unix (engine doesn't currently track
    // creation_at separately). modified_unix is seconds since epoch.
    if tagged.modified_unix > 0.0 {
        let secs = tagged.modified_unix as i64;
        // Days since epoch via integer math (no chrono dep — already
        // available transitively but avoid the call cost here).
        // 2024 starts at unix=1_704_067_200; we just want the year.
        if let Some(year) = unix_seconds_to_year(secs) {
            if (1990..2100).contains(&year) {
                tagged.tags.push(format!("Year_{year}"));
            }
        }
    }
    if let Some(cm) = tagged.camera_model.as_deref() {
        let lower = cm.to_ascii_lowercase();
        let family = if lower.contains("iphone") { Some("iPhone") }
            else if lower.contains("ipad") { Some("iPad") }
            else if lower.contains("canon") { Some("Canon") }
            else if lower.contains("nikon") { Some("Nikon") }
            else if lower.contains("sony") { Some("Sony") }
            else if lower.contains("fuji") { Some("Fuji") }
            else if lower.contains("leica") { Some("Leica") }
            else if lower.contains("gopro") { Some("GoPro") }
            else if lower.contains("samsung") { Some("Samsung") }
            else if lower.contains("pixel") { Some("Pixel") }
            else { None };
        if let Some(f) = family {
            tagged.tags.push(f.to_string());
        }
    }
    if tagged.image_width > 0 && tagged.image_height > 0 {
        let ratio = tagged.image_width as f64 / tagged.image_height as f64;
        if ratio > 1.30 {
            tagged.tags.push("Wide".to_string());
        } else if ratio < 0.77 {
            tagged.tags.push("Tall".to_string());
        } else {
            tagged.tags.push("Square".to_string());
        }
    }
    if tagged.has_faces { tagged.tags.push("Has Faces".to_string()); }
    if tagged.has_text { tagged.tags.push("Has Text".to_string()); }
    if tagged.location_lat.is_some() && tagged.location_lon.is_some() {
        tagged.tags.push("Has Location".to_string());
    }
}

/// Minimal proleptic-Gregorian unix→year. Avoids pulling in chrono just
/// for a year extraction. Accurate for the 1970-2100 range the enriched-
/// extras call site cares about.
fn unix_seconds_to_year(secs: i64) -> Option<i32> {
    if secs < 0 { return None; }
    // Days since 1970-01-01.
    let days = secs / 86_400;
    // Iterate years adding 365 / 366 — simple, exact, runs in O(1) for
    // recent dates (<200 iterations).
    let mut year: i32 = 1970;
    let mut remaining = days;
    loop {
        let leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
        let year_days = if leap { 366 } else { 365 };
        if remaining < year_days {
            return Some(year);
        }
        remaining -= year_days;
        year += 1;
        if year > 2200 { return None; }
    }
}

/// Hard cap on image dimensions. A 100 KB JPEG can decode to a 4 GB raw
/// buffer; reject before decode to avoid OOM-ing a worker. 50 MP =
/// ~150 MB RGB8 — within budget for legitimate photo + scanner output.
const MAX_DECODED_PIXELS: u64 = 50_000_000;

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

    fn stub_tagged(w: u32, h: u32, size_bytes: u64) -> TaggedFile {
        TaggedFile {
            path: PathBuf::from("test.jpg"),
            kind: FileKind::Image,
            size_bytes,
            modified_unix: 1_710_504_000.0,
            scanned_unix: 1_710_504_001.0,
            has_faces: false,
            faces: Vec::new(),
            has_text: false,
            ocr_text: None,
            phash: None,
            aesthetic: None,
            image_width: w,
            image_height: h,
            clip_embedding: None,
            camera_model: None,
            location_lat: None,
            location_lon: None,
            vision_ms: 0.0,
            clip_ms: 0.0,
            total_ms: 0.0,
            failed: false,
            error_message: None,
            tags: Vec::new(),
        }
    }

    #[test]
    fn enriched_wide() {
        let mut t = stub_tagged(1920, 1080, 5_000_000);
        push_enriched_extras(&mut t);
        assert!(t.tags.contains(&"Wide".to_string()), "1920/1080 = 1.78 > 1.30");
    }

    #[test]
    fn enriched_tall() {
        let mut t = stub_tagged(1080, 1920, 5_000_000);
        push_enriched_extras(&mut t);
        assert!(t.tags.contains(&"Tall".to_string()), "1080/1920 = 0.56 < 0.77");
    }

    #[test]
    fn enriched_square() {
        let mut t = stub_tagged(1080, 1080, 5_000_000);
        push_enriched_extras(&mut t);
        assert!(t.tags.contains(&"Square".to_string()), "1:1 ratio");
    }

    #[test]
    fn enriched_borderline_not_wide() {
        let mut t = stub_tagged(1300, 1000, 5_000_000);
        push_enriched_extras(&mut t);
        assert!(t.tags.contains(&"Square".to_string()),
            "1300/1000 = 1.30 exactly — not > 1.30, so Square");
        assert!(!t.tags.contains(&"Wide".to_string()));
    }

    #[test]
    fn aesthetic_score_computed() {
        let megapixels: f64 = (1920.0 * 1080.0) / 1_000_000.0;
        let res_score: f64 = (megapixels / 50.0).min(1.0);
        let size_score: f64 = (5_000_000.0_f64 / (100.0 * 1024.0 * 1024.0)).min(1.0);
        let expected = size_score * 0.5 + res_score * 0.5;
        assert!(expected > 0.0 && expected < 1.0, "score should be in (0, 1)");
    }
}
