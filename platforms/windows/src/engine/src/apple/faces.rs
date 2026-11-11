//! Stage 6: faces on the Mac, end to end. Vision (landmarks, capture quality,
//! human rectangles) and the ImageIO / HEIF decode run in FileIDAppleKit; the
//! landmark match, the one-buffer alignment, SFace and every decision stay
//! here. The result lands in the Swift engine's face space
//! (`macos-landmark-overlap-v3-orientation`), so People computed by either
//! engine keep comparing.
//!
//! [`extract_file`] is the Swift engine's `FaceClustering.extractOneFile`: the
//! scan, clustering and the face refresh all call it with a photo's stored
//! boxes. [`VisionFaces`] is the Mac `TakeFaceAnalyzer`.

use std::ffi::c_char;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use parking_lot::Mutex;
use serde::Deserialize;

use super::{c_path, ffi, take_string};
use crate::models::face_align;
use crate::models::face_landmark_match::{self, NormRect};

/// Long edge of the decode faces are embedded from (`FaceClustering.loadCGImage`).
pub const EXTRACT_LONG_SIDE: u32 = 2048;

/// One Vision landmark detection: its normalized bottom-left box and five
/// points in top-left pixels of the decoded image.
#[derive(Debug, Clone, PartialEq)]
pub struct Landmarks {
    pub bounds: NormRect,
    pub points: [[f32; 2]; 5],
}

#[derive(Deserialize)]
struct OpenedJson {
    width: u32,
    height: u32,
    orientation: u8,
    faces: Vec<LandmarksJson>,
}

#[derive(Deserialize)]
struct LandmarksJson {
    bounds: [u64; 4],
    points: [u32; 10],
}

/// A photo decoded upright by the adapter, held there until dropped.
pub struct FaceImage {
    handle: u64,
    pub width: u32,
    pub height: u32,
    pub orientation: u8,
    pub landmarks: Vec<Landmarks>,
}

impl FaceImage {
    /// Decode `path` at most `max_pixel_size` on its long edge; with
    /// `landmarks`, Vision's face landmarks run once on the image. `None` when
    /// the file is tiny, unreadable or carries an invalid orientation.
    pub fn open(path: &Path, max_pixel_size: u32, landmarks: bool) -> Option<Self> {
        let path = c_path(path).ok()?;
        let mut json: *mut c_char = std::ptr::null_mut();
        // SAFETY: `path` is NUL-terminated; `json` is a local out-pointer.
        let handle = unsafe { ffi::fileid_apple_faces_open(path.as_ptr(), max_pixel_size, landmarks, &raw mut json) };
        let text = take_string(json);
        if handle == 0 {
            return None;
        }
        let opened: Option<OpenedJson> = text.and_then(|bytes| serde_json::from_slice(&bytes).ok());
        let Some(opened) = opened else {
            // SAFETY: the handle came from fileid_apple_faces_open and is closed once.
            unsafe { ffi::fileid_apple_faces_close(handle) };
            return None;
        };
        let landmarks = opened
            .faces
            .iter()
            .map(|face| {
                let [x, y, w, h] = face.bounds.map(f64::from_bits);
                let p = face.points.map(f32::from_bits);
                Landmarks { bounds: NormRect::new(x, y, w, h), points: [[p[0], p[1]], [p[2], p[3]], [p[4], p[5]], [p[6], p[7]], [p[8], p[9]]] }
            })
            .collect();
        Some(Self { handle, width: opened.width, height: opened.height, orientation: opened.orientation, landmarks })
    }

    /// The image as tightly packed device-RGB RGBA8 (`FaceAlign.pixels`).
    pub fn pixels(&self) -> Option<Vec<u8>> {
        let (mut pointer, mut length, mut width, mut height) = (std::ptr::null_mut::<u8>(), 0usize, 0u32, 0u32);
        // SAFETY: the handle is live; the out-pointers point at locals.
        let ok = unsafe { ffi::fileid_apple_faces_pixels(self.handle, &raw mut pointer, &raw mut length, &raw mut width, &raw mut height) };
        let bytes = take_bytes(pointer, length)?;
        (ok && width == self.width && height == self.height && bytes.len() == width as usize * height as usize * 4).then_some(bytes)
    }

    /// An integral pixel rect drawn at 112×112 the way SFace's input is drawn,
    /// and the rect's JPEG for `face_crops/`.
    pub fn crop_112(&self, rect: [i64; 4]) -> Option<(Vec<u8>, Option<Vec<u8>>)> {
        let mut rgba = vec![0u8; 112 * 112 * 4];
        let (mut jpeg, mut length) = (std::ptr::null_mut::<u8>(), 0usize);
        let [x, y, w, h] = rect;
        // SAFETY: `rgba` holds 112·112·4 bytes; the out-pointers point at locals.
        let ok = unsafe { ffi::fileid_apple_faces_crop(self.handle, x, y, w, h, rgba.as_mut_ptr(), &raw mut jpeg, &raw mut length) };
        let jpeg = take_bytes(jpeg, length);
        ok.then_some((rgba, jpeg))
    }
}

impl Drop for FaceImage {
    fn drop(&mut self) {
        // SAFETY: the handle came from fileid_apple_faces_open and is closed once.
        unsafe { ffi::fileid_apple_faces_close(self.handle) };
    }
}

/// Takes ownership of an adapter-allocated buffer.
fn take_bytes(pointer: *mut u8, length: usize) -> Option<Vec<u8>> {
    if pointer.is_null() {
        return None;
    }
    // SAFETY: the adapter allocated `length` bytes at `pointer` for us.
    let bytes = unsafe { std::slice::from_raw_parts(pointer, length) }.to_vec();
    // SAFETY: released exactly once, with the adapter's allocator.
    unsafe { ffi::fileid_apple_free(pointer.cast()) };
    Some(bytes)
}

/// JPEG (quality 0.85) of tightly packed RGBA8, as `face_crops/<id>.jpg`.
pub fn jpeg_rgba(rgba: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    if rgba.len() != width as usize * height as usize * 4 || width == 0 || height == 0 {
        return None;
    }
    let mut length = 0usize;
    // SAFETY: `rgba` holds width·height·4 bytes; `length` is a local.
    let pointer = unsafe { ffi::fileid_apple_faces_jpeg(rgba.as_ptr(), width, height, &raw mut length) };
    take_bytes(pointer, length)
}

/// The 112×112 RGBA8 SFace input for one stored face box and whether it was
/// aligned on matched landmarks, as `FaceClustering.extractOneFile` picks it.
pub struct FaceCrop {
    pub rgba: Vec<u8>,
    pub jpeg: Option<Vec<u8>>,
    pub aligned: bool,
}

/// The face crops for `boxes` on one photo, `None` per box that yields no crop
/// (and an empty list when the photo does not decode). Landmarks run only
/// when alignment is on (`FILEID_FACE_ALIGN` is not `0`).
pub fn face_crops(path: &Path, boxes: &[String]) -> Vec<Option<FaceCrop>> {
    let align = std::env::var("FILEID_FACE_ALIGN").as_deref() != Ok("0");
    let Some(image) = FaceImage::open(path, EXTRACT_LONG_SIDE, align) else { return Vec::new() };
    let pixels = if image.landmarks.is_empty() { None } else { image.pixels() };
    let detected: Vec<NormRect> = image.landmarks.iter().map(|l| l.bounds).collect();
    boxes
        .iter()
        .map(|bbox| {
            let aligned = pixels.as_ref().and_then(|pixels| {
                let stored = face_landmark_match::parse_normalized(bbox, image.width, image.height, image.orientation)?;
                let index = face_landmark_match::index(&stored, &detected)?;
                face_align::align_112_rgba(pixels, image.width, image.height, &image.landmarks[index].points)
            });
            if let Some(rgba) = aligned {
                let jpeg = jpeg_rgba(&rgba, 112, 112);
                return Some(FaceCrop { rgba, jpeg, aligned: true });
            }
            let rect = face_landmark_match::padded_crop_rect(bbox, image.width, image.height, image.orientation)?;
            let (rgba, jpeg) = image.crop_112(rect)?;
            Some(FaceCrop { rgba, jpeg, aligned: false })
        })
        .collect()
}

/// SFace as the Mac pipeline runs it, shared by the extraction workers. `ort`
/// runs a session through `&mut`, so a small pool of sessions lets workers
/// embed at once as `ArcFaceService` does with one thread-safe ORT session.
pub struct MacSFace {
    sessions: Vec<Mutex<crate::models::sface::SFace>>,
    next: std::sync::atomic::AtomicUsize,
    version: String,
    provider: &'static str,
}

/// Sessions in the pool (measured in DECISIONS 2026-11-11); `FILEID_SFACE_SESSIONS`
/// (1–4) overrides it.
fn sface_sessions() -> usize {
    std::env::var("FILEID_SFACE_SESSIONS").ok().and_then(|v| v.trim().parse::<usize>().ok()).map_or(2, |n| n.clamp(1, 4))
}

impl MacSFace {
    /// Loads the installed SFace weights as `ArcFaceService` does: the CoreML
    /// execution provider (compiled program cached under `ModelCache/CoreML`),
    /// ORT's CPU provider when CoreML cannot bind or `FILEID_FACE_EP=cpu`.
    /// CoreML gives the Swift engine's vectors bit for bit (DECISIONS 2026-11-11).
    pub fn load() -> Result<Self> {
        use crate::models::sface::{Provider, SFace};
        let path = crate::models::sface::default_weights_path()?;
        anyhow::ensure!(path.is_file(), "SFace weights missing");
        let digest = crate::commands::face_refresh::current_model().context("SFace weights unreadable")?;
        let cpu_only = std::env::var("FILEID_FACE_EP").is_ok_and(|v| v.trim().eq_ignore_ascii_case("cpu"));
        let cache = if cpu_only { None } else { Some(coreml_cache_dir(&digest)?) };
        let first = cache
            .clone()
            .and_then(|cache_dir| SFace::load_with(&path, Provider::CoreMl { cache_dir }).map_err(|error| tracing::warn!(?error, "arcface_coreml_ep_unavailable")).ok());
        let (first, provider) = match first {
            Some(model) => (model, "coreml"),
            None => (SFace::load_with(&path, Provider::Cpu)?, "cpu"),
        };
        let version = first.model_version().to_string();
        let mut sessions = vec![Mutex::new(first)];
        while sessions.len() < sface_sessions() {
            let provider = match (&cache, provider) {
                (Some(cache_dir), "coreml") => Provider::CoreMl { cache_dir: cache_dir.clone() },
                _ => Provider::Cpu,
            };
            let model = SFace::load_with(&path, provider)?;
            anyhow::ensure!(model.model_version() == version, "SFace weights changed during load");
            sessions.push(Mutex::new(model));
        }
        Ok(Self { sessions, next: std::sync::atomic::AtomicUsize::new(0), version, provider })
    }

    pub fn model_version(&self) -> &str {
        &self.version
    }

    pub fn provider(&self) -> &'static str {
        self.provider
    }

    pub fn embed(&self, rgba_112: &[u8]) -> Option<Vec<f32>> {
        let start = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let count = self.sessions.len();
        let mut session = (0..count)
            .find_map(|offset| self.sessions[(start + offset) % count].try_lock())
            .unwrap_or_else(|| self.sessions[start % count].lock());
        session.embed_rgba(rgba_112).map_err(|error| tracing::warn!(?error, "SFace embed failed")).ok()
    }
}

/// `ModelCache/CoreML/<digest>-mlprogram-sub-ort<release>-macos<version>`,
/// named like the Swift engine's `ORTSessions.CoreMLCache`.
fn coreml_cache_dir(model: &str) -> Result<PathBuf> {
    let digest = model.rsplit(':').next().unwrap_or(model);
    let digest: String = digest.chars().take(32).collect();
    let name = format!("{digest}-mlprogram-sub-ort{}-macos{}", crate::models::sface::runtime_release(), macos_version());
    Ok(crate::paths::root()?.join("ModelCache").join("CoreML").join(name))
}

fn macos_version() -> String {
    let mut buffer = [0u8; 64];
    let mut length = buffer.len();
    // SAFETY: the name is NUL-terminated; `buffer` holds `length` bytes.
    let status = unsafe {
        libc::sysctlbyname(c"kern.osproductversion".as_ptr(), buffer.as_mut_ptr().cast(), &raw mut length, std::ptr::null_mut(), 0)
    };
    if status != 0 || length == 0 {
        return "unknown".into();
    }
    String::from_utf8_lossy(&buffer[..length.min(buffer.len())]).trim_end_matches('\0').to_string()
}

/// One stored face's vector and crop JPEG.
pub struct Extracted {
    pub embedding: Vec<f32>,
    pub jpeg: Option<Vec<u8>>,
}

/// `FaceClustering.extractOneFile` without the file-identity checks (the
/// callers hold the catalog's size and time): per box, its SFace vector from
/// the aligned crop when one Vision landmark set matches the box clearly, else
/// from the padded box crop; `None` when neither embeds. Returns the number
/// of aligned faces too.
pub fn extract_file(path: &Path, boxes: &[String], sface: &MacSFace) -> (Vec<Option<Extracted>>, usize) {
    let crops = face_crops(path, boxes);
    if crops.is_empty() {
        return (boxes.iter().map(|_| None).collect(), 0);
    }
    let aligned = crops.iter().flatten().filter(|crop| crop.aligned).count();
    let out = crops
        .into_iter()
        .map(|crop| {
            let crop = crop?;
            let embedding = sface.embed(&crop.rgba)?;
            Some(Extracted { embedding, jpeg: crop.jpeg })
        })
        .collect();
    (out, aligned)
}

impl crate::commands::face_extract_mac::Extractor for MacSFace {
    fn model(&self) -> &str {
        &self.version
    }

    fn extract(&self, path: &Path, boxes: &[String]) -> Vec<Option<crate::commands::face_extract_mac::Embedded>> {
        let (faces, aligned) = extract_file(path, boxes, self);
        tracing::debug!(faces = boxes.len(), aligned, bbox_fallback = boxes.len() - aligned, "face_align_applied");
        faces
            .into_iter()
            .map(|face| face.map(|face| crate::commands::face_extract_mac::Embedded { embedding: face.embedding, jpeg: face.jpeg }))
            .collect()
    }
}

/// The process's SFace, loaded once (as `ArcFaceService.shared`) and again
/// when the installed weights change. `None` when no weights are installed.
pub fn shared_sface() -> Option<Result<std::sync::Arc<MacSFace>>> {
    static SHARED: Mutex<Option<std::sync::Arc<MacSFace>>> = Mutex::new(None);
    let installed = crate::commands::face_refresh::current_model()?;
    let mut shared = SHARED.lock();
    if let Some(model) = shared.as_ref()
        && model.model_version() == installed
    {
        return Some(Ok(model.clone()));
    }
    let started = std::time::Instant::now();
    Some(MacSFace::load().map(|model| {
        tracing::info!(ep = model.provider(), ms = started.elapsed().as_millis() as u64, "arcface_model_loaded");
        let model = std::sync::Arc::new(model);
        *shared = Some(model.clone());
        model
    }))
}

/// Phase 1 of the Swift engine's `runClustering`: embed every pending face in
/// the Mac face space with the installed SFace, leaving out faces of people
/// marked Unknown. Without weights nothing runs (the clustering pass then works
/// over the vectors the catalog has).
pub fn extract_before_clustering(database: &std::sync::Arc<Mutex<rusqlite::Connection>>) {
    let processing = crate::commands::face_refresh::processing();
    if !processing.starts_with(crate::commands::face_clustering_mac::MAC_PREFIX) {
        return;
    }
    let sface = match shared_sface() {
        None => return,
        Some(Err(error)) => {
            tracing::warn!(?error, "face_cluster_embedder_load_failed");
            return;
        }
        Some(Ok(sface)) => sface,
    };
    let unknown: std::collections::HashSet<i64> = {
        let conn = database.lock();
        let found: rusqlite::Result<std::collections::HashSet<i64>> = conn
            .prepare("SELECT fp.id FROM face_prints fp JOIN persons p ON p.id=fp.person_id WHERE COALESCE(p.is_unknown,0)<>0")
            .and_then(|mut statement| statement.query_map([], |row| row.get(0))?.collect());
        match found {
            Ok(found) => found,
            Err(error) => {
                tracing::warn!(?error, "face_print_pending_query_failed");
                return;
            }
        }
    };
    let crops = crate::paths::faces_dir().ok();
    match crate::commands::face_extract_mac::extract_pending(database, sface.as_ref(), processing, &unknown, crops.as_deref(), &|| false) {
        Ok(summary) => tracing::info!(?summary, "face_print_extract_finished"),
        Err(error) => tracing::error!(?error, "face_print_persist_failed"),
    }
}

// MARK: - Best Takes (VisionTakeFaceAnalyzer, TakeMediaAnalysis.signals)

/// Which decode of a photo Best Takes reads (`fileid_apple_take_decode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TakeDecode {
    /// `decodeBoundedImage`, fitted into sRGB at most the given long side.
    Analysis = 0,
    /// `decodeScanImage` drawn unscaled (Face Mesh's detail image).
    Detail = 1,
    /// `decodeBoundedImage` drawn unscaled (the tile source).
    Wide = 2,
}

/// Packed sRGB RGB8 of a photo and the file's long side.
pub struct TakeImage {
    pub rgb: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub source_long_side: u32,
}

pub fn take_decode(path: &Path, kind: TakeDecode, long_side: u32) -> Option<TakeImage> {
    let path = c_path(path).ok()?;
    let (mut pointer, mut length, mut width, mut height, mut source) = (std::ptr::null_mut::<u8>(), 0usize, 0u32, 0u32, 0u32);
    // SAFETY: `path` is NUL-terminated; the out-pointers point at locals.
    let ok = unsafe {
        ffi::fileid_apple_take_decode(path.as_ptr(), kind as u32, long_side, &raw mut pointer, &raw mut length, &raw mut width, &raw mut height, &raw mut source)
    };
    let rgb = take_bytes(pointer, length)?;
    (ok && rgb.len() == width as usize * height as usize * 3).then_some(TakeImage { rgb, width, height, source_long_side: source })
}

#[derive(Deserialize)]
struct MeshJson {
    bits: [u32; 10],
}

#[derive(Deserialize)]
struct TakeFaceJson {
    #[serde(rename = "box")]
    bounds: [u64; 4],
    #[serde(rename = "minSide")]
    min_side: u64,
    quality: Option<u64>,
    eyes: Option<u64>,
    mesh: MeshJson,
}

#[derive(Deserialize)]
struct TakePersonJson {
    #[serde(rename = "box")]
    bounds: [u64; 4],
}

#[derive(Deserialize)]
struct TakeAnalysisJson {
    complete: bool,
    faces: Vec<TakeFaceJson>,
    people: Option<Vec<TakePersonJson>>,
}

/// Faces Vision found, kept in the adapter for Face Mesh inputs at another size.
struct KeptFaces(u64);

impl KeptFaces {
    fn mesh(&self, width: u32, height: u32) -> Option<Vec<MeshJson>> {
        // SAFETY: the id came from fileid_apple_take_faces and is still kept.
        let json = take_string(unsafe { ffi::fileid_apple_take_mesh(self.0, width, height) })?;
        serde_json::from_slice(&json).ok()
    }
}

impl Drop for KeptFaces {
    fn drop(&mut self) {
        if self.0 != 0 {
            // SAFETY: released once; unknown ids are ignored by the adapter.
            unsafe { ffi::fileid_apple_take_release(self.0) };
        }
    }
}

fn take_faces(rgb: &[u8], width: u32, height: u32, people: bool) -> Option<(TakeAnalysisJson, KeptFaces)> {
    if width == 0 || height == 0 || rgb.len() != width as usize * height as usize * 3 {
        return None;
    }
    let mut kept = 0u64;
    // SAFETY: `rgb` holds width·height·3 bytes; `kept` is a local.
    let json = unsafe { ffi::fileid_apple_take_faces(rgb.as_ptr(), width, height, people, &raw mut kept) };
    let kept = KeptFaces(kept);
    let analysis = serde_json::from_slice(&take_string(json)?).ok()?;
    Some((analysis, kept))
}

fn mesh_detection(mesh: &MeshJson) -> crate::models::face_geometry::Detection {
    let v = mesh.bits.map(f32::from_bits);
    crate::models::face_geometry::Detection {
        bbox: [v[0], v[1], v[2], v[3]],
        landmarks: [[v[4], v[5]], [v[6], v[7]], [v[8], v[9]], [v[8], v[9]], [v[8], v[9]]],
        score: 0.0,
    }
}

/// The Mac `TakeFaceAnalyzer` (`VisionTakeFaceAnalyzer`): Vision faces with
/// capture quality and people; eye openness from Face Mesh when its model is
/// installed, else from Vision's eye contours.
pub struct VisionFaces {
    mesh: Option<crate::models::face_mesh::FaceMesh>,
}

impl VisionFaces {
    pub fn load() -> Self {
        let mesh = crate::models::face_mesh::default_weights_path()
            .ok()
            .filter(|path| path.is_file())
            .and_then(|path| crate::models::face_mesh::FaceMesh::load(path).map_err(|error| tracing::warn!(?error, "face_mesh_model_load_failed")).ok());
        Self { mesh }
    }

    /// `VisionTakeFaceAnalyzer.faces(_:qualities:mesh:image:detail:)`.
    fn run(
        &mut self,
        rgb: &[u8],
        width: u32,
        height: u32,
        people: bool,
        detail: Option<&mut crate::commands::catalog_take_auto::Detail>,
    ) -> Option<(Vec<crate::pipeline::take_scoring::Face>, Option<Vec<crate::pipeline::take_scoring::Box2>>)> {
        use crate::pipeline::take_scoring::{self, Box2, Face};
        let (analysis, kept) = take_faces(rgb, width, height, people)?;
        if !analysis.complete {
            return None;
        }
        let mut eyes: Vec<Option<f64>> = analysis.faces.iter().map(|face| face.eyes.map(f64::from_bits)).collect();
        if let Some(mesh) = self.mesh.as_mut() {
            let judged: Vec<bool> = analysis.faces.iter().map(|face| f64::from_bits(face.min_side) >= take_scoring::MIN_FACE_PIXELS).collect();
            if judged.contains(&true) {
                let larger = detail.map(|detail| detail.pixels()).filter(|(_, w, _)| *w > width);
                let (pixels, pw, ph, inputs) = match larger {
                    Some((pixels, w, h)) => (pixels, w, h, kept.mesh(w, h)?),
                    None => (rgb, width, height, analysis.faces.iter().map(|face| MeshJson { bits: face.mesh.bits }).collect()),
                };
                for (index, judge) in judged.iter().enumerate() {
                    if !judge {
                        eyes[index] = None;
                        continue;
                    }
                    let detection = mesh_detection(inputs.get(index)?);
                    match mesh.eyes_open(pixels, pw, ph, &detection) {
                        Ok(open) => eyes[index] = open,
                        Err(error) => {
                            tracing::warn!(?error, "face_mesh_failed");
                            return None;
                        }
                    }
                }
            } else {
                eyes.fill(None);
            }
        }
        let faces = analysis
            .faces
            .iter()
            .zip(eyes)
            .map(|(face, eyes)| {
                let [x, y, w, h] = face.bounds.map(f64::from_bits);
                Face {
                    x: take_scoring::round4(x),
                    y: take_scoring::round4(y),
                    width: take_scoring::round4(w),
                    height: take_scoring::round4(h),
                    quality: face.quality.map(|q| take_scoring::round4(f64::from_bits(q))),
                    eyes_open: eyes.map(take_scoring::round4),
                }
            })
            .collect();
        let people = analysis.people.map(|people| {
            people
                .iter()
                .map(|person| {
                    let [x, y, w, h] = person.bounds.map(f64::from_bits);
                    Box2 { x: take_scoring::round4(x), y: take_scoring::round4(y), width: take_scoring::round4(w), height: take_scoring::round4(h) }
                })
                .collect()
        });
        Some((faces, people))
    }
}

impl crate::commands::catalog_take_auto::TakeFaceAnalyzer for VisionFaces {
    fn id(&self) -> &str {
        if self.mesh.is_some() { "vision-mesh-humans-v1" } else { "vision-landmarks-humans-v1" }
    }

    fn reads_detail(&self) -> bool {
        self.mesh.is_some()
    }

    fn faces(
        &mut self,
        rgb: &[u8],
        width: u32,
        height: u32,
        detail: Option<&mut crate::commands::catalog_take_auto::Detail>,
    ) -> Option<Vec<crate::pipeline::take_scoring::Face>> {
        self.run(rgb, width, height, false, detail).map(|(faces, _)| faces)
    }

    fn analyze(
        &mut self,
        rgb: &[u8],
        width: u32,
        height: u32,
        detail: Option<&mut crate::commands::catalog_take_auto::Detail>,
    ) -> Option<(Vec<crate::pipeline::take_scoring::Face>, Option<Vec<crate::pipeline::take_scoring::Box2>>)> {
        self.run(rgb, width, height, true, detail)
    }
}

/// `TakeMediaAnalysis.signals` for a photo: the analysis decode (1024 px),
/// the detail decode for Face Mesh and wide frames, 2×2 tiles of a 2048 px
/// decode when small faces may hide, all through the adapter's decoders.
pub fn take_image_signals(
    path: &Path,
    faces: &mut dyn crate::commands::catalog_take_auto::TakeFaceAnalyzer,
) -> Option<(crate::pipeline::take_scoring::Signals, bool)> {
    use crate::commands::catalog_take_auto::{first_pass, tiled_faces, Detail};
    use crate::pipeline::take_scoring::{self, Luma};
    let fitted = take_decode(path, TakeDecode::Analysis, take_scoring::ANALYSIS_LONG_SIDE)?;
    let (w, h) = (fitted.width, fitted.height);
    let luma = Luma::from_rgb(&fitted.rgb, w as usize, h as usize);
    let long_side = fitted.source_long_side;
    // Swift decodes the detail image lazily: for Face Mesh, or as the tile source.
    let large = long_side > take_scoring::ANALYSIS_LONG_SIDE;
    let detail_decode = || take_decode(path, TakeDecode::Detail, take_scoring::TILE_LONG_SIDE);
    let mut detail = (large && faces.reads_detail()).then(detail_decode).flatten();
    let mut view = detail.as_ref().map(|d| Detail::new(&d.rgb, d.width, d.height));
    let Some((found, people)) = first_pass(faces, &fitted.rgb, w, h, view.as_mut()) else {
        return Some((take_scoring::signals(&luma, vec![], None), false));
    };
    if !faces.detects() || !take_scoring::wants_tiles(long_side, w as usize, h as usize, &found, people.as_deref()) {
        return Some((take_scoring::signals(&luma, found, people), true));
    }
    if detail.is_none() && large {
        detail = detail_decode();
    }
    let reuse = detail.filter(|d| d.width.max(d.height) >= long_side.min(take_scoring::TILE_LONG_SIDE));
    let Some(wide) = reuse.or_else(|| take_decode(path, TakeDecode::Wide, take_scoring::TILE_LONG_SIDE)) else {
        return Some((take_scoring::signals(&luma, found, people), true));
    };
    if wide.width.max(wide.height) * 2 < take_scoring::ANALYSIS_LONG_SIDE * 3 {
        return Some((take_scoring::signals(&luma, found, people), true));
    }
    let Some(tiled) = tiled_faces(faces, &wide.rgb, wide.width, wide.height) else {
        return Some((take_scoring::signals(&luma, found, people), false));
    };
    let merged = take_scoring::merge_faces(&found, &tiled);
    Some((take_scoring::signals_at(&luma, merged, people, Some(wide.width.max(wide.height) as usize)), true))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("fileid-apple-faces-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_gradient_jpeg(dir: &Path, width: u32, height: u32) -> PathBuf {
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                rgba.extend_from_slice(&[(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8, 255]);
            }
        }
        let jpeg = jpeg_rgba(&rgba, width, height).expect("jpeg");
        let path = dir.join("gradient.jpg");
        std::fs::write(&path, jpeg).unwrap();
        path
    }

    #[test]
    fn opens_decodes_and_crops_through_the_adapter() {
        let dir = Scratch::new();
        let path = write_gradient_jpeg(dir.path(), 900, 600);
        let image = FaceImage::open(&path, 300, true).expect("decoded");
        assert_eq!((image.width, image.height, image.orientation), (300, 200, 1));
        assert!(image.landmarks.is_empty());
        let pixels = image.pixels().expect("pixels");
        assert_eq!(pixels.len(), 300 * 200 * 4);
        let (rgba, jpeg) = image.crop_112([10, 10, 50, 60]).expect("crop");
        assert_eq!(rgba.len(), 112 * 112 * 4);
        assert!(jpeg.is_some_and(|bytes| bytes.starts_with(&[0xFF, 0xD8])));
        assert!(image.crop_112([0, 0, 4, 4]).is_none());
        assert!(FaceImage::open(&dir.path().join("missing.jpg"), 300, false).is_none());
    }

    #[test]
    fn take_signals_decode_like_swift_and_tile_a_wide_frame_without_faces() {
        use crate::commands::catalog_take_auto::TakeFaceAnalyzer;
        let dir = Scratch::new();
        let path = write_gradient_jpeg(dir.path(), 3000, 2000);
        let fitted = take_decode(&path, TakeDecode::Analysis, 1024).expect("analysis decode");
        assert_eq!((fitted.width, fitted.height, fitted.source_long_side), (1024, 683, 3000));
        let wide = take_decode(&path, TakeDecode::Wide, 2048).expect("wide decode");
        assert_eq!((wide.width, wide.height), (2048, 1365));
        let mut faces = VisionFaces { mesh: None };
        assert_eq!(faces.id(), "vision-landmarks-humans-v1");
        let (signals, complete) = take_image_signals(&path, &mut faces).expect("signals");
        assert!(complete);
        assert!(signals.faces.is_empty());
        assert_eq!(signals.face_long_side, Some(2048), "a large frame without faces is tiled");
        assert!(take_image_signals(&dir.path().join("missing.jpg"), &mut faces).is_none());
    }

    #[test]
    fn crops_fall_back_to_the_padded_box_without_landmarks() {
        let dir = Scratch::new();
        let path = write_gradient_jpeg(dir.path(), 640, 480);
        let crops = face_crops(&path, &["0.4,0.4,0.2,0.2".into(), "garbage".into(), "0.5,0.5,0.001,0.001".into()]);
        assert_eq!(crops.len(), 3);
        let first = crops[0].as_ref().expect("padded crop");
        assert!(!first.aligned && first.rgba.len() == 112 * 112 * 4);
        assert!(crops[1].is_none() && crops[2].is_none());
        assert!(face_crops(&dir.path().join("missing.jpg"), &["0.4,0.4,0.2,0.2".into()]).is_empty());
    }
}
