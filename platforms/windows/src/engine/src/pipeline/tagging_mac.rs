//! The scan's per-file work when the Mac app hosts the engine. //! Vision faces are stored without vectors (box, quality, clustering hold) and get their vectors in the Mac face space later; this path never writes `rust-align112-v1` faces. `FILEID_APPLE_SCAN=0` falls back to the portable pipeline.

use super::{
    composite_hashes, load_bge_text, load_pool, local_unix_seconds_to_year, maybe_emit_stats, open_image_file,
    claim_twin, parse_exif_fields, record_stage, resize_rgb_quality, resolve_pool_size, sha256_bytes, twin_reuse_enabled, video_capture_fields, Arc,
    DiscoveredFile, Duration, ExifFields, FileKind, Instant, LegacyHashes, MobileClipImage, ModelStack, Ordering,
    PreDecoded, TaggedFile, TwinClaim, TwinRegistry, VisionFace, STATS_CLIP_US, STATS_DECODED_FILES, STATS_RAMPLUS_US,
    STATS_DECODE_US, STATS_OCR_RAN, STATS_OCR_US, STATS_TOTAL_US, STATS_TWIN_REUSED, STATS_VISION_US, TIMED_OUT_MESSAGE,
};
use crate::apple::scan;
use crate::pipeline::catch_up;
use crate::pipeline::audio_meta::AudioTags;
use std::path::Path;
use std::sync::atomic::AtomicBool;

/// Cap on the document text stored per file.
const STORED_TEXT_CHARS: usize = 256 * 1024;
/// Cap on the document text fed to BGE.
const EMBEDDING_CHARS: usize = 4000;
const VIDEO_KEYFRAME_EDGE: u32 = 512;
const VIDEO_KEYFRAME_TIMEOUT: Duration = Duration::from_secs(6);
const DECODE_FAILED: &str = "Could not decode image";
const CUT_OFF_IMAGE: &str = "The image file is cut off: it ends before the picture does";
const PERMISSION_DENIED: &str = "FileID isn't allowed to read this file";
const WENT_MISSING: &str = "The file went missing while FileID was reading it (will retry next scan)";
const PDF_OPEN_FAILED: &str = "Could not open PDF (will retry next scan)";

pub(super) fn enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("FILEID_APPLE_SCAN").map_or(true, |v| v.trim() != "0"))
}

pub(crate) struct Frame {
    image: Option<Arc<scan::Image>>,
    audio: AudioTags,
    large_pdf: bool,
}

static CLIP_READY: AtomicBool = AtomicBool::new(false);
static RAM_PLUS_READY: AtomicBool = AtomicBool::new(false);

pub(crate) fn unload_adapters() {
    CLIP_READY.store(false, Ordering::Release);
    RAM_PLUS_READY.store(false, Ordering::Release);
    scan::unload_models();
}

/// The models this path runs: CLIP and RAM++ on MLX through the adapter (both
/// fall back to the shared ONNX engine when MLX cannot load, RAM++ only when the
/// installed memory allows it), BGE through the shared ONNX engine.
/// Face models stay unloaded: faces come from Vision.
pub(super) fn model_stack(worker_count: usize) -> ModelStack {
    let pool_size = resolve_pool_size(worker_count);
    let (adapters, adapter_hold) = crate::pipeline::scan_models::ADAPTER_MODELS.hold(
        || {
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
            let ram_plus = crate::models::ram_plus::default_tags_path()
                .ok()
                .and_then(|tags| tags.parent().map(Path::to_path_buf))
                .filter(|dir| dir.join("ram_plus.onnx").is_file() && dir.join("ram_plus_tags.txt").is_file())
                .is_some_and(|dir| scan::load_ram_plus(&dir));
            (clip, ram_plus)
        },
        |(clip, ram_plus)| *clip || *ram_plus,
        unload_adapters,
    );
    let (clip, ram_plus) = adapters;
    CLIP_READY.store(clip, Ordering::Release);
    RAM_PLUS_READY.store(ram_plus, Ordering::Release);
    tracing::info!(clip, ram_plus, "[TAGGING] Mac scan adapters (MLX)");
    let mobileclip_pool = (!clip)
        .then(|| load_pool("MobileCLIP", pool_size, crate::models::mobileclip::default_weights_path(), |p| MobileClipImage::load(p)))
        .flatten();
    let ram_plus_pool = (!ram_plus).then(tract_ram_plus_pool).flatten();
    ModelStack {
        bge_text: load_bge_text(),
        mobileclip_pool,
        ram_plus: ram_plus_pool,
        pool_size,
        apple_scan: true,
        apple_clip: clip,
        adapter_hold,
        ..ModelStack::empty()
    }
}

/// RAM++ on tract, one session: the MLX load failed (no Metal, `FILEID_RAMPLUS_BACKEND=onnx`), and the installed memory holds the f32 graph.
fn tract_ram_plus_pool() -> Option<Vec<parking_lot::Mutex<crate::models::ram_plus::RamPlusTagger>>> {
    let tags = crate::models::ram_plus::default_tags_path().ok().filter(|tags| tags.is_file())?;
    let installed_gb = crate::platform::physical_memory_gb();
    if !crate::platform::ram_plus_fits_in_memory(installed_gb) {
        tracing::warn!(model = "RAM++", installed_gb = format!("{installed_gb:.1}"), min_gb = crate::platform::RAM_PLUS_MIN_TOTAL_GB, "MLX RAM++ unavailable and too little installed memory for the tract copy; tags come from Vision only");
        return None;
    }
    let pool = load_pool("RAM++", 1, crate::models::ram_plus::default_onnx_path(), |onnx| crate::models::ram_plus::RamPlusTagger::load(onnx, tags.clone()));
    if pool.is_some() {
        tracing::info!(model = "RAM++", "MLX RAM++ unavailable; tagging with the tract copy");
    }
    pool
}

fn ram_plus_ready(models: &ModelStack) -> bool {
    RAM_PLUS_READY.load(Ordering::Acquire) || models.ram_plus.is_some()
}

/// RAM++ tags of one image: the adapter's (which agrees them with Vision) or, when MLX is out, the tract copy's.
fn ram_plus_tags(image: &scan::Image, pass: &scan::PrimaryPass, models: &ModelStack) -> Vec<(String, f32)> {
    if RAM_PLUS_READY.load(Ordering::Acquire) {
        return image.ram_plus_tags(Some(pass)).unwrap_or_default();
    }
    let Some(pool) = models.ram_plus.as_ref() else { return Vec::new() };
    let Some((rgb, width, height)) = image.rgb() else { return Vec::new() };
    let started = Instant::now();
    let tags = crate::models::ram_plus::RamPlusTagger::preprocess_tensor(&rgb, width, height)
        .and_then(|chw| crate::models::checkout(pool, 0).tag_prepared(chw))
        .map_err(|error| tracing::warn!(?error, "RAM++ (tract) failed"))
        .unwrap_or_default();
    record_stage(&STATS_RAMPLUS_US, started);
    super::without_people_words(tags, pass.face_count == 0)
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

/// An image the decoder gave up on is unreadable for the reason the operating system gives when it is opened: a file
/// FileID may not read, or one that vanished (renamed, deleted, volume unplugged) is not a damaged image. The "will
/// retry" wording keeps a vanished file out of the Unreadable count and in the next scan.
fn decode_failure(path: &Path) -> &'static str {
    match std::fs::File::open(crate::util::path_safety::to_extended_length(path)) {
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => PERMISSION_DENIED,
        Err(error) if went_missing(&error) => WENT_MISSING,
        _ => DECODE_FAILED,
    }
}

fn went_missing(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound
        || matches!(error.raw_os_error(), Some(libc::ENXIO | libc::ENODEV | libc::EIO | libc::ENOTDIR))
}

fn stored_text(text: Option<String>) -> Option<String> {
    let trimmed = text?.trim().to_string();
    (!trimmed.is_empty()).then(|| trimmed.chars().take(STORED_TEXT_CHARS).collect())
}

pub(super) fn predecode(file: DiscoveredFile, legacy: bool, twins: Option<&TwinRegistry>, from_decoder: bool) -> PreDecoded {
    let started = Instant::now();
    let (bytes, content_hash, legacy_hashes) = read_for_hash(&file, legacy);
    let twin = match (content_hash, file.kind, twins, bytes.as_deref()) {
        (Some(hash), FileKind::Image, Some(twins), Some(_)) if twin_reuse_enabled() => claim_twin(twins, hash, from_decoder),
        _ => TwinClaim::None,
    };
    let awaits_twin = matches!(twin, TwinClaim::Ready | TwinClaim::Pending(_));
    let exif = match bytes.as_deref() {
        Some(bytes) if awaits_twin => Some(parse_exif_fields(bytes).unwrap_or((None, None, None, None))),
        _ if file.kind == FileKind::Video && !file.online_only => video_capture_fields(&file.path),
        _ => None,
    };
    let mut frame = Frame { image: None, audio: AudioTags::default(), large_pdf: false };
    let mut decoded = None;
    let mut doc_text = None;
    if !file.online_only {
        match file.kind {
            FileKind::Image if awaits_twin => {}
            FileKind::Image if bytes.as_deref().is_some_and(crate::util::jpeg_complete::is_cut_off) => {
                decoded = Some(Err(anyhow::anyhow!(CUT_OFF_IMAGE)));
            }
            FileKind::Image => match scan::decode_image(&file.path, file.size_bytes) {
                Some(image) => frame.image = Some(Arc::new(image)),
                None => decoded = Some(Err(anyhow::anyhow!(decode_failure(&file.path)))),
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
        exif,
        twin,
        budget: None,
        mac: Some(frame),
    }
}

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
    if let Some(year) = created.and_then(|t| crate::util::not_in_the_future(t, crate::util::unix_now())).and_then(local_unix_seconds_to_year)
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
    let mut model = crate::models::checkout(pool, 0);
    model.embed(&resized).map_err(|error| tracing::warn!(?error, "MobileCLIP embed failed")).ok()
}

pub(super) async fn process(predecoded: PreDecoded, models: &Arc<ModelStack>) -> TaggedFile {
    let started = Instant::now();
    let PreDecoded { file, decoded, doc_text, content_hash, legacy_hashes, exif, twin, mac, .. } = predecoded;
    let frame = mac.unwrap_or(Frame { image: None, audio: AudioTags::default(), large_pdf: false });
    let mut row = TaggedFile::bare(&file);
    if file.online_only {
        row.total_ms = started.elapsed().as_secs_f64() * 1000.0;
        return row;
    }
    row.content_hash = content_hash;
    row.legacy_hashes = legacy_hashes;
    if file.kind == FileKind::Image && matches!(twin, TwinClaim::Ready) {
        twin_row(&mut row, &file, exif);
        row.total_ms = started.elapsed().as_secs_f64() * 1000.0;
        STATS_TWIN_REUSED.fetch_add(1, Ordering::Relaxed);
        record_stage(&STATS_TOTAL_US, started);
        maybe_emit_stats();
        return row;
    }
    match file.kind {
        FileKind::Image => image_row(&mut row, &file, decoded, frame.image, models).await,
        FileKind::Video => {
            row.tags = vec![("Video".into(), None)];
            row.tags_evaluated = true;
            if let Some(captured) = exif.and_then(|fields| fields.3) {
                row.created_unix = Some(captured);
                row.captured_from_exif = true;
            }
            if let Some(image) = frame.image {
                let models = models.clone();
                let clip_started = Instant::now();
                row.clip_embedding = blocking(move || clip_embedding(&image, &models)).await.flatten();
                row.clip_ms = clip_started.elapsed().as_secs_f64() * 1000.0;
            } else {
                row.text_stage_done = true;
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

/// A copy of an analyzed image: the twin's model output (copied by the writer, with the camera
/// and location the same bytes carry) under this file's own dates and Year tag.
fn twin_row(row: &mut TaggedFile, file: &DiscoveredFile, exif: Option<ExifFields>) {
    let captured = exif.and_then(|(_, _, _, captured)| captured);
    let created = captured.or(file.created_unix);
    row.created_unix = created;
    row.captured_from_exif = captured.is_some();
    row.tags = extra_tags(created, None).into_iter().map(|tag| (tag, None)).collect();
    // Marks the row as the Mac scan's for the writer: copied faces stay
    // without vectors and the file revision is left to the Mac face pipeline.
    row.vision_faces = Some(Vec::new());
    row.reuse_twin = true;
}

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
        if read.careful_owed {
            row.catch_up = Some(catch_up::Marker {
                stages: catch_up::CAREFUL_TEXT_OWED | catch_up::TEXT_READ_AT_SCAN,
                vision: None,
                face_count: 0,
            });
        }
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
    // Deferred (the default): the row gets the classifier's tags now and the
    // catch-up pass adds RAM++'s after the main pass, so the scan runs at the
    // classifier's speed. `FILEID_RAMPLUS_INLINE=1` keeps RAM++ in the scan.
    let ram_ready = ram_plus_ready(models);
    let deferred = ram_ready && catch_up::ram_plus_deferred(true);
    let ram_models = models.clone();
    let analysis = blocking(move || {
        let pass = image.primary_pass().filter(|pass| pass.complete)?;
        let ram = if ram_ready && !deferred { ram_plus_tags(&image, &pass, &ram_models) } else { Vec::new() };
        let text_tags: Vec<String> = pass.classify_tags.iter().cloned().chain(ram.iter().map(|(tag, _)| tag.clone())).collect();
        let read = image.text_read(&pass.classify_tags, &text_tags, has_camera).unwrap_or_default();
        Some((pass, ram, read, text_tags, image.dhash()))
    })
    .await
    .flatten();
    row.vision_ms = vision_started.elapsed().as_secs_f64() * 1000.0;
    STATS_VISION_US.fetch_add(vision_started.elapsed().as_micros() as u64, Ordering::Relaxed);
    let (clip, clip_elapsed) = clip_task.await.unwrap_or((None, Duration::ZERO));
    STATS_CLIP_US.fetch_add(clip_elapsed.as_micros() as u64, Ordering::Relaxed);
    let Some((pass, ram, read, text_tags, dhash)) = analysis else {
        row.failed = true;
        row.error_message = Some(TIMED_OUT_MESSAGE.into());
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
    row.camera_model.clone_from(&info.camera_model);
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
    let extras = extra_tags(created, info.camera_model.as_deref());
    let owed = (if deferred { catch_up::TAGS_OWED } else { 0 }) | (if read.careful_owed { catch_up::CAREFUL_TEXT_OWED } else { 0 });
    if owed != 0 {
        let confidences = pass.classify_confidences.iter().map(|(name, value)| (name.clone(), *value as f32));
        row.catch_up = Some(catch_up::Marker {
            stages: owed | if read.ran { catch_up::TEXT_READ_AT_SCAN } else { 0 },
            vision: catch_up::Context {
                x: Some(extras.clone()),
                t: (!deferred).then_some(text_tags),
                ..catch_up::Context::from_classifier(&pass.classify_tags, confidences)
            }
            .encoded(),
            face_count: pass.face_count as i64,
        });
    }
    let classify = pass.classify_tags.into_iter().map(|tag| (tag, None)).collect();
    row.tags = catch_up::assemble_tags(ram, classify, extras, 16, catch_up::unicode_fold);
    row.tags_evaluated = true;
}

/// The Mac scan's decoder, RAM++ and text read behind the catch-up pass.
/// Holds the stack so the adapter's RAM++ stays loaded until the catch-up ends.
pub(super) struct CatchUp {
    pub(super) models: Arc<ModelStack>,
}

impl CatchUp {
    fn read(image: &scan::Image, classify: &[String], text_tags: &[String], has_camera: bool) -> catch_up::TextRead {
        let read = image.text_read(classify, text_tags, has_camera).unwrap_or_default();
        catch_up::TextRead { ran: read.ran, text: read.text, handwritten: read.handwritten, careful_owed: read.careful_owed }
    }
}

impl catch_up::Analyzer for CatchUp {
    fn ready(&self, phase: catch_up::Phase) -> bool {
        match phase {
            catch_up::Phase::Tags => ram_plus_ready(&self.models),
            catch_up::Phase::CarefulText => scan::ocr_ready(),
        }
    }

    fn parallelism(&self) -> usize {
        std::env::var("FILEID_CATCHUP_WORKERS").ok().and_then(|value| value.parse().ok()).filter(|n| (1..=16).contains(n)).unwrap_or(4)
    }

    fn batch(&self) -> usize {
        self.parallelism() * 6
    }

    fn ready_wait(&self, phase: catch_up::Phase) -> Duration {
        let warming = std::env::var("FILEID_OCR_WARMUP").ok().is_none_or(|value| value != "0");
        match phase {
            // Wait only for a reader that is still compiling; one paused after failures stays paused for minutes.
            catch_up::Phase::CarefulText if warming && !scan::ocr_warmed() => Duration::from_secs(180),
            _ => Duration::ZERO,
        }
    }

    fn analyse(&self, file: &catch_up::Pending, phase: catch_up::Phase, context: Option<&catch_up::Context>) -> catch_up::Outcome {
        use catch_up::Outcome;
        if file.kind == "pdf" {
            return match scan::pdf_ocr(&file.path) {
                Some(read) if read.opened && read.careful_owed => Outcome::Blocked,
                Some(read) if read.opened => Outcome::Done(catch_up::Update {
                    tags: None,
                    text: Some(catch_up::TextUpdate { handwritten: read.handwritten && read.text.is_some(), text: read.text }),
                    stages: file.stages & !catch_up::CAREFUL_TEXT_OWED,
                    vision: file.vision.clone(),
                }),
                _ => Outcome::Failed,
            };
        }
        let Some(context) = context else { return Outcome::Stale };
        let Some(image) = scan::decode_image(&file.path, file.size_bytes.max(0) as u64) else {
            return Outcome::Failed;
        };
        let has_camera = image.info.camera_model.is_some();
        match phase {
            catch_up::Phase::Tags => {
                let pass = scan::PrimaryPass {
                    complete: true,
                    classify_tags: context.k.clone(),
                    classify_confidences: context.confidences(),
                    face_count: file.face_count.max(0) as usize,
                    faces: Vec::new(),
                };
                let ram = ram_plus_tags(&image, &pass, &self.models);
                // A marker written by the Swift engine carries no extras; derive them from the capture time or the file's birth.
                let extras = context.x.clone().unwrap_or_else(|| {
                    let born = std::fs::metadata(&file.path).ok().and_then(|meta| meta.created().ok()).and_then(crate::util::catalog_unix_seconds);
                    extra_tags(image.info.captured_at.or(born), image.info.camera_model.as_deref())
                });
                Outcome::Done(catch_up::classifier_update(file, context, ram, extras, |text_tags| {
                    Self::read(&image, &context.k, text_tags, has_camera)
                }))
            }
            catch_up::Phase::CarefulText => catch_up::careful_update(file, context, |text_tags| {
                Self::read(&image, &context.k, text_tags, has_camera)
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_image_the_decoder_gave_up_on_is_reported_for_the_reason_the_file_cannot_be_opened() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("fileid-decode-failure-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let broken = dir.join("broken.jpg");
        std::fs::write(&broken, b"not a jpeg").unwrap();
        assert_eq!(decode_failure(&broken), DECODE_FAILED);
        assert_eq!(decode_failure(&dir.join("missing.jpg")), WENT_MISSING);
        assert!(!decode_failure(&dir.join("missing.jpg")).contains("decode"), "a vanished file is not a damaged image");
        assert!(WENT_MISSING.contains("will retry"), "the Library's Unreadable count leaves out rows that say will retry");
        std::fs::set_permissions(&broken, std::fs::Permissions::from_mode(0o000)).unwrap();
        let denied = std::fs::File::open(&broken).is_err();
        if denied {
            assert_eq!(decode_failure(&broken), PERMISSION_DENIED);
        }
        std::fs::set_permissions(&broken, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_video_row_takes_the_container_creation_time_not_the_file_system_time() {
        let dir = std::env::temp_dir().join(format!("fileid-video-capture-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("C0029.mp4");
        std::fs::write(&clip, crate::util::video_creation_date::tests::synthetic_mp4(1_791_128_655)).unwrap();
        let copied_on = 1_795_118_879.0;
        let file = DiscoveredFile {
            path: clip,
            kind: FileKind::Video,
            size_bytes: 4_000,
            modified_unix: copied_on,
            created_unix: Some(copied_on),
            online_only: false,
            file_ref: None,
            moved_from_known_row: false,
        };
        let row = process(predecode(file, false, None, false), &Arc::new(ModelStack::empty())).await;
        assert_eq!(row.created_unix, Some(1_791_128_655.0));
        assert!(row.captured_from_exif, "the writer must replace the file-system date with the capture date");
        assert!(row.text_stage_done, "a clip that gave no frame is recorded as attempted so a rescan does not walk it again");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn aesthetic_matches_the_swift_proxy() {
        assert!((aesthetic(1536, 1024, 5 * 1_048_576) - (0.5 + 0.5 * (1536.0 * 1024.0 / 1e6 / 12.0))).abs() < 1e-12);
        assert!((aesthetic(6000, 4000, 50 * 1_048_576) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn extra_tags_follow_tagging_swift() {
        let tags = extra_tags(Some(1_717_243_200.0), Some("ILCE-7M3 Sony"));
        assert_eq!(tags, vec!["Year_2024".to_string(), "Sony".to_string()]);
        assert!(extra_tags(Some(100.0), Some("Mystery Cam")).is_empty());
        assert!(extra_tags(None, None).is_empty());
    }

    #[test]
    fn a_twin_row_keeps_its_own_dates_and_leaves_the_analysis_to_the_writer() {
        let file = DiscoveredFile {
            path: "/lib/copy.jpg".into(),
            kind: FileKind::Image,
            size_bytes: 10,
            modified_unix: 5.0,
            created_unix: Some(1_561_982_400.0),
            online_only: false,
            file_ref: None,
            moved_from_known_row: false,
        };
        let mut row = TaggedFile::bare(&file);
        twin_row(&mut row, &file, Some((Some("iPhone 15".into()), Some(1.0), Some(2.0), Some(1_717_243_200.0))));
        assert!(row.reuse_twin && row.captured_from_exif);
        assert_eq!(row.created_unix, Some(1_717_243_200.0));
        assert_eq!(row.tags, vec![("Year_2024".to_string(), None)]);
        assert!(row.camera_model.is_none() && row.location_lat.is_none(), "camera and location come from the twin row");
        assert!(!row.tags_evaluated && !row.faces_evaluated && !row.ocr_stage_ran, "nothing of an earlier scan is deleted before the copy");
        assert_eq!(row.vision_faces, Some(Vec::new()));

        let mut plain = TaggedFile::bare(&file);
        twin_row(&mut plain, &file, Some((None, None, None, None)));
        assert!(!plain.captured_from_exif);
        assert_eq!(plain.tags, vec![("Year_2019".to_string(), None)]);
    }

    #[test]
    fn stored_text_trims_and_caps_by_characters() {
        assert_eq!(stored_text(Some("  é  \n".into())), Some("é".into()));
        assert_eq!(stored_text(Some(" \n".into())), None);
        let long = "a".repeat(STORED_TEXT_CHARS + 5);
        assert_eq!(stored_text(Some(long)).map(|t| t.chars().count()), Some(STORED_TEXT_CHARS));
    }
}
