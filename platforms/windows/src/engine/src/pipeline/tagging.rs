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
use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;
use tokio::sync::{mpsc, Semaphore};

use crate::coordinator::ScanCoordinator;
use crate::models::{arcface::ArcFace, mobileclip::MobileClipImage, scrfd::{self, Scrfd}};
use crate::pipeline::discovery::{DiscoveredFile, FileKind};
use crate::shell;

/// Bounded channel capacity, Tagging → DBWriter. Matches macOS's 256 —
/// one transaction worth + slack so workers don't stall on a slow flush.
pub const TAGGING_CHANNEL_CAP: usize = 256;

/// Cap concurrent ORT vision-model inferences.
const VISION_CONCURRENCY: usize = 4;

/// Cap concurrent CLIP image embeds.
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
pub struct ModelStack {
    pub arcface: Option<Mutex<ArcFace>>,
    pub scrfd: Option<Mutex<Scrfd>>,
    pub mobileclip: Option<Mutex<MobileClipImage>>,
}

impl ModelStack {
    /// Load whatever model files are installed at the canonical paths.
    /// Missing weights are silently skipped; `tracing::info!` on success
    /// makes the install state visible in the engine log.
    pub fn load_default() -> Self {
        let arcface = load_optional("ArcFace", crate::models::arcface::default_weights_path(), |p| {
            ArcFace::load(p)
        });
        let scrfd = load_optional("SCRFD", scrfd::default_weights_path(), Scrfd::load);
        let mobileclip = load_optional("MobileCLIP", crate::models::mobileclip::default_weights_path(), |p| {
            MobileClipImage::load(p)
        });
        Self { arcface, scrfd, mobileclip }
    }

    pub fn empty() -> Self {
        Self { arcface: None, scrfd: None, mobileclip: None }
    }
}

fn load_optional<T, F>(label: &str, path: anyhow::Result<PathBuf>, loader: F) -> Option<Mutex<T>>
where
    F: FnOnce(PathBuf) -> anyhow::Result<T>,
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
    match loader(p.clone()) {
        Ok(model) => {
            tracing::info!(model = label, path = %p.display(), "model loaded");
            Some(Mutex::new(model))
        }
        Err(err) => {
            tracing::warn!(model = label, ?err, "model load failed; stage will skip");
            None
        }
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

        for _ in 0..self.worker_count {
            let rx = fan_rx.clone();
            let tx = out_tx.clone();
            let coord = self.coordinator.clone();
            let vision_sem = vision_sem.clone();
            let clip_sem = clip_sem.clone();
            let models = self.models.clone();

            tokio::spawn(async move {
                while let Ok(file) = rx.recv().await {
                    if coord.check().await.is_err() {
                        break;
                    }
                    let tagged = process_file(&file, &models, &vision_sem, &clip_sem).await;
                    if tx.send(tagged).await.is_err() {
                        break;
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

    let image_source: Option<(Vec<u8>, u32, u32)> = match file.kind {
        FileKind::Image => match load_image_rgb(&file.path).await {
            Ok(t) => Some(t),
            Err(err) => {
                tracing::warn!(?err, path = %crate::platform::redact_path_for_log(&file.path), "image decode failed");
                tagged.failed = true;
                tagged.error_message = Some(format!("image decode: {err:#}"));
                None
            }
        },
        FileKind::Video => extract_video_keyframe_blocking(file.path.clone()).await.ok(),
        _ => None,
    };

    if let Some((rgb, w, h)) = image_source {
            if matches!(file.kind, FileKind::Image) {
                if let Some((cam, lat, lon)) = parse_exif_blocking(file.path.clone()).await {
                    tagged.camera_model = cam;
                    tagged.location_lat = lat;
                    tagged.location_lon = lon;
                }
            }

                tagged.phash = Some(compute_dhash(&rgb, w as usize, h as usize));

                if let (Some(scrfd_mu), Some(arcface_mu)) = (&models.scrfd, &models.arcface) {
                    let permit = vision_sem.acquire().await;
                    let vision_started = Instant::now();
                    if permit.is_ok() {
                        let detections = {
                            let mut s = scrfd_mu.lock();
                            s.detect(&rgb, w, h)
                        };
                        match detections {
                            Ok(dets) => {
                                for det in dets {
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
                                                tracing::warn!(?err, "ArcFace embed failed");
                                            }
                                        }
                                    }
                                }
                            }
                            Err(err) => {
                                tracing::warn!(?err, "SCRFD detect failed");
                            }
                        }
                    }
                    tagged.vision_ms = vision_started.elapsed().as_secs_f64() * 1000.0;
                }
                tagged.has_faces = !tagged.faces.is_empty();

                if let Some(clip_mu) = &models.mobileclip {
                    let permit = clip_sem.acquire().await;
                    let clip_started = Instant::now();
                    if permit.is_ok() {
                        let resized = resize_rgb_nearest(&rgb, w as usize, h as usize, 256, 256);
                        let embed_result = {
                            let mut c = clip_mu.lock();
                            c.embed(&resized)
                        };
                        match embed_result {
                            Ok(emb) => tagged.clip_embedding = Some(emb),
                            Err(err) => tracing::warn!(?err, "MobileCLIP embed failed"),
                        }
                    }
                    tagged.clip_ms = clip_started.elapsed().as_secs_f64() * 1000.0;
                }

                if matches!(file.kind, FileKind::Image) {
                    if let Ok(Some(ocr)) = run_ocr_blocking(rgb.clone(), w, h).await {
                        if !ocr.text.trim().is_empty() {
                            tagged.has_text = true;
                            tagged.ocr_text = Some(ocr.text);
                        }
                    }
                }
    }

    tagged.total_ms = started.elapsed().as_secs_f64() * 1000.0;
    tagged
}

/// Load an image from disk and return its RGB8 bytes + dimensions.
/// Done on a blocking thread to keep the tokio reactor free.
async fn load_image_rgb(path: &std::path::Path) -> anyhow::Result<(Vec<u8>, u32, u32)> {
    let p = path.to_path_buf();
    tokio::task::spawn_blocking(move || -> anyhow::Result<(Vec<u8>, u32, u32)> {
        let reader = image::ImageReader::open(&p)?
            .with_guessed_format()?;
        let dyn_img = reader.decode()?;
        let rgb = dyn_img.to_rgb8();
        let (w, h) = rgb.dimensions();
        Ok((rgb.into_raw(), w, h))
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
