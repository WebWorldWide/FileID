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

/// SFace as the Mac pipeline runs it, shared by the extraction workers.
pub struct MacSFace {
    model: Mutex<crate::models::sface::SFace>,
    version: String,
    provider: &'static str,
}

impl MacSFace {
    /// Loads the installed SFace weights. `FILEID_FACE_EP=coreml` binds the
    /// CoreML execution provider (compiled program cached under
    /// `ModelCache/CoreML`), `cpu` or the default keeps ORT's CPU provider:
    /// the 2026-11-10 A/B in DECISIONS.md.
    pub fn load() -> Result<Self> {
        let path = crate::models::sface::default_weights_path()?;
        anyhow::ensure!(path.is_file(), "SFace weights missing");
        let digest = crate::commands::face_refresh::current_model().context("SFace weights unreadable")?;
        let coreml = std::env::var("FILEID_FACE_EP").map(|v| v.trim().eq_ignore_ascii_case("coreml")).unwrap_or(false);
        let provider = if coreml {
            crate::models::sface::Provider::CoreMl { cache_dir: coreml_cache_dir(&digest)? }
        } else {
            crate::models::sface::Provider::Cpu
        };
        let label = if coreml { "coreml" } else { "cpu" };
        let model = crate::models::sface::SFace::load_with(&path, provider)?;
        let version = model.model_version().to_string();
        Ok(Self { model: Mutex::new(model), version, provider: label })
    }

    pub fn model_version(&self) -> &str {
        &self.version
    }

    pub fn provider(&self) -> &'static str {
        self.provider
    }

    pub fn embed(&self, rgba_112: &[u8]) -> Option<Vec<f32>> {
        self.model.lock().embed_rgba(rgba_112).map_err(|error| tracing::warn!(?error, "SFace embed failed")).ok()
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
