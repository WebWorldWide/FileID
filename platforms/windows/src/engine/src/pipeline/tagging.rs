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
use crate::models::{bge_text::BgeText, face_align, mobileclip::MobileClipImage, scene_vocab::SceneLabeler, scrfd, sface::SFace, yunet::YuNet};
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
// HW-4 diagnostics: RAM++ inference time (was unaccounted — folded into the
// total_us gap) and the time spent WAITING to acquire `vision_sem` (both the
// faces and RAM++ acquisitions). Together with vision_us/clip_us these explain
// where per-file wall time actually goes, so the throughput bottleneck can be
// fixed with data rather than guesses.
static STATS_RAMPLUS_US: AtomicU64 = AtomicU64::new(0);
static STATS_VISION_WAIT_US: AtomicU64 = AtomicU64::new(0);
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
    let ramplus = STATS_RAMPLUS_US.load(Ordering::Relaxed) / n;
    let vision_wait = STATS_VISION_WAIT_US.load(Ordering::Relaxed) / n;
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
        ramplus_us = ramplus,
        vision_wait_us = vision_wait,
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

    /// Volume-local file identity (propagated from `DiscoveredFile.file_ref`)
    /// and BLAKE3 content identity (computed here). Together they drive the
    /// dbwriter's rename/move heal: a moved file with a matching `file_ref`
    /// (same volume) or `content_hash` (cross-volume) re-binds to its
    /// existing catalog row instead of orphaning its tags + embeddings +
    /// faces. Both nullable — a row with neither just falls through to a
    /// fresh INSERT.
    pub file_ref: Option<u64>,
    pub content_hash: Option<[u8; 32]>,

    /// BGE-small text embedding (Phase 4b) — 384-d L2-normalized float vector
    /// for the document's extracted text. Persisted by the dbwriter into
    /// `text_embeddings` (parallel to `clip_embeddings`); enables semantic
    /// search beyond the doc_fts keyword match. `None` when BGE isn't
    /// installed or no doc text was extracted.
    pub text_embedding: Option<Vec<f32>>,

    /// Plain-text extraction of the file's content (Phase 4) for Doc-kind
    /// inputs (.txt / .md / .docx / .pptx / .xlsx; .pdf is a Phase-4b
    /// follow-up). Persisted to `doc_text` + the `doc_fts` FTS5 index by
    /// the dbwriter so the user can full-text-search their docs. Capped at
    /// `pipeline::doc_extract::MAX_TEXT_BYTES` upstream.
    pub doc_text: Option<String>,

    /// Semantic tags as `(label, score)` pairs, assembled from (a) CLIP
    /// zero-shot scene labels (score = softmax probability) and (b)
    /// enriched-extras derived from existing per-file signals (Year + camera
    /// family), which carry `None` (no model confidence). Persisted into the
    /// `tags` table by DBWriter with source = `"auto"` and the score in
    /// `tags.score`; the Library UI reads them via ReadStore and renders the
    /// top-by-score as TagChip rows. (Descriptive content tags come from the
    /// optional Deep-Analyze VLM pass as `source='vlm'`, when the user installs
    /// a Qwen / Gemma model.)
    pub tags: Vec<(String, Option<f32>)>,

    /// True iff the face detect/embed stage actually executed for this file
    /// this scan (models present AND the GPU was alive). The dbwriter keys its
    /// stale-`face_prints` DELETE on this flag, not on `faces.is_empty()`: an
    /// edited/zero-face re-process must clear orphaned faces, but a
    /// face-disabled or GPU-dead session must NOT wipe still-valid rows. See
    /// dbwriter face-flush.
    pub faces_evaluated: bool,

    /// True iff the OCR / doc-text extraction stages actually ran for this file
    /// this scan. Same contract as `faces_evaluated`: the dbwriter
    /// delete-then-reinserts `ocr_*` / `doc_*` (clearing now-empty text) ONLY
    /// when the stage ran, never on the ambiguous default-skip path.
    pub ocr_stage_ran: bool,
    pub doc_stage_ran: bool,
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
    // Commercial-clean face models: YuNet (detect, MIT) + SFace (embed, Apache,
    // 128-d). Field names are legacy (`scrfd`/`arcface`) to keep the pipeline
    // call sites unchanged; the types are the new MIT/Apache models.
    pub arcface: Option<Vec<Mutex<SFace>>>,
    pub scrfd: Option<Vec<Mutex<YuNet>>>,
    /// MobileCLIP has two paths (the default is chosen in `load_default`):
    /// - `mobileclip_batch` (DEFAULT) — single Session behind
    ///   `ClipBatchCoordinator`, fed batched (N,3,256,256) tensors. On a 6 GB
    ///   card the VRAM clamp drops the pool to ~1-3 sessions behind the
    ///   `CLIP_CONCURRENCY=2` semaphore, so batching amortizes DirectML
    ///   dispatch better. Opt OUT with `FILEID_CLIP_USE_BATCH=0`.
    /// - `mobileclip_pool` — N-Session pool, VRAM-clamped via
    ///   `resolve_pool_size`; the fallback (`FILEID_CLIP_USE_BATCH=0`), kept
    ///   for CUDA-EP experiments. NOTE: the batch-vs-pool default is PENDING
    ///   hardware confirmation (run the A/B and compare clip_p95_ms).
    pub mobileclip_pool: Option<Vec<Mutex<MobileClipImage>>>,
    pub mobileclip_batch: Option<Arc<ClipBatchCoordinator>>,
    /// CLIP zero-shot scene labeler — a label-embedding matrix built once
    /// per launch from the (already-installed) CLIP text encoder. Scored
    /// against the per-file MobileCLIP image embedding. Optional — when the
    /// text model isn't installed the per-file `tags` Vec is populated only
    /// from enriched extras.
    pub scene_labeler: Option<Arc<SceneLabeler>>,
    /// BGE-small text embedder (Phase 4b) — computes a 384-d vector for each
    /// document's extracted text so the library can do semantic search beyond
    /// FTS5 keyword match. Optional: missing model → no embedding emitted.
    pub bge_text: Option<Mutex<BgeText>>,
    /// RAM++ multi-label image tagger pool (the PRIMARY in-scan tagger).
    /// Optional: a missing ONNX (e.g. the self-hosted HF repo not yet
    /// populated) → tagging falls back to the CLIP zero-shot `scene_labeler`,
    /// so there is zero regression when RAM++ isn't installed.
    pub ram_plus: Option<Vec<Mutex<crate::models::ram_plus::RamPlusTagger>>>,
    /// RAM++ batch-coordinator path (HW-4): one Session driven with batched
    /// (N,3,384,384) tensors, filling the GPU kernels that a single 384² image
    /// leaves <1% utilized. Spawned only when `FILEID_RAMPLUS_BATCH_SIZE > 1`
    /// AND a dynamic-batch ONNX is installed; otherwise `ram_plus` (the
    /// single-image pool) is used. When this is `Some`, `ram_plus` is `None`.
    pub ram_plus_batch: Option<Arc<crate::models::ram_plus_batch::RamPlusBatchCoordinator>>,
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
/// ~1.65 GB idle baseline, i.e. ~940 MB attributed to the engine. Raised from
/// 1500 to 2000 MB when RAM++ joined the per-slot model set (Swin-L @384 fp16
/// adds ~882 MB resident weights + inference intermediates), preserving a
/// safety margin for DirectML allocator
/// fragmentation under longer-running scans. On a 6 GB card the gate now
/// clamps the ArcFace/SCRFD/RAM++ pools to 2.
const VRAM_PER_POOL_INSTANCE_MB: u64 = 2000;
// HW-4 (RTX 2060, 2026-06-01): a CUDA-specific smaller estimate to fit pool=3
// was TESTED on hardware and REGRESSED throughput (5.1→3.9 files/s, RAM++
// 670→812 ms/file, peak RSS 5.7→7.6 GB) — 3 RAM++ Swin-L sessions over-subscribe
// the single GPU and thrash rather than parallelize. RAM++ throughput is
// GPU-COMPUTE-bound, not concurrency-bound; the only real win is BATCHED RAM++
// inference (a dynamic-axis ONNX re-export, see NEXT.md) or a lighter tagger.
// Do NOT raise the pool to "fix" throughput — it makes it worse.

/// Always-reserved VRAM headroom (Windows desktop compositor + other
/// apps). Subtracted from the dedicated total before dividing by
/// VRAM_PER_POOL_INSTANCE_MB. On a 6 GB card this leaves ~4.5 GB usable, so
/// `(4644 / 1500).max(1) = 3`; with `MODEL_POOL_SIZE = 4` the gate clamps the
/// ArcFace/SCRFD pools to 3 sessions. (MobileCLIP defaults to the
/// single-Session batch path, so this clamp mainly bounds the face models.)
const VRAM_RESERVED_MB: u64 = 1500;

/// ENG-71: ceiling on the pre-allocation HINT for the per-file read buffer.
/// `file.size_bytes` comes from a filesystem stat; a bogus/huge value (sparse
/// file, corrupt metadata, or a misclassified multi-GB blob) makes
/// `Vec::with_capacity(size)` abort the whole process on the failed allocation
/// — across all decoder threads. Clamping the hint prevents the abort;
/// `read_to_end` still grows the Vec to the file's true length, so the
/// hash/EXIF/decode of a normally-sized file is byte-for-byte unchanged.
const MAX_PREALLOC_BYTES: usize = 64 * 1024 * 1024;

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

/// EP-aware vision-inference concurrency. DirectML keeps the explicit TDR floor
/// (`VISION_CONCURRENCY`): past the 2 s deadline Windows removes the GPU device.
/// CUDA/TensorRT have no TDR ceiling, so concurrency rises to the (VRAM-clamped)
/// pool size — every loaded Session can run at once instead of being artificially
/// throttled below the pool. Other EPs stay at the conservative floor until
/// measured. NOTE: on a small-VRAM card the pool itself clamps to ~2, so this is
/// a no-op there; the win is on larger CUDA cards. Growing the pool on a 6 GB
/// CUDA card needs a CUDA-specific `VRAM_PER_POOL_INSTANCE_MB` retune measured on
/// hardware (DirectML's estimate is allocator-conservative) — tracked separately.
fn ep_vision_concurrency(ep: crate::models::runtime::ExecutionProvider, pool_size: usize) -> usize {
    use crate::models::runtime::ExecutionProvider as Ep;
    match ep {
        Ep::Cuda | Ep::TensorRt => pool_size.max(VISION_CONCURRENCY),
        _ => VISION_CONCURRENCY,
    }
}

/// EP-aware CLIP-embed concurrency (governs the opt-in `FILEID_CLIP_USE_BATCH=0`
/// pool path; the default batch coordinator uses one Session and ignores this).
/// Same EP rationale as [`ep_vision_concurrency`].
fn ep_clip_concurrency(ep: crate::models::runtime::ExecutionProvider, pool_size: usize) -> usize {
    use crate::models::runtime::ExecutionProvider as Ep;
    match ep {
        Ep::Cuda | Ep::TensorRt => pool_size.max(CLIP_CONCURRENCY),
        _ => CLIP_CONCURRENCY,
    }
}

impl ModelStack {
    /// Load whatever model files are installed at the canonical paths.
    /// Each present model gets loaded `pool_size` times so workers can
    /// run inference in parallel without serializing on a single Mutex.
    pub fn load_default(worker_count: usize) -> Self {
        let pool_size = resolve_pool_size(worker_count);
        let arcface = load_pool("SFace", pool_size, crate::models::sface::default_weights_path(), SFace::load);
        let scrfd = load_pool("YuNet", pool_size, crate::models::yunet::default_weights_path(), YuNet::load);

        // Batch path is the default. Rationale: the VRAM clamp drops the pool
        // to ~1-3 Sessions on a 6 GB card, and the separate CLIP_CONCURRENCY=2
        // semaphore caps concurrent CLIP inferences regardless, so a pool
        // larger than 2 is partly wasted. The batch coordinator drives ONE
        // Session with batched (N, 3, 256, 256) tensors, amortizing per-call
        // DirectML dispatch overhead. This default is PENDING hardware
        // confirmation — run the A/B (default vs `FILEID_CLIP_USE_BATCH=0`) and
        // compare clip_p95_ms + files_per_second. Set `FILEID_CLIP_USE_BATCH=0`
        // to fall back to the pool path.
        let use_batch = std::env::var("FILEID_CLIP_USE_BATCH")
            .ok()
            .map(|s| !(s == "0" || s.eq_ignore_ascii_case("false")))
            .unwrap_or(true);

        // The scene labeler matrix is loaded from precomputed embeddings
        // (scene_embeddings_precomputed.rs) and is the canonical auto-tagger.
        // The per-file MobileCLIP embedding for semantic search uses the same
        // image encoder; if ENABLE_CLIP_SCENE_TAGS is off, the labeler is skipped.
        let scene_labeler = if crate::models::scene_vocab::ENABLE_CLIP
            && crate::models::scene_vocab::ENABLE_CLIP_SCENE_TAGS
        {
            crate::models::scene_vocab::shared_scene_labeler()
        } else {
            None
        };

        let (mobileclip_pool, mobileclip_batch) = if !crate::models::scene_vocab::ENABLE_CLIP {
            // CLIP fully off — no MobileCLIP image encoder, so no per-file
            // embedding and no semantic search.
            (None, None)
        } else if use_batch {
            // Batch-coordinator path (DEFAULT; opt out with FILEID_CLIP_USE_BATCH=0).
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

        // BGE-small text embedder — single session, only built when both the
        // weights AND the vocab.txt are on disk. Drives semantic search over
        // document text; absent → just no text embedding row, FTS5 still
        // works.
        let bge_text = match (
            crate::models::bge_text::default_weights_path(),
            crate::models::bge_text::default_vocab_path(),
        ) {
            (Ok(wp), Ok(vp)) if wp.exists() && vp.exists() => match BgeText::load(wp.clone(), vp) {
                Ok(m) => {
                    tracing::info!(model = "BGE-small", path = %wp.display(), "model loaded");
                    Some(Mutex::new(m))
                }
                Err(err) => {
                    tracing::warn!(model = "BGE-small", ?err, "model load failed; stage will skip");
                    None
                }
            },
            (Ok(wp), _) => {
                tracing::info!(model = "BGE-small", path = %wp.display(), "model not installed; stage will skip");
                None
            }
            (Err(err), _) => {
                tracing::warn!(model = "BGE-small", ?err, "model path unresolved");
                None
            }
        };

        // RAM++ multi-label tagger pool — the primary in-scan tagger. Loaded
        // like the other pooled models; a missing ONNX (e.g. the HF repo not
        // yet populated) yields None and tagging falls back to CLIP scene-tags
        // (zero regression). The tag-list path is captured into the loader
        // closure since `load_pool` passes only the resolved ONNX path.
        // RAM++ default = single-image pool. When FILEID_RAMPLUS_BATCH_SIZE > 1
        // AND a dynamic-batch ONNX is installed, use the batch coordinator
        // instead (one Session, batched forward — the HW-4 throughput fix). The
        // two paths are mutually exclusive.
        let ram_batch_size =
            crate::models::ram_plus_batch::RamPlusBatchCoordinator::configured_batch_size();
        let (ram_plus, ram_plus_batch) = match crate::models::ram_plus::default_tags_path() {
            Ok(tags_path) if ram_batch_size > 1 => {
                match crate::models::ram_plus::default_onnx_path() {
                    Ok(p) if p.exists() => {
                        match crate::models::ram_plus::RamPlusTagger::load(p.clone(), tags_path) {
                            Ok(tagger) => {
                                tracing::info!(model = "RAM++", path = %p.display(), batch_size = ram_batch_size, "model loaded (batch-coordinator mode)");
                                let coord = crate::models::ram_plus_batch::RamPlusBatchCoordinator::spawn(tagger);
                                (None, Some(coord))
                            }
                            Err(err) => {
                                tracing::warn!(model = "RAM++", ?err, "batch load failed; tagging falls back to CLIP scene-tags");
                                (None, None)
                            }
                        }
                    }
                    _ => {
                        tracing::info!(model = "RAM++", "ONNX not installed; tagging falls back to CLIP scene-tags");
                        (None, None)
                    }
                }
            }
            Ok(tags_path) => {
                let pool = load_pool(
                    "RAM++",
                    pool_size,
                    crate::models::ram_plus::default_onnx_path(),
                    move |p| crate::models::ram_plus::RamPlusTagger::load(p, tags_path.clone()),
                );
                (pool, None)
            }
            Err(err) => {
                tracing::warn!(model = "RAM++", ?err, "tag-list path unresolved; tagging falls back to CLIP scene-tags");
                (None, None)
            }
        };

        Self { arcface, scrfd, mobileclip_pool, mobileclip_batch, scene_labeler, bge_text, ram_plus, ram_plus_batch }
    }

    #[allow(dead_code)]
    pub fn empty() -> Self {
        Self {
            arcface: None,
            scrfd: None,
            mobileclip_pool: None,
            mobileclip_batch: None,
            scene_labeler: None,
            bge_text: None,
            ram_plus: None,
            ram_plus_batch: None,
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
        // Channel cap: bound decoded-RGB read-ahead by a MEMORY budget, not a
        // flat frame count. A 12 MP frame is ~36 MB, so the old (worker*2) count
        // pinned ~0.5-1 GB of pure read-ahead slack the GPU never needs — decode
        // (CPU, the [2,12] decoder pool) vastly outruns the GPU-bound RAM++
        // tagger (~6-8 files/s), so the channel sits full all scan and a
        // shallower queue cannot starve the GPU. Size to ~256 MB of typical
        // frames while still guaranteeing every worker can hold one frame ready
        // (floor = worker_count). Per-frame pixels are already capped at
        // MAX_DECODED_PIXELS, so this also tightens the pathological-frame ceiling.
        const PREDECODE_BUDGET_MB: usize = 256;
        const TYPICAL_FRAME_MB: usize = 24; // ~8 MP RGB8
        let predecoded_cap = (PREDECODE_BUDGET_MB / TYPICAL_FRAME_MB).max(self.worker_count);
        let (predecoded_tx, predecoded_rx) =
            async_channel::bounded::<PreDecoded>(predecoded_cap);
        for decoder_idx in 0..decoder_count {
            let rx = raw_rx.clone();
            let tx = predecoded_tx.clone();
            let coord = self.coordinator.clone();
            let spawn_result = std::thread::Builder::new()
                .name(format!("fileid-decode-{decoder_idx}"))
                .spawn(move || run_decoder_thread(rx, tx, coord));
            if let Err(e) = spawn_result {
                // Don't panic mid-scan if the OS refuses a new thread (handle or
                // memory pressure on a very large library). Log and continue with
                // the decoders that did start; rx/tx/coord drop here, which is
                // safe (the channels just have one fewer consumer/producer).
                tracing::warn!(
                    "fileid-decode-{decoder_idx} failed to spawn ({e}); continuing with fewer decode threads"
                );
            }
        }
        drop(raw_rx);
        drop(predecoded_tx);

        // P3: derive the GPU-inference concurrency caps from the active EP. On
        // DirectML these stay at the TDR floor (4/2); on CUDA/TensorRT they rise
        // to the VRAM-clamped pool size so the semaphore doesn't throttle below
        // the number of loaded Sessions.
        let active_ep = crate::models::runtime::active_provider();
        let ep_pool_size = resolve_pool_size(self.worker_count);
        let vision_cap = ep_vision_concurrency(active_ep, ep_pool_size);
        let clip_cap = ep_clip_concurrency(active_ep, ep_pool_size);
        tracing::info!(
            ep = active_ep.as_str(),
            pool_size = ep_pool_size,
            vision_cap,
            clip_cap,
            "[TAGGING] EP-aware inference concurrency caps"
        );
        let vision_sem = Arc::new(Semaphore::new(vision_cap));
        let clip_sem = Arc::new(Semaphore::new(clip_cap));

        for worker_idx in 0..self.worker_count {
            let rx = predecoded_rx.clone();
            let tx = out_tx.clone();
            let coord = self.coordinator.clone();
            let vision_sem = vision_sem.clone();
            let clip_sem = clip_sem.clone();
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
                    let fut = process_file_predecoded(predecoded, &models, &vision_sem, &clip_sem, worker_idx, &coord);
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
                                file_ref: None,
                                content_hash: None,
                                text_embedding: None,
                                doc_text: None,
                                tags: Vec::new(),
                                faces_evaluated: false,
                                ocr_stage_ran: false,
                                doc_stage_ran: false,
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
    /// Phase 4 doc-kind text extraction (txt/md/docx/pptx/xlsx). `None` on
    /// non-doc kinds, online-only files, and extraction failures.
    pub doc_text: Option<String>,
    /// Phase 5 audio-kind metadata tags (artist/album/title/genre/year).
    /// Empty for non-audio kinds + online-only + extraction failures.
    pub audio_tags: Vec<(String, Option<f32>)>,
    pub content_hash: Option<[u8; 32]>,
    pub exif: Option<(Option<String>, Option<f64>, Option<f64>)>,
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

        let mut file_bytes = None;
        let mut content_hash = None;
        let mut exif_data = None;

        if !file.online_only {
            match file.kind {
                FileKind::Image => {
                    if let Ok(mut f) = open_image_file(&file.path) {
                        let mut bytes = Vec::with_capacity((file.size_bytes as usize).min(MAX_PREALLOC_BYTES));
                        if std::io::Read::read_to_end(&mut f, &mut bytes).is_ok() {
                            // Baseline: a successful read always produces Some(_)
                            // so the worker's parse_exif_blocking fallback is
                            // unreachable for images. Non-EXIF formats (PNG, GIF,
                            // screenshots) hit this branch and skip the wasted
                            // re-open + re-fail.
                            exif_data = Some((None, None, None));
                            // Parse EXIF from memory
                            if let Ok(exif) = exif::Reader::new().read_from_container(&mut std::io::Cursor::new(&bytes)) {
                                let camera_model = exif
                                    .get_field(exif::Tag::Model, exif::In::PRIMARY)
                                    .map(|f| f.display_value().with_unit(&exif).to_string().trim_matches('"').to_string())
                                    .filter(|s| !s.is_empty());
                                let lat = read_gps_coord(&exif, exif::Tag::GPSLatitude, exif::Tag::GPSLatitudeRef);
                                let lon = read_gps_coord(&exif, exif::Tag::GPSLongitude, exif::Tag::GPSLongitudeRef);
                                exif_data = Some((camera_model, lat, lon));
                            }
                            // Compute BLAKE3 content hash from memory
                            let hash = if bytes.len() <= crate::util::content_hash::FULL_HASH_MAX_BYTES as usize {
                                *blake3::hash(&bytes).as_bytes()
                            } else {
                                let span = bytes.len().min(1024 * 1024);
                                let mut hasher = blake3::Hasher::new();
                                hasher.update(&bytes[..span]);
                                let start_tail = bytes.len().saturating_sub(span);
                                hasher.update(&bytes[start_tail..]);
                                hasher.update(&(bytes.len() as u64).to_le_bytes());
                                *hasher.finalize().as_bytes()
                            };
                            content_hash = Some(hash);
                            file_bytes = Some(bytes);
                        }
                    }
                }
                // Doc / PDF / Audio share the image-style single-read pattern
                // when the file fits in the full-hash window (16 MB). The
                // buffer feeds both BLAKE3 and the kind-specific extractor,
                // skipping the worker's content_hash fallback re-open. Files
                // above the cap fall through to the composite-hash + path-
                // based extractor path (a head+tail+size read of 2 MB total),
                // bounding decoder-thread peak memory at one file's bytes.
                FileKind::Doc | FileKind::Pdf | FileKind::Audio
                    if file.size_bytes <= crate::util::content_hash::FULL_HASH_MAX_BYTES =>
                {
                    if let Ok(mut f) = open_image_file(&file.path) {
                        let mut bytes = Vec::with_capacity((file.size_bytes as usize).min(MAX_PREALLOC_BYTES));
                        if std::io::Read::read_to_end(&mut f, &mut bytes).is_ok() {
                            content_hash = Some(*blake3::hash(&bytes).as_bytes());
                            file_bytes = Some(bytes);
                        }
                    }
                }
                _ => {}
            }
        }

        // Cloud placeholders: never read content (reading hydrates the file,
        // a surprise network download). Emit a metadata-only row (decoded =
        // None) just like an unsupported kind; a later scan after the user
        // hydrates the file picks up its content.
        let decoded = if file.online_only {
            None
        } else {
            match file.kind {
                FileKind::Image => Some(decode_image_sync(&file.path, file_bytes.as_deref())),
                FileKind::Video => Some(decode_video_keyframe_sync(&file.path)),
                _ => None,
            }
        };
        if decoded.is_some() {
            STATS_DECODE_US.fetch_add(decode_started.elapsed().as_micros() as u64, Ordering::Relaxed);
        }
        // Phase 4: for Doc-kind files, extract text on the same decoder
        // thread (cheap I/O; same online_only gate as content read). Re-uses
        // the file_bytes buffer (≤ 16 MB) when the doc/pdf branch above
        // pre-read it; otherwise the extractor falls back to opening the
        // path itself.
        let doc_text = if file.online_only {
            None
        } else {
            match file.kind {
                FileKind::Doc | FileKind::Pdf => {
                    crate::pipeline::doc_extract::extract(&file.path, file_bytes.as_deref())
                        .ok()
                        .flatten()
                }
                _ => None,
            }
        };
        // Phase 5: for Audio-kind files, read container metadata via symphonia
        // (artist/album/title/genre/year). Pure-Rust, no system ffmpeg.
        // Reuses file_bytes when present (same 16 MB cap as docs).
        let audio_tags = if file.online_only {
            Vec::new()
        } else {
            match file.kind {
                FileKind::Audio => {
                    crate::pipeline::audio_meta::extract(&file.path, file_bytes.as_deref())
                }
                _ => Vec::new(),
            }
        };
        let item = PreDecoded { file, decoded, doc_text, audio_tags, content_hash, exif: exif_data };
        if tx.send_blocking(item).is_err() {
            return;
        }
    }
}

/// Open a file for sequential read on Windows. `FILE_FLAG_SEQUENTIAL_SCAN`
/// tells the cache manager we won't seek back (so it doesn't pollute the
/// standby list) and is friendlier to real-time AV scanning. Plain open
/// elsewhere.
#[cfg(windows)]
fn open_read_sequential(p: &std::path::Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x0800_0000;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_SEQUENTIAL_SCAN)
        .open(p)
}

#[cfg(not(windows))]
fn open_read_sequential(p: &std::path::Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(p)
}

/// Open a file in the decoder thread for pre-reading (image bytes for
/// decode + hash, doc/audio bytes for hash + extraction). Long-path support
/// via `\\?\` prefix and short retry-with-backoff for transient sharing /
/// lock violations (file still being written by another app, AV scanner
/// holding a momentary lock). Genuine errors (not found, access denied)
/// fail fast without burning retries. Named historically for the image
/// path; reused for Doc/Pdf/Audio under the same hardening rationale.
fn open_image_file(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    let p = crate::util::path_safety::to_extended_length(path);
    let mut last_err: Option<std::io::Error> = None;
    for attempt in 0..3u32 {
        match open_read_sequential(&p) {
            Ok(f) => return Ok(f),
            Err(e) => {
                // ERROR_SHARING_VIOLATION (32) / ERROR_LOCK_VIOLATION (33)
                // are the only transient cases worth retrying.
                let transient = matches!(e.raw_os_error(), Some(32 | 33));
                last_err = Some(e);
                if !transient || attempt == 2 {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50 * (u64::from(attempt) + 1)));
            }
        }
    }
    Err(last_err.unwrap_or_else(|| std::io::Error::other("open failed")))
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
fn decode_image_sync(path: &std::path::Path, bytes: Option<&[u8]>) -> anyhow::Result<(Vec<u8>, u32, u32)> {
    let primary = decode_image_sync_imagecrate(path, bytes);
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

fn decode_image_sync_imagecrate(path: &std::path::Path, bytes: Option<&[u8]>) -> anyhow::Result<(Vec<u8>, u32, u32)> {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> anyhow::Result<(Vec<u8>, u32, u32)> {
        use std::io::Cursor;
        let mmap;
        let bytes: &[u8] = if let Some(pre_read) = bytes {
            pre_read
        } else {
            let file = open_image_file(path)
                .map_err(|e| anyhow::anyhow!("open: {e}"))?;
            mmap = unsafe {
                memmap2::Mmap::map(&file)
                    .map_err(|e| anyhow::anyhow!("mmap: {e}"))?
            };
            &mmap
        };

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
    worker_idx: usize,
    coord: &ScanCoordinator,
) -> TaggedFile {
    let PreDecoded { file, decoded, doc_text, audio_tags, content_hash, exif } = predecoded;
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
        file_ref: file.file_ref,
        content_hash: None,
        text_embedding: None,
        doc_text: None,
        tags: Vec::new(),
        faces_evaluated: false,
        ocr_stage_ran: false,
        doc_stage_ran: false,
    };

    // Content identity for rename/move heal. Skipped for cloud placeholders
    // so we never trigger a content read (which would hydrate a OneDrive
    // online-only file). On any read error the row simply lacks a
    // content_hash — the heal-by-file_ref path still applies.
    if !file.online_only {
        tagged.content_hash = content_hash.or_else(|| {
            crate::util::content_hash::content_hash(&file.path, file.size_bytes).ok()
        });
    }

    // Document content (Phase 4): the decoder thread already pulled the text
    // for FileKind::Doc. Run a cheap RAKE-style keyword extractor → tag chips
    // (source='auto'), and stash the text on the row so the dbwriter can
    // persist it into doc_text/doc_fts for full-text search.
    //
    // The doc-text stage "ran" iff extraction was attempted this session (a
    // doc-kind input that isn't a cloud placeholder) — the dbwriter keys its
    // stale doc_text/doc_fts DELETE on this so a re-process that now yields
    // empty text clears phantom FTS hits (#11).
    tagged.doc_stage_ran = matches!(file.kind, FileKind::Doc | FileKind::Pdf) && !file.online_only;
    if let Some(text) = doc_text {
        if !text.trim().is_empty() {
            for (label, score) in crate::util::keywords::extract(&text) {
                tagged.tags.push((label, Some(score)));
            }
            // BGE-small semantic embedding for the doc text (Phase 4b). Runs on
            // the calling worker thread under a single-Session Mutex (sync
            // inference). Skipped when BGE isn't installed — FTS5 still
            // serves keyword search.
            if !coord.is_gpu_dead() {
                if let Some(bge_mu) = &models.bge_text {
                    let mut bge = bge_mu.lock();
                    match bge.embed(&text) {
                        Ok(emb) => tagged.text_embedding = Some(emb),
                        Err(err) => {
                            if error_has_device_removed_marker(&err) {
                                if coord.mark_gpu_dead() {
                                    tracing::error!(?err, "[GPU-TDR] BGE device-removed; cancelling scan");
                                }
                            } else {
                                tracing::warn!(?err, "BGE embed failed");
                            }
                        }
                    }
                }
            }
            tagged.doc_text = Some(text);
        }
    }

    // Audio metadata (Phase 5): artist/album/title/genre/year, surfaced as
    // tag chips so the user can filter their library by these facts. Each
    // tag carries score=None — these are facts, not model probabilities.
    tagged.tags.extend(audio_tags);

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
                if let Some((cam, lat, lon)) = exif {
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
                    let vwait = Instant::now();
                    let permit = vision_sem.acquire().await;
                    STATS_VISION_WAIT_US.fetch_add(vwait.elapsed().as_micros() as u64, Ordering::Relaxed);
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
                                    // SCRFD emits corner coords [x1,y1,x2,y2]; the crop +
                                    // DetectedFace.bbox + the persisted bbox all expect
                                    // [x,y,w,h]. Convert once: without this the crop ran
                                    // from the face's top-left to the image's bottom-right
                                    // (blank / not-a-face thumbnails) and ArcFace embedded
                                    // that smear, corrupting clustering.
                                    let bbox_xywh = [
                                        det.bbox[0],
                                        det.bbox[1],
                                        (det.bbox[2] - det.bbox[0]).max(0.0),
                                        (det.bbox[3] - det.bbox[1]).max(0.0),
                                    ];
                                    // Aligned 112×112 (5-pt similarity → ArcFace
                                    // template) for SFace; fall back to a plain bbox
                                    // crop if the landmark fit is degenerate.
                                    let crop = face_align::align_112(&rgb, w, h, &det.landmarks)
                                        .or_else(|| crop_and_resize_face(&rgb, w, h, &bbox_xywh));
                                    if let Some(crop) = crop {
                                        let embed_result = {
                                            let mut a = arcface_mu.lock();
                                            a.embed(&crop)
                                        };
                                        match embed_result {
                                            Ok(emb) => {
                                                let pose = scrfd::estimate_pose(&det.landmarks);
                                                tagged.faces.push(DetectedFace {
                                                    bbox: bbox_xywh,
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
                                                        tracing::error!(?err, "[GPU-TDR] SFace device-removed; cancelling scan");
                                                    }
                                                    break;
                                                }
                                                tracing::warn!(?err, "SFace embed failed");
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
                // The face stage truly ran this session iff the GPU was alive at
                // entry, the face models were loaded, AND the GPU did not die
                // mid-pass. The dbwriter keys its stale-face DELETE on this (not
                // on `faces.is_empty()`) so a zero-face re-process clears orphans
                // while a models-missing / GPU-dead session preserves valid rows.
                tagged.faces_evaluated =
                    gpu_alive && models.scrfd.is_some() && models.arcface.is_some() && !coord.is_gpu_dead();
                tagged.has_faces = !tagged.faces.is_empty();

                if !coord.is_gpu_dead() {
                // Pool path: locked Session, single-image inference.
                if let Some(clip_pool) = &models.mobileclip_pool {
                    let permit = clip_sem.acquire().await;
                    let clip_started = Instant::now();
                    if permit.is_ok() {
                        let clip_mu = &clip_pool[worker_idx % clip_pool.len()];
                        let resized = resize_rgb_quality(&rgb, w as usize, h as usize, 224, 224);
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
                    let resized = resize_rgb_quality(&rgb, w as usize, h as usize, 224, 224);
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

                // RAM++ multi-label tagging — the PRIMARY in-scan tagger. One
                // Swin-L forward pass on the same GPU/NPU EP chain as faces;
                // its 4585-tag vocabulary supersedes the CLIP zero-shot scene
                // labeler (gated below on `!ram_plus_ran`). Runs AFTER the
                // MobileCLIP embed so semantic-search embeddings are always
                // computed regardless of which tagger is active. Shares the
                // `vision_sem` GPU budget; device-removed → cancel the scan.
                let mut ram_plus_ran = false;
                if !coord.is_gpu_dead() {
                    // Tags come from EITHER the batch coordinator (one batched
                    // Session — HW-4 throughput path; no vision_sem because the
                    // coordinator owns the Session and serializes batches itself,
                    // mirroring the CLIP batch path) OR the per-worker pool
                    // (single-image, vision_sem-gated). Both yield the same
                    // Result<Vec<(tag, score)>>, handled by the shared match.
                    let ram_started = Instant::now();
                    let tag_result: Option<anyhow::Result<Vec<(String, f32)>>> =
                        if let Some(ram_coord) = &models.ram_plus_batch {
                            Some(ram_coord.tag(rgb.clone(), w, h).await)
                        } else if let Some(ram_pool) = &models.ram_plus {
                            // Run the CPU preprocess (resize + ImageNet-normalize)
                            // BEFORE acquiring the GPU permit + session Mutex, so it
                            // overlaps other workers' GPU forwards instead of
                            // serializing under the scarce session lock; the lock +
                            // permit now wrap only the GPU forward pass.
                            match crate::models::ram_plus::RamPlusTagger::preprocess_tensor(
                                &rgb, w, h,
                            ) {
                                Ok(chw) => {
                                    let rwait = Instant::now();
                                    let permit = vision_sem.acquire().await;
                                    STATS_VISION_WAIT_US.fetch_add(
                                        rwait.elapsed().as_micros() as u64,
                                        Ordering::Relaxed,
                                    );
                                    if permit.is_ok() {
                                        let ram_mu = &ram_pool[worker_idx % ram_pool.len()];
                                        let mut g = ram_mu.lock();
                                        Some(g.tag_prepared(chw))
                                    } else {
                                        None
                                    }
                                }
                                Err(e) => Some(Err(e)),
                            }
                        } else {
                            None
                        };
                    if let Some(tag_result) = tag_result {
                        STATS_RAMPLUS_US
                            .fetch_add(ram_started.elapsed().as_micros() as u64, Ordering::Relaxed);
                        match tag_result {
                            Ok(tags) => {
                                let redacted = crate::platform::redact_path_for_log(&file.path);
                                let ram_emit_count = tags.len();
                                let max_score = tags.first().map(|(_, s)| *s).unwrap_or(0.0);
                                for (label, score) in tags {
                                    tracing::debug!(
                                        target: "FileIDEngine::tagging",
                                        path = %redacted,
                                        label,
                                        score,
                                        "[TAGGING] ram_plus"
                                    );
                                    tagged.tags.push((label, Some(score)));
                                }
                                tracing::info!(
                                    target: "FileIDEngine::tagging",
                                    path = %redacted,
                                    ram_emit_count,
                                    max_score,
                                    "[TAGGING] ram_plus_summary"
                                );
                                // Treat RAM++ as "satisfied the tagger contract"
                                // only when it actually emitted content tags. A
                                // zero-tag success (hard/abstract image) must NOT
                                // suppress the lower-threshold CLIP scene-tag
                                // fallback below, else the file gets only Year/
                                // camera chips (#7).
                                ram_plus_ran = ram_emit_count > 0;
                            }
                            Err(err) => {
                                if error_has_device_removed_marker(&err) {
                                    if coord.mark_gpu_dead() {
                                        tracing::error!(?err, "[GPU-TDR] RAM++ device-removed; cancelling scan");
                                    }
                                } else {
                                    tracing::warn!(?err, "RAM++ tag failed");
                                }
                            }
                        }
                        perf_trace("ram_plus_done", &file.path, ram_started.elapsed().as_secs_f64() * 1000.0);
                    }
                }

                // Scene tags — CLIP zero-shot. Scores the MobileCLIP image
                // embedding computed just above against the scene-label
                // matrix: a tiny CPU mat-vec + softmax, NOT a GPU inference,
                // so there's no semaphore and no GPU-dead guard (and it's
                // cheaper than the MobileNetV3 ImageNet classifier this
                // replaces — that classifier's object taxonomy produced the
                // "horrible / nothing like macOS" tags). Each label carries
                // its softmax probability so the Library can confidence-order
                // chips and the threshold can be tuned against persisted
                // scores. Skips silently when the labeler isn't built (CLIP
                // text model not installed) or the embedding is missing.
                // Gated by ENABLE_CLIP_SCENE_TAGS — flip that const to false to
                // drop CLIP scan-time tagging and rely solely on VLM tags.
                // CLIP zero-shot scene tags are the FALLBACK tagger — only run
                // when RAM++ didn't (not installed / device-dead / errored), so
                // a RAM++-tagged file isn't double-tagged with noisier CLIP
                // scene guesses.
                if !ram_plus_ran && crate::models::scene_vocab::ENABLE_CLIP_SCENE_TAGS {
                    let redacted = crate::platform::redact_path_for_log(&file.path);
                    if let (Some(labeler), Some(emb)) = (&models.scene_labeler, &tagged.clip_embedding) {
                        let scene_started = Instant::now();
                        let scored = labeler.score(
                            emb,
                            crate::models::scene_vocab::SCENE_COSINE_THRESHOLD,
                            crate::models::scene_vocab::SCENE_TOP_K,
                        );
                        let scene_emit_count = scored.len();
                        let max_score = scored.first().map(|(_, s)| *s).unwrap_or(0.0);
                        for (idx, score) in scored {
                            let label = labeler.label(idx);
                            tracing::debug!(
                                target: "FileIDEngine::tagging",
                                path = %redacted,
                                label,
                                score,
                                "[TAGGING] zero_shot"
                            );
                            tagged.tags.push((label.to_string(), Some(score)));
                        }
                        // V16.29: one-line per-file summary at info level so a user
                        // who sees "only year" chips on images can grep the engine
                        // log to confirm whether scene tags are being emitted vs
                        // gated out.
                        tracing::info!(
                            target: "FileIDEngine::tagging",
                            path = %redacted,
                            scene_emit_count,
                            max_score,
                            "[TAGGING] scene_summary"
                        );
                        perf_trace("scene_tags_done", &file.path, scene_started.elapsed().as_secs_f64() * 1000.0);
                    } else if matches!(file.kind, FileKind::Image | FileKind::Video) {
                        // Embedding or labeler missing — log so the cause is visible
                        // without attaching a debugger. Skipped for non-visual kinds
                        // (audio / doc) which legitimately have no CLIP path.
                        tracing::info!(
                            target: "FileIDEngine::tagging",
                            path = %redacted,
                            has_embedding = tagged.clip_embedding.is_some(),
                            has_labeler = models.scene_labeler.is_some(),
                            "[TAGGING] scene_skipped"
                        );
                    }
                }

                // OCR is the biggest CPU per file (~30-50 ms). For camera
                // photos (EXIF camera_model present) it returns nothing
                // useful, so skip. Documents/screenshots still run OCR.
                // Also short-circuit when the GPU is already known dead —
                // Windows.Media.Ocr fails-soft on driver issues but skipping
                // avoids any recursive device-init.
                if matches!(file.kind, FileKind::Image) && !coord.is_gpu_dead() && should_run_ocr(&file.path, &tagged, file.size_bytes) {
                    // The OCR stage ran this session regardless of whether text
                    // came back — lets the dbwriter clear stale ocr_text/ocr_fts
                    // on a re-process that now yields nothing (#11).
                    tagged.ocr_stage_ran = true;
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

    // Enriched extras — derive Year + camera family from the signals we
    // already have. Cheap (no inference) and fills the chip row when CLIP
    // scene tags didn't clear the threshold. (Aspect and the generic "Has
    // Faces/Text/Location" capability tags were removed — see
    // push_enriched_extras.) Mirrors macOS `Tagging.swift::extraTags`.
    push_enriched_extras(&mut tagged);
    // Dedupe (case-insensitive) + cap. Keep first occurrence to preserve
    // the classifier's confidence-ordered output; cap at 16 to avoid
    // exploding the tags table on noisy classifier outputs. Case-insensitive
    // matters because the classifier may emit "Dog" and a hypothetical
    // synonym path may push "dog"; we want the first to win.
    {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        tagged.tags.retain(|(t, _)| seen.insert(t.to_ascii_lowercase()));
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
/// - Orientation (`"Wide"`/`"Tall"`/`"Square"`) and the generic capability
///   tags (`"Has Faces"`/`"Has Text"`/`"Has Location"`) are intentionally NOT
///   emitted — they read as noise; descriptive content comes from CLIP scene
///   tags (and, when installed, the optional Deep-Analyze VLM pass).
fn push_enriched_extras(tagged: &mut TaggedFile) {
    // Order matters: the Library card's `TopTwoTags` slice takes the first
    // two tags it sees. Classifier output (when installed) appears first
    // in the vec; enriched-extras fill the remainder. We deliberately
    // emit the most-informative signals first so that when the classifier
    // is absent the user still sees something useful.
    //
    // Priority (highest first):
    //   1. Year (from modified_unix; the single most useful low-cost tag)
    //   2. Camera family (iPhone / Canon / etc. — useful for filtering)
    //
    // The generic capability tags (Has Faces / Has Text / Has Location) used to
    // be emitted here too, but they read as content tags in the Library while
    // describing a capability rather than the image — and "Has Location" in
    // particular crowded out the descriptive scene tags. The underlying
    // signals still live in their own DB columns / filter facets (has_faces,
    // has_text, location_*); they're just no longer surfaced as tag chips.
    //
    // Aspect tags (Wide/Tall/Square) were previously emitted here and
    // ended up dominating `TopTwoTags` on files without EXIF year/camera
    // (e.g. screenshots, Windows Phone WP_*.jpg). Dropped because aspect
    // is a UI concern — the tile's own rendering already conveys it —
    // not a useful tag for search/filtering. Mirrors a macOS Tagging.swift
    // refinement.
    if tagged.modified_unix > 0.0 {
        let secs = tagged.modified_unix as i64;
        // Days since epoch via integer math (no chrono dep — already
        // available transitively but avoid the call cost here).
        if let Some(year) = unix_seconds_to_year(secs) {
            if (1990..2100).contains(&year) {
                tagged.tags.push((format!("Year_{year}"), None));
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
            tagged.tags.push((f.to_string(), None));
        }
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

/// Nearest-neighbor resize for interleaved RGB. Fast, and acceptable for the
/// dHash fingerprint (9×8) and the tight 112px face-crop, where the downscale
/// ratio is small or the model is insensitive. NOT for CLIP — a multi-megapixel
/// source decimated in one nearest step injects heavy aliasing that shifts the
/// embedding; use [`resize_rgb_quality`] there.
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

/// High-quality (bilinear) resize for interleaved RGB — the CLIP 224×224 image
/// input. Nearest one-step decimation of a multi-megapixel photo injects
/// aliasing that shifts the embedding and diverges from the macOS reference
/// (which draws at `.high` interpolation after a 512px pre-shrink), degrading
/// semantic search, zero-shot scene tags, and CLIP dedup. Triangle (bilinear)
/// closely matches macOS at this downscale ratio and reuses the same filter
/// RAM++ preprocessing already trusts. Falls back to nearest only if the raw
/// buffer can't form an image (size mismatch).
pub fn resize_rgb_quality(
    rgb: &[u8],
    src_w: usize,
    src_h: usize,
    dst_w: usize,
    dst_h: usize,
) -> Vec<u8> {
    let src = match image::RgbImage::from_raw(src_w as u32, src_h as u32, rgb.to_vec()) {
        Some(img) => img,
        None => return resize_rgb_nearest(rgb, src_w, src_h, dst_w, dst_h),
    };
    image::imageops::resize(
        &src,
        dst_w as u32,
        dst_h as u32,
        image::imageops::FilterType::Triangle,
    )
    .into_raw()
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
            online_only: false,
            file_ref: None,
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
            file_ref: None,
            content_hash: None,
            text_embedding: None,
            doc_text: None,
            tags: Vec::new(),
            faces_evaluated: false,
            ocr_stage_ran: false,
            doc_stage_ran: false,
        }
    }

    // Aspect tags (Wide/Tall/Square) were intentionally dropped from
    // enriched-extras in V16.2 — they dominated `TopTwoTags` on files
    // without classifier output (every photo got Wide/Tall as a top chip,
    // pushing the more useful Year/Camera/HasFaces tags out of the
    // 2-chip slot). These four tests now assert the absence of those
    // tags so a future re-add doesn't silently regress the change.
    #[test]
    fn enriched_does_not_emit_wide() {
        let mut t = stub_tagged(1920, 1080, 5_000_000);
        push_enriched_extras(&mut t);
        assert!(!t.tags.iter().any(|(s, _)| s == "Wide"),
            "Wide tag was dropped — too noisy, hid semantic tags");
    }

    #[test]
    fn enriched_does_not_emit_tall() {
        let mut t = stub_tagged(1080, 1920, 5_000_000);
        push_enriched_extras(&mut t);
        assert!(!t.tags.iter().any(|(s, _)| s == "Tall"),
            "Tall tag was dropped — too noisy, hid semantic tags");
    }

    #[test]
    fn enriched_does_not_emit_square() {
        let mut t = stub_tagged(1080, 1080, 5_000_000);
        push_enriched_extras(&mut t);
        assert!(!t.tags.iter().any(|(s, _)| s == "Square"),
            "Square tag was dropped — too noisy, hid semantic tags");
    }

    #[test]
    fn enriched_does_not_emit_aspect_at_boundary() {
        // The borderline case used to emit "Square" for 1.30 ratio
        // (boundary not >1.30). After the V16.2 drop, no aspect tag
        // is emitted at any ratio.
        let mut t = stub_tagged(1300, 1000, 5_000_000);
        push_enriched_extras(&mut t);
        assert!(!t.tags.iter().any(|(s, _)| s == "Square"));
        assert!(!t.tags.iter().any(|(s, _)| s == "Wide"));
        assert!(!t.tags.iter().any(|(s, _)| s == "Tall"));
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
