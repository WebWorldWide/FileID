//! Stage 5 (convergence): the scan's per-file work when the Mac app hosts the
//! engine. Rust keeps the orchestration (discovery, decoder threads, workers,
//! admission, the DB writer); FileIDAppleKit decodes and runs Vision, the text
//! read, CLIP and RAM++, and each kind's stored result follows the Swift
//! engine's `Tagging.processFile`, so a library scanned by either engine reads
//! the same. Faces are stored as the Swift scan stores them (Vision boxes,
//! quality and the clustering hold, no vectors yet) and get their vectors in the
//! Mac face space later; this path never writes `rust-align112-v1` faces.
//! `FILEID_APPLE_SCAN=0` falls back to the portable pipeline.

use super::*;
use crate::apple::scan;
use std::path::Path;
use std::sync::atomic::AtomicBool;

/// The Swift engine's `Tagging.storedTextChars`.
const STORED_TEXT_CHARS: usize = 256 * 1024;
/// `DocText.embeddingChars`: the BGE input.
const EMBEDDING_CHARS: usize = 4000;
const VIDEO_KEYFRAME_EDGE: u32 = 512;
const VIDEO_KEYFRAME_TIMEOUT: Duration = Duration::from_secs(6);
const DECODE_FAILED: &str = "Could not decode image";
const VISION_TIMEOUT: &str = "Vision pass timed out (will retry next scan)";
const PDF_OPEN_FAILED: &str = "Could not open PDF (will retry next scan)";

pub(super) fn enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("FILEID_APPLE_SCAN").map_or(true, |v| v.trim() != "0"))
}

/// What the decoder thread hands the worker besides the shared fields.
pub(crate) struct Frame {
    image: Option<Arc<scan::Image>>,
    audio: crate::pipeline::audio_meta::AudioTags,
    large_pdf: bool,
}

static CLIP_READY: AtomicBool = AtomicBool::new(false);
static RAM_PLUS_READY: AtomicBool = AtomicBool::new(false);

/// The models this path runs: CLIP and RAM++ on MLX through the adapter (CLIP
/// falls back to ONNX Runtime when MLX cannot load), BGE through ONNX Runtime.
/// Face models stay unloaded: faces come from Vision.
pub(super) fn model_stack(worker_count: usize, runtime_available: bool) -> ModelStack {
    let pool_size = if runtime_available { resolve_pool_size(worker_count) } else { 1 };
    let clip = crate::models::mobileclip::default_weights_path()
        .ok()
        .filter(|path| path.is_file())
        .is_some_and(|path| {
            crate::models::clip_space::load_verified(&[(&path, "mobileclip_s2", 0)], || {
                if scan::load_clip(&path) { Ok(()) } else { anyhow::bail!("MLX CLIP unavailable") }
            })
            .map_err(|error| tracing::warn!(model = "CLIP", %error, "MLX image tower not loaded"))
            .is_ok()
        });
    CLIP_READY.store(clip, Ordering::Release);
    let ram_plus = crate::models::ram_plus::default_tags_path()
        .ok()
        .and_then(|tags| tags.parent().map(Path::to_path_buf))
        .filter(|dir| dir.join("ram_plus.onnx").is_file() && dir.join("ram_plus_tags.txt").is_file())
        .is_some_and(|dir| scan::load_ram_plus(&dir));
    RAM_PLUS_READY.store(ram_plus, Ordering::Release);
    tracing::info!(clip, ram_plus, "[TAGGING] Mac scan adapters (MLX)");
    let mobileclip_pool = (!clip && runtime_available)
        .then(|| load_pool("MobileCLIP", pool_size, crate::models::mobileclip::default_weights_path(), |p| MobileClipImage::load(p)))
        .flatten();
    ModelStack {
        bge_text: if runtime_available { load_bge_text() } else { None },
        mobileclip_pool,
        pool_size,
        apple_scan: true,
        apple_clip: clip,
        ..ModelStack::empty()
    }
}

fn read_for_hash(file: &DiscoveredFile, legacy: bool) -> (Option<Vec<u8>>, Option<[u8; 32]>, Option<LegacyHashes>) {
    if file.online_only {
        return (None, None, None);
    }
    let fits = file.size_bytes <= crate::util::content_hash::FULL_HASH_MAX_BYTES;
    if fits && matches!(file.kind, FileKind::Image | FileKind::Doc | FileKind::Pdf | FileKind::Audio) {
        let bytes = open_image_file(&file.path).ok().and_then(|mut f| {
            let mut bytes = Vec::new();
            (bytes.try_reserve_exact(file.size_bytes as usize).is_ok() && std::io::Read::read_to_end(&mut f, &mut bytes).is_ok())
                .then_some(bytes)
        });
        if let Some(bytes) = bytes {
            let hash = sha256_bytes(&bytes);
            let legacy = legacy.then(|| LegacyHashes::of_full_bytes(&bytes));
            return (Some(bytes), Some(hash), legacy);
        }
        return (None, None, None);
    }
    let (hash, legacy) = composite_hashes(file, legacy);
    (None, hash, legacy)
}

/// `DocText.extract` trimming and cap over any extractor's output.
fn stored_text(text: Option<String>) -> Option<String> {
    let trimmed = text?.trim().to_string();
    (!trimmed.is_empty()).then(|| trimmed.chars().take(STORED_TEXT_CHARS).collect())
}

/// Reads, hashes and decodes one file on a decoder thread.
pub(super) fn predecode(file: DiscoveredFile, legacy: bool) -> PreDecoded {
    let started = Instant::now();
    let (bytes, content_hash, legacy_hashes) = read_for_hash(&file, legacy);
    let mut frame = Frame { image: None, audio: Default::default(), large_pdf: false };
    let mut decoded = None;
    let mut doc_text = None;
    if !file.online_only {
        match file.kind {
            FileKind::Image => match scan::decode_image(&file.path, file.size_bytes) {
                Some(image) => frame.image = Some(Arc::new(image)),
                None => decoded = Some(Err(anyhow::anyhow!(DECODE_FAILED))),
            },
            FileKind::Video => {
                frame.image = scan::video_keyframe(&file.path, VIDEO_KEYFRAME_EDGE, VIDEO_KEYFRAME_TIMEOUT).map(Arc::new);
            }
            FileKind::Pdf => {
                frame.large_pdf = file.size_bytes as f64 / 1_048_576.0 > 20.0;
                if !frame.large_pdf {
                    doc_text = stored_text(scan::document_text(&file.path, STORED_TEXT_CHARS).flatten());
                }
            }
            FileKind::Doc => {
                doc_text = stored_text(match scan::document_text(&file.path, STORED_TEXT_CHARS) {
                    Some(text) => text,
                    None => crate::pipeline::doc_extract::extract(&file.path, bytes.as_deref()).ok().flatten(),
                });
            }
            FileKind::Audio => frame.audio = crate::pipeline::audio_meta::extract_structured(&file.path),
            FileKind::Other => doc_text = crate::pipeline::archive_names::list(&file.path),
            FileKind::Model => {}
        }
    }
    if frame.image.is_some() {
        STATS_DECODE_US.fetch_add(started.elapsed().as_micros() as u64, Ordering::Relaxed);
        STATS_DECODED_FILES.fetch_add(1, Ordering::Relaxed);
    }
    PreDecoded {
        file,
        decoded,
        doc_text,
        audio_tags: Vec::new(),
        content_hash,
        legacy_hashes,
        exif: None,
        budget: None,
        mac: Some(frame),
    }
}

fn now_unix() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs_f64()
}

fn base_row(file: &DiscoveredFile) -> TaggedFile {
    TaggedFile {
        path: file.path.clone(),
        kind: file.kind,
        size_bytes: file.size_bytes,
        modified_unix: file.modified_unix,
        created_unix: file.created_unix,
        captured_from_exif: false,
        scanned_unix: now_unix(),
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
    }
}

/// `Tagging.lightweightAesthetic` over the working image's size.
fn aesthetic(width: u32, height: u32, size_bytes: u64) -> f64 {
    let megapixels = f64::from(width) * f64::from(height) / 1_000_000.0;
    let size_score = (size_bytes as f64 / 1_048_576.0 / 5.0).min(1.0);
    let res_score = (megapixels / 12.0).min(1.0);
    (size_score * 0.5 + res_score * 0.5).min(1.0)
}

/// `Tagging.extraTags`: Year (capture time, else the file's birth time) and
/// camera family.
fn extra_tags(created: Option<f64>, camera: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(year) = created.and_then(local_unix_seconds_to_year)
        && year > 1990
        && year < 2100
    {
        out.push(format!("Year_{year}"));
    }
    if let Some(camera) = camera.filter(|c| !c.is_empty()) {
        let lower = camera.to_lowercase();
        let family = [
            ("iphone", "iPhone"),
            ("ipad", "iPad"),
            ("canon", "Canon"),
            ("nikon", "Nikon"),
            ("sony", "Sony"),
            ("fuji", "Fuji"),
            ("leica", "Leica"),
            ("gopro", "GoPro"),
            ("samsung", "Samsung"),
            ("pixel", "Pixel"),
        ]
        .into_iter()
        .find(|(needle, _)| lower.contains(needle));
        if let Some((_, family)) = family {
            out.push(family.to_string());
        }
    }
    out
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    tokio::task::spawn_blocking(work).await.ok()
}

fn clip_embedding(image: &scan::Image, models: &ModelStack) -> Option<Vec<f32>> {
    if CLIP_READY.load(Ordering::Acquire) {
        return image.clip_embedding();
    }
    let pool = models.mobileclip_pool.as_ref()?;
    let (rgb, w, h) = image.rgb()?;
    let resized = resize_rgb_quality(&rgb, w as usize, h as usize, 224, 224);
    let slot = pool.iter().find_map(|m| m.try_lock()).unwrap_or_else(|| pool[0].lock());
    let mut model = slot;
    model.embed(&resized).map_err(|error| tracing::warn!(?error, "MobileCLIP embed failed")).ok()
}

/// `Tagging.processFile` for one file.
pub(super) async fn process(predecoded: PreDecoded, models: &Arc<ModelStack>) -> TaggedFile {
    let started = Instant::now();
    let PreDecoded { file, decoded, doc_text, content_hash, legacy_hashes, mac, .. } = predecoded;
    let frame = mac.unwrap_or(Frame { image: None, audio: Default::default(), large_pdf: false });
    let mut row = base_row(&file);
    // A cloud placeholder is catalogued from its metadata only (reading it
    // would download it), as the portable pipeline does.
    if file.online_only {
        row.total_ms = started.elapsed().as_secs_f64() * 1000.0;
        return row;
    }
    row.content_hash = content_hash;
    row.legacy_hashes = legacy_hashes;
    match file.kind {
        FileKind::Image => image_row(&mut row, &file, decoded, frame.image, models).await,
        FileKind::Video => {
            row.tags = vec![("Video".into(), None)];
            row.tags_evaluated = true;
            if let Some(image) = frame.image {
                let models = models.clone();
                let clip_started = Instant::now();
                row.clip_embedding = blocking(move || clip_embedding(&image, &models)).await.flatten();
                row.clip_ms = clip_started.elapsed().as_secs_f64() * 1000.0;
            }
        }
        FileKind::Pdf => pdf_row(&mut row, &file, doc_text, frame.large_pdf, models).await,
        FileKind::Doc => {
            row.tags = vec![("Doc".into(), None)];
            document_text(&mut row, doc_text, models).await;
            row.text_stage_done = true;
            row.doc_stage_ran = true;
            row.tags_evaluated = true;
        }
        FileKind::Audio => {
            row.tags = vec![("Audio".into(), None)];
            for value in [frame.audio.artist, frame.audio.album, frame.audio.title].into_iter().flatten() {
                let value = value.trim().to_string();
                if !value.is_empty() && !row.tags.iter().any(|(tag, _)| *tag == value) {
                    row.tags.push((value, None));
                }
            }
            row.tags_evaluated = true;
        }
        FileKind::Model => {
            row.tags = vec![("3D Model".into(), None)];
            row.text_stage_done = true;
            row.tags_evaluated = true;
        }
        FileKind::Other => {
            row.tags = vec![("Other".into(), None)];
            row.tags_evaluated = true;
            if file.path.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) {
                row.doc_text = doc_text;
                row.text_stage_done = true;
                row.doc_stage_ran = true;
            }
        }
    }
    row.total_ms = started.elapsed().as_secs_f64() * 1000.0;
    record_stage(&STATS_TOTAL_US, started);
    maybe_emit_stats();
    row
}

/// Document keywords and the BGE embedding, as `Tagging.processDoc` stores them.
async fn document_text(row: &mut TaggedFile, text: Option<String>, models: &Arc<ModelStack>) {
    let Some(text) = text else { return };
    for (label, score) in crate::util::keywords::extract(&text) {
        row.tags.push((label, Some(score)));
    }
    if models.bge_text.is_some() {
        let (models, input) = (models.clone(), text.chars().take(EMBEDDING_CHARS).collect::<String>());
        row.text_embedding = blocking(move || models.bge_text.as_ref().and_then(|bge| bge.lock().embed(&input).ok())).await.flatten();
    }
    row.doc_text = Some(text);
}

async fn pdf_row(row: &mut TaggedFile, file: &DiscoveredFile, text: Option<String>, large: bool, models: &Arc<ModelStack>) {
    if large {
        row.tags = vec![("PDF".into(), None), ("Large_Document".into(), None)];
        row.tags_evaluated = true;
        return;
    }
    row.tags = vec![("PDF".into(), None)];
    if text.is_some() {
        document_text(row, text, models).await;
    } else {
        let path = file.path.clone();
        let read = blocking(move || scan::pdf_ocr(&path)).await.flatten().unwrap_or_default();
        if !read.opened {
            row.tags.clear();
            row.failed = true;
            row.error_message = Some(PDF_OPEN_FAILED.into());
            return;
        }
        row.ocr_stage_ran = true;
        row.has_text = read.text.is_some();
        row.ocr_handwritten = read.handwritten;
        row.ocr_text = read.text;
    }
    row.text_stage_done = true;
    row.doc_stage_ran = true;
    row.tags_evaluated = true;
}

async fn image_row(
    row: &mut TaggedFile,
    file: &DiscoveredFile,
    decoded: Option<anyhow::Result<(Vec<u8>, u32, u32)>>,
    image: Option<Arc<scan::Image>>,
    models: &Arc<ModelStack>,
) {
    let Some(image) = image else {
        let message = match decoded {
            Some(Err(error)) => format!("{error:#}"),
            _ => DECODE_FAILED.into(),
        };
        row.failed = true;
        row.error_message = Some(message);
        return;
    };
    let clip_task = {
        let (image, models) = (image.clone(), models.clone());
        tokio::task::spawn_blocking(move || {
            let started = Instant::now();
            (clip_embedding(&image, &models), started.elapsed())
        })
    };
    let info = image.info.clone();
    let has_camera = info.camera_model.is_some();
    let vision_started = Instant::now();
    let analysis = blocking(move || {
        let pass = image.primary_pass().filter(|pass| pass.complete)?;
        let ram = if RAM_PLUS_READY.load(Ordering::Acquire) { image.ram_plus_tags(Some(&pass)).unwrap_or_default() } else { Vec::new() };
        let text_tags: Vec<String> = pass.classify_tags.iter().cloned().chain(ram.iter().map(|(tag, _)| tag.clone())).collect();
        let read = image.text_read(&pass.classify_tags, &text_tags, has_camera).unwrap_or_default();
        Some((pass, ram, read, image.dhash()))
    })
    .await
    .flatten();
    row.vision_ms = vision_started.elapsed().as_secs_f64() * 1000.0;
    STATS_VISION_US.fetch_add(vision_started.elapsed().as_micros() as u64, Ordering::Relaxed);
    let (clip, clip_elapsed) = clip_task.await.unwrap_or((None, Duration::ZERO));
    STATS_CLIP_US.fetch_add(clip_elapsed.as_micros() as u64, Ordering::Relaxed);
    let Some((pass, ram, read, dhash)) = analysis else {
        row.failed = true;
        row.error_message = Some(VISION_TIMEOUT.into());
        return;
    };
    if read.ran {
        STATS_OCR_RAN.fetch_add(1, Ordering::Relaxed);
        STATS_OCR_US.fetch_add(((read.fast_ms + read.careful_ms) * 1000.0) as u64, Ordering::Relaxed);
    }
    let created = info.captured_at.or(file.created_unix);
    row.created_unix = created;
    row.captured_from_exif = info.captured_at.is_some();
    row.image_width = info.width;
    row.image_height = info.height;
    row.phash = Some(dhash);
    row.aesthetic = Some(aesthetic(info.width, info.height, file.size_bytes));
    row.camera_model = info.camera_model.clone();
    row.location_lat = info.lat;
    row.location_lon = info.lon;
    row.has_faces = pass.face_count > 0;
    row.vision_faces = Some(
        pass.faces
            .into_iter()
            .map(|face| VisionFace { bbox: face.bbox, quality: face.quality, excluded: face.excluded })
            .collect(),
    );
    row.faces_evaluated = true;
    row.ocr_stage_ran = read.ran;
    row.ocr_handwritten = read.handwritten && read.text.is_some();
    row.has_text = read.text.is_some();
    row.ocr_text = read.text;
    row.clip_embedding = clip;
    row.clip_ms = clip_elapsed.as_secs_f64() * 1000.0;
    let mut tags: Vec<(String, Option<f32>)> = if ram.is_empty() {
        pass.classify_tags.into_iter().map(|tag| (tag, None)).collect()
    } else {
        ram.into_iter().map(|(tag, score)| (tag, Some(score))).collect()
    };
    tags.extend(extra_tags(created, info.camera_model.as_deref()).into_iter().map(|tag| (tag, None)));
    let mut seen = std::collections::HashSet::new();
    tags.retain(|(tag, _)| seen.insert(tag.to_lowercase()));
    tags.truncate(16);
    row.tags = tags;
    row.tags_evaluated = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aesthetic_matches_the_swift_proxy() {
        assert!((aesthetic(1536, 1024, 5 * 1_048_576) - (0.5 + 0.5 * (1536.0 * 1024.0 / 1e6 / 12.0))).abs() < 1e-12);
        assert!((aesthetic(6000, 4000, 50 * 1_048_576) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn extra_tags_follow_tagging_swift() {
        // 2024-06-01T12:00:00Z
        let tags = extra_tags(Some(1_717_243_200.0), Some("ILCE-7M3 Sony"));
        assert_eq!(tags, vec!["Year_2024".to_string(), "Sony".to_string()]);
        assert!(extra_tags(Some(100.0), Some("Mystery Cam")).is_empty());
        assert!(extra_tags(None, None).is_empty());
    }

    #[test]
    fn stored_text_trims_and_caps_by_characters() {
        assert_eq!(stored_text(Some("  é  \n".into())), Some("é".into()));
        assert_eq!(stored_text(Some(" \n".into())), None);
        let long = "a".repeat(STORED_TEXT_CHARS + 5);
        assert_eq!(stored_text(Some(long)).map(|t| t.chars().count()), Some(STORED_TEXT_CHARS));
    }
}
