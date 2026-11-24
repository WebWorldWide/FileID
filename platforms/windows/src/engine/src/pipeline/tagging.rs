// Tagging workers run the per-file ML pipeline and emit TaggedFile rows for DBWriter; missing models degrade to None fields.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use sha2::Digest;
use tokio::sync::{mpsc, Semaphore};

fn sha256_bytes(bytes: &[u8]) -> [u8; 32] {
    let digest = sha2::Sha256::digest(bytes);
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

fn composite_hashes(file: &DiscoveredFile, legacy: bool) -> (Option<[u8; 32]>, Option<LegacyHashes>) {
    crate::util::content_hash::content_hashes(&file.path, file.size_bytes, legacy)
        .map_or((None, None), |(hash, legacy)| (Some(hash), legacy))
}

/// Per-stage `[PERF]` tracing for perf investigations; off unless `FILEID_PERF_TRACE=1`.
pub(crate) fn perf_trace_enabled() -> bool {
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
use crate::models::{bge_text::BgeText, face_align, face_geometry, mobileclip::MobileClipImage, scene_vocab::SceneLabeler, sface::SFace, yunet::YuNet};
use crate::pipeline::batch_clip::ClipBatchCoordinator;
use crate::pipeline::discovery::{DiscoveredFile, FileKind};
use crate::pipeline::scan_models::ModelSource;
use crate::pipeline::twin_reuse::TwinClaim;

type TwinRegistry = crate::pipeline::twin_reuse::TwinRegistry<PreDecoded>;
use crate::shell;
use crate::util::content_hash::LegacyHashes;

static STATS_FILES: AtomicU64 = AtomicU64::new(0);
static STATS_DECODE_US: AtomicU64 = AtomicU64::new(0);
/// Files with an image/video decode step; the denominator for decode_us so non-image files don't understate per-image cost.
static STATS_DECODED_FILES: AtomicU64 = AtomicU64::new(0);
static STATS_EXIF_US: AtomicU64 = AtomicU64::new(0);
static STATS_DHASH_US: AtomicU64 = AtomicU64::new(0);
static STATS_VISION_US: AtomicU64 = AtomicU64::new(0);
static STATS_CLIP_US: AtomicU64 = AtomicU64::new(0);
static STATS_OCR_US: AtomicU64 = AtomicU64::new(0);
static STATS_OCR_RAN: AtomicU64 = AtomicU64::new(0);
static STATS_TOTAL_US: AtomicU64 = AtomicU64::new(0);
/// Time RAM++ runs inside the scan itself; 0 while it is deferred to the catch-up pass.
static STATS_RAMPLUS_US: AtomicU64 = AtomicU64::new(0);
static STATS_VISION_WAIT_US: AtomicU64 = AtomicU64::new(0);
pub static STATS_TWIN_REUSED: AtomicU64 = AtomicU64::new(0);
pub static STATS_MPF_PREVIEWS: AtomicU64 = AtomicU64::new(0);
const STATS_PERIOD: u64 = 100;

fn record_stage(stage: &AtomicU64, started: Instant) {
    stage.fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
}

fn maybe_emit_stats() {
    let n = STATS_FILES.fetch_add(1, Ordering::Relaxed) + 1;
    if n % STATS_PERIOD != 0 {
        return;
    }
    let decoded_n = STATS_DECODED_FILES.load(Ordering::Relaxed);
    let decode = STATS_DECODE_US.load(Ordering::Relaxed).checked_div(decoded_n).unwrap_or(0);
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
        decoded_files = decoded_n,
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
        twin_reused = STATS_TWIN_REUSED.load(Ordering::Relaxed),
        mpf_previews = STATS_MPF_PREVIEWS.load(Ordering::Relaxed),
        clip_batches = batch_count,
        clip_avg_batch_x10 = avg_batch,
        "[STATS] per-file avg microseconds"
    );
}

/// One transaction worth plus slack so workers don't stall on a slow flush.
pub const TAGGING_CHANNEL_CAP: usize = 256;

const VISION_CONCURRENCY: usize = 4;

const CLIP_CONCURRENCY: usize = 2;

/// Must match the macOS value (0.15) so the same library yields the same embeddings and cluster IDs across platforms.
const FACE_CROP_PAD: f32 = 0.15;

/// Embeddings are L2-normalized float32 vectors stored as raw little-endian bytes.
#[derive(Debug, Clone)]
pub struct TaggedFile {
    pub path: PathBuf,
    pub kind: FileKind,
    pub size_bytes: u64,
    pub modified_unix: f64,
    /// Unix seconds from `DiscoveredFile.created_unix`; excluded from the INSERT's ON CONFLICT DO UPDATE so a rescan never clobbers it.
    pub created_unix: Option<f64>,
    /// `created_unix` is the EXIF capture time, so a rescan replaces the stored value (a filesystem birth time never does).
    pub captured_from_exif: bool,
    pub scanned_unix: f64,

    pub has_faces: bool,
    pub faces: Vec<DetectedFace>,
    /// Vision boxes without vectors; `Some` only on the Mac adapters path, which leaves the file revision to the Mac face pipeline.
    pub vision_faces: Option<Vec<VisionFace>>,

    pub has_text: bool,
    pub ocr_text: Option<String>,
    pub ocr_handwritten: bool,

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

    /// Volume-local identity (`DiscoveredFile.file_ref`) and SHA-256 content identity. The dbwriter re-binds a moved file matching either to its existing row instead of orphaning its tags, embeddings and faces. Both nullable.
    pub file_ref: Option<u64>,
    pub content_hash: Option<[u8; 32]>,
    /// Frozen pre-SHA-256 BLAKE3 digests, computed only when the catalog may hold rows they could heal.
    pub legacy_hashes: Option<LegacyHashes>,

    /// 384-d L2-normalized BGE-small vector of the doc text, persisted to `text_embeddings`; `None` when BGE isn't installed or no doc text.
    pub text_embedding: Option<Vec<f32>>,

    /// Extracted doc text, persisted to `doc_text` + `doc_fts`; capped at `doc_extract::MAX_TEXT_BYTES` upstream.
    pub doc_text: Option<String>,

    /// `(label, score)` pairs: CLIP scene labels (softmax probability) plus enriched extras (Year, camera family) with a `None` score. Persisted as `source='auto'`; VLM tags come from Deep Analyze.
    pub tags: Vec<(String, Option<f32>)>,

    /// True iff face detect/embed actually ran this scan. The dbwriter's stale-`face_prints` DELETE keys on this, not `faces.is_empty()`: a zero-face re-process must clear orphaned faces, but a face-disabled or GPU-dead session must not wipe valid rows.
    pub faces_evaluated: bool,

    /// True iff OCR / doc-text extraction ran this scan; same contract as `faces_evaluated` for the `ocr_*` / `doc_*` delete-then-reinsert.
    pub ocr_stage_ran: bool,
    pub doc_stage_ran: bool,
    /// Doc/pdf extraction ran (whether or not text was found); persisted as `files.text_stage_done` so the BGE backfill stops re-walking docs with no embeddable text.
    pub text_stage_done: bool,

    /// True iff tagging actually produced this file's tag set; same contract as `faces_evaluated` for the `source='auto'` delete-then-reinsert, so timeout and GPU-dead rows (empty `tags`) never wipe prior auto-tags. Left false on those rows.
    pub tags_evaluated: bool,
    /// A byte-identical image was already analyzed: the writer copies that row's tags, embedding, faces and text. Stage flags stay false so nothing from a prior scan at this path is deleted before the copy.
    pub reuse_twin: bool,
    /// Known row that moved (same id, size and date, old path gone): nothing was read; the writer re-binds the row to this path or writes nothing.
    pub heal_only: bool,
    /// `failed` came from machine state (cancelled, admission refused, GPU lost), not the file; not counted toward the stop-retrying memory.
    pub failure_is_transient: bool,
    pub catch_up: Option<crate::pipeline::catch_up::Marker>,
}

impl TaggedFile {
    pub(crate) fn bare(file: &DiscoveredFile) -> Self {
        Self {
            path: file.path.clone(),
            kind: file.kind,
            size_bytes: file.size_bytes,
            modified_unix: file.modified_unix,
            created_unix: file.created_unix,
            captured_from_exif: false,
            scanned_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64(),
            has_faces: false,
            faces: Vec::new(),
            vision_faces: None,
            has_text: false,
            ocr_text: None,
            ocr_handwritten: false,
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
            legacy_hashes: None,
            text_embedding: None,
            doc_text: None,
            tags: Vec::new(),
            faces_evaluated: false,
            ocr_stage_ran: false,
            doc_stage_ran: false,
            text_stage_done: false,
            tags_evaluated: false,
            reuse_twin: false,
            heal_only: false,
            failure_is_transient: false,
            catch_up: None,
        }
    }
}

/// A face from Apple Vision's primary pass: normalized "x,y,w,h" box (lower-left origin), capture quality and the clustering hold; the vector comes later.
#[derive(Debug, Clone, PartialEq)]
pub struct VisionFace {
    pub bbox: String,
    pub quality: Option<f64>,
    pub excluded: bool,
}

#[derive(Debug, Clone)]
pub struct DetectedFace {
    pub bbox: [f32; 4],          // x, y, w, h in image pixels
    pub embedding: Vec<f32>,
    pub embedding_model: Option<String>,
    pub roll: f32,
    pub yaw: f32,
    pub pitch: f32,
    pub quality: f32,
    /// True when the face should not anchor clustering; the row + crop still persist so the user can attach it to a person later.
    pub excluded: bool,
    /// Tightly-packed RGB8 112×112 ArcFace crop, written by the dbwriter to face_crops/<face_id>.jpg; cleared after persist to avoid holding ~37 KB per face.
    pub crop_rgb_112: Option<Vec<u8>>,
}

// Face-quality limits mirror macOS DBWriter.swift (pose in radians). No quality floor: `det.score * geom_conf` never drops below 0.045. FACE_MIN_BBOX_MIN_DIM_PX is the real size gate; the area fraction only backstops degenerate detections.
// The floor is in decode-space pixels and video keyframes decode ~3x smaller than stills; one shared constant is deliberate because a per-kind floor would need an exact Swift mirror.
const FACE_MIN_BBOX_AREA_FRACTION: f32 = 0.0002;
const FACE_MIN_BBOX_MIN_DIM_PX: f32 = 40.0;
const FACE_MAX_YAW_RADIANS: f32 = 50.0 * std::f32::consts::PI / 180.0;
const FACE_MAX_PITCH_RADIANS: f32 = 30.0 * std::f32::consts::PI / 180.0;

/// True if the face should be excluded from clustering: tiny in pixels or relative to the image, or heavy profile/pitch (noisy embeddings that pollute clusters).
pub fn face_is_excluded(
    yaw: f32,
    pitch: f32,
    bbox_area_fraction: f32,
    bbox_min_dim_px: f32,
) -> bool {
    bbox_area_fraction < FACE_MIN_BBOX_AREA_FRACTION
        || bbox_min_dim_px < FACE_MIN_BBOX_MIN_DIM_PX
        || yaw.abs() > FACE_MAX_YAW_RADIANS
        || pitch.abs() > FACE_MAX_PITCH_RADIANS
}

/// Loaded ML weights shared across tagging workers. Each model is optional: missing weights make that stage no-op. Each is a pool of Sessions a worker checks out (`models::checkout`), capped at MODEL_POOL_SIZE.
pub struct ModelStack {
    // YuNet (detect, MIT) + SFace (embed, Apache, 128-d); the `scrfd`/`arcface` field names are legacy.
    pub arcface: Option<Vec<Mutex<SFace>>>,
    pub scrfd: Option<Vec<Mutex<YuNet>>>,
    /// MobileCLIP has two paths chosen in `load_default`: `mobileclip_batch` (default, one Session behind `ClipBatchCoordinator`) and this pool, used when `FILEID_CLIP_USE_BATCH=0`.
    pub mobileclip_pool: Option<Vec<Mutex<MobileClipImage>>>,
    pub mobileclip_batch: Option<Arc<ClipBatchCoordinator>>,
    /// CLIP zero-shot scene labeler, scored against the per-file MobileCLIP embedding; without the text model `tags` carries only enriched extras.
    pub scene_labeler: Option<Arc<SceneLabeler>>,
    pub bge_text: Option<Mutex<BgeText>>,
    /// RAM++ tagger pool, the primary in-scan tagger; when missing, tagging falls back to the CLIP `scene_labeler`.
    pub ram_plus: Option<Vec<Mutex<crate::models::ram_plus::RamPlusTagger>>>,
    /// RAM++ batch-coordinator path (one Session driven with batched (N,3,384,384) tensors); spawned only when `FILEID_RAMPLUS_BATCH_SIZE > 1` AND a dynamic-batch ONNX is installed. When `Some`, `ram_plus` is `None`.
    pub ram_plus_batch: Option<Arc<crate::models::ram_plus_batch::RamPlusBatchCoordinator>>,
    /// Pool size the Sessions were loaded with. Concurrency caps must derive from this, not a fresh `resolve_pool_size`: its live memory-tier probe can flip after loading and silently serialize a 4-session pool to one permit (an overnight regression).
    pub pool_size: usize,
    /// The Mac adapters run this scan: unloaded face and RAM++ pools are expected, not load failures.
    pub apple_scan: bool,
    pub apple_clip: bool,
    /// Keeps the Mac adapter's CLIP and RAM++ loaded; the last drop frees them.
    pub adapter_hold: Option<crate::pipeline::scan_models::AdapterHold>,
}

/// Sessions per pooled model. Sessions of one model share a compiled graph, so a pool costs scratch buffers (~1.5 GB for RAM++), not weights; two keep a second worker busy during a forward pass (DECISIONS 2026-11-18). `FILEID_MODEL_POOL_SIZE` overrides it.
const MODEL_POOL_SIZE: usize = 2;

/// Ceiling on the read-buffer pre-allocation hint: a bogus `file.size_bytes` (sparse file, corrupt metadata) would make `Vec::with_capacity` abort the whole process. `read_to_end` still grows to the true length.
const MAX_PREALLOC_BYTES: usize = 64 * 1024 * 1024;

/// Per-image ceiling on persisted faces (top-K by quality); caps crowd photos whose 37 KB crops would amplify through the channel and DBWriter batch. The environment override may lower, never raise it.
const FACE_MAX_PER_IMAGE: usize = 32;

fn parse_face_max_per_image(raw: Option<&str>) -> usize {
    raw.and_then(|s| s.trim().parse::<usize>().ok())
        .map(|v| v.clamp(1, FACE_MAX_PER_IMAGE))
        .unwrap_or(FACE_MAX_PER_IMAGE)
}

fn face_max_per_image() -> usize {
    let raw = std::env::var("FILEID_FACE_MAX_PER_IMAGE").ok();
    parse_face_max_per_image(raw.as_deref())
}

struct FaceCandidate {
    ordinal: usize,
    detection: face_geometry::Detection,
    bbox_xywh: [f32; 4],
    quality: f32,
}

fn prioritize_face_candidates(candidates: &mut [FaceCandidate], cap: usize) -> bool {
    if candidates.len() <= cap {
        return false;
    }
    candidates.sort_by(|a, b| {
        b.quality
            .total_cmp(&a.quality)
            .then_with(|| a.ordinal.cmp(&b.ordinal))
    });
    true
}

fn resolve_pool_size(worker_count: usize) -> usize {
    let env_override = std::env::var("FILEID_MODEL_POOL_SIZE")
        .ok()
        .and_then(|s| s.parse::<usize>().ok());
    let mut cap = env_override.unwrap_or(MODEL_POOL_SIZE);
    // Session activations live in system RAM: on a low-memory box one session is all that fits beside the scan's own buffers.
    if env_override.is_none() && crate::platform::memory_tier() == crate::platform::MemoryTier::Low {
        cap = cap.min(1);
    }
    cap.max(1).min(worker_count.max(1))
}

impl ModelStack {
    pub fn load_default(worker_count: usize) -> Self {
        let pool_size = resolve_pool_size(worker_count);
        let arcface = load_pool("SFace", pool_size, crate::models::sface::default_weights_path(), SFace::load);
        let scrfd = load_pool("YuNet", pool_size, crate::models::yunet::default_weights_path(), YuNet::load);

        // FILEID_CLIP_USE_BATCH=0/false selects the session pool; any other value the batch coordinator.
        let use_batch = std::env::var("FILEID_CLIP_USE_BATCH")
            .ok()
            .is_none_or(|value| !(value == "0" || value.eq_ignore_ascii_case("false")));
        tracing::info!(model = "MobileCLIP", use_batch, "dispatch mode selected");

        let scene_labeler = Some(crate::models::scene_vocab::shared_scene_labeler());

        let (mobileclip_pool, mobileclip_batch) = if use_batch {
            let coord = match crate::models::mobileclip::default_weights_path() {
                Ok(p) if p.exists() => match MobileClipImage::load(p.clone()) {
                    Ok(model) => {
                        tracing::info!(model = "MobileCLIP", path = %crate::platform::redact_path_for_log(&p), "model loaded (batch-coordinator mode)");
                        Some(ClipBatchCoordinator::spawn(model))
                    }
                    Err(err) => {
                        tracing::warn!(model = "MobileCLIP", ?err, "model load failed; stage will skip");
                        None
                    }
                },
                Ok(p) => {
                    tracing::info!(model = "MobileCLIP", path = %crate::platform::redact_path_for_log(&p), "model not installed; stage will skip");
                    None
                }
                Err(err) => {
                    tracing::warn!(model = "MobileCLIP", ?err, "model path unresolved");
                    None
                }
            };
            (None, coord)
        } else {
            let pool = load_pool(
                "MobileCLIP",
                pool_size,
                crate::models::mobileclip::default_weights_path(),
                |p| MobileClipImage::load(p),
            );
            (pool, None)
        };

        // The tag list is captured into the loader closure: `load_pool` passes only the resolved ONNX path.
        let ram_batch_size =
            crate::models::ram_plus_batch::RamPlusBatchCoordinator::configured_batch_size();
        let load_ram_plus_pool = |
            tags_path: PathBuf,
            seed: Option<crate::models::ram_plus::RamPlusTagger>,
        | {
            load_pool_with_seed(
                "RAM++",
                pool_size,
                crate::models::ram_plus::default_onnx_path(),
                move |p| crate::models::ram_plus::RamPlusTagger::load(p, tags_path.clone()),
                seed,
            )
        };
        let installed_gb = crate::platform::physical_memory_gb();
        let ram_plus_fits = crate::platform::ram_plus_fits_in_memory(installed_gb);
        let (ram_plus, ram_plus_batch) = match crate::models::ram_plus::default_tags_path() {
            Ok(_) if !ram_plus_fits => {
                tracing::warn!(
                    model = "RAM++",
                    installed_gb = format!("{installed_gb:.1}"),
                    min_gb = crate::platform::RAM_PLUS_MIN_TOTAL_GB,
                    "not enough installed memory for RAM++; tagging falls back to CLIP scene-tags"
                );
                (None, None)
            }
            Ok(tags_path) if ram_batch_size > 1 => {
                match crate::models::ram_plus::default_onnx_path() {
                    Ok(p) if p.exists() => {
                        match crate::models::ram_plus::RamPlusTagger::load(p.clone(), tags_path.clone()) {
                            Ok(tagger) => {
                                if tagger.supports_dynamic_batch() {
                                    tracing::info!(model = "RAM++", path = %crate::platform::redact_path_for_log(&p), batch_size = ram_batch_size, "model loaded (batch-coordinator mode)");
                                    let coord = crate::models::ram_plus_batch::RamPlusBatchCoordinator::spawn(tagger);
                                    (None, coord)
                                } else {
                                    tracing::warn!(
                                        model = "RAM++",
                                        path = %crate::platform::redact_path_for_log(&p),
                                        batch_size = ram_batch_size,
                                        "FILEID_RAMPLUS_BATCH_SIZE requested but installed ONNX does not expose a dynamic batch axis; using single-image pool"
                                    );
                                    (load_ram_plus_pool(tags_path, Some(tagger)), None)
                                }
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
                (load_ram_plus_pool(tags_path, None), None)
            }
            Err(err) => {
                tracing::warn!(model = "RAM++", ?err, "tag-list path unresolved; tagging falls back to CLIP scene-tags");
                (None, None)
            }
        };

        Self {
            arcface,
            scrfd,
            mobileclip_pool,
            mobileclip_batch,
            scene_labeler,
            bge_text: load_bge_text(),
            ram_plus,
            ram_plus_batch,
            pool_size,
            apple_scan: false,
            apple_clip: false,
            adapter_hold: None,
        }
    }

    pub fn load_for_scan(worker_count: usize) -> Self {
        #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
        if mac::enabled() {
            return mac::model_stack(worker_count);
        }
        Self::load_default(worker_count)
    }

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
            pool_size: 1,
            apple_scan: false,
            apple_clip: false,
            adapter_hold: None,
        }
    }
}

fn load_bge_text() -> Option<Mutex<BgeText>> {
    match (
        crate::models::bge_text::default_weights_path(),
        crate::models::bge_text::default_vocab_path(),
    ) {
        (Ok(wp), Ok(vp)) if wp.exists() && vp.exists() => match BgeText::load(wp.clone(), vp) {
            Ok(m) => {
                tracing::info!(model = "BGE-small", path = %crate::platform::redact_path_for_log(&wp), "model loaded");
                Some(Mutex::new(m))
            }
            Err(err) => {
                tracing::warn!(model = "BGE-small", ?err, "model load failed; stage will skip");
                None
            }
        },
        (Ok(wp), _) => {
            tracing::info!(model = "BGE-small", path = %crate::platform::redact_path_for_log(&wp), "model not installed; stage will skip");
            None
        }
        (Err(err), _) => {
            tracing::warn!(model = "BGE-small", ?err, "model path unresolved");
            None
        }
    }
}

fn load_pool<T, F>(label: &str, pool_size: usize, path: anyhow::Result<PathBuf>, loader: F) -> Option<Vec<Mutex<T>>>
where
    F: Fn(PathBuf) -> anyhow::Result<T>,
{
    load_pool_with_seed(label, pool_size, path, loader, None)
}

fn load_pool_with_seed<T, F>(label: &str, pool_size: usize, path: anyhow::Result<PathBuf>, loader: F, seed: Option<T>) -> Option<Vec<Mutex<T>>>
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
        tracing::info!(model = label, path = %crate::platform::redact_path_for_log(&p), "model not installed; stage will skip");
        return None;
    }
    let mut pool = Vec::with_capacity(pool_size);
    if let Some(model) = seed {
        pool.push(Mutex::new(model));
    }
    for idx in pool.len()..pool_size {
        match loader(p.clone()) {
            Ok(model) => pool.push(Mutex::new(model)),
            Err(err) => {
                tracing::warn!(model = label, slot = idx, ?err, "model pool load failed; stage will skip if pool empty");
                break;
            }
        }
    }
    if pool.is_empty() {
        None
    } else {
        tracing::info!(model = label, path = %crate::platform::redact_path_for_log(&p), pool_size = pool.len(), "model pool loaded");
        Some(pool)
    }
}

static ACTIVE_DECODER_THREADS: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn active_decoder_threads() -> usize {
    ACTIVE_DECODER_THREADS.load(Ordering::Acquire)
}

struct DecoderThreadGuard;

impl DecoderThreadGuard {
    fn enter() -> Self {
        ACTIVE_DECODER_THREADS.fetch_add(1, Ordering::AcqRel);
        Self
    }
}

impl Drop for DecoderThreadGuard {
    fn drop(&mut self) {
        ACTIVE_DECODER_THREADS.fetch_sub(1, Ordering::AcqRel);
    }
}

pub struct Tagger {
    coordinator: ScanCoordinator,
    worker_count: usize,
    models: Arc<ModelSource>,
    scan_root: Option<std::path::PathBuf>,
    legacy_hashes: bool,
    scheduler: Arc<crate::resource_scheduler::ResourceScheduler>,
    twins: Arc<TwinRegistry>,
    retrying_failures: bool,
}

pub(crate) fn apple_scan_enabled() -> bool {
    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    {
        mac::enabled()
    }
    #[cfg(not(all(target_os = "macos", feature = "apple-adapters")))]
    {
        false
    }
}

/// `FILEID_TWIN_REUSE=0` analyzes every copy itself (the A/B control).
fn twin_reuse_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("FILEID_TWIN_REUSE").map_or(true, |v| v.trim() != "0"))
}

const PER_FILE_TIMEOUT: Duration = Duration::from_secs(60);
/// What a row says when a read timed out: the file is fine, the machine was too busy, and the engine reads it again.
pub(crate) const TIMED_OUT_MESSAGE: &str = "FileID ran out of time reading this file; it will try again";
/// The wording earlier builds stored for the same failure; catalogs written by them still hold it.
const LEGACY_TIMEOUT_MESSAGES: [&str; 2] = ["per-file timeout after 60s", "Vision pass timed out"];
/// Stored `error_message` of a row that failed only because the machine was too busy, so a retry may succeed.
pub(crate) const TIMEOUT_MESSAGES: [&str; 3] = [TIMED_OUT_MESSAGE, LEGACY_TIMEOUT_MESSAGES[0], LEGACY_TIMEOUT_MESSAGES[1]];

/// Wait for the first copy's analysis before analyzing itself; shorter than `PER_FILE_TIMEOUT` and spent before that timer starts, so waiting never fails the copy.
const TWIN_WAIT_PATIENCE: Duration = Duration::from_secs(45);

/// Times a copy re-claims the hash after its owner failed before analyzing unshared.
const TWIN_RECLAIM_LIMIT: u32 = 2;

/// At most pool-1 workers may wait for a twin: a copy can reach a worker before its owner (still decoding), and with every worker waiting nothing would pick the owner up until the waits ran out.
struct TwinWaiters {
    waiting: std::sync::atomic::AtomicUsize,
    limit: usize,
}

struct TwinWaiting(Arc<TwinWaiters>);

impl TwinWaiters {
    fn new(workers: usize) -> Arc<Self> {
        Arc::new(Self { waiting: std::sync::atomic::AtomicUsize::new(0), limit: workers.saturating_sub(1) })
    }

    fn enter(self: &Arc<Self>) -> Option<TwinWaiting> {
        use std::sync::atomic::Ordering::SeqCst;
        self.waiting
            .fetch_update(SeqCst, SeqCst, |waiting| (waiting < self.limit).then_some(waiting + 1))
            .ok()
            .map(|_| TwinWaiting(self.clone()))
    }
}

impl Drop for TwinWaiting {
    fn drop(&mut self) {
        self.0.waiting.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Max wait for CPU/memory admission before a file is recorded as failed; the wait ends at once on cancel.
const SCAN_ADMISSION_PATIENCE: Duration = Duration::from_secs(600);

fn scan_working_mb(predecoded: &PreDecoded) -> u64 {
    predecoded
        .decoded
        .as_ref()
        .and_then(|result| result.as_ref().ok())
        .map(|(rgb, _, _)| (rgb.len() as u64).saturating_mul(4).div_ceil(1_048_576))
        .unwrap_or(0)
        + 32
}

/// A USB or network volume gets 4 readers: on the owner's USB exFAT SSD, 1/2/4/10 threads read 101/136/151/135 files/s, so more than 4 only adds contention (and thrashes a spinning disk).
fn decoder_count_for_storage(count: usize, storage: Option<crate::platform::StorageType>) -> usize {
    match storage {
        Some(
            crate::platform::StorageType::Hdd
            | crate::platform::StorageType::Removable
            | crate::platform::StorageType::Network,
        ) => count.min(4),
        _ => count,
    }
}

impl Tagger {
    pub fn new(coordinator: ScanCoordinator, worker_count: usize, models: Arc<ModelStack>) -> Self {
        Self::with_model_source(coordinator, worker_count, ModelSource::ready(models))
    }

    pub fn with_model_source(coordinator: ScanCoordinator, worker_count: usize, models: Arc<ModelSource>) -> Self {
        Self {
            coordinator,
            worker_count: worker_count.max(1),
            models,
            scan_root: None,
            legacy_hashes: false,
            scheduler: crate::resource_scheduler::ResourceScheduler::shared(),
            twins: TwinRegistry::new(),
            retrying_failures: false,
        }
    }

    /// The pass that retries what failed earlier in the same scan: a failure here is not remembered as another attempt, so one busy moment cannot use up a file's retries.
    pub fn with_retrying_failures(mut self, retrying: bool) -> Self {
        self.retrying_failures = retrying;
        self
    }

    pub fn with_known_twins(self, hashes: impl IntoIterator<Item = [u8; 32]>) -> Self {
        let seeded = self.twins.seed(hashes);
        if seeded > 0 {
            tracing::info!(seeded, "[TWIN] known image hashes loaded for analysis reuse");
        }
        self
    }

    #[cfg(test)]
    fn with_scheduler(mut self, scheduler: Arc<crate::resource_scheduler::ResourceScheduler>) -> Self {
        self.scheduler = scheduler;
        self
    }

    /// Also compute the legacy BLAKE3 digests for the dbwriter's rename-heal probes (only useful on a non-empty catalog).
    pub fn with_legacy_hashes(mut self, enabled: bool) -> Self {
        self.legacy_hashes = enabled;
        self
    }

    /// Lets the decoder pool clamp to the scan root's storage type.
    pub fn with_scan_root(mut self, root: std::path::PathBuf) -> Self {
        self.scan_root = Some(root);
        self
    }

    /// Wires Discovery → decoder pool (OS threads, blocking decode) → N inference workers (async tasks, semaphore-bounded face/CLIP/OCR) → DBWriter.
    pub fn spawn(
        self,
        mut input: mpsc::Receiver<DiscoveredFile>,
    ) -> (mpsc::Receiver<TaggedFile>, tokio::task::JoinHandle<()>) {
        // Reset per-scan so the [STATS] log reflects only this session; STATS_* are process-global.
        for counter in &[
            &STATS_FILES as &AtomicU64,
            &STATS_DECODE_US,
            &STATS_DECODED_FILES,
            &STATS_EXIF_US,
            &STATS_DHASH_US,
            &STATS_VISION_US,
            &STATS_CLIP_US,
            &STATS_OCR_US,
            &STATS_OCR_RAN,
            &STATS_TOTAL_US,
            &STATS_RAMPLUS_US,
            &STATS_VISION_WAIT_US,
        ] {
            counter.store(0, Ordering::Relaxed);
        }
        crate::pipeline::batch_clip::STATS_BATCH_COUNT.store(0, Ordering::Relaxed);
        crate::pipeline::batch_clip::STATS_BATCH_SIZE_SUM.store(0, Ordering::Relaxed);

        let (out_tx, out_rx) = mpsc::channel(TAGGING_CHANNEL_CAP);

        let (raw_tx, raw_rx) = async_channel::bounded::<DiscoveredFile>(TAGGING_CHANNEL_CAP);
        let coordinator_pump = self.coordinator.clone();
        let models_for_pump = self.models.clone();
        tokio::spawn(async move {
            let mut models_requested = false;
            loop {
                let file = tokio::select! {
                    value = input.recv() => match value {
                        Some(value) => value,
                        None => break,
                    },
                    _ = coordinator_pump.wait_cancelled() => break,
                };
                // The first file with work starts the model load, overlapping the walk and first decodes.
                if !models_requested && !file.moved_from_known_row {
                    models_requested = true;
                    let models = models_for_pump.clone();
                    tokio::spawn(async move {
                        models.get().await;
                    });
                }
                tokio::select! {
                    result = raw_tx.send(file) => {
                        if result.is_err() {
                            break;
                        }
                    }
                    _ = coordinator_pump.wait_cancelled() => break,
                }
            }
        });

        // Decoder pool: sync OS threads sized by physical CPU topology, clamped to [2, 12] so the GPU side and the WinUI app aren't starved; more than 12 only adds per-decode latency (large-JPEG decode is memory-bandwidth-bound).
        let topo = crate::platform::cpu_topology();
        let decoder_count = ((topo.p_cores + topo.e_cores) as usize).clamp(2, 12);
        // Low tier: each blocked decoder still owns one decoded frame (~150 MB at MAX_DECODED_PIXELS), so cap the pool, not just the channel, on the 4 GB target.
        let decoder_count = match crate::platform::memory_tier() {
            crate::platform::MemoryTier::Low => decoder_count.min(4),
            _ => decoder_count,
        };
        // Seek-penalty media (USB HDD, network share): a 12-deep read pool thrashes and runs slower; 4 keeps the drive busy while threads overlap CPU decode (discovery's stat walk uses 2).
        let decoder_count = {
            let storage = self.scan_root.as_deref().map(crate::platform::storage_type_for_path);
            let clamped = decoder_count_for_storage(decoder_count, storage);
            if clamped != decoder_count {
                tracing::info!(
                    storage = storage.map_or("unknown", crate::platform::StorageType::as_str),
                    decoders = clamped,
                    "decode pool clamped for seek-penalty storage"
                );
            }
            clamped
        };
        // Slot bound only (floor = one frame per worker). The real memory bound is the byte-weighted budget below: MAX_DECODED_PIXELS admits ~150 MB frames, far above TYPICAL_FRAME_MB per slot.
        const TYPICAL_FRAME_MB: usize = 24; // ~8 MP RGB8
        let predecode_budget_mb = match crate::platform::memory_tier() {
            crate::platform::MemoryTier::Low => 192,
            _ => 768,
        };
        let budget_cap = predecode_budget_mb / TYPICAL_FRAME_MB;
        let predecoded_cap = match crate::platform::memory_tier() {
            crate::platform::MemoryTier::Low => budget_cap.max(1),
            _ => budget_cap.max(self.worker_count),
        };
        let (predecoded_tx, predecoded_rx) =
            async_channel::bounded::<PreDecoded>(predecoded_cap);
        let byte_budget = PredecodeBudget::new(predecode_budget_mb * 1024 * 1024);
        let mut decoder_handles = Vec::with_capacity(decoder_count);
        for decoder_idx in 0..decoder_count {
            let rx = raw_rx.clone();
            let tx = predecoded_tx.clone();
            let coord = self.coordinator.clone();
            let budget = byte_budget.clone();
            let twins = self.twins.clone();
            let legacy = self.legacy_hashes;
            let spawn_result = std::thread::Builder::new()
                .name(format!("fileid-decode-{decoder_idx}"))
                .spawn(move || run_decoder_thread(rx, tx, coord, budget, legacy, twins));
            match spawn_result {
                Ok(handle) => decoder_handles.push(handle),
                Err(e) => {
                    tracing::warn!(
                        "fileid-decode-{decoder_idx} failed to spawn ({e}); continuing with fewer decode threads"
                    );
                }
            }
        }
        drop(raw_rx);
        drop(predecoded_tx);
        let decoder_join = tokio::task::spawn_blocking(move || {
            for handle in decoder_handles {
                if let Err(payload) = handle.join() {
                    std::panic::resume_unwind(payload);
                }
            }
        });

        let loaded_models: Arc<tokio::sync::OnceCell<Option<WorkerModels>>> = Arc::new(tokio::sync::OnceCell::new());

        let runtime = tokio::runtime::Handle::current();
        let mut workers_started = 0usize;
        let twin_waiters = TwinWaiters::new(self.worker_count);
        for worker_idx in 0..self.worker_count {
            let twin_waiters = twin_waiters.clone();
            let rx = predecoded_rx.clone();
            let tx = out_tx.clone();
            let coord = self.coordinator.clone();
            let source = self.models.clone();
            let worker_models_cell = loaded_models.clone();
            let scheduler = self.scheduler.clone();
            let twins = self.twins.clone();
            let budget = byte_budget.clone();
            let legacy = self.legacy_hashes;
            let retrying = self.retrying_failures;

            let worker = async move {
                // Sleep briefly every YIELD_AFTER files to give foreground apps and DWM breathing room (<1 % cost).
                const YIELD_AFTER: u64 = 500;
                let mut files_done: u64 = 0;
                loop {
                    let predecoded = tokio::select! {
                        received = rx.recv() => match received {
                            Ok(value) => value,
                            Err(_) => break,
                        },
                        _ = coord.wait_cancelled() => break,
                    };
                    if coord.check().await.is_err() {
                        break;
                    }
                    if predecoded.file.moved_from_known_row {
                        let moved = TaggedFile { heal_only: true, ..TaggedFile::bare(&predecoded.file) };
                        if tx.send(moved).await.is_err() {
                            break;
                        }
                        continue;
                    }
                    // Models load on the first file that needs them; if they can't, no file is written as analysed: the scan stops and reports why.
                    let loaded = worker_models_cell
                        .get_or_init(|| async { source.get().await.map(worker_models) })
                        .await
                        .clone();
                    let Some(WorkerModels { models, vision_sem, clip_sem }) = loaded else {
                        coord.request_cancel();
                        break;
                    };
                    // A copy of an image another worker is analyzing waits here, holding no frame, admission or per-file timer.
                    let Some(predecoded) =
                        settle_twin(predecoded, &twins, &twin_waiters, &coord, &budget, legacy, TWIN_WAIT_PATIENCE)
                            .await
                    else {
                        break;
                    };
                    let file_for_failure = predecoded.file.clone();
                    let path_for_timeout = &file_for_failure.path;
                    let twin_hash = predecoded
                        .content_hash
                        .filter(|_| !matches!(predecoded.twin, TwinClaim::None));
                    // Admission waits outside the per-file timeout, so a busy machine delays a file but never fails it.
                    let demand = crate::resource_scheduler::Demand { memory_mb: scan_working_mb(&predecoded), cpu: 1, io: 0 };
                    let admission = scheduler.reserve_patient(
                        demand,
                        crate::resource_scheduler::Priority::Background,
                        || coord.is_cancelled(),
                        SCAN_ADMISSION_PATIENCE,
                    );
                    let (_lease, refused) = match admission {
                        Ok(lease) => (Some(lease), None),
                        Err(_) if coord.is_cancelled() => break,
                        Err(error) => (None, Some(error.to_string())),
                    };
                    // Bounds async inference after decoding (platform decoders enforce their own limits). A panic on one file becomes that file's failure so the worker keeps draining.
                    let transient = refused.is_some();
                    let outcome = if let Some(message) = refused {
                        Err((message, 0.0))
                    } else {
                        let fut = futures_util::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(
                            process_file_predecoded(predecoded, &models, &vision_sem, &clip_sem, worker_idx, &coord),
                        ));
                        match tokio::time::timeout(PER_FILE_TIMEOUT, fut).await {
                            Ok(Ok(t)) => Ok(t),
                            Ok(Err(_panic)) => {
                                tracing::error!(
                                    path = %crate::platform::redact_path_for_log(path_for_timeout),
                                    "per-file processing panicked; marking failed and continuing"
                                );
                                Err(("the file could not be analyzed (internal error)".to_string(), 0.0))
                            }
                            Err(_elapsed) => {
                                tracing::warn!(
                                    path = %crate::platform::redact_path_for_log(path_for_timeout),
                                    "per-file timeout after 60s; marking failed and continuing"
                                );
                                Err((TIMED_OUT_MESSAGE.to_string(), 60000.0))
                            }
                        }
                    };
                    let tagged = match outcome {
                        Ok(t) => t,
                        Err((message, total_ms)) => {
                            TaggedFile {
                                total_ms,
                                failed: true,
                                error_message: Some(message),
                                file_ref: None,
                                ..TaggedFile::bare(&file_for_failure)
                            }
                        }
                    };
                    let mut tagged = tagged;
                    #[cfg(test)]
                    if tagged.path.file_name().is_some_and(|name| name == "fileid-test-worker-thread.txt") {
                        tagged.camera_model = std::thread::current().name().map(str::to_owned);
                    }
                    if tagged.failed && !tagged.failure_is_transient {
                        tagged.failure_is_transient = transient
                            || retrying
                            || coord.is_cancelled()
                            || coord.is_gpu_dead()
                            || !crate::pipeline::catch_up::failure_is_about_the_file(&tagged.path);
                    }
                    let publish = twin_hash.filter(|_| !tagged.reuse_twin).map(|hash| {
                        (hash, !tagged.failed && tagged.tags_evaluated)
                    });
                    if tx.send(tagged).await.is_err() {
                        break;
                    }
                    if let Some((hash, ok)) = publish {
                        twins.publish(hash, ok);
                    }
                    files_done += 1;
                    if files_done % YIELD_AFTER == 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    }
                }
            };
            // Workers run on their own thread: CPU/GPU work on the runtime's threads stalled IPC, cancel and progress on low-core machines.
            let runtime = runtime.clone();
            let spawned = std::thread::Builder::new().name(format!("fileid-tag-{worker_idx}")).spawn(move || {
                crate::platform::set_worker_background_priority();
                runtime.block_on(worker);
            });
            match spawned {
                Ok(_) => workers_started += 1,
                Err(error) => tracing::warn!("fileid-tag-{worker_idx} failed to spawn ({error}); continuing with fewer workers"),
            }
        }
        if workers_started == 0 {
            tracing::error!("[TAGGING] no analysis worker could start; stopping the scan");
            self.coordinator.request_cancel();
        }

        drop(out_tx);
        (out_rx, decoder_join)
    }
}

#[derive(Clone)]
struct WorkerModels {
    models: Arc<ModelStack>,
    vision_sem: Arc<Semaphore>,
    clip_sem: Arc<Semaphore>,
}

fn worker_models(models: Arc<ModelStack>) -> WorkerModels {
    // Caps come from the pool size the models were loaded with, never a fresh resolve_pool_size call: its live memory probe can flip after loading and silently issue 1 permit against a 4-session pool.
    let pool_size = models.pool_size.max(1);
    // One permit per session lets excess workers await the async semaphore instead of blocking on a session's mutex.
    let vision_cap = pool_size.min(VISION_CONCURRENCY);
    let clip_cap = CLIP_CONCURRENCY;
    tracing::info!(
        backend = if models.apple_scan { "apple-adapters (MLX, Vision) with tract fallbacks" } else { crate::models::runtime::inference_backend() },
        pool_size,
        vision_cap,
        clip_cap,
        "[TAGGING] inference concurrency caps"
    );
    WorkerModels {
        models,
        vision_sem: Arc::new(Semaphore::new(vision_cap)),
        clip_sem: Arc::new(Semaphore::new(clip_cap)),
    }
}

/// Settles a copy waiting on its image's first copy: `Ready` when the owner's analysis can be copied, otherwise reads and decodes the copy now. `None` means the scan was cancelled.
async fn settle_twin(
    mut predecoded: PreDecoded,
    twins: &Arc<TwinRegistry>,
    waiters: &Arc<TwinWaiters>,
    coord: &ScanCoordinator,
    budget: &Arc<PredecodeBudget>,
    legacy: bool,
    patience: Duration,
) -> Option<PreDecoded> {
    let mut reclaims = 0;
    loop {
        let TwinClaim::Pending(notify) = &predecoded.twin else {
            return Some(predecoded);
        };
        let hash = predecoded.content_hash.expect("a pending twin claim carries a hash");
        let outcome = match twins.settled(&hash, notify) {
            Some(ok) => Some(ok),
            None => match waiters.enter() {
                Some(_waiting) => tokio::select! {
                    ok = twins.wait(&hash, notify) => Some(ok),
                    _ = tokio::time::sleep(patience) => None,
                    _ = coord.wait_cancelled() => return None,
                },
                // Every other worker is waiting already: analyze unshared and
                // stay free for the owner.
                None => None,
            },
        };
        if outcome == Some(true) {
            predecoded.twin = TwinClaim::Ready;
            return Some(predecoded);
        }
        // The owner failed: claim again, so one of the remaining copies
        // becomes the owner. The owner is only slow: analyze unshared.
        reclaims += 1;
        let share = (outcome.is_some() && reclaims <= TWIN_RECLAIM_LIMIT).then(|| twins.clone());
        let file = predecoded.file;
        let fallback = file.clone();
        let (coord, budget) = (coord.clone(), budget.clone());
        let decoded = tokio::task::spawn_blocking(move || {
            let _thread_guard = DecoderThreadGuard::enter();
            predecode_one(file, &coord, &budget, legacy, share.as_deref(), false)
        })
        .await;
        predecoded = match decoded {
            Ok(Some(item)) => item,
            Ok(None) => return None,
            Err(_) => PreDecoded::failed(fallback, "the file could not be read (decoder error)"),
        };
    }
}

/// One unit of work between the decoder pool and the workers. `decoded` is `Err(_)` when the file couldn't be opened or decoded (the worker emits a failed row so the DB records the file); `None` for kinds that are not decoded.
pub struct PreDecoded {
    pub file: DiscoveredFile,
    pub decoded: Option<anyhow::Result<(Vec<u8>, u32, u32)>>,
    /// `None` on non-doc kinds, online-only files and extraction failures.
    pub doc_text: Option<String>,
    /// Empty for non-audio kinds, online-only files and extraction failures.
    pub audio_tags: Vec<(String, Option<f32>)>,
    pub content_hash: Option<[u8; 32]>,
    pub legacy_hashes: Option<LegacyHashes>,
    pub exif: Option<ExifFields>,
    pub twin: TwinClaim,
    budget: Option<PredecodeBudgetGuard>,
    #[cfg_attr(not(all(target_os = "macos", feature = "apple-adapters")), allow(dead_code))]
    mac: Option<MacFrame>,
}

#[cfg(all(target_os = "macos", feature = "apple-adapters"))]
#[path = "tagging_mac.rs"]
mod mac;
#[cfg(all(target_os = "macos", feature = "apple-adapters"))]
type MacFrame = mac::Frame;
#[cfg(not(all(target_os = "macos", feature = "apple-adapters")))]
enum MacFrame {}

impl PreDecoded {
    fn unread(file: DiscoveredFile) -> Self {
        Self { decoded: None, ..Self::failed(file, "") }
    }

    fn failed(file: DiscoveredFile, message: &str) -> Self {
        Self {
            file,
            decoded: Some(Err(anyhow::anyhow!(message.to_string()))),
            doc_text: None,
            audio_tags: Vec::new(),
            content_hash: None,
            legacy_hashes: None,
            exif: None,
            twin: TwinClaim::None,
            budget: None,
            mac: None,
        }
    }
}

/// Byte-weighted read-ahead budget: decoders reserve their heap peak (the full capacity for an uncertain codec) before any full-frame allocation, then shrink to retained RGB bytes.
struct PredecodeBudget {
    capacity: usize,
    used: Mutex<usize>,
    freed: parking_lot::Condvar,
}

impl PredecodeBudget {
    fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            capacity,
            used: Mutex::new(0),
            freed: parking_lot::Condvar::new(),
        })
    }

    /// Blocks the decoder thread until `bytes` fit; a frame larger than the budget clamps to capacity so it is admitted alone instead of deadlocking. `None` if the scan is cancelled.
    fn acquire(
        self: &Arc<Self>,
        bytes: usize,
        coord: &ScanCoordinator,
    ) -> Option<PredecodeBudgetGuard> {
        let bytes = bytes.min(self.capacity);
        let mut used = self.used.lock();
        loop {
            if coord.is_cancelled() {
                return None;
            }
            if used
                .checked_add(bytes)
                .is_some_and(|total| total <= self.capacity)
            {
                break;
            }
            self.freed
                .wait_for(&mut used, std::time::Duration::from_millis(100));
        }
        if coord.is_cancelled() {
            return None;
        }
        *used += bytes;
        Some(PredecodeBudgetGuard {
            budget: self.clone(),
            bytes,
        })
    }

    fn capacity(&self) -> usize {
        self.capacity
    }

    fn release(&self, bytes: usize) {
        let mut used = self.used.lock();
        *used = used.saturating_sub(bytes);
        self.freed.notify_all();
    }
}

struct PredecodeBudgetGuard {
    budget: Arc<PredecodeBudget>,
    bytes: usize,
}

impl PredecodeBudgetGuard {
    fn shrink_to(&mut self, bytes: usize) -> bool {
        let bytes = bytes.min(self.budget.capacity);
        if bytes > self.bytes {
            return false;
        }
        let released = self.bytes - bytes;
        self.bytes = bytes;
        self.budget.release(released);
        true
    }
}

impl Drop for PredecodeBudgetGuard {
    fn drop(&mut self) {
        self.budget.release(self.bytes);
    }
}

fn decode_reserved<T>(
    budget: &Arc<PredecodeBudget>,
    bytes: usize,
    coord: &ScanCoordinator,
    decode: impl FnOnce() -> anyhow::Result<T>,
) -> Option<(anyhow::Result<T>, PredecodeBudgetGuard)> {
    let guard = budget.acquire(bytes, coord)?;
    Some((decode(), guard))
}

/// Runs `future` on the calling thread until it finishes or the scan is cancelled (`None`); needs no runtime.
fn wait_unless_cancelled<T>(coord: &ScanCoordinator, future: impl std::future::Future<Output = T>) -> Option<T> {
    struct Unpark(std::thread::Thread);
    impl std::task::Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = std::task::Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut context = std::task::Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    let mut cancelled = std::pin::pin!(coord.wait_cancelled());
    loop {
        if cancelled.as_mut().poll(&mut context).is_ready() {
            return None;
        }
        if let std::task::Poll::Ready(value) = future.as_mut().poll(&mut context) {
            return Some(value);
        }
        std::thread::park();
    }
}

/// Decoder-pool worker: a sync OS thread so blocking decode doesn't tie up tokio's runtime threads.
fn run_decoder_thread(
    rx: async_channel::Receiver<DiscoveredFile>,
    tx: async_channel::Sender<PreDecoded>,
    coord: ScanCoordinator,
    budget: Arc<PredecodeBudget>,
    legacy: bool,
    twins: Arc<TwinRegistry>,
) {
    let _thread_guard = DecoderThreadGuard::enter();
    loop {
        if coord.is_cancelled() {
            return;
        }
        let Some(Ok(file)) = wait_unless_cancelled(&coord, rx.recv()) else {
            return;
        };
        // Trace-gated forensics: a native crash never reaches the log as a Rust panic, so the last "decode start" lines name the in-flight victims.
        tracing::trace!(
            path = %crate::platform::redact_path_for_log(&file.path),
            kind = ?file.kind,
            size = file.size_bytes,
            "decode start"
        );
        let fallback = file.clone();
        let item = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            predecode_one(file, &coord, &budget, legacy, Some(&twins), true)
        })) {
            Ok(Some(item)) => item,
            Ok(None) => return,
            Err(_) => {
                tracing::error!(
                    path = %crate::platform::redact_path_for_log(&fallback.path),
                    "decoder panicked on a file; recording it as failed and continuing"
                );
                PreDecoded::failed(fallback, "the file could not be read (decoder error)")
            }
        };
        // A copy waits behind an owner still decoding on another thread; the owner brings its copies once queued. An owner whose decode panicked lets go, so its copies analyse themselves.
        let owned = OWNED_TWIN.take();
        if let Some(hash) = owned.filter(|_| !matches!(item.twin, TwinClaim::Owner)) {
            twins.publish(hash, false);
        }
        let item = match (&item.twin, item.content_hash) {
            (TwinClaim::Pending(_), Some(hash)) => twins.hold(&hash, item),
            _ => Some(item),
        };
        if let Some(item) = item {
            let Some(Ok(())) = wait_unless_cancelled(&coord, tx.send(item)) else {
                return;
            };
        }
        for follower in owned.map(|hash| twins.owner_queued(&hash)).unwrap_or_default() {
            let Some(Ok(())) = wait_unless_cancelled(&coord, tx.send(follower)) else {
                return;
            };
        }
    }
}

thread_local! {
    static OWNED_TWIN: std::cell::Cell<Option<[u8; 32]>> = const { std::cell::Cell::new(None) };
}

/// A decoder's owner collects its copies until queued; a worker's owner is already running.
fn claim_twin(twins: &TwinRegistry, hash: [u8; 32], from_decoder: bool) -> TwinClaim {
    if !from_decoder {
        return twins.claim(hash);
    }
    let claim = twins.claim_queued(hash);
    if matches!(claim, TwinClaim::Owner) {
        OWNED_TWIN.set(Some(hash));
    }
    claim
}

/// `None` means the scan was cancelled while waiting for decode budget.
fn predecode_one(
    file: DiscoveredFile,
    coord: &ScanCoordinator,
    budget: &Arc<PredecodeBudget>,
    legacy: bool,
    twins: Option<&TwinRegistry>,
    from_decoder: bool,
) -> Option<PreDecoded> {
    #[cfg(test)]
    assert!(
        file.path.file_name().is_none_or(|name| name != "fileid-test-decoder-panic"),
        "injected decoder panic"
    );
    if file.moved_from_known_row {
        return Some(PreDecoded::unread(file));
    }
    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    if mac::enabled() {
        let _ = (coord, budget);
        return Some(mac::predecode(file, legacy, twins, from_decoder));
    }
    {
        let decode_started = Instant::now();

        let mut file_bytes = None;
        let mut content_hash = None;
        let mut legacy_hashes = None;
        let mut exif_data = None;
        // EXIF block read once here for camera, place and date, then handed to the decoder for orientation and colour space.
        let mut primary_exif = None;

        if !file.online_only {
            match file.kind {
                FileKind::Image
                    if file.size_bytes <= crate::util::content_hash::FULL_HASH_MAX_BYTES =>
                {
                    if let Ok(mut f) = open_image_file(&file.path) {
                        // Bounded pre-read: capacity is capped at the 16 MB full-hash window and try_reserve_exact turns a rare allocation failure into a per-file skip instead of an abort() that would kill the whole engine.
                        let cap = (file.size_bytes as usize)
                            .min(crate::util::content_hash::FULL_HASH_MAX_BYTES as usize);
                        let mut bytes = Vec::new();
                        if bytes.try_reserve_exact(cap).is_ok()
                            && std::io::Read::read_to_end(&mut f, &mut bytes).is_ok()
                        {
                            // A successful read always yields Some(_), so non-EXIF formats record an explicit no-EXIF result.
                            let read = read_primary_exif(&bytes);
                            exif_data = Some(read.fields.clone().unwrap_or((None, None, None, None)));
                            primary_exif = Some(read);
                            content_hash = Some(sha256_bytes(&bytes));
                            legacy_hashes = legacy.then(|| LegacyHashes::of_full_bytes(&bytes));
                            file_bytes = Some(bytes);
                        }
                    }
                }
                FileKind::Image => {
                    // Over-cap image (huge RAW/TIFF/PSD or a crafted/sparse file): never read into a Vec (12 decoder threads would OOM, and a bogus size aborts). Hash via the bounded composite and leave file_bytes = None so decode_image_sync mmaps it. EXIF parses from a bounded head read since there is no later fallback.
                    (content_hash, legacy_hashes) = composite_hashes(&file, legacy);
                    exif_data = exif_from_head(&file.path);
                }
                // Doc / PDF / Audio share the single-read pattern when the file fits the 16 MB full-hash window: one buffer feeds SHA-256 and the extractor. Larger files use the composite hash + path-based extractor, bounding peak memory at one file.
                FileKind::Doc | FileKind::Pdf | FileKind::Audio
                    if file.size_bytes <= crate::util::content_hash::FULL_HASH_MAX_BYTES =>
                {
                    if let Ok(mut f) = open_image_file(&file.path) {
                        let mut bytes = Vec::with_capacity((file.size_bytes as usize).min(MAX_PREALLOC_BYTES));
                        if std::io::Read::read_to_end(&mut f, &mut bytes).is_ok() {
                            content_hash = Some(sha256_bytes(&bytes));
                            legacy_hashes = legacy.then(|| LegacyHashes::of_full_bytes(&bytes));
                            file_bytes = Some(bytes);
                        }
                    }
                }
                FileKind::Video => {
                    (content_hash, legacy_hashes) = composite_hashes(&file, legacy);
                    exif_data = video_capture_fields(&file.path);
                }
                // Over-cap Doc/Pdf/Audio and Other: compute the rename-heal hash here via the bounded composite so the async worker never re-opens the file as blocking IO.
                _ => (content_hash, legacy_hashes) = composite_hashes(&file, legacy),
            }
        }

        // Reserve before every full-frame allocation. Header-probeable images charge source buffer plus final RGB; large/path-backed or unsupported images (incl. HEIC) and videos take the whole budget exclusively, bounding the unaccounted BGRA/RGB peak to one decoder. The guard shrinks to retained RGB after success.
        let mut budget_guard = None;

        // Only a full SHA-256 of an image is a shareable identity; a ready twin needs no frame.
        let twin = match (content_hash, file.kind, twins) {
            (Some(hash), FileKind::Image, Some(twins))
                if twin_reuse_enabled()
                    && !file.online_only
                    && file.size_bytes <= crate::util::content_hash::FULL_HASH_MAX_BYTES =>
            {
                claim_twin(twins, hash, from_decoder)
            }
            _ => TwinClaim::None,
        };
        // A copy of an image being analyzed right now isn't decoded either; the worker waits for the owner and decodes only on failure.
        let awaits_twin = matches!(twin, TwinClaim::Ready | TwinClaim::Pending(_));

        // Cloud placeholders: never read content (reading hydrates the file, a surprise network download); emit a metadata-only row like an unsupported kind.
        let mut decoded = if file.online_only || awaits_twin {
            None
        } else {
            match file.kind {
                FileKind::Image => {
                    let reservation = scaled_jpeg_reservation_for(&file.path, file_bytes.as_deref())
                        .or_else(|| file_bytes.as_deref().and_then(probe_image_decode_reservation_bytes))
                        .unwrap_or_else(|| budget.capacity());
                    let (result, guard) = decode_reserved(
                        budget,
                        reservation,
                        coord,
                        || decode_image_sync_with(&file.path, file_bytes.as_deref(), primary_exif.as_ref()),
                    )?;
                    budget_guard = Some(guard);
                    Some(result)
                }
                FileKind::Video => {
                    // Media Foundation negotiates a <=1280px RGB32 working frame; the bounded fallback rejects native BGRA + RGB peaks above this shared reservation before allocating.
                    let (result, guard) = decode_reserved(
                        budget,
                        crate::shell::video::VIDEO_DECODE_RESERVATION_BYTES,
                        coord,
                        || decode_video_keyframe_sync(&file.path),
                    )?;
                    // Without a frame decoder the video is still catalogued; the skip-set video gate backfills it later.
                    if result.is_err() && !video_frames_available() {
                        None
                    } else {
                        budget_guard = Some(guard);
                        Some(result)
                    }
                }
                // A 3D `.obj` renders to RGB for CLIP. A render failure is not a file failure (the model still groups under 3D Models/), so Err maps to None; non-obj 3D formats produce no frame.
                FileKind::Model => {
                    let is_obj = file
                        .path
                        .extension()
                        .and_then(|s| s.to_str())
                        .is_some_and(|e| e.eq_ignore_ascii_case("obj"));
                    if is_obj {
                        let (result, guard) = decode_reserved(
                            budget,
                            budget.capacity(),
                            coord,
                            || crate::pipeline::obj_render::render_obj_to_rgb(&file.path),
                        )?;
                        match result {
                            Ok(frame) => {
                                budget_guard = Some(guard);
                                Some(Ok(frame))
                            }
                            Err(_) => None,
                        }
                    } else {
                        None
                    }
                }
                _ => None,
            }
        };
        if decoded.is_some() {
            STATS_DECODE_US.fetch_add(decode_started.elapsed().as_micros() as u64, Ordering::Relaxed);
            STATS_DECODED_FILES.fetch_add(1, Ordering::Relaxed);
        }
        // Doc text is extracted on the decoder thread (cheap I/O), reusing file_bytes (≤ 16 MB) when pre-read, else the extractor opens the path.
        let doc_text = if file.online_only {
            None
        } else {
            match file.kind {
                FileKind::Doc | FileKind::Pdf => {
                    crate::pipeline::doc_extract::extract(&file.path, file_bytes.as_deref())
                        .ok()
                        .flatten()
                }
                FileKind::Other => crate::pipeline::archive_names::list(&file.path),
                _ => None,
            }
        };
        // Audio container metadata via symphonia (pure Rust); reuses file_bytes when present.
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
        // Video, WIC and OBJ frames come from decoders without a pixel check of
        // their own; every consumer downstream indexes by w*h*3.
        if let Some(Ok((rgb, w, h))) = &decoded {
            let expected = (*w as usize).checked_mul(*h as usize).and_then(|p| p.checked_mul(3));
            if *w == 0 || *h == 0 || expected != Some(rgb.len()) {
                decoded = Some(Err(anyhow::anyhow!("decoder returned a {w}x{h} frame of {} bytes", rgb.len())));
            }
        }
        let frame_bytes = match &decoded {
            Some(Ok((rgb, _, _))) => rgb.len(),
            _ => 0,
        };
        if frame_bytes > 0 {
            let covered = budget_guard
                .as_mut()
                .is_some_and(|guard| guard.shrink_to(frame_bytes));
            if !covered {
                decoded = Some(Err(anyhow::anyhow!(
                    "decoded frame exceeded its pre-allocation reservation"
                )));
                budget_guard = None;
            }
        } else {
            // Decode failed or produced no frame: release any pre-decode reservation immediately.
            budget_guard = None;
        }
        Some(PreDecoded {
            file,
            decoded,
            doc_text,
            audio_tags,
            content_hash,
            legacy_hashes,
            exif: exif_data,
            twin,
            budget: budget_guard,
            mac: None,
        })
    }
}

/// Sequential-read open on Windows: FILE_FLAG_SEQUENTIAL_SCAN keeps the cache manager from polluting the standby list and is friendlier to real-time AV scanning.
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

/// Opens a file for decoder-thread pre-reading with `\\?\` long-path support and a short backoff for transient sharing/lock violations (another app writing, AV scanner); genuine errors fail fast.
pub(crate) fn open_image_file(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    let p = crate::util::path_safety::to_extended_length(path);
    let mut last_err: Option<std::io::Error> = None;
    for attempt in 0..3u32 {
        match open_read_sequential(&p) {
            Ok(f) => return Ok(f),
            Err(e) => {
                // ERROR_SHARING_VIOLATION (32) / ERROR_LOCK_VIOLATION (33) are the only retryable cases.
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

/// Decodes an image to RGB8 on the calling thread, rejecting images over `MAX_DECODED_PIXELS` before commit; `catch_unwind` turns a panicking codec into Err.
/// On Windows it falls back to the WinRT BitmapDecoder (HEIF, camera RAW, system codecs) when image-rs fails; a missing codec only marks that file undecodable.
pub(crate) fn decode_image_sync(path: &std::path::Path, bytes: Option<&[u8]>) -> anyhow::Result<(Vec<u8>, u32, u32)> {
    decode_image_sync_with(path, bytes, None)
}

fn decode_image_sync_with(
    path: &std::path::Path,
    bytes: Option<&[u8]>,
    exif: Option<&PrimaryExif>,
) -> anyhow::Result<(Vec<u8>, u32, u32)> {
    let is_raw = path
        .extension()
        .and_then(|s| s.to_str())
        .is_some_and(super::raw_preview::is_raw_extension);
    if is_raw
        && let Some(out) = decode_raw_preview(path, bytes) {
            return Ok(out);
        }
    let primary = decode_image_sync_imagecrate(path, bytes, exif);
    if primary.is_ok() {
        return primary;
    }
    // WIC may provide codecs image-rs lacks (HEIF, camera RAW); decode_image_sync is only called for files already classified as images.
    #[cfg(windows)]
    {
        match shell::heic::decode(path) {
            Ok(out) => return Ok(out),
            Err(wic_err) => {
                tracing::debug!(?wic_err, "Windows bitmap decoder fallback did not decode image");
            }
        }
    }
    // Linux (libheif CLI tools) and macOS (sips): best-effort HEIC/HEIF via shell::heic::decode; if the tools are absent or fail, fall through to the image-rs error so the file is just skipped.
    #[cfg(not(windows))]
    {
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        if (ext == "heic" || ext == "heif")
            && let Ok(out) = shell::heic::decode(path) {
                return Ok(out);
            }
    }
    primary
}

/// Camera RAW: largest embedded JPEG preview (≥ 1024 px) with the container's orientation; None falls back to the full decoders.
fn decode_raw_preview(path: &std::path::Path, bytes: Option<&[u8]>) -> Option<(Vec<u8>, u32, u32)> {
    decode_raw_preview_at_least(path, bytes, 1024, JPEG_SCALED_DECODE_FLOOR_EDGE)
}

/// Upright RGB of a RAW's largest embedded preview with long edge ≥ `min_edge`, DCT-scaled to no less than `scale_floor`; thumbnails accept smaller previews than the scan.
pub(crate) fn decode_raw_preview_at_least(
    path: &std::path::Path,
    bytes: Option<&[u8]>,
    min_edge: u32,
    scale_floor: u32,
) -> Option<(Vec<u8>, u32, u32)> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mmap;
        let bytes: &[u8] = if let Some(pre_read) = bytes {
            pre_read
        } else {
            let file = open_image_file(path).ok()?;
            mmap = unsafe { memmap2::Mmap::map(&file).ok()? };
            &mmap
        };
        let jpeg = super::raw_preview::largest_embedded_jpeg(bytes)?;
        let (w, h) = image::ImageReader::with_format(std::io::Cursor::new(jpeg), image::ImageFormat::Jpeg)
            .into_dimensions()
            .ok()?;
        if w.max(h) < min_edge {
            return None;
        }
        let orientation = raw_orientation(bytes, jpeg);
        let (buf, w, h) = decode_jpeg_scaled_to(jpeg, scale_floor).or_else(|| {
            let rgb = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg).ok()?.into_rgb8();
            let (w, h) = rgb.dimensions();
            Some((rgb.into_raw(), w, h))
        })?;
        apply_exif_orientation(buf, w, h, orientation).ok()
    }))
    .ok()
    .flatten()
}

/// Conservative reservation for an image decoded from pre-read bytes: decoder output plus a possible RGB8 conversion. Only headers are read, so admission precedes allocation.
pub(crate) fn probe_image_decode_reservation_bytes(bytes: &[u8]) -> Option<usize> {
    use image::ImageDecoder;
    use std::io::Cursor;

    let decoder = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_decoder()
        .ok()?;
    let (w, h) = decoder.dimensions();
    let pixels = u64::from(w).checked_mul(u64::from(h))?;
    if pixels == 0 || pixels > MAX_DECODED_PIXELS {
        return None;
    }
    let final_rgb = pixels.checked_mul(3)?;
    usize::try_from(decoder.total_bytes().checked_add(final_rgb)?).ok()
}

/// Budget charge for a sequential JPEG that `decode_jpeg_scaled_to` decodes at a DCT-reduced scale: component planes + interleaved RGB + orientation copy at scaled size. Mirrors its eligibility checks; `None` keeps the full-frame reservation.
fn scaled_jpeg_reservation(bytes: &[u8]) -> Option<usize> {
    if !jpeg_scaled_decode_enabled() || !bytes.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let floor = JPEG_SCALED_DECODE_FLOOR_EDGE;
    let mut dec = jpeg_decoder::Decoder::new(std::io::Cursor::new(bytes));
    dec.read_info().ok()?;
    let info = dec.info()?;
    if info.coding_process != jpeg_decoder::CodingProcess::DctSequential
        || !matches!(info.pixel_format, jpeg_decoder::PixelFormat::RGB24 | jpeg_decoder::PixelFormat::L8)
    {
        return None;
    }
    let (pw, ph) = (u32::from(info.width), u32::from(info.height));
    if u64::from(pw) * u64::from(ph) > MAX_DECODED_PIXELS || pw.max(ph) < floor.saturating_mul(2) {
        return None;
    }
    let long = pw.max(ph);
    let req = |d: u32| -> u16 { ((u64::from(d) * u64::from(floor)) / u64::from(long)).clamp(1, u64::from(u16::MAX)) as u16 };
    let (ow, oh) = dec.scale(req(pw), req(ph)).ok()?;
    (usize::from(ow)).checked_mul(usize::from(oh))?.checked_mul(9)
}

fn scaled_jpeg_reservation_for(path: &std::path::Path, bytes: Option<&[u8]>) -> Option<usize> {
    if let Some(bytes) = bytes {
        return scaled_jpeg_reservation(bytes);
    }
    if !jpeg_scaled_decode_enabled() {
        return None;
    }
    let file = open_image_file(path).ok()?;
    // SAFETY: read-only map of a file opened for reading, dropped before returning.
    let map = unsafe { memmap2::Mmap::map(&file).ok()? };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| scaled_jpeg_reservation(&map))).ok().flatten()
}

fn jpeg_scaled_decode_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("FILEID_JPEG_SCALED_DECODE").map_or(true, |v| v.trim() != "0"))
}

/// A camera JPEG's MPF large preview, decoded in place of the full frame (the Mac engine's `JPEGPreview.decode` rules): full image ≥ a third larger than `floor`, tagged sRGB, upright, whole frame, long side ≥ two thirds of `floor`.
/// Native size up to a third above `floor`, DCT-scaled above. The caller applies the primary orientation; `None` keeps the existing paths. `FILEID_JPEG_MPF_PREVIEW=0` disables it.
#[cfg(test)]
pub(crate) fn decode_mpf_preview(bytes: &[u8], primary_w: u32, primary_h: u32, floor: u32) -> Option<(Vec<u8>, u32, u32)> {
    decode_mpf_preview_with(bytes, primary_w, primary_h, floor, None)
}

fn decode_mpf_preview_with(
    bytes: &[u8],
    primary_w: u32,
    primary_h: u32,
    floor: u32,
    srgb: Option<bool>,
) -> Option<(Vec<u8>, u32, u32)> {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ENABLED.get_or_init(|| std::env::var("FILEID_JPEG_MPF_PREVIEW").map_or(true, |v| v.trim() != "0")) {
        return None;
    }
    let cap = floor.saturating_mul(4) / 3;
    if primary_w.max(primary_h) < cap || !srgb.unwrap_or_else(|| exif_is_srgb(bytes)) {
        return None;
    }
    let preview = super::mpf_preview::largest_preview(bytes)?;
    let (w, h) = image::ImageReader::with_format(std::io::Cursor::new(preview), image::ImageFormat::Jpeg)
        .into_dimensions()
        .ok()?;
    if !super::mpf_preview::same_shape(w, h, primary_w, primary_h) || w.max(h) * 3 < floor * 2 {
        return None;
    }
    if exif_orientation(preview).is_some_and(|o| o != image::metadata::Orientation::NoTransforms) {
        return None;
    }
    let decoded = if w.max(h) < cap {
        let rgb = image::load_from_memory_with_format(preview, image::ImageFormat::Jpeg).ok()?.into_rgb8();
        let (w, h) = rgb.dimensions();
        (rgb.into_raw(), w, h)
    } else {
        decode_jpeg_scaled_to(preview, floor).or_else(|| {
            let rgb = image::load_from_memory_with_format(preview, image::ImageFormat::Jpeg).ok()?.into_rgb8();
            let (w, h) = rgb.dimensions();
            Some((rgb.into_raw(), w, h))
        })?
    };
    STATS_MPF_PREVIEWS.fetch_add(1, Ordering::Relaxed);
    Some(decoded)
}

/// EXIF ColorSpace == 1 (sRGB); false for Adobe RGB, uncalibrated files or no EXIF, whose preview colour may not match.
pub(crate) fn exif_is_srgb(bytes: &[u8]) -> bool {
    let Ok(exif) = exif::Reader::new().read_from_container(&mut std::io::Cursor::new(bytes)) else {
        return false;
    };
    exif.get_field(exif::Tag::ColorSpace, exif::In::PRIMARY)
        .and_then(|field| field.value.get_uint(0))
        == Some(1)
}

/// Minimum long edge when DCT-scaling a JPEG. Detection, tagging and CLIP downsample far below it; only SFace's 112² crops read decode resolution, and 2048 px keeps faces wider than ~5 % of the frame at full crop fidelity (corpus-validated: identical RAM++ tags and face counts, embedding cosine ≥ 0.99).
const JPEG_SCALED_DECODE_FLOOR_EDGE: u32 = 2048;

/// DCT-scaled decode for oversized JPEGs via jpeg-decoder (zune-jpeg has no scaling API); None means use the full decoder (small images, non-RGB/L8 output, any error).
fn decode_jpeg_scaled(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    decode_jpeg_scaled_to(bytes, JPEG_SCALED_DECODE_FLOOR_EDGE)
}

pub(crate) fn decode_jpeg_scaled_to(bytes: &[u8], floor: u32) -> Option<(Vec<u8>, u32, u32)> {
    let floor = floor.max(1);
    // FILEID_JPEG_SCALED_DECODE=0 restores full-resolution decode (also the A/B control).
    if !jpeg_scaled_decode_enabled() {
        return None;
    }
    let mut dec = jpeg_decoder::Decoder::new(std::io::Cursor::new(bytes));
    dec.read_info().ok()?;
    let info = dec.info()?;
    let (pw, ph) = (u32::from(info.width), u32::from(info.height));
    // Progressive JPEGs keep full-resolution coefficients whatever the DCT
    // scale, so the header size, not the output size, bounds the allocation.
    if u64::from(pw) * u64::from(ph) > MAX_DECODED_PIXELS {
        return None;
    }
    let long = pw.max(ph);
    // DCT scales are powers of two; below 2× the floor even 1/2 would land
    // under it, so only genuinely oversized frames take this path.
    if long < floor.saturating_mul(2) {
        return None;
    }
    let req = |d: u32| -> u16 {
        ((u64::from(d) * u64::from(floor)) / u64::from(long))
            .clamp(1, u64::from(u16::MAX)) as u16
    };
    // jpeg-decoder picks the smallest DCT scale whose output covers the
    // request, so the long edge stays >= the floor.
    dec.scale(req(pw), req(ph)).ok()?;
    let pixels = dec.decode().ok()?;
    let out = dec.info()?;
    let (w, h) = (u32::from(out.width), u32::from(out.height));
    let rgb = match out.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => pixels,
        jpeg_decoder::PixelFormat::L8 => {
            let mut rgb = Vec::with_capacity(pixels.len() * 3);
            for l in pixels {
                rgb.extend_from_slice(&[l, l, l]);
            }
            rgb
        }
        _ => return None,
    };
    if rgb.len() != (w as usize) * (h as usize) * 3 {
        return None;
    }
    Some((rgb, w, h))
}

fn decode_image_sync_imagecrate(
    path: &std::path::Path,
    bytes: Option<&[u8]>,
    exif: Option<&PrimaryExif>,
) -> anyhow::Result<(Vec<u8>, u32, u32)> {
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
        // Oversized JPEGs decode at a DCT-reduced scale (full decode is memory-bandwidth-bound; every vision consumer downsamples far below the floor); any failure falls through to the full decoder.
        // Orientation is read once from the same bytes both paths use; neither the image crate nor the scaled path applies it.
        let orientation = exif.map_or_else(|| exif_orientation(bytes), |exif| exif.orientation);
        if reader.format() == Some(image::ImageFormat::Jpeg) {
            let srgb = exif.map(|exif| exif.srgb);
            if let Some((buf, w, h)) = decode_mpf_preview_with(bytes, pw, ph, JPEG_SCALED_DECODE_FLOOR_EDGE, srgb) {
                return apply_exif_orientation(buf, w, h, orientation);
            }
            if let Some((buf, w, h)) = decode_jpeg_scaled(bytes) {
                return apply_exif_orientation(buf, w, h, orientation);
            }
        }
        let dyn_img = reader.decode().map_err(|e| anyhow::anyhow!("decode: {e}"))?;
        let rgb = dyn_img.into_rgb8();
        let (w, h) = rgb.dimensions();
        apply_exif_orientation(rgb.into_raw(), w, h, orientation)
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

/// macOS gets video frames only from AVFoundation in the Mac app's engine or FileID's optional FFmpeg runtime; Windows (Media Foundation) and Linux (system ffmpeg) always have a decoder.
#[cfg(all(not(windows), not(target_os = "linux")))]
pub(crate) fn video_frames_available() -> bool {
    cfg!(all(target_os = "macos", feature = "apple-adapters")) || crate::commands::ffmpeg_runtime_available()
}
#[cfg(any(windows, target_os = "linux"))]
pub(crate) fn video_frames_available() -> bool {
    true
}

fn decode_video_keyframe_sync(path: &std::path::Path) -> anyhow::Result<(Vec<u8>, u32, u32)> {
    let frame = shell::video::keyframe_25pct(path)?;
    Ok((frame.rgb, frame.width, frame.height))
}

/// Per-file ML body over a pre-decoded image or keyframe: face detect → embed, CLIP embed, dHash, EXIF, each gated on its semaphore and the model being installed. A failing stage is non-fatal (field = None, `failed=0`); only image decode failure marks the row failed.
async fn process_file_predecoded(
    predecoded: PreDecoded,
    models: &Arc<ModelStack>,
    vision_sem: &Arc<Semaphore>,
    clip_sem: &Arc<Semaphore>,
    worker_idx: usize,
    coord: &ScanCoordinator,
) -> TaggedFile {
    // `_budget` keeps the byte-budget reservation alive until this function returns.
    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    if predecoded.mac.is_some() {
        let _ = (vision_sem, clip_sem, worker_idx, coord);
        return mac::process(predecoded, models).await;
    }
    let PreDecoded { file, decoded, doc_text, audio_tags, content_hash, legacy_hashes, exif, twin, budget: _budget, mac: _ } =
        predecoded;
    let file = &file;
    let started = Instant::now();
    let mut tagged = TaggedFile::bare(file);

    // Content identity for rename/move heal comes from the decoder, so this async worker never re-opens the file; cloud placeholders stay None so an online-only file is never hydrated.
    if !file.online_only {
        tagged.content_hash = content_hash;
        tagged.legacy_hashes = legacy_hashes;
    }

    // Doc text was pulled on the decoder thread; a RAKE-style keyword extractor yields auto tag chips and the text is stashed for doc_text/doc_fts.
    // The doc stage "ran" iff extraction was attempted (a non-placeholder doc-kind input); the dbwriter keys its stale doc_text/doc_fts DELETE on it so empty re-extractions clear phantom FTS hits.
    tagged.doc_stage_ran = (matches!(file.kind, FileKind::Doc | FileKind::Pdf)
        || (file.kind == FileKind::Other
            && file.path.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip"))))
        && !file.online_only;
    // The content-derivation stage ran iff doc-text extraction was attempted or a model render; persisted to text_stage_done so the BGE and model-CLIP carve-outs stop re-walking files that can never embed.
    tagged.text_stage_done =
        tagged.doc_stage_ran || (matches!(file.kind, FileKind::Model) && !file.online_only);
    if let Some(text) = doc_text
        && !text.trim().is_empty() {
            let keywords = if file.kind == FileKind::Other { Vec::new() } else { crate::util::keywords::extract(&text) };
            for (label, score) in keywords {
                tagged.tags.push((label, Some(score)));
            }
            // BGE-small embedding is sync inference under a single-Session Mutex, so it runs on the blocking pool: on an async worker each waiting doc would pin a runtime thread and starve IPC. Skipped when BGE isn't installed.
            if !coord.is_gpu_dead() && models.bge_text.is_some() && file.kind != FileKind::Other {
                let (embed_models, embed_text) = (models.clone(), text.clone());
                let embedded = tokio::task::spawn_blocking(move || {
                    embed_models.bge_text.as_ref().map(|bge| bge.lock().embed(&embed_text))
                })
                .await
                .unwrap_or_else(|join| Some(Err(anyhow::anyhow!("BGE embedding task failed: {join}"))));
                if let Some(result) = embedded {
                    match result {
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

    // Audio metadata becomes tag chips; score=None since these are facts, not model probabilities.
    tagged.tags.extend(audio_tags);

    if file.kind == FileKind::Video
        && let Some(captured) = exif.as_ref().and_then(|fields| fields.3)
    {
        tagged.created_unix = Some(captured);
        tagged.captured_from_exif = true;
    }

    // A byte-identical image already analyzed: keep the file's own EXIF, year and camera chips and let the writer copy the twin's rows.
    let reuse = matches!(twin, TwinClaim::Ready);
    if reuse && matches!(file.kind, FileKind::Image) {
        if let Some((cam, lat, lon, captured)) = exif {
            tagged.camera_model = cam;
            tagged.location_lat = lat;
            tagged.location_lon = lon;
            if let Some(captured) = captured {
                tagged.created_unix = Some(captured);
                tagged.captured_from_exif = true;
            }
        }
        push_enriched_extras(&mut tagged);
        tagged.reuse_twin = true;
        tagged.total_ms = started.elapsed().as_secs_f64() * 1000.0;
        STATS_TWIN_REUSED.fetch_add(1, Ordering::Relaxed);
        record_stage(&STATS_TOTAL_US, started);
        maybe_emit_stats();
        return tagged;
    }

    // Once the GPU is marked dead (TDR or device removed), the ML pipeline is skipped for the rest of the process.
    // failed=true: the file never reached the ML stages and the skip-set only skips failed=0 rows, so the next scan re-processes it.
    if coord.is_gpu_dead() {
        // Only files that need the skipped GPU stages (images, videos, .obj models) are marked failed for retry; Doc/Pdf/Audio already finished CPU-only extraction.
        let needed_gpu = matches!(file.kind, FileKind::Image | FileKind::Video | FileKind::Model);
        tagged.failed = needed_gpu;
        if needed_gpu {
            tagged.error_message = Some(
                "GPU device removed mid-scan; file not processed (will retry next scan)".into(),
            );
        } else {
            // Doc/Audio finished CPU-only extraction before the GPU died: mark the set authoritative, else the dbwriter drops the tags and failed=false strands the row in the skip-set.
            tagged.tags_evaluated = true;
        }
        tagged.total_ms = started.elapsed().as_secs_f64() * 1000.0;
        return tagged;
    }

    // Decode happened upstream; an image-decode failure emits a failed row so the DB records the file and the next scan retries it.
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

    let mut ram_plus_ran = false;
    let mut defer_ram = false;
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
                if let Some((cam, lat, lon, captured)) = exif {
                    tagged.camera_model = cam;
                    tagged.location_lat = lat;
                    tagged.location_lon = lon;
                    if let Some(captured) = captured {
                        tagged.created_unix = Some(captured);
                        tagged.captured_from_exif = true;
                    }
                }
                record_stage(&STATS_EXIF_US, exif_started);
                perf_trace("exif_done", &file.path, exif_started.elapsed().as_secs_f64() * 1000.0);
            }

                let dhash_started = Instant::now();
                tagged.phash = Some(compute_dhash(&rgb, w as usize, h as usize));
                record_stage(&STATS_DHASH_US, dhash_started);
                perf_trace("dhash_done", &file.path, dhash_started.elapsed().as_secs_f64() * 1000.0);

                // Short-circuit GPU stages after a device-removed: new work against a dead GPU wedges the system when TDR fires.
                let face_cap = face_max_per_image();
                let gpu_alive = !coord.is_gpu_dead();
                if gpu_alive
                && let (Some(scrfd_pool), Some(arcface_pool)) = (&models.scrfd, &models.arcface) {
                    let vwait = Instant::now();
                    let permit = vision_sem.acquire().await;
                    STATS_VISION_WAIT_US.fetch_add(vwait.elapsed().as_micros() as u64, Ordering::Relaxed);
                    let vision_started = Instant::now();
                    if permit.is_ok() && !coord.is_gpu_dead() {
                        let scrfd_started = Instant::now();
                        let detections = YuNet::detect_shared(scrfd_pool, worker_idx, &rgb, w, h);
                        perf_trace("scrfd_done", &file.path, scrfd_started.elapsed().as_secs_f64() * 1000.0);
                        let arcface_started = Instant::now();
                        match detections {
                            Ok(dets) => {
                                let mut candidates: Vec<FaceCandidate> = dets
                                    .into_iter()
                                    .enumerate()
                                    .filter_map(|(ordinal, detection)| {
                                        let quality = face_geometry::validate_face_geometry(&detection, w, h)?;
                                        let bbox_xywh = [
                                            detection.bbox[0],
                                            detection.bbox[1],
                                            (detection.bbox[2] - detection.bbox[0]).max(0.0),
                                            (detection.bbox[3] - detection.bbox[1]).max(0.0),
                                        ];
                                        Some(FaceCandidate {
                                            ordinal,
                                            detection,
                                            bbox_xywh,
                                            quality,
                                        })
                                    })
                                    .collect();
                                prioritize_face_candidates(&mut candidates, face_cap);

                                for candidate in candidates {
                                    if coord.is_gpu_dead() || tagged.faces.len() >= face_cap {
                                        break;
                                    }
                                    let FaceCandidate {
                                        detection,
                                        bbox_xywh,
                                        quality,
                                        ..
                                    } = candidate;
                                    // Aligned 112×112 (5-pt similarity to the ArcFace template) for SFace; plain bbox crop if the landmark fit is degenerate.
                                    let crop = face_align::align_112(
                                        &rgb,
                                        w,
                                        h,
                                        &detection.landmarks,
                                    )
                                    .or_else(|| crop_and_resize_face(&rgb, w, h, &bbox_xywh));
                                    if let Some(crop) = crop {
                                        let embed_result = {
                                            let mut a = crate::models::checkout(arcface_pool, worker_idx);
                                            a.embed(&crop).map(|embedding| (embedding, a.model_version().to_owned()))
                                        };
                                        match embed_result {
                                            Ok((emb, embedding_model)) => {
                                                let pose = face_geometry::estimate_pose(&detection.landmarks);
                                                let img_area = (w as f32) * (h as f32);
                                                let area_fraction = if img_area > 0.0 {
                                                    (bbox_xywh[2] * bbox_xywh[3]) / img_area
                                                } else {
                                                    0.0
                                                };
                                                let min_dim_px = bbox_xywh[2].min(bbox_xywh[3]);
                                                tagged.faces.push(DetectedFace {
                                                    bbox: bbox_xywh,
                                                    embedding: emb,
                                                    embedding_model: Some(embedding_model),
                                                    roll: pose.roll,
                                                    yaw: pose.yaw,
                                                    pitch: pose.pitch,
                                                    quality,
                                                    excluded: face_is_excluded(
                                                        pose.yaw,
                                                        pose.pitch,
                                                        area_fraction,
                                                        min_dim_px,
                                                    ),
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
                // The face stage ran iff the GPU was alive at entry, the models were loaded and the GPU did not die mid-pass; the dbwriter's stale-face DELETE keys on it (see `faces_evaluated`).
                tagged.faces_evaluated =
                    gpu_alive && models.scrfd.is_some() && models.arcface.is_some() && !coord.is_gpu_dead();
                tagged.has_faces = !tagged.faces.is_empty();

                if !coord.is_gpu_dead() {
                if let Some(clip_pool) = &models.mobileclip_pool {
                    let permit = clip_sem.acquire().await;
                    let clip_started = Instant::now();
                    if permit.is_ok() && !coord.is_gpu_dead() {
                        let resized = resize_rgb_quality(&rgb, w as usize, h as usize, 224, 224);
                        let embed_result = {
                            let mut c = crate::models::checkout(clip_pool, worker_idx);
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

                // RAM++ is the primary in-scan tagger (4585-tag vocabulary); it supersedes the CLIP zero-shot labeler (gated on !ram_plus_ran) and runs after the MobileCLIP embed so semantic-search embeddings are always computed.
                // `vision_sem` GPU budget; device-removed → cancel the scan.
                ram_plus_ran = false;
                // Deferred mode leaves RAM++ (and the text read its tags decide) to the catch-up pass; the row gets the scene labels now.
                defer_ram = matches!(file.kind, FileKind::Image)
                    && crate::pipeline::catch_up::ram_plus_deferred(false)
                    && (models.ram_plus.is_some() || models.ram_plus_batch.is_some());
                if !coord.is_gpu_dead() && !defer_ram {
                    // The batch coordinator and the per-worker pool yield the same Result; the batch path needs no vision_sem since the coordinator serializes batches.
                    let ram_started = Instant::now();
                    let tag_result: Option<anyhow::Result<Vec<(String, f32)>>> =
                        if let Some(ram_coord) = &models.ram_plus_batch {
                            let side = crate::models::ram_plus::INPUT_SIZE;
                            match crate::models::ram_plus::RamPlusTagger::resize_to_input(&rgb, w, h) {
                                Ok(input) => Some(ram_coord.tag(input, side, side).await),
                                Err(e) => Some(Err(e)),
                            }
                        } else if let Some(ram_pool) = &models.ram_plus {
                            // Run the CPU preprocess (resize + ImageNet-normalize) BEFORE acquiring the GPU permit and session Mutex so it overlaps other workers' forwards; the lock and permit wrap only the forward pass.
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
                                    if permit.is_ok() && !coord.is_gpu_dead() {
                                        let mut g = crate::models::checkout(ram_pool, worker_idx);
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
                                let tags = without_people_words(tags, tagged.faces_evaluated && tagged.faces.is_empty());
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
                                tracing::debug!(
                                    target: "FileIDEngine::tagging",
                                    path = %redacted,
                                    ram_emit_count,
                                    max_score,
                                    "[TAGGING] ram_plus_summary"
                                );
                                // RAM++ counts as satisfying the tagger contract only if it emitted content tags: a zero-tag success must not suppress the CLIP scene-tag fallback, or the file gets only Year/camera chips.
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

                // Scene tags: CLIP zero-shot (CPU mat-vec + softmax, so no semaphore or GPU-dead guard), each label keeping its softmax probability.
                // Fallback only when RAM++ did not tag, so a RAM++-tagged file isn't double-tagged with noisier CLIP guesses.
                if !ram_plus_ran {
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
                        // One-line per-file summary at info level so "only year" chips can be traced to scene tags emitted vs gated out.
                        tracing::info!(
                            target: "FileIDEngine::tagging",
                            path = %redacted,
                            scene_emit_count,
                            max_score,
                            "[TAGGING] scene_summary"
                        );
                        perf_trace("scene_tags_done", &file.path, scene_started.elapsed().as_secs_f64() * 1000.0);
                    } else if matches!(file.kind, FileKind::Image | FileKind::Video) {
                        // Embedding or labeler missing: logged so the cause is visible; skipped for non-visual kinds that have no CLIP path.
                        tracing::info!(
                            target: "FileIDEngine::tagging",
                            path = %redacted,
                            has_embedding = tagged.clip_embedding.is_some(),
                            has_labeler = models.scene_labeler.is_some(),
                            "[TAGGING] scene_skipped"
                        );
                    }
                }

                // OCR is the biggest CPU per file (~30-50 ms) and finds nothing useful on camera photos (EXIF camera_model present), so skip those; documents/screenshots still run it. Also skipped when the GPU is known dead to avoid device re-init after a TDR.
                if matches!(file.kind, FileKind::Image) && !coord.is_gpu_dead() && !defer_ram && should_run_ocr(&file.path, &tagged, file.size_bytes) {
                    // Stage ran whether or not text came back, so the dbwriter can clear stale ocr_text/ocr_fts on a re-process that yields nothing.
                    tagged.ocr_stage_ran = true;
                    let ocr_started = Instant::now();
                    // Moved, not cloned: this is the last use of `rgb` in the block.
                    if let Ok(Some(text)) = run_ocr_blocking(rgb, w, h).await
                        && !text.trim().is_empty() {
                            tagged.has_text = true;
                            tagged.ocr_text = Some(text);
                        }
                    STATS_OCR_RAN.fetch_add(1, Ordering::Relaxed);
                    record_stage(&STATS_OCR_US, ocr_started);
                    perf_trace("ocr_done", &file.path, ocr_started.elapsed().as_secs_f64() * 1000.0);
                }
    }

    // Enriched extras: Year + camera family from existing signals (no inference), filling the chip row when scene tags missed the threshold; mirrors macOS `Tagging.swift::extraTags`. Year and camera belong to images only; every other kind starts with its kind tag.
    let extras = if file.kind == FileKind::Image {
        enriched_extras(tagged.created_unix, tagged.modified_unix, tagged.camera_model.as_deref())
    } else {
        Vec::new()
    };
    if let Some(label) = kind_tag(file.kind) {
        tagged.tags.insert(0, (label.to_string(), None));
    }
    let stored_in_place: Vec<String> = tagged.tags.iter().map(|(tag, _)| tag.clone()).collect();
    // Dedupe case-insensitively keeping the first occurrence (classifier confidence order) and cap at 16 so noisy output can't explode the tags table. The catch-up pass uses the same function.
    tagged.tags = crate::pipeline::catch_up::assemble_tags(
        Vec::new(),
        std::mem::take(&mut tagged.tags),
        extras.clone(),
        stored_tag_cap(),
        crate::pipeline::catch_up::ascii_fold,
    );

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
    // Mark the tag set authoritative only if the GPU survived the pass AND a decode happened; otherwise `tags` is partial/empty and must not replace persisted auto-tags (timeout and GPU-dead-bail rows never reach here).
    // If the GPU died during this file's ML stages, an image/video got neither complete face detect nor embed: mark it failed so the skip-set re-processes it next scan.
    if coord.is_gpu_dead()
        && matches!(file.kind, FileKind::Image | FileKind::Video)
        && !tagged.failed
    {
        tagged.failed = true;
        tagged.error_message.get_or_insert_with(|| {
            "GPU device removed mid-scan; file not fully processed (will retry next scan)".into()
        });
    }
    // A visual file's tag set is authoritative only when a tagger was available to evaluate it: with RAM++ absent and no usable CLIP scene path, `tags` holds only Year/camera extras, and tags_evaluated would make the dbwriter wipe persisted content tags.
    let visual_tagger_ran = ram_plus_ran
        || (models.scene_labeler.is_some() && tagged.clip_embedding.is_some());
    tagged.tags_evaluated = !coord.is_gpu_dead()
        && !tagged.failed
        && !file.online_only
        && (!matches!(file.kind, FileKind::Image | FileKind::Video) || visual_tagger_ran);
    if defer_ram && !tagged.failed && !coord.is_gpu_dead() {
        // RAM++ will supply the tags, so the stored set stands in for it and is authoritative.
        tagged.tags_evaluated = true;
        tagged.catch_up = Some(crate::pipeline::catch_up::Marker {
            stages: crate::pipeline::catch_up::TAGS_OWED,
            vision: crate::pipeline::catch_up::Context {
                k: stored_in_place,
                f: Some(tagged.faces_evaluated),
                m: Some(tagged.camera_model.is_some()),
                x: Some(extras),
                ..Default::default()
            }
            .encoded(),
            face_count: tagged.faces.len() as i64,
        });
    }
    tagged
}

fn without_people_words(tags: Vec<(String, f32)>, no_face_found: bool) -> Vec<(String, f32)> {
    if !no_face_found {
        return tags;
    }
    tags.into_iter()
        .filter(|(label, _)| !crate::models::ram_plus::PEOPLE_WORDS.contains(&label.to_ascii_lowercase().as_str()))
        .collect()
}

struct PortableCatchUp {
    models: Arc<ModelStack>,
    runtime: tokio::runtime::Handle,
    coord: ScanCoordinator,
}

impl PortableCatchUp {
    fn ram_plus(&self, rgb: &[u8], w: u32, h: u32, preferred: usize) -> Option<anyhow::Result<Vec<(String, f32)>>> {
        use crate::models::ram_plus::RamPlusTagger;
        if let Some(batch) = &self.models.ram_plus_batch {
            let side = crate::models::ram_plus::INPUT_SIZE;
            Some(RamPlusTagger::resize_to_input(rgb, w, h).and_then(|input| self.runtime.block_on(batch.tag(input, side, side))))
        } else {
            let pool = self.models.ram_plus.as_ref()?;
            Some(RamPlusTagger::preprocess_tensor(rgb, w, h).and_then(|chw| crate::models::checkout(pool, preferred).tag_prepared(chw)))
        }
    }
}

impl crate::pipeline::catch_up::Analyzer for PortableCatchUp {
    fn ready(&self, phase: crate::pipeline::catch_up::Phase) -> bool {
        phase == crate::pipeline::catch_up::Phase::Tags
            && (self.models.ram_plus.is_some() || self.models.ram_plus_batch.is_some())
            && !self.coord.is_gpu_dead()
    }

    fn parallelism(&self) -> usize {
        if self.models.ram_plus_batch.is_some() { 8 } else { self.models.pool_size.max(1) }
    }

    fn analyse(
        &self,
        file: &crate::pipeline::catch_up::Pending,
        phase: crate::pipeline::catch_up::Phase,
        context: Option<&crate::pipeline::catch_up::Context>,
    ) -> crate::pipeline::catch_up::Outcome {
        use crate::pipeline::catch_up::{Outcome, Phase};
        let (Some(context), Phase::Tags) = (context, phase) else {
            return Outcome::Stale;
        };
        if self.coord.is_gpu_dead() {
            return Outcome::Leave;
        }
        let Ok((rgb, w, h)) = decode_image_sync(&file.path, None) else {
            return Outcome::Failed;
        };
        let ram = match self.ram_plus(&rgb, w, h, file.file_id as usize) {
            Some(Ok(tags)) => tags,
            Some(Err(err)) if error_has_device_removed_marker(&err) => {
                if self.coord.mark_gpu_dead() {
                    tracing::error!(?err, "[GPU-TDR] RAM++ device-removed during catch-up");
                }
                return Outcome::Leave;
            }
            Some(Err(err)) => {
                tracing::warn!(?err, "RAM++ tag failed during catch-up");
                return Outcome::Failed;
            }
            None => return Outcome::Leave,
        };
        let ram = without_people_words(ram, context.f == Some(true) && file.face_count == 0);
        let has_camera = context.m.unwrap_or(file.camera_model.is_some());
        let extras = context.x.clone().unwrap_or_else(|| {
            enriched_extras(file.created_at, file.modified_at.unwrap_or(0.0), file.camera_model.as_deref())
        });
        Outcome::Done(crate::pipeline::catch_up::portable_update(
            file,
            context,
            ram,
            extras,
            stored_tag_cap(),
            |names| ocr_gate(&file.path, names, w, h, has_camera),
            || match shell::ocr::recognize(&rgb, w, h) {
                Ok(text) => Some(text),
                Err(err) => {
                    tracing::warn!(?err, "OCR failed");
                    None
                }
            },
        ))
    }
}

#[cfg(all(target_os = "macos", feature = "apple-adapters"))]
pub(crate) fn mac_unload_adapters() {
    mac::unload_adapters();
}

pub(crate) fn catch_up_analyzer(
    models: Arc<ModelStack>,
    coord: ScanCoordinator,
) -> Box<dyn crate::pipeline::catch_up::Analyzer + Send> {
    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    if models.apple_scan {
        return Box::new(mac::CatchUp { models });
    }
    Box::new(PortableCatchUp { models, runtime: tokio::runtime::Handle::current(), coord })
}

/// Persisted tags per file (16 on both engines); `FILEID_TAG_CAP` widens it
/// for threshold sweeps that need every candidate stored.
fn stored_tag_cap() -> usize {
    static CAP: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CAP.get_or_init(|| {
        std::env::var("FILEID_TAG_CAP")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|n| (1..=512).contains(n))
            .unwrap_or(16)
    })
}

/// Heuristic OCR gate: photo libraries are 99% camera snapshots where OCR is wasted CPU; documents/screenshots still run it.
fn should_run_ocr(path: &std::path::Path, tagged: &TaggedFile, _size_bytes: u64) -> bool {
    let tag_names: Vec<&str> = tagged.tags.iter().map(|(tag, _)| tag.as_str()).collect();
    ocr_gate(path, &tag_names, tagged.image_width, tagged.image_height, tagged.camera_model.is_some())
}

fn ocr_gate(path: &std::path::Path, tag_names: &[&str], width: u32, height: u32, has_camera: bool) -> bool {
    // Paper, whiteboard and handwriting tags read even on camera photos; the
    // OCR has no handwriting tier, so Deep Analyze's transcription picks up what
    // it mangles (text_reading::needs_transcription).
    if super::text_reading::is_text_bearing(tag_names)
        || super::text_reading::is_document_shape(width, height, has_camera)
    {
        return true;
    }
    if has_camera {
        return false;
    }
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
    if let Some(ext) = path.extension().and_then(|e| e.to_str())
        && ext.eq_ignore_ascii_case("png") {
            return true;
        }
    false
}

/// Year + camera-family chips from signals already on hand (no inference, no I/O), matching macOS `Tagging.swift::extraTags`.
/// `Year_2024` keeps the underscore (the Library formatter strips it). Orientation and capability tags (Has Faces/Text/Location) are deliberately not emitted: they read as noise.
fn push_enriched_extras(tagged: &mut TaggedFile) {
    let extras = enriched_extras(tagged.created_unix, tagged.modified_unix, tagged.camera_model.as_deref());
    tagged.tags.extend(extras.into_iter().map(|tag| (tag, None)));
}

/// The tag every non-image file starts with, as Swift stores it (`Doc`, `PDF`, `Audio`, `Video`, `3D Model`, `Other`).
fn kind_tag(kind: FileKind) -> Option<&'static str> {
    match kind {
        FileKind::Image => None,
        FileKind::Video => Some("Video"),
        FileKind::Pdf => Some("PDF"),
        FileKind::Doc => Some("Doc"),
        FileKind::Audio => Some("Audio"),
        FileKind::Model => Some("3D Model"),
        FileKind::Other => Some("Other"),
    }
}

fn enriched_extras(created_unix: Option<f64>, modified_unix: f64, camera_model: Option<&str>) -> Vec<String> {
    let mut extras = Vec::new();
    // Order matters: the Library card's `TopTwoTags` takes the first two tags, so the most informative signals go first.
    // Year_: creation date when present, else modification date; local timezone; strictly inside (1990, 2100), per macOS `extraTags`. Camera family follows.
    let year_source = created_unix.filter(|&c| c > 0.0).unwrap_or(modified_unix);
    if year_source > 0.0
        && let Some(year) = local_unix_seconds_to_year(year_source)
            && year > 1990 && year < 2100 {
                extras.push(format!("Year_{year}"));
            }
    if let Some(cm) = camera_model {
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
            extras.push(f.to_string());
        }
    }
    extras
}

/// Unix seconds → calendar year in the machine's local timezone, matching macOS's local gregorianCalendar so a New Year's-Eve photo lands in the same year on both platforms.
fn local_unix_seconds_to_year(unix: f64) -> Option<i32> {
    use chrono::{Datelike, Local, TimeZone};
    let secs = unix as i64;
    let nanos = ((unix - secs as f64) * 1_000_000_000.0) as u32;
    match Local.timestamp_opt(secs, nanos) {
        chrono::LocalResult::Single(dt) => Some(dt.year()),
        chrono::LocalResult::Ambiguous(dt, _) => Some(dt.year()),
        chrono::LocalResult::None => None,
    }
}

/// Hard cap on image dimensions: a 100 KB JPEG can decode to 4 GB, so reject before decode. 50 MP ≈ 150 MB RGB8.
pub(crate) const MAX_DECODED_PIXELS: u64 = 50_000_000;

async fn run_ocr_blocking(rgb: Vec<u8>, w: u32, h: u32) -> anyhow::Result<Option<String>> {
    tokio::task::spawn_blocking(move || -> anyhow::Result<Option<String>> {
        match shell::ocr::recognize(&rgb, w, h) {
            Ok(r) => Ok(Some(r)),
            Err(err) => {
                // warn!, not debug!: a non-functional OCR engine (COM not initialized, missing language pack) must be visible.
                tracing::warn!(?err, "OCR failed");
                Ok(None)
            }
        }
    })
    .await?
}

/// EXIF orientation (IFD0 tag 0x0112) or `None`. Camera JPEGs are stored in sensor order with a tag; macOS honours it but the `image` crate and the DCT-scaled path do not, which would feed every consumer (phash, CLIP, RAM++, faces) sideways frames and cost face recall on portraits.
pub(crate) fn exif_orientation(bytes: &[u8]) -> Option<image::metadata::Orientation> {
    let exif = exif::Reader::new()
        .read_from_container(&mut std::io::Cursor::new(bytes))
        .ok()?;
    let raw = exif
        .get_field(exif::Tag::Orientation, exif::In::PRIMARY)?
        .value
        .get_uint(0)?;
    image::metadata::Orientation::from_exif(u8::try_from(raw).ok()?)
}

/// A RAW's orientation: the container's IFD0 tag (ORF, RW2, CR3 need the RAW-aware reader), then generic EXIF, then the preview's own EXIF (the only record Fujifilm RAF keeps).
pub(crate) fn raw_orientation(container: &[u8], jpeg: &[u8]) -> Option<image::metadata::Orientation> {
    super::raw_preview::orientation(container)
        .and_then(|v| image::metadata::Orientation::from_exif(u8::try_from(v).ok()?))
        .or_else(|| exif_orientation(container))
        .or_else(|| exif_orientation(jpeg))
}

/// Rotates/flips a decoded RGB frame upright; a missing tag or the identity orientation returns the buffer untouched.
fn apply_exif_orientation(
    buf: Vec<u8>,
    w: u32,
    h: u32,
    orientation: Option<image::metadata::Orientation>,
) -> anyhow::Result<(Vec<u8>, u32, u32)> {
    let Some(orientation) = orientation else {
        return Ok((buf, w, h));
    };
    if orientation == image::metadata::Orientation::NoTransforms {
        return Ok((buf, w, h));
    }
    let img = image::RgbImage::from_raw(w, h, buf)
        .ok_or_else(|| anyhow::anyhow!("orientation: {w}x{h} does not match the decoded buffer"))?;
    let mut dyn_img = image::DynamicImage::ImageRgb8(img);
    dyn_img.apply_orientation(orientation);
    let rgb = dyn_img.into_rgb8();
    let (ow, oh) = rgb.dimensions();
    Ok((rgb.into_raw(), ow, oh))
}

/// Camera model, latitude, longitude and capture time (Unix seconds).
pub(crate) type ExifFields = (Option<String>, Option<f64>, Option<f64>, Option<f64>);

pub(crate) fn parse_exif_fields(bytes: &[u8]) -> Option<ExifFields> {
    read_primary_exif(bytes).fields
}

struct PrimaryExif {
    fields: Option<ExifFields>,
    orientation: Option<image::metadata::Orientation>,
    srgb: bool,
}

fn read_primary_exif(bytes: &[u8]) -> PrimaryExif {
    match exif::Reader::new().read_from_container(&mut std::io::Cursor::new(bytes)) {
        Ok(exif) => PrimaryExif {
            fields: Some(exif_fields(|tag| exif.get_field(tag, exif::In::PRIMARY))),
            orientation: exif
                .get_field(exif::Tag::Orientation, exif::In::PRIMARY)
                .and_then(|field| field.value.get_uint(0))
                .and_then(|raw| image::metadata::Orientation::from_exif(u8::try_from(raw).ok()?)),
            srgb: exif.get_field(exif::Tag::ColorSpace, exif::In::PRIMARY).and_then(|field| field.value.get_uint(0)) == Some(1),
        },
        Err(_) => PrimaryExif { fields: raw_exif_fields(bytes), orientation: None, srgb: false },
    }
}

/// Camera RAWs the generic reader rejects (ORF, RW2, CR3, RAF), read as ImageIO does: the container's EXIF first, gaps filled from the preview's.
fn raw_exif_fields(bytes: &[u8]) -> Option<ExifFields> {
    use super::raw_preview::RawExif;
    let read = |block: Vec<u8>| {
        exif::Reader::new()
            .continue_on_error(true)
            .read_raw(block)
            .or_else(|error| error.distill_partial_result(|_| {}))
            .ok()
    };
    let container = match super::raw_preview::exif_blocks(bytes) {
        Some(RawExif::Tiff(tiff)) => read(tiff).map(|exif| exif_fields(|tag| exif.get_field(tag, exif::In::PRIMARY))),
        Some(RawExif::Cr3 { ifd0, exif, gps }) => {
            let [ifd0, exif, gps] = [ifd0, exif, gps].map(|block| block.and_then(|block| read(block.to_vec())));
            Some(exif_fields(|tag| {
                let block = match tag.context() {
                    exif::Context::Exif => exif.as_ref(),
                    exif::Context::Gps => gps.as_ref(),
                    _ => ifd0.as_ref(),
                }?;
                block.get_field(exif::Tag(exif::Context::Tiff, tag.number()), exif::In::PRIMARY)
            }))
        }
        None => None,
    };
    let preview = super::raw_preview::embedded_jpeg_head(bytes)
        .and_then(|jpeg| exif::Reader::new().read_from_container(&mut std::io::Cursor::new(jpeg)).ok())
        .map(|exif| exif_fields(|tag| exif.get_field(tag, exif::In::PRIMARY)));
    match (container, preview) {
        (Some((model, lat, lon, captured)), Some((p_model, p_lat, p_lon, p_captured))) => {
            let (lat, lon) = if lat.is_some() && lon.is_some() { (lat, lon) } else { (p_lat, p_lon) };
            Some((model.or(p_model), lat, lon, captured.or(p_captured)))
        }
        (container, preview) => container.or(preview),
    }
}

fn exif_fields<'a>(field: impl Fn(exif::Tag) -> Option<&'a exif::Field>) -> ExifFields {
    let camera_model = field(exif::Tag::Model)
        .map(|f| f.display_value().to_string().trim_matches('"').to_string())
        .filter(|s| !s.is_empty());
    // A zero denominator or a junk value would store NaN/inf or an impossible place.
    let lat = read_gps_coord(&field, exif::Tag::GPSLatitude, exif::Tag::GPSLatitudeRef)
        .filter(|v| v.is_finite() && v.abs() <= 90.0);
    let lon = read_gps_coord(&field, exif::Tag::GPSLongitude, exif::Tag::GPSLongitudeRef)
        .filter(|v| v.is_finite() && v.abs() <= 180.0);
    let captured = [exif::Tag::DateTimeOriginal, exif::Tag::DateTimeDigitized]
        .into_iter()
        .find_map(|tag| exif_capture_unix(&field, tag));
    (camera_model, lat, lon, captured)
}

/// EXIF wall-clock time with its OffsetTime* companion when present, else read
/// in the machine's zone (what the camera clock meant). Out-of-range or
/// placeholder ("0000:00:00 …") values yield None.
fn exif_capture_unix<'a>(field: &impl Fn(exif::Tag) -> Option<&'a exif::Field>, tag: exif::Tag) -> Option<f64> {
    let ascii = |tag: exif::Tag| match &field(tag)?.value {
        exif::Value::Ascii(parts) => parts.first().map(|p| String::from_utf8_lossy(p).into_owned()),
        _ => None,
    };
    let text = ascii(tag)?;
    let (offset_tag, subsec_tag) = if tag == exif::Tag::DateTimeOriginal {
        (exif::Tag::OffsetTimeOriginal, exif::Tag::SubSecTimeOriginal)
    } else {
        (exif::Tag::OffsetTimeDigitized, exif::Tag::SubSecTimeDigitized)
    };
    let seconds = exif_datetime_to_unix(&text, ascii(offset_tag).as_deref(), chrono::Utc::now().timestamp() as f64)?;
    Some(seconds + ascii(subsec_tag).as_deref().map_or(0.0, exif_subsec_fraction))
}

/// SubSecTime* digits as a fraction of a second ("5" → 0.5, "123" → 0.123), so
/// burst frames shot within one second keep their order. Non-digits end it.
pub(crate) fn exif_subsec_fraction(text: &str) -> f64 {
    let digits: String = text.trim().chars().take_while(char::is_ascii_digit).take(9).collect();
    if digits.is_empty() {
        return 0.0;
    }
    digits.parse::<f64>().map_or(0.0, |value| value / 10f64.powi(digits.len() as i32))
}

pub(crate) fn exif_datetime_to_unix(text: &str, offset: Option<&str>, now_unix: f64) -> Option<f64> {
    exif_datetime_in(text, offset, now_unix, &chrono::Local)
}

/// Like macOS `Tagging.exifCaptureDate`: a wall time that occurs twice when
/// clocks fall back reads as the later (standard-time) instant, and one that
/// never occurs when they spring forward is rejected.
fn exif_datetime_in<Tz: chrono::TimeZone>(text: &str, offset: Option<&str>, now_unix: f64, zone: &Tz) -> Option<f64> {
    use chrono::TimeZone;
    let naive = chrono::NaiveDateTime::parse_from_str(text.trim().trim_end_matches('\0'), "%Y:%m:%d %H:%M:%S").ok()?;
    let unix = match offset.and_then(exif_offset) {
        Some(fixed) => fixed.from_local_datetime(&naive).single()?.timestamp(),
        None => zone.from_local_datetime(&naive).latest()?.timestamp(),
    } as f64;
    // Before 1990 is almost always a reset camera clock; the future is a wrong one.
    (unix >= 631_152_000.0 && unix <= now_unix + 86_400.0).then_some(unix)
}

/// `±HH:MM`, the only OffsetTime* form macOS accepts.
fn exif_offset(text: &str) -> Option<chrono::FixedOffset> {
    let text = text.trim();
    let sign = match text.as_bytes() {
        [b'+', _, _, _, _, _] => 1,
        [b'-', _, _, _, _, _] => -1,
        _ => return None,
    };
    let hours: i32 = text.get(1..3)?.parse().ok()?;
    let minutes: i32 = text.get(4..6)?.parse().ok()?;
    chrono::FixedOffset::east_opt(sign * (hours * 3_600 + minutes * 60))
}

/// EXIF for an image too large to read fully: the APP1/TIFF IFD chain sits at the front, so a bounded 4 MB head read recovers camera/GPS. Returns `Some((None, None, None))` for a readable file with no parseable EXIF.
const EXIF_HEAD_BYTES: u64 = 4 * 1024 * 1024;

/// EXIF capture time read from the front of the file only. JPEG, PNG, CR3,
/// RAF and the TIFF RAWs keep it in the first 256 KiB; HEIF/AVIF and TIFF
/// containers can place it later, so they get the full head window.
pub(crate) fn capture_time_from_head(path: &std::path::Path) -> Option<f64> {
    use std::io::Read;
    const FIRST: u64 = 256 * 1024;
    let file = open_image_file(path).ok()?;
    let mut head = Vec::new();
    (&file).take(FIRST).read_to_end(&mut head).ok()?;
    if let Some(captured) = parse_exif_fields(&head).and_then(|fields| fields.3) {
        return Some(captured);
    }
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    let deep = matches!(ext.as_str(), "heic" | "heif" | "avif" | "tif" | "tiff") || super::raw_preview::is_raw_extension(&ext);
    if !deep || (head.len() as u64) < FIRST {
        return None;
    }
    (&file).take(EXIF_HEAD_BYTES - FIRST).read_to_end(&mut head).ok()?;
    parse_exif_fields(&head)?.3
}

/// A video's container creation time shaped like an image's EXIF result, so both scan paths carry it the same way.
pub(crate) fn video_capture_fields(path: &std::path::Path) -> Option<ExifFields> {
    crate::util::video_creation_date::container_creation_unix(path).map(|captured| (None, None, None, Some(captured)))
}

fn exif_from_head(path: &std::path::Path) -> Option<ExifFields> {
    let f = open_image_file(path).ok()?;
    let mut head = Vec::new();
    std::io::Read::read_to_end(&mut std::io::Read::take(f, EXIF_HEAD_BYTES), &mut head).ok()?;
    Some(parse_exif_fields(&head).unwrap_or((None, None, None, None)))
}

fn read_gps_coord<'a>(
    field: &impl Fn(exif::Tag) -> Option<&'a exif::Field>,
    value_tag: exif::Tag,
    ref_tag: exif::Tag,
) -> Option<f64> {
    let value_field = field(value_tag)?;
    let ref_field = field(ref_tag)?;

    let dms = match &value_field.value {
        exif::Value::Rational(r) if r.len() >= 3 => [
            r[0].to_f64(),
            r[1].to_f64(),
            r[2].to_f64(),
        ],
        _ => return None,
    };
    let mut decimal = dms[0] + dms[1] / 60.0 + dms[2] / 3600.0;

    let hemisphere = match &ref_field.value {
        exif::Value::Ascii(parts) => parts.first().and_then(|part| part.first()).copied(),
        _ => None,
    };
    if matches!(hemisphere, Some(b'S' | b'W')) {
        decimal = -decimal;
    }
    Some(decimal)
}

pub(crate) fn crop_and_resize_face(
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

/// Nearest-neighbor resize: fine for the dHash (9×8) and the 112px face crop, NOT for CLIP, where one-step decimation of a multi-megapixel source aliases and shifts the embedding; use [`resize_rgb_quality`].
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

/// Bilinear resize for the CLIP 224×224 input. One-step nearest decimation aliases and diverges from the macOS reference (`.high` after a 512px pre-shrink); Triangle closely matches it and is the filter RAM++ preprocessing already trusts. Falls back to nearest if the buffer can't form an image.
pub fn resize_rgb_quality(
    rgb: &[u8],
    src_w: usize,
    src_h: usize,
    dst_w: usize,
    dst_h: usize,
) -> Vec<u8> {
    let src = match image::ImageBuffer::<image::Rgb<u8>, &[u8]>::from_raw(
        src_w as u32,
        src_h as u32,
        rgb,
    ) {
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

pub fn compute_dhash_for_path(path: &std::path::Path) -> anyhow::Result<i64> {
    let (width, height) = image::ImageReader::open(path)?
        .with_guessed_format()?
        .into_dimensions()?;
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    anyhow::ensure!(
        pixels > 0 && pixels <= MAX_DECODED_PIXELS,
        "image dimensions exceed the perceptual-hash safety cap"
    );
    let image = image::ImageReader::open(path)?
        .with_guessed_format()?
        .decode()?
        .to_rgb8();
    Ok(compute_dhash(
        image.as_raw(),
        width as usize,
        height as usize,
    ))
}

/// Difference hash: 9×8 grayscale → 64 bits, one per horizontal-adjacent comparison; matches macOS dHash.
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
    #[test]
    fn a_usb_or_network_volume_narrows_the_decode_pool_to_four() {
        use crate::platform::StorageType::{Hdd, Network, Nvme, Removable, Unknown};
        assert_eq!(decoder_count_for_storage(10, Some(Removable)), 4);
        assert_eq!(decoder_count_for_storage(10, Some(Network)), 4);
        assert_eq!(decoder_count_for_storage(10, Some(Hdd)), 4);
        assert_eq!(decoder_count_for_storage(2, Some(Removable)), 2);
        assert_eq!(decoder_count_for_storage(10, Some(Nvme)), 10);
        assert_eq!(decoder_count_for_storage(10, Some(Unknown)), 10);
        assert_eq!(decoder_count_for_storage(10, None), 10);
    }

    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    #[test]
    fn face_is_excluded_gates_on_absolute_size_not_just_area_fraction() {
        // A 30x36px face in 1280x720: its area fraction (~0.00117) clears the backstop, so only the absolute-pixel floor can reject it.
        let area_fraction = (30.0_f32 * 36.0) / (1280.0 * 720.0);
        assert!(
            area_fraction > FACE_MIN_BBOX_AREA_FRACTION,
            "sanity: the area backstop must not be what rejects this"
        );
        assert!(face_is_excluded(0.0, 0.0, area_fraction, 30.0_f32.min(36.0)));
    }

    #[test]
    fn face_is_excluded_keeps_typical_video_keyframe_faces() {
        // Video keyframes decode at a 1280px long-edge cap, so a normal video face is ~52px min-dimension (p50 on the Adlon catalog); guards the regression where a 64px floor dropped every face in 30% of face-bearing videos.
        let area_fraction = (52.0_f32 * 64.0) / (1280.0 * 720.0);
        assert!(!face_is_excluded(0.0, 0.0, area_fraction, 52.0));
    }

    #[test]
    fn face_is_excluded_keeps_well_resolved_faces() {
        let area_fraction = (400.0_f32 * 400.0) / (8000.0 * 6000.0);
        assert!(!face_is_excluded(0.0, 0.0, area_fraction, 400.0));
    }

    #[test]
    fn face_is_excluded_still_gates_on_pose() {
        assert!(face_is_excluded(60.0_f32.to_radians(), 0.0, 1.0, 200.0));
        assert!(face_is_excluded(0.0, 40.0_f32.to_radians(), 1.0, 200.0));
    }

    /// A catalog search holds an interactive lease for the whole scan and free
    /// memory dips at the start: every file must still be catalogued, not
    /// marked failed as "deferred" or "memory too low".
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn scan_files_wait_for_admission_instead_of_failing() {
        use crate::resource_scheduler::{Demand, Priority, ResourceScheduler};
        use std::sync::atomic::AtomicU64;
        let dir = std::env::temp_dir().join(format!("fileid-scan-admission-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let free = Arc::new(AtomicU64::new(12_000));
        let reading = free.clone();
        let scheduler = ResourceScheduler::new(move || (16_384, reading.load(Ordering::Relaxed)), 2, 2);
        let search = scheduler
            .reserve(Demand { memory_mb: 32, cpu: 1, io: 1 }, Priority::Interactive, || false)
            .unwrap();
        free.store(600, Ordering::Relaxed);
        let restore = free.clone();
        let recovery = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            restore.store(12_000, Ordering::Relaxed);
        });

        let coord = ScanCoordinator::new();
        let (tx, rx) = mpsc::channel(32);
        let tagger = Tagger::new(coord, 4, Arc::new(ModelStack::empty())).with_scheduler(scheduler.clone());
        let (mut out, decoder_join) = tagger.spawn(rx);
        let total = 12;
        for i in 0..total {
            let path = dir.join(format!("note{i}.txt"));
            std::fs::write(&path, format!("garden notes number {i}")).unwrap();
            tx.send(DiscoveredFile {
                path,
                kind: FileKind::Doc,
                size_bytes: 20,
                modified_unix: 1.0,
                created_unix: None,
                online_only: false,
                moved_from_known_row: false,
                file_ref: None,
            })
            .await
            .unwrap();
        }
        drop(tx);
        let mut failed = Vec::new();
        for _ in 0..total {
            let file = tokio::time::timeout(Duration::from_secs(30), out.recv())
                .await
                .expect("every file is emitted")
                .expect("channel open");
            if file.failed {
                failed.push(file.error_message);
            }
        }
        drop(out);
        decoder_join.await.unwrap();
        recovery.join().unwrap();
        drop(search);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(failed.is_empty(), "files failed during a search: {failed:?}");
    }

    #[cfg(not(all(target_os = "macos", feature = "apple-adapters")))]
    fn twin_image(dir: &std::path::Path, name: &str, bytes: &[u8]) -> DiscoveredFile {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        DiscoveredFile {
            path,
            kind: FileKind::Image,
            size_bytes: bytes.len() as u64,
            modified_unix: 1.0,
            created_unix: None,
            online_only: false,
            moved_from_known_row: false,
            file_ref: None,
        }
    }

    fn tiny_png() -> Vec<u8> {
        let mut bytes = Vec::new();
        image::RgbImage::from_pixel(8, 8, image::Rgb([200, 40, 40]))
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        bytes
    }

    #[cfg(not(all(target_os = "macos", feature = "apple-adapters")))]
    #[test]
    fn a_copy_of_an_image_in_flight_is_hashed_but_not_decoded() {
        let dir = std::env::temp_dir().join(format!("fileid-twin-pending-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = tiny_png();
        let (coord, budget, twins) = (ScanCoordinator::new(), PredecodeBudget::new(64 << 20), TwinRegistry::new());
        let owner = predecode_one(twin_image(&dir, "a.png", &png), &coord, &budget, false, Some(&twins), false).unwrap();
        assert!(matches!(owner.twin, TwinClaim::Owner));
        assert!(matches!(owner.decoded, Some(Ok(_))));
        let copy = predecode_one(twin_image(&dir, "b.png", &png), &coord, &budget, false, Some(&twins), false).unwrap();
        assert!(matches!(copy.twin, TwinClaim::Pending(_)));
        assert!(copy.decoded.is_none() && copy.budget.is_none(), "the waiting copy holds no frame");
        assert_eq!(copy.content_hash, owner.content_hash);
        let unshared = predecode_one(twin_image(&dir, "c.png", &png), &coord, &budget, false, None, false).unwrap();
        assert!(matches!(unshared.twin, TwinClaim::None));
        assert!(matches!(unshared.decoded, Some(Ok(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(not(all(target_os = "macos", feature = "apple-adapters")))]
    #[tokio::test]
    async fn a_waiting_copy_reuses_a_finished_owner_and_decodes_only_when_it_failed() {
        let dir = std::env::temp_dir().join(format!("fileid-twin-settle-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = tiny_png();
        let (coord, budget) = (ScanCoordinator::new(), PredecodeBudget::new(64 << 20));
        let pending = |twins: &Arc<TwinRegistry>, name: &str| {
            let item = predecode_one(twin_image(&dir, name, &png), &coord, &budget, false, Some(twins), false).unwrap();
            assert!(matches!(item.twin, TwinClaim::Pending(_)));
            item
        };
        let patience = Duration::from_secs(30);
        let waiters = TwinWaiters::new(3);

        let twins = TwinRegistry::new();
        let owner = predecode_one(twin_image(&dir, "a.png", &png), &coord, &budget, false, Some(&twins), false).unwrap();
        let hash = owner.content_hash.unwrap();
        let copy = pending(&twins, "b.png");
        let publisher = {
            let twins = twins.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(30)).await;
                twins.publish(hash, true);
            })
        };
        let settled = settle_twin(copy, &twins, &waiters, &coord, &budget, false, patience).await.unwrap();
        publisher.await.unwrap();
        assert!(matches!(settled.twin, TwinClaim::Ready));
        assert!(settled.decoded.is_none());

        // The owner fails: the copy claims the image, is decoded now, and the
        // copies behind it wait on it instead of on the failed owner.
        let twins = TwinRegistry::new();
        let _owner = predecode_one(twin_image(&dir, "a.png", &png), &coord, &budget, false, Some(&twins), false).unwrap();
        let copy = pending(&twins, "b.png");
        twins.publish(hash, false);
        let settled = settle_twin(copy, &twins, &waiters, &coord, &budget, false, patience).await.unwrap();
        assert!(matches!(settled.twin, TwinClaim::Owner));
        assert!(matches!(settled.decoded, Some(Ok(_))));
        assert!(matches!(twins.claim(hash), TwinClaim::Pending(_)));

        // The owner is only slow: the copy is analyzed on its own, unshared.
        let twins = TwinRegistry::new();
        let _owner = predecode_one(twin_image(&dir, "a.png", &png), &coord, &budget, false, Some(&twins), false).unwrap();
        let copy = pending(&twins, "b.png");
        let settled = settle_twin(copy, &twins, &waiters, &coord, &budget, false, Duration::from_millis(20)).await.unwrap();
        assert!(matches!(settled.twin, TwinClaim::None));
        assert!(matches!(settled.decoded, Some(Ok(_))));

        // The last free worker does not wait: it analyzes the copy unshared,
        // even though the owner never finishes.
        let twins = TwinRegistry::new();
        let _owner = predecode_one(twin_image(&dir, "a.png", &png), &coord, &budget, false, Some(&twins), false).unwrap();
        let copy = pending(&twins, "b.png");
        let lone = TwinWaiters::new(2);
        let _other_worker = lone.enter().expect("the first waiter is admitted");
        let settled = settle_twin(copy, &twins, &lone, &coord, &budget, false, patience).await.unwrap();
        assert!(matches!(settled.twin, TwinClaim::None));
        assert!(matches!(settled.decoded, Some(Ok(_))));
        let twins = TwinRegistry::new();
        let _owner = predecode_one(twin_image(&dir, "a.png", &png), &coord, &budget, false, Some(&twins), false).unwrap();
        let copy = pending(&twins, "b.png");
        twins.publish(hash, true);
        let settled = settle_twin(copy, &twins, &lone, &coord, &budget, false, patience).await.unwrap();
        assert!(matches!(settled.twin, TwinClaim::Ready));
        assert!(TwinWaiters::new(1).enter().is_none(), "a lone worker never waits");

        let twins = TwinRegistry::new();
        let _owner = predecode_one(twin_image(&dir, "a.png", &png), &coord, &budget, false, Some(&twins), false).unwrap();
        let copy = pending(&twins, "b.png");
        coord.request_cancel();
        let cancelled = tokio::time::timeout(Duration::from_secs(5), settle_twin(copy, &twins, &waiters, &coord, &budget, false, patience));
        assert!(cancelled.await.expect("a cancelled wait returns at once, not after its patience").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Copies only hash, so they are ready before the owner that is still
    /// decoding. Queued ahead of it they would occupy every worker, each
    /// waiting for an owner no worker is free to analyse.
    #[cfg(not(all(target_os = "macos", feature = "apple-adapters")))]
    #[test]
    fn decoders_queue_an_owner_before_the_copies_that_wait_for_it() {
        let dir = std::env::temp_dir().join(format!("fileid-twin-order-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = tiny_png();
        let (coord, budget, twins) = (ScanCoordinator::new(), PredecodeBudget::new(64 << 20), TwinRegistry::new());
        let owner = predecode_one(twin_image(&dir, "owner.png", &png), &coord, &budget, false, Some(&twins), true).unwrap();
        assert_eq!(OWNED_TWIN.take(), owner.content_hash);
        let hash = owner.content_hash.unwrap();

        let (raw_tx, raw_rx) = async_channel::bounded::<DiscoveredFile>(4);
        let (predecoded_tx, predecoded_rx) = async_channel::bounded::<PreDecoded>(8);
        raw_tx.send_blocking(twin_image(&dir, "copy1.png", &png)).unwrap();
        raw_tx.send_blocking(twin_image(&dir, "copy2.png", &png)).unwrap();
        drop(raw_tx);
        run_decoder_thread(raw_rx, predecoded_tx.clone(), coord.clone(), budget.clone(), false, twins.clone());
        assert!(predecoded_rx.is_empty(), "the copies are held back");

        predecoded_tx.send_blocking(owner).unwrap();
        for follower in twins.owner_queued(&hash) {
            predecoded_tx.send_blocking(follower).unwrap();
        }
        drop(predecoded_tx);
        let order: Vec<String> = std::iter::from_fn(|| predecoded_rx.recv_blocking().ok())
            .map(|item| item.file.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(order, ["owner.png", "copy1.png", "copy2.png"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_waiting_copy_gives_up_before_the_per_file_timeout() {
        assert!(TWIN_WAIT_PATIENCE < PER_FILE_TIMEOUT);
    }

    #[test]
    fn a_timeout_failure_reads_as_unreadable_in_the_library_not_as_waiting_for_a_retry() {
        for message in TIMEOUT_MESSAGES {
            assert!(!message.contains("will retry"), "the Library leaves rows saying \"will retry\" out of its Unreadable count: {message}");
        }
    }

    #[cfg(not(all(target_os = "macos", feature = "apple-adapters")))]
    #[tokio::test]
    async fn identical_images_all_complete_when_no_analysis_can_be_shared() {
        let dir = std::env::temp_dir().join(format!("fileid-twin-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = tiny_png();
        let (tx, rx) = mpsc::channel(16);
        // No models: every analysis publishes "not reusable", so each copy in
        // turn claims the image and analyzes itself.
        let tagger = Tagger::new(ScanCoordinator::new(), 3, Arc::new(ModelStack::empty()));
        let (mut out, decoder_join) = tagger.spawn(rx);
        for i in 0..6 {
            tx.send(twin_image(&dir, &format!("copy{i}.png"), &png)).await.unwrap();
        }
        drop(tx);
        let mut rows = Vec::new();
        while let Some(row) = tokio::time::timeout(Duration::from_secs(20), out.recv()).await.expect("no copy hangs") {
            rows.push(row);
        }
        decoder_join.await.unwrap();
        assert_eq!(rows.len(), 6);
        assert!(rows.iter().all(|row| !row.failed && !row.reuse_twin && row.phash.is_some()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn counting_source(loads: Arc<AtomicUsize>, fail: bool) -> Arc<ModelSource> {
        ModelSource::lazy(move || {
            Box::pin(async move {
                loads.fetch_add(1, Ordering::SeqCst);
                if fail {
                    Err(crate::pipeline::scan_models::ModelLoadFailure::new("model_load_failed", "broken weights"))
                } else {
                    Ok(Arc::new(ModelStack::empty()))
                }
            })
        })
    }

    fn text_file(dir: &std::path::Path, name: &str) -> DiscoveredFile {
        let path = dir.join(name);
        std::fs::write(&path, "garden notes").unwrap();
        DiscoveredFile {
            path,
            kind: FileKind::Doc,
            size_bytes: 12,
            modified_unix: 1.0,
            created_unix: None,
            online_only: false,
            moved_from_known_row: false,
            file_ref: None,
        }
    }

    fn wav_with_info(entries: &[(&[u8; 4], &str)]) -> Vec<u8> {
        let mut info = b"INFO".to_vec();
        for (id, value) in entries {
            let mut text = value.as_bytes().to_vec();
            text.push(0);
            if text.len() % 2 == 1 {
                text.push(0);
            }
            info.extend_from_slice(*id);
            info.extend_from_slice(&(text.len() as u32).to_le_bytes());
            info.extend_from_slice(&text);
        }
        let mut body = b"WAVEfmt ".to_vec();
        body.extend_from_slice(&16u32.to_le_bytes());
        for field in [1u16, 1] {
            body.extend_from_slice(&field.to_le_bytes());
        }
        body.extend_from_slice(&8000u32.to_le_bytes());
        body.extend_from_slice(&16000u32.to_le_bytes());
        for field in [2u16, 16] {
            body.extend_from_slice(&field.to_le_bytes());
        }
        body.extend_from_slice(b"LIST");
        body.extend_from_slice(&(info.len() as u32).to_le_bytes());
        body.extend_from_slice(&info);
        body.extend_from_slice(b"data");
        body.extend_from_slice(&8u32.to_le_bytes());
        body.extend_from_slice(&[0; 8]);
        let mut wav = b"RIFF".to_vec();
        wav.extend_from_slice(&(body.len() as u32).to_le_bytes());
        wav.extend_from_slice(&body);
        wav
    }

    #[tokio::test]
    async fn a_file_that_is_not_an_image_stores_its_kind_tag_and_no_year() {
        let dir = std::env::temp_dir().join(format!("fileid-kind-tags-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut note = text_file(&dir, "note.txt");
        note.created_unix = Some(1_500_000_000.0);
        note.modified_unix = 1_500_000_000.0;
        let blob = dir.join("blob.bin");
        std::fs::write(&blob, vec![7u8; 64]).unwrap();
        let song = dir.join("song.wav");
        std::fs::write(&song, wav_with_info(&[(b"IART", "The Band"), (b"INAM", "Song One"), (b"IGNR", "Rock"), (b"ICRD", "2009")])).unwrap();
        let file = |path: std::path::PathBuf, kind, size| DiscoveredFile {
            path,
            kind,
            size_bytes: size,
            modified_unix: 1_500_000_000.0,
            created_unix: Some(1_500_000_000.0),
            online_only: false,
            moved_from_known_row: false,
            file_ref: None,
        };
        let song_size = std::fs::metadata(&song).unwrap().len();
        let (tx, rx) = mpsc::channel(8);
        let tagger = Tagger::with_model_source(ScanCoordinator::new(), 2, counting_source(Arc::new(AtomicUsize::new(0)), false));
        let (mut out, decoder_join) = tagger.spawn(rx);
        for item in [note, file(blob, FileKind::Other, 64), file(song, FileKind::Audio, song_size)] {
            tx.send(item).await.unwrap();
        }
        drop(tx);
        let mut stored = std::collections::BTreeMap::new();
        while let Some(row) = tokio::time::timeout(Duration::from_secs(10), out.recv()).await.unwrap() {
            assert!(!row.failed && row.tags_evaluated);
            let name = row.path.file_name().unwrap().to_string_lossy().to_string();
            let mut tags: Vec<String> = row.tags.iter().map(|(tag, _)| tag.clone()).collect();
            tags.sort();
            stored.insert(name, tags);
        }
        decoder_join.await.unwrap();
        assert_eq!(stored["note.txt"], ["Doc", "garden notes"]);
        assert_eq!(stored["blob.bin"], ["Other"]);
        assert_eq!(stored["song.wav"], ["Audio", "Song One", "The Band"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn models_load_only_when_a_file_needs_analysing() {
        let dir = std::env::temp_dir().join(format!("fileid-lazy-models-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let loads = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = mpsc::channel(8);
        let tagger = Tagger::with_model_source(ScanCoordinator::new(), 3, counting_source(loads.clone(), false));
        let (mut out, decoder_join) = tagger.spawn(rx);
        let mut moved = text_file(&dir, "moved.txt");
        moved.moved_from_known_row = true;
        moved.file_ref = Some(3);
        tx.send(moved).await.unwrap();
        drop(tx);
        let row = tokio::time::timeout(Duration::from_secs(5), out.recv()).await.unwrap().unwrap();
        assert!(row.heal_only);
        assert!(tokio::time::timeout(Duration::from_secs(5), out.recv()).await.unwrap().is_none());
        decoder_join.await.unwrap();
        assert_eq!(loads.load(Ordering::SeqCst), 0);

        let (tx, rx) = mpsc::channel(8);
        let tagger = Tagger::with_model_source(ScanCoordinator::new(), 3, counting_source(loads.clone(), false));
        let (mut out, decoder_join) = tagger.spawn(rx);
        for i in 0..5 {
            tx.send(text_file(&dir, &format!("note{i}.txt"))).await.unwrap();
        }
        drop(tx);
        let mut rows = 0;
        while let Some(row) = tokio::time::timeout(Duration::from_secs(10), out.recv()).await.unwrap() {
            assert!(!row.failed);
            rows += 1;
        }
        decoder_join.await.unwrap();
        assert_eq!((rows, loads.load(Ordering::SeqCst)), (5, 1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_failed_model_load_stops_the_scan_without_writing_any_file() {
        let dir = std::env::temp_dir().join(format!("fileid-lazy-models-fail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let loads = Arc::new(AtomicUsize::new(0));
        let source = counting_source(loads.clone(), true);
        let coord = ScanCoordinator::new();
        let (tx, rx) = mpsc::channel(8);
        let (mut out, decoder_join) = Tagger::with_model_source(coord.clone(), 2, source.clone()).spawn(rx);
        for i in 0..4 {
            tx.send(text_file(&dir, &format!("note{i}.txt"))).await.unwrap();
        }
        assert!(tokio::time::timeout(Duration::from_secs(10), out.recv()).await.unwrap().is_none());
        drop(tx);
        decoder_join.await.unwrap();
        assert!(coord.is_cancelled());
        assert_eq!(loads.load(Ordering::SeqCst), 1);
        assert_eq!(source.failure().map(|failure| failure.kind), Some("model_load_failed"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The runtime here has one thread, and it is the test's: analysis that
    /// ran on runtime threads would have to run on it.
    #[tokio::test]
    async fn files_are_analysed_on_dedicated_threads_not_on_the_runtime() {
        let dir = std::env::temp_dir().join(format!("fileid-worker-thread-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (tx, rx) = mpsc::channel(8);
        let (mut out, decoder_join) = Tagger::new(ScanCoordinator::new(), 2, Arc::new(ModelStack::empty())).spawn(rx);
        tx.send(text_file(&dir, "fileid-test-worker-thread.txt")).await.unwrap();
        drop(tx);
        let row = tokio::time::timeout(Duration::from_secs(10), out.recv()).await.unwrap().unwrap();
        let thread = row.camera_model.expect("the worker reports its thread");
        assert!(thread.starts_with("fileid-tag-"), "analysed on {thread}");
        assert_ne!(Some(thread.as_str()), std::thread::current().name());
        drop(out);
        decoder_join.await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_moved_file_reaches_the_writer_unread() {
        let (tx, rx) = mpsc::channel(8);
        let tagger = Tagger::new(ScanCoordinator::new(), 2, Arc::new(ModelStack::empty()));
        let (mut out, decoder_join) = tagger.spawn(rx);
        tx.send(DiscoveredFile {
            path: PathBuf::from("/fileid-missing/moved.jpg"),
            kind: FileKind::Image,
            size_bytes: 1024,
            modified_unix: 1.0,
            created_unix: None,
            online_only: false,
            moved_from_known_row: true,
            file_ref: Some(7),
        })
        .await
        .unwrap();
        drop(tx);
        let got = tokio::time::timeout(Duration::from_secs(2), out.recv()).await.unwrap().unwrap();
        assert!(got.heal_only);
        assert!(!got.failed, "the missing file was never opened");
        assert_eq!(got.file_ref, Some(7));
        assert!(got.content_hash.is_none() && !got.tags_evaluated && !got.faces_evaluated);
        drop(out);
        decoder_join.await.unwrap();
    }

    #[tokio::test]
    async fn tagger_passes_discovered_through_to_tagged() {
        let coord = ScanCoordinator::new();
        let (tx, rx) = mpsc::channel(8);
        let models = Arc::new(ModelStack::empty());
        let tagger = Tagger::new(coord, 2, models);
        let (mut out, decoder_join) = tagger.spawn(rx);

        tx.send(DiscoveredFile {
            path: PathBuf::from("C:/tmp/a.jpg"),
            kind: FileKind::Image,
            size_bytes: 1024,
            modified_unix: 1.0,
            created_unix: None,
            online_only: false,
            moved_from_known_row: false,
            file_ref: None,
        })
        .await
        .unwrap();
        drop(tx);

        let got = tokio::time::timeout(Duration::from_secs(2), out.recv())
            .await
            .expect("worker emitted")
            .expect("channel not closed");
        assert_eq!(got.size_bytes, 1024);
        assert_eq!(got.kind, FileKind::Image);
        assert!(got.failed);
        drop(out);
        decoder_join.await.unwrap();
    }

    #[test]
    fn detected_face_has_crop_field() {
        let f = DetectedFace {
            bbox: [0.0; 4],
            embedding: vec![0.0; 512],
            embedding_model: None,
            roll: 0.0,
            yaw: 0.0,
            pitch: 0.0,
            quality: 0.0,
            excluded: false,
            crop_rgb_112: None,
        };
        assert!(f.crop_rgb_112.is_none());
    }

    #[test]
    fn model_pool_never_exceeds_the_default_or_the_worker_count() {
        assert_eq!(resolve_pool_size(1), 1);
        assert!((1..=MODEL_POOL_SIZE).contains(&resolve_pool_size(16)));
    }

    #[test]
    fn dhash_solid_color_is_zero() {
        let rgb = vec![128u8; 64 * 64 * 3];
        assert_eq!(compute_dhash(&rgb, 64, 64), 0);
    }

    /// A third frame that would overflow the byte-weighted budget must wait until an earlier reservation drops.
    #[test]
    fn predecode_budget_weighs_frames_by_actual_bytes() {
        let coord = ScanCoordinator::new();
        let budget = PredecodeBudget::new(100);
        let g1 = budget.acquire(60, &coord).expect("first frame fits");
        let _g2 = budget.acquire(40, &coord).expect("second frame fits");
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            drop(g1);
        });
        let started = std::time::Instant::now();
        let _g3 = budget
            .acquire(30, &coord)
            .expect("third frame admitted once g1 releases");
        assert!(
            started.elapsed() >= Duration::from_millis(100),
            "third acquire must have blocked on the full budget"
        );
        releaser.join().unwrap();
    }

    #[test]
    fn decode_callback_waits_for_preallocation_reservation() {
        let coord = ScanCoordinator::new();
        let budget = PredecodeBudget::new(100);
        let held = budget.acquire(100, &coord).unwrap();
        let entered = Arc::new(AtomicBool::new(false));
        let entered_thread = entered.clone();
        let budget_thread = budget.clone();
        let coord_thread = coord.clone();
        let worker = std::thread::spawn(move || {
            decode_reserved(&budget_thread, 100, &coord_thread, || {
                entered_thread.store(true, Ordering::Release);
                Ok(())
            })
            .unwrap()
        });
        std::thread::sleep(Duration::from_millis(100));
        assert!(!entered.load(Ordering::Acquire));
        drop(held);
        let (result, _guard) = worker.join().unwrap();
        result.unwrap();
        assert!(entered.load(Ordering::Acquire));
    }

    #[test]
    fn predecode_guard_shrinks_to_retained_frame_bytes() {
        let coord = ScanCoordinator::new();
        let budget = PredecodeBudget::new(100);
        let mut guard = budget.acquire(100, &coord).unwrap();
        assert!(guard.shrink_to(40));
        let _remaining = budget.acquire(60, &coord).expect("released peak bytes");
    }

    #[test]
    fn image_probe_accounts_source_and_rgb_conversion() {
        let image = image::RgbImage::from_pixel(4, 3, image::Rgb([1, 2, 3]));
        let mut encoded = Vec::new();
        image::DynamicImage::ImageRgb8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut encoded),
                image::ImageFormat::Png,
            )
            .unwrap();
        assert_eq!(probe_image_decode_reservation_bytes(&encoded), Some(4 * 3 * 6));
    }

    #[test]
    fn predecode_budget_clamps_oversize_frame_to_capacity() {
        let coord = ScanCoordinator::new();
        let budget = PredecodeBudget::new(100);
        let g = budget
            .acquire(10_000, &coord)
            .expect("a frame larger than the budget must still be admitted alone");
        drop(g);
        assert!(budget.acquire(100, &coord).is_some());
    }

    #[test]
    fn predecode_budget_rejects_cancel_even_when_capacity_is_free() {
        let coord = ScanCoordinator::new();
        coord.request_cancel();
        let budget = PredecodeBudget::new(100);
        assert!(budget.acquire(50, &coord).is_none());
    }

    #[test]
    fn decoder_blocked_on_full_output_exits_on_cancel() {
        let (raw_tx, raw_rx) = async_channel::bounded(2);
        let (predecoded_tx, _predecoded_rx) = async_channel::bounded(1);
        let coord = ScanCoordinator::new();
        let worker_coord = coord.clone();
        let budget = PredecodeBudget::new(1024);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            run_decoder_thread(raw_rx, predecoded_tx, worker_coord, budget, false, TwinRegistry::new());
            let _ = done_tx.send(());
        });
        for index in 0..2 {
            raw_tx
                .send_blocking(DiscoveredFile {
                    path: PathBuf::from(format!("missing-{index}.bin")),
                    kind: FileKind::Other,
                    size_bytes: 0,
                    modified_unix: 0.0,
                    created_unix: None,
                    online_only: true,
                    moved_from_known_row: false,
                    file_ref: None,
                })
                .unwrap();
        }

        std::thread::sleep(Duration::from_millis(30));
        coord.request_cancel();
        done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("full decoder output queue must remain cancellation-aware");
        worker.join().unwrap();
    }

    #[test]
    fn decoder_panic_on_one_file_fails_that_file_and_keeps_decoding() {
        let dir = std::env::temp_dir().join(format!("fileid-decoder-panic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let names = ["fileid-test-decoder-panic", "after.txt"];
        for name in names {
            std::fs::write(dir.join(name), b"text").unwrap();
        }
        let (raw_tx, raw_rx) = async_channel::bounded(names.len());
        let (predecoded_tx, predecoded_rx) = async_channel::bounded(names.len());
        for name in names {
            raw_tx
                .send_blocking(DiscoveredFile {
                    path: dir.join(name),
                    kind: FileKind::Doc,
                    size_bytes: 4,
                    modified_unix: 0.0,
                    created_unix: None,
                    online_only: false,
                    moved_from_known_row: false,
                    file_ref: None,
                })
                .unwrap();
        }
        drop(raw_tx);
        run_decoder_thread(raw_rx, predecoded_tx, ScanCoordinator::new(), PredecodeBudget::new(1 << 20), false, TwinRegistry::new());
        let failed = predecoded_rx.try_recv().expect("panicking file still reported");
        assert!(matches!(failed.decoded, Some(Err(_))));
        let next = predecoded_rx.try_recv().expect("decoder kept running");
        assert_eq!(next.file.path, dir.join("after.txt"));
        assert!(next.content_hash.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(all(not(windows), not(target_os = "linux")))]
    #[test]
    fn video_is_catalogued_without_a_frame_when_the_ffmpeg_runtime_is_absent() {
        if std::env::var_os("FILEID_FFMPEG_DIR").is_some() || video_frames_available() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("fileid-decoder-video-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clip.mov");
        std::fs::write(&path, vec![7u8; 4096]).unwrap();
        let item = predecode_one(
            DiscoveredFile {
                path: path.clone(),
                kind: FileKind::Video,
                size_bytes: 4096,
                modified_unix: 0.0,
                created_unix: None,
                online_only: false,
                moved_from_known_row: false,
                file_ref: None,
            },
            &ScanCoordinator::new(),
            &PredecodeBudget::new(256 << 20),
            false,
            Some(&TwinRegistry::new()),
            true,
        )
        .expect("not cancelled");
        assert!(item.decoded.is_none(), "a missing frame runtime must not mark the video failed");
        assert!(item.content_hash.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn decoder_computes_legacy_digests_from_its_single_read_only_when_asked() {
        let dir = std::env::temp_dir().join(format!("fileid-decoder-legacy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let files = [("note.txt", FileKind::Doc), ("clip.bin", FileKind::Other)];
        for (name, _) in files {
            std::fs::write(dir.join(name), name.repeat(5000)).unwrap();
        }
        for legacy in [true, false] {
            let (raw_tx, raw_rx) = async_channel::bounded(files.len());
            let (predecoded_tx, predecoded_rx) = async_channel::bounded(files.len());
            for (name, kind) in files {
                raw_tx
                    .send_blocking(DiscoveredFile {
                        path: dir.join(name),
                        kind,
                        size_bytes: (name.len() * 5000) as u64,
                        modified_unix: 0.0,
                        created_unix: None,
                        online_only: false,
                        moved_from_known_row: false,
                        file_ref: None,
                    })
                    .unwrap();
            }
            drop(raw_tx);
            run_decoder_thread(raw_rx, predecoded_tx, ScanCoordinator::new(), PredecodeBudget::new(1 << 20), legacy, TwinRegistry::new());
            assert_eq!(predecoded_rx.len(), files.len());
            while let Ok(item) = predecoded_rx.try_recv() {
                let size = item.file.size_bytes;
                let expected = crate::util::content_hash::content_hashes(&item.file.path, size, true).unwrap();
                assert_eq!(item.content_hash, Some(expected.0));
                assert_eq!(item.legacy_hashes.map(|h| h.v2), legacy.then(|| expected.1.unwrap().v2));
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn idle_decoder_exits_on_cancel_while_discovery_sender_stays_open() {
        let (_raw_tx, raw_rx) = async_channel::bounded(1);
        let (predecoded_tx, _predecoded_rx) = async_channel::bounded(1);
        let coord = ScanCoordinator::new();
        let worker_coord = coord.clone();
        let budget = PredecodeBudget::new(1024);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            run_decoder_thread(raw_rx, predecoded_tx, worker_coord, budget, false, TwinRegistry::new());
            let _ = done_tx.send(());
        });

        std::thread::sleep(Duration::from_millis(20));
        coord.request_cancel();
        done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("idle decoder must observe cancellation without channel closure");
        worker.join().unwrap();
    }

    #[test]
    fn predecode_budget_acquire_unblocks_on_cancel() {
        let coord = ScanCoordinator::new();
        let budget = PredecodeBudget::new(100);
        let _held = budget.acquire(100, &coord).unwrap();
        coord.request_cancel();
        assert!(budget.acquire(50, &coord).is_none());
    }

    #[test]
    fn mpf_preview_replaces_the_full_decode_under_the_mac_rules() {
        use crate::pipeline::mpf_preview::test_support::{jpeg_with_mpf_preview, solid_jpeg};
        let primary = solid_jpeg(600, 400, [200, 30, 30]);
        let preview = solid_jpeg(300, 200, [30, 200, 30]);
        let file = jpeg_with_mpf_preview(&primary, &preview, true);
        let (rgb, w, h) = decode_mpf_preview(&file, 600, 400, 300).expect("preview decodes");
        assert_eq!((w, h), (300, 200));
        assert!(rgb[1] > 150 && rgb[0] < 100, "the frame comes from the green preview");
        assert!(decode_mpf_preview(&file, 600, 400, 2048).is_none(), "a full image under 4/3 of the floor decodes itself");
        assert!(decode_mpf_preview(&file, 600, 400, 460).is_none(), "a preview under 2/3 of the floor is too small");
        let not_srgb = jpeg_with_mpf_preview(&primary, &preview, false);
        assert!(decode_mpf_preview(&not_srgb, 600, 400, 300).is_none());
        assert!(decode_mpf_preview(&primary, 600, 400, 300).is_none(), "no MPF index, no preview");
        let (_, w, h) = decode_image_sync_imagecrate(std::path::Path::new("mpf-test.jpg"), Some(&file), None).unwrap();
        assert_eq!((w, h), (600, 400), "the scan floor is 2048, so this small file still decodes in full");
    }

    #[test]
    fn exif_orientation_transforms_the_decoded_frame() {
        use image::metadata::Orientation;

        let buf = vec![255, 0, 0, 0, 255, 0];

        let (out, w, h) = apply_exif_orientation(buf.clone(), 2, 1, None).unwrap();
        assert_eq!((out, w, h), (buf.clone(), 2, 1));
        let (out, w, h) =
            apply_exif_orientation(buf.clone(), 2, 1, Some(Orientation::NoTransforms)).unwrap();
        assert_eq!((out, w, h), (buf.clone(), 2, 1));

        // A quarter turn swaps the reported dimensions — this is what stops face
        // detection from seeing portrait photos on their side.
        for turn in [Orientation::Rotate90, Orientation::Rotate270] {
            let (out, w, h) = apply_exif_orientation(buf.clone(), 2, 1, Some(turn)).unwrap();
            assert_eq!((w, h), (1, 2), "{turn:?} must swap dimensions");
            assert_eq!(out.len(), buf.len(), "{turn:?} must preserve pixel count");
        }

        let (out, w, h) =
            apply_exif_orientation(buf.clone(), 2, 1, Some(Orientation::Rotate180)).unwrap();
        assert_eq!((w, h), (2, 1));
        assert_eq!(out, vec![0, 255, 0, 255, 0, 0]);

        assert!(apply_exif_orientation(vec![1, 2, 3], 9, 9, Some(Orientation::Rotate90)).is_err());
    }

    #[test]
    fn one_exif_read_agrees_with_the_separate_readers_and_decodes_the_same_frame() {
        let path = std::path::Path::new("oriented.jpg");
        for bytes in [jpeg_with_orientation(6, 4, Some(6)), jpeg_with_orientation(6, 4, None), tiny_png(), b"not an image".to_vec()] {
            let read = read_primary_exif(&bytes);
            assert_eq!(read.orientation, exif_orientation(&bytes));
            assert_eq!(read.srgb, exif_is_srgb(&bytes));
            assert_eq!(read.fields, parse_exif_fields(&bytes));
            let hinted = decode_image_sync_with(path, Some(&bytes), Some(&read)).ok();
            assert_eq!(hinted, decode_image_sync(path, Some(&bytes)).ok());
        }
        let turned = jpeg_with_orientation(6, 4, Some(6));
        let (_, w, h) = decode_image_sync_with(path, Some(&turned), Some(&read_primary_exif(&turned))).unwrap();
        assert_eq!((w, h), (4, 6), "the orientation handed down is applied");
    }

    fn jpeg_with_orientation(width: u32, height: u32, orientation: Option<u8>) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(width, height, image::Rgb([90, 140, 200]));
        let mut encoded = std::io::Cursor::new(Vec::new());
        img.write_to(&mut encoded, image::ImageFormat::Jpeg).unwrap();
        let encoded = encoded.into_inner();
        let Some(value) = orientation else { return encoded };
        let mut out = vec![0xFF, 0xD8, 0xFF, 0xE1, 0x00, 0x22];
        out.extend_from_slice(b"Exif\0\0");
        out.extend_from_slice(&[0x49, 0x49, 0x2A, 0x00, 0x08, 0x00, 0x00, 0x00, 0x01, 0x00]);
        out.extend_from_slice(&[0x12, 0x01, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00, value, 0x00, 0x00, 0x00]);
        out.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        out.extend_from_slice(&encoded[2..]);
        out
    }

    #[test]
    fn scaled_jpeg_reservation_tracks_the_reduced_frame_not_the_full_one() {
        let big = jpeg_with_orientation(4400, 3000, None);
        let reserved = scaled_jpeg_reservation(&big).expect("sequential JPEG above twice the floor is scaled");
        let (rgb, w, h) = decode_jpeg_scaled(&big).expect("scaled decode");
        assert!((w as usize) * (h as usize) * 9 == reserved, "{w}x{h} vs {reserved}");
        assert!(reserved >= rgb.len() * 3);
        let full = probe_image_decode_reservation_bytes(&big).unwrap();
        assert!(reserved * 2 < full, "scaled {reserved} should be far below full {full}");

        let dir = std::env::temp_dir().join(format!("fileid-jpeg-reservation-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.jpg");
        std::fs::write(&path, &big).unwrap();
        assert_eq!(scaled_jpeg_reservation_for(&path, None), Some(reserved));
        assert_eq!(scaled_jpeg_reservation_for(&path, Some(&big)), Some(reserved));
        std::fs::write(dir.join("not.jpg"), b"not a jpeg at all").unwrap();
        assert_eq!(scaled_jpeg_reservation_for(&dir.join("not.jpg"), None), None);
        assert_eq!(scaled_jpeg_reservation_for(&dir.join("missing.jpg"), None), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scaled_jpeg_reservation_defers_to_the_full_reservation_when_no_scaling_happens() {
        assert_eq!(scaled_jpeg_reservation(&jpeg_with_orientation(800, 600, None)), None);
        assert_eq!(scaled_jpeg_reservation(&jpeg_with_orientation(4000, 3000, None)), None, "long edge below twice the floor");
        assert_eq!(scaled_jpeg_reservation(&[0x89, b'P', b'N', b'G']), None);
        assert_eq!(scaled_jpeg_reservation(&[]), None);
    }

    #[test]
    fn raw_orientation_reads_olympus_headers_and_fuji_previews() {
        use image::metadata::Orientation;
        let mut orf = b"IIRO".to_vec();
        orf.extend_from_slice(&[0x08, 0x00, 0x00, 0x00, 0x01, 0x00]);
        orf.extend_from_slice(&[0x12, 0x01, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00]);
        orf.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        assert!(exif_orientation(&orf).is_none(), "the generic reader rejects ORF");
        let plain = jpeg_with_orientation(8, 8, None);
        assert_eq!(raw_orientation(&orf, &plain), Some(Orientation::Rotate90));

        let preview = jpeg_with_orientation(1200, 800, Some(6));
        let mut raf = b"FUJIFILMCCD-RAW 0201FF383501".to_vec();
        raf.resize(100, 0);
        raf[84..88].copy_from_slice(&100u32.to_be_bytes());
        raf[88..92].copy_from_slice(&(preview.len() as u32).to_be_bytes());
        raf.extend_from_slice(&preview);
        let (_, w, h) = decode_raw_preview(std::path::Path::new("portrait.RAF"), Some(&raf)).expect("RAF preview");
        assert!(h > w, "the preview's own EXIF turns a portrait RAF upright: {w}x{h}");
    }

    #[test]
    fn exif_datetime_reads_offsets_and_rejects_placeholders() {
        let now = 1_900_000_000.0;
        assert_eq!(exif_datetime_to_unix("2019:07:04 12:00:00", Some("+00:00"), now), Some(1_562_241_600.0));
        assert_eq!(exif_datetime_to_unix("2019:07:04 14:00:00", Some("+02:00"), now), Some(1_562_241_600.0));
        assert!(exif_datetime_to_unix("2019:07:04 12:00:00", None, now).is_some());
        assert_eq!(exif_datetime_to_unix("0000:00:00 00:00:00", None, now), None);
        assert_eq!(exif_datetime_to_unix("1980:01:01 00:00:00", Some("+00:00"), now), None);
        assert_eq!(exif_datetime_to_unix("2099:01:01 00:00:00", Some("+00:00"), now), None);
        assert_eq!(exif_datetime_to_unix("    :  :     :  :  ", None, now), None);
        for (text, want) in [("5", 0.5), ("123", 0.123), (" 07\0", 0.07), ("", 0.0), ("ab", 0.0)] {
            assert!((exif_subsec_fraction(text) - want).abs() < 1e-9, "{text:?}");
        }
        for malformed in ["+0200", "+02", "Z", "02:00", "+02:00:00"] {
            assert_eq!(exif_offset(malformed), None, "{malformed}");
        }
        assert_eq!(exif_offset(" -05:30 "), chrono::FixedOffset::west_opt(5 * 3_600 + 30 * 60));
    }

    /// America/Chicago in 2026: CDT from 2026-03-08 08:00 UTC to 2026-11-01 07:00 UTC.
    #[derive(Clone, Copy)]
    struct Chicago2026;

    impl Chicago2026 {
        const SPRING: i64 = 1_772_956_800;
        const FALL: i64 = 1_793_516_400;

        fn at(utc: i64) -> chrono::FixedOffset {
            let hours = if (Self::SPRING..Self::FALL).contains(&utc) { 5 } else { 6 };
            chrono::FixedOffset::west_opt(hours * 3_600).unwrap()
        }
    }

    impl chrono::TimeZone for Chicago2026 {
        type Offset = chrono::FixedOffset;
        fn from_offset(_: &chrono::FixedOffset) -> Self {
            Chicago2026
        }
        fn offset_from_local_date(&self, local: &chrono::NaiveDate) -> chrono::MappedLocalTime<chrono::FixedOffset> {
            self.offset_from_local_datetime(&local.and_hms_opt(12, 0, 0).unwrap())
        }
        fn offset_from_local_datetime(&self, local: &chrono::NaiveDateTime) -> chrono::MappedLocalTime<chrono::FixedOffset> {
            let wall = local.and_utc().timestamp();
            let [cdt, cst] = [5, 6].map(|hours| chrono::FixedOffset::west_opt(hours * 3_600).unwrap());
            let fits = |offset: chrono::FixedOffset| Self::at(wall - i64::from(offset.local_minus_utc())) == offset;
            match (fits(cdt), fits(cst)) {
                (true, true) => chrono::MappedLocalTime::Ambiguous(cdt, cst),
                (true, false) => chrono::MappedLocalTime::Single(cdt),
                (false, true) => chrono::MappedLocalTime::Single(cst),
                (false, false) => chrono::MappedLocalTime::None,
            }
        }
        fn offset_from_utc_date(&self, utc: &chrono::NaiveDate) -> chrono::FixedOffset {
            Self::at(utc.and_hms_opt(12, 0, 0).unwrap().and_utc().timestamp())
        }
        fn offset_from_utc_datetime(&self, utc: &chrono::NaiveDateTime) -> chrono::FixedOffset {
            Self::at(utc.and_utc().timestamp())
        }
    }

    /// Values macOS `Tagging.exifCaptureDate` returns in America/Chicago: the
    /// repeated 01:30 on 2026-11-01 is the CST instant, the skipped 02:30 on
    /// 2026-03-08 is no date, and its neighbours are unchanged.
    #[test]
    fn exif_wall_times_across_daylight_saving_match_macos() {
        let now = 1_900_000_000.0;
        let at = |text: &str| exif_datetime_in(text, None, now, &Chicago2026);
        assert_eq!(at("2026:11:01 01:30:00"), Some(1_793_518_200.0));
        assert_eq!(at("2026:11:01 00:59:59"), Some(1_793_512_799.0));
        assert_eq!(at("2026:11:01 02:00:00"), Some(1_793_520_000.0));
        assert_eq!(at("2026:03:08 02:30:00"), None);
        assert_eq!(at("2026:03:08 03:30:00"), Some(1_772_958_600.0));
        assert_eq!(exif_datetime_in("2026:11:01 01:30:00", Some("-05:00"), now, &Chicago2026), Some(1_793_514_600.0));
    }

    #[test]
    fn exif_from_head_extracts_camera_model() {
        let mut jpeg: Vec<u8> = vec![0xFF, 0xD8, 0xFF, 0xE1, 0x00, 0x28];
        jpeg.extend_from_slice(b"Exif\0\0");
        jpeg.extend_from_slice(&[
            0x49, 0x49, 0x2A, 0x00, 0x08, 0x00, 0x00, 0x00, // II*\0, IFD0 @ 8
            0x01, 0x00, // 1 entry
            0x10, 0x01, 0x02, 0x00, 0x06, 0x00, 0x00, 0x00, // Model, ASCII, count 6
            0x1A, 0x00, 0x00, 0x00, // value @ 26
            0x00, 0x00, 0x00, 0x00, // next IFD: none
        ]);
        jpeg.extend_from_slice(b"Pixel\0");
        jpeg.extend_from_slice(&[0xFF, 0xD9]);

        let p = std::env::temp_dir().join(format!(
            "fileid-exifhead-{}.jpg",
            std::process::id()
        ));
        std::fs::write(&p, &jpeg).unwrap();
        let (model, lat, lon, _) = exif_from_head(&p).expect("readable file must yield Some");
        assert_eq!(model.as_deref(), Some("Pixel"));
        assert_eq!((lat, lon), (None, None));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn dhash_changes_with_horizontal_gradient() {
        // A decreasing gradient sets every bit: the result is -1 (all ones).
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

    fn synthetic_candidate(ordinal: usize, quality: f32) -> FaceCandidate {
        FaceCandidate {
            ordinal,
            detection: face_geometry::Detection {
                bbox: [ordinal as f32, 0.0, 1.0, 1.0],
                landmarks: [[0.0; 2]; 5],
                score: quality,
            },
            bbox_xywh: [ordinal as f32, 0.0, 1.0, 1.0],
            quality,
        }
    }

    #[test]
    fn crowded_face_candidates_rank_by_quality_then_detector_order() {
        let mut candidates = vec![
            synthetic_candidate(0, 0.5),
            synthetic_candidate(1, 0.9),
            synthetic_candidate(2, 0.9),
            synthetic_candidate(3, 0.8),
        ];
        assert!(prioritize_face_candidates(&mut candidates, 2));
        assert_eq!(
            candidates.iter().map(|candidate| candidate.ordinal).collect::<Vec<_>>(),
            [1, 2, 3, 0]
        );
    }

    #[test]
    fn ordinary_face_candidates_keep_detector_order() {
        let mut candidates = vec![
            synthetic_candidate(0, 0.2),
            synthetic_candidate(1, 0.9),
        ];
        assert!(!prioritize_face_candidates(&mut candidates, 2));
        assert_eq!(
            candidates.iter().map(|candidate| candidate.ordinal).collect::<Vec<_>>(),
            [0, 1]
        );
    }

    #[test]
    fn ranked_candidates_continue_past_failures_until_the_cap_is_full() {
        let mut candidates: Vec<_> = (0..1024)
            .map(|ordinal| synthetic_candidate(ordinal, ordinal as f32 / 1024.0))
            .collect();
        prioritize_face_candidates(&mut candidates, FACE_MAX_PER_IMAGE);
        let successful: Vec<_> = candidates
            .iter()
            .filter(|candidate| candidate.ordinal != 1023)
            .take(FACE_MAX_PER_IMAGE)
            .map(|candidate| candidate.ordinal)
            .collect();
        assert_eq!(successful.len(), FACE_MAX_PER_IMAGE);
        assert_eq!(successful[0], 1022);
        assert_eq!(successful[FACE_MAX_PER_IMAGE - 1], 991);
    }

    #[test]
    fn face_cap_override_can_only_lower_the_production_bound() {
        assert_eq!(parse_face_max_per_image(None), FACE_MAX_PER_IMAGE);
        assert_eq!(parse_face_max_per_image(Some("0")), 1);
        assert_eq!(parse_face_max_per_image(Some("8")), 8);
        assert_eq!(parse_face_max_per_image(Some("512")), FACE_MAX_PER_IMAGE);
        assert_eq!(parse_face_max_per_image(Some("invalid")), FACE_MAX_PER_IMAGE);
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
            created_unix: None,
            captured_from_exif: false,
            scanned_unix: 1_710_504_001.0,
            has_faces: false,
            faces: Vec::new(),
            vision_faces: None,
            has_text: false,
            ocr_text: None,
            ocr_handwritten: false,
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
            legacy_hashes: None,
            text_embedding: None,
            doc_text: None,
            tags: Vec::new(),
            faces_evaluated: false,
            ocr_stage_ran: false,
            doc_stage_ran: false,
            text_stage_done: false,
            tags_evaluated: true,
            reuse_twin: false,
            heal_only: false,
            failure_is_transient: false,
            catch_up: None,
        }
    }

    // Aspect tags are not emitted: they dominated `TopTwoTags` on files without classifier output.
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
        let mut t = stub_tagged(1300, 1000, 5_000_000);
        push_enriched_extras(&mut t);
        assert!(!t.tags.iter().any(|(s, _)| s == "Square"));
        assert!(!t.tags.iter().any(|(s, _)| s == "Wide"));
        assert!(!t.tags.iter().any(|(s, _)| s == "Tall"));
    }

    // Year_ is pinned to the macOS `extraTags` rule; mid-year noon-UTC timestamps resolve to the same calendar year in every timezone, keeping the test deterministic across CI hosts.

    const TS_2021_MIDYEAR: f64 = 1_625_140_800.0;
    const TS_2018_MIDYEAR: f64 = 1_530_446_400.0;
    // Exactly on the 1990 boundary: the rule is strictly `> 1990`, so no tag.
    const TS_1990_MIDYEAR: f64 = 646_833_600.0;

    fn year_tag(t: &TaggedFile) -> Option<i32> {
        t.tags.iter().find_map(|(s, _)| {
            s.strip_prefix("Year_").and_then(|y| y.parse::<i32>().ok())
        })
    }

    #[test]
    fn year_tag_prefers_creation_date_over_modified() {
        let mut t = stub_tagged(800, 600, 1_000);
        // Creation date wins over a later modification date.
        t.modified_unix = TS_2021_MIDYEAR;
        t.created_unix = Some(TS_2018_MIDYEAR);
        push_enriched_extras(&mut t);
        assert_eq!(year_tag(&t), Some(2018),
            "Year_ tag must derive from creation date, not modified");
    }

    #[test]
    fn year_tag_falls_back_to_modified_when_no_creation() {
        let mut t = stub_tagged(800, 600, 1_000);
        t.modified_unix = TS_2021_MIDYEAR;
        t.created_unix = None;
        push_enriched_extras(&mut t);
        assert_eq!(year_tag(&t), Some(2021),
            "no birth time → fall back to modified date");
    }

    #[test]
    fn year_tag_respects_strict_1990_lower_bound() {
        let mut t = stub_tagged(800, 600, 1_000);
        t.created_unix = Some(TS_1990_MIDYEAR);
        t.modified_unix = TS_1990_MIDYEAR;
        push_enriched_extras(&mut t);
        assert_eq!(year_tag(&t), None,
            "year == 1990 must not emit a Year_ tag (macOS uses strict > 1990)");
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
