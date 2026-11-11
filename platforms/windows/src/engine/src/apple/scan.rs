//! Stage 5: safe wrappers over the FileIDAppleKit scan adapters. Images stay in
//! the adapter as decoded CGImages behind a handle; Vision, the text read, CLIP
//! and RAM++ read them there, so the scan sees the same pixels the Swift engine
//! sees. Results cross as JSON. Policy (which stages run, what is stored) stays
//! in `pipeline::tagging`.

use super::ffi;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::ffi::{CStr, CString, c_char};
use std::path::Path;
use std::time::Duration;

fn c_path(path: &Path) -> Option<CString> {
    CString::new(path.as_os_str().as_encoded_bytes()).ok()
}

/// Take ownership of an adapter-allocated string.
fn take_string(pointer: *mut c_char) -> Option<String> {
    if pointer.is_null() {
        return None;
    }
    // SAFETY: the adapter returned a NUL-terminated string it allocated for us.
    let text = unsafe { CStr::from_ptr(pointer) }.to_string_lossy().into_owned();
    // SAFETY: released exactly once, with the adapter's allocator.
    unsafe { ffi::fileid_apple_free(pointer.cast()) };
    Some(text)
}

fn take_json<T: for<'de> Deserialize<'de>>(pointer: *mut c_char) -> Option<T> {
    let text = take_string(pointer)?;
    match serde_json::from_str(&text) {
        Ok(value) => Some(value),
        Err(error) => {
            tracing::warn!(%error, "unreadable FileIDAppleKit reply");
            None
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageInfo {
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub camera_model: Option<String>,
    #[serde(default)]
    pub lat: Option<f64>,
    #[serde(default)]
    pub lon: Option<f64>,
    #[serde(default)]
    pub captured_at: Option<f64>,
}

/// A decoded image held by the adapter, released on drop.
#[derive(Debug)]
pub struct Image {
    id: u64,
    pub info: ImageInfo,
}

impl Drop for Image {
    fn drop(&mut self) {
        // SAFETY: `self.id` came from the adapter and is released once.
        unsafe { ffi::fileid_apple_image_release(self.id) };
    }
}

fn image_from(reply: *mut c_char, id: u64) -> Option<Image> {
    let info = take_json::<ImageInfo>(reply);
    match (id, info) {
        (0, _) => None,
        (id, Some(info)) => Some(Image { id, info }),
        (id, None) => {
            // SAFETY: a handle without its info is unusable; release it once.
            unsafe { ffi::fileid_apple_image_release(id) };
            None
        }
    }
}

/// The scan's working image (ImageIO, MPF and RAW previews, the VideoToolbox
/// HEIF decoder) with its EXIF and dHash; `None` when it cannot be decoded.
pub fn decode_image(path: &Path, size_bytes: u64) -> Option<Image> {
    let path = c_path(path)?;
    let mut id = 0u64;
    // SAFETY: `path` is NUL-terminated; `id` is a valid out-pointer.
    let reply = unsafe { ffi::fileid_apple_image_decode(path.as_ptr(), size_bytes.min(i64::MAX as u64) as i64, 0, &raw mut id) };
    image_from(reply, id)
}

/// AVFoundation keyframe at a quarter of the video's duration.
pub fn video_keyframe(path: &Path, max_pixel_size: u32, timeout: Duration) -> Option<Image> {
    let path = c_path(path)?;
    let mut id = 0u64;
    let timeout_ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
    // SAFETY: `path` is NUL-terminated; `id` is a valid out-pointer.
    let reply = unsafe { ffi::fileid_apple_video_keyframe(path.as_ptr(), max_pixel_size, timeout_ms, &raw mut id) };
    image_from(reply, id)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VisionFace {
    /// Normalized "x,y,w,h" (Vision's lower-left origin), as the Swift engine stores it.
    pub bbox: String,
    #[serde(default)]
    pub quality: Option<f64>,
    pub excluded: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrimaryPass {
    pub complete: bool,
    #[serde(default)]
    pub classify_tags: Vec<String>,
    #[serde(default)]
    pub classify_confidences: BTreeMap<String, f64>,
    #[serde(default)]
    pub face_count: usize,
    #[serde(default)]
    pub faces: Vec<VisionFace>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextRead {
    pub ran: bool,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub handwritten: bool,
    #[serde(default)]
    pub fast_ms: f64,
    #[serde(default)]
    pub careful_ms: f64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfRead {
    pub opened: bool,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub handwritten: bool,
}

impl Image {
    /// The Swift engine's 9x8 CoreGraphics dHash, stored bit for bit. Call it
    /// after Vision and CLIP: a JPEG-backed image keeps the decode its first
    /// draw needed, and a 9x8 draw first would leave CLIP a coarse decode.
    pub fn dhash(&self) -> i64 {
        // SAFETY: plain handle argument.
        unsafe { ffi::fileid_apple_image_dhash(self.id) as i64 }
    }

    /// Packed sRGB RGB8 at the image's own size.
    pub fn rgb(&self) -> Option<(Vec<u8>, u32, u32)> {
        let (mut pixels, mut length, mut width, mut height) = (std::ptr::null_mut::<u8>(), 0usize, 0u32, 0u32);
        // SAFETY: every out-pointer points at a local.
        let ok = unsafe { ffi::fileid_apple_image_rgb(self.id, &raw mut pixels, &raw mut length, &raw mut width, &raw mut height) };
        if !ok || pixels.is_null() {
            return None;
        }
        // SAFETY: the adapter allocated `length` bytes at `pixels` for us.
        let rgb = unsafe { std::slice::from_raw_parts(pixels, length) }.to_vec();
        // SAFETY: released exactly once, with the adapter's allocator.
        unsafe { ffi::fileid_apple_free(pixels.cast()) };
        (rgb.len() == width as usize * height as usize * 3).then_some((rgb, width, height))
    }

    /// Vision's classifier, face rectangles and capture quality in one pass.
    pub fn primary_pass(&self) -> Option<PrimaryPass> {
        // SAFETY: plain handle argument.
        take_json(unsafe { ffi::fileid_apple_vision_primary(self.id) })
    }

    /// The Swift engine's text read: whether to read, the fast tier, then the
    /// accurate tier (`FILEID_CAREFUL_READ`) when the fast read missed or mangled it.
    pub fn text_read(&self, classify_tags: &[String], tags: &[String], has_camera: bool) -> Option<TextRead> {
        let request = serde_json::json!({ "classifyTags": classify_tags, "tags": tags, "hasCamera": has_camera });
        let request = CString::new(request.to_string()).ok()?;
        // SAFETY: `request` is NUL-terminated for the call.
        take_json(unsafe { ffi::fileid_apple_text_read(self.id, request.as_ptr()) })
    }

    /// L2-normalized CLIP embedding from the MLX image tower.
    pub fn clip_embedding(&self) -> Option<Vec<f32>> {
        let mut out = vec![0f32; 512];
        // SAFETY: `out` holds `out.len()` floats.
        let ok = unsafe { ffi::fileid_apple_clip_embed(self.id, out.as_mut_ptr(), out.len()) };
        ok.then_some(out)
    }

    /// RAM++ tags agreed with the primary pass, highest first.
    pub fn ram_plus_tags(&self, pass: Option<&PrimaryPass>) -> Option<Vec<(String, f32)>> {
        let confidences = serde_json::to_string(&pass.map(|p| &p.classify_confidences)).ok()?;
        let confidences = CString::new(confidences).ok()?;
        let faces = pass.map_or(0, |p| p.face_count);
        // SAFETY: `confidences` is NUL-terminated for the call.
        let tags: Vec<(String, f64)> = take_json(unsafe { ffi::fileid_apple_ramplus_tag(self.id, confidences.as_ptr(), faces) })?;
        Some(tags.into_iter().map(|(tag, score)| (tag, score as f32)).collect())
    }
}

/// Text of an image-only PDF's first three pages.
pub fn pdf_ocr(path: &Path) -> Option<PdfRead> {
    let path = c_path(path)?;
    // SAFETY: `path` is NUL-terminated.
    take_json(unsafe { ffi::fileid_apple_pdf_ocr(path.as_ptr()) })
}

/// Document text as the Swift engine extracts it. `None` for types the caller
/// extracts itself; `Some(None)` when the document has no text.
pub fn document_text(path: &Path, max_chars: usize) -> Option<Option<String>> {
    let path = c_path(path)?;
    let mut handled = false;
    // SAFETY: `path` is NUL-terminated; `handled` is a valid out-pointer.
    let text = take_string(unsafe { ffi::fileid_apple_document_text(path.as_ptr(), max_chars, &raw mut handled) });
    handled.then_some(text)
}

/// Load the MLX CLIP image tower from a verified ONNX file.
pub fn load_clip(onnx: &Path) -> bool {
    let Some(path) = c_path(onnx) else { return false };
    // SAFETY: `path` is NUL-terminated.
    unsafe { ffi::fileid_apple_clip_load(path.as_ptr()) }
}

/// Load MLX RAM++ from its model folder.
pub fn load_ram_plus(directory: &Path) -> bool {
    let Some(path) = c_path(directory) else { return false };
    // SAFETY: `path` is NUL-terminated.
    unsafe { ffi::fileid_apple_ramplus_load(path.as_ptr()) }
}

/// Whether the accurate text tier has been compiled.
pub fn ocr_ready() -> bool {
    // SAFETY: no arguments.
    unsafe { ffi::fileid_apple_ocr_ready() }
}

/// Compile the accurate text tier once the engine is idle, as the Swift engine
/// does: after three seconds, and not while a scan is decoding. A scan that
/// starts first keeps its fast reads until the compile finishes.
pub fn start_ocr_warmup_when_idle() {
    let spawned = std::thread::Builder::new().name("fileid-ocr-warmup".into()).spawn(|| {
        std::thread::sleep(Duration::from_secs(3));
        while crate::pipeline::tagging::active_decoder_threads() > 0 {
            std::thread::sleep(Duration::from_secs(1));
        }
        // SAFETY: no arguments; the adapter runs the compile on its own queue.
        unsafe { ffi::fileid_apple_ocr_warmup() };
    });
    if let Err(error) = spawned {
        tracing::warn!(%error, "text-read warmup thread did not start");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(std::path::PathBuf);

    impl TestDir {
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tempdir() -> TestDir {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("fileid-apple-scan-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        TestDir(dir)
    }

    fn png(dir: &Path, name: &str, width: u32, height: u32) -> std::path::PathBuf {
        let path = dir.join(name);
        let image = image::RgbImage::from_fn(width, height, |x, y| image::Rgb([(255 - x * 255 / width) as u8, (y * 255 / height) as u8, 90]));
        image.save(&path).expect("write png");
        path
    }

    #[test]
    fn decodes_with_dhash_and_releases() {
        let dir = tempdir();
        let path = png(dir.path(), "gradient.png", 640, 480);
        let size = std::fs::metadata(&path).unwrap().len();
        let image = decode_image(&path, size).expect("decode");
        assert_eq!((image.info.width, image.info.height), (640, 480));
        assert!(image.dhash().count_ones() >= 56, "a left-bright gradient sets nearly every bit");
        assert!(image.info.camera_model.is_none());
        let (rgb, w, h) = image.rgb().expect("pixels");
        assert_eq!(rgb.len(), (w * h * 3) as usize);
        assert!(decode_image(&dir.path().join("missing.png"), 0).is_none());
    }

    #[test]
    fn tiny_files_are_refused() {
        let dir = tempdir();
        let path = dir.path().join("tiny.jpg");
        std::fs::write(&path, b"not an image").unwrap();
        assert!(decode_image(&path, 12).is_none());
    }

    #[test]
    fn primary_pass_and_text_read_complete() {
        let dir = tempdir();
        let path = png(dir.path(), "plain.png", 320, 240);
        let image = decode_image(&path, std::fs::metadata(&path).unwrap().len()).unwrap();
        let pass = image.primary_pass().expect("primary pass");
        assert!(pass.complete);
        assert_eq!(pass.face_count, pass.faces.len());
        let read = image.text_read(&[], &[], false).expect("text read");
        assert!(!read.ran, "a gradient with no text tags is not read");
        let read = image.text_read(&["document".into()], &[], false).expect("text read");
        assert!(read.ran);
    }

    #[test]
    fn documents_follow_the_swift_extractor() {
        let dir = tempdir();
        let txt = dir.path().join("notes.txt");
        std::fs::write(&txt, "  hello from the adapter \n").unwrap();
        assert_eq!(document_text(&txt, 4000), Some(Some("hello from the adapter".to_string())));
        let empty = dir.path().join("empty.md");
        std::fs::write(&empty, " \n").unwrap();
        assert_eq!(document_text(&empty, 4000), Some(None));
        let rtf = dir.path().join("letter.rtf");
        std::fs::write(&rtf, r"{\rtf1\ansi Dear reader,\par adapters read RTF.}").unwrap();
        assert_eq!(document_text(&rtf, 4000), Some(Some("Dear reader,\nadapters read RTF.".to_string())));
        let pptx = dir.path().join("deck.pptx");
        std::fs::write(&pptx, b"PK\x03\x04").unwrap();
        assert_eq!(document_text(&pptx, 4000), None);
    }

    #[test]
    fn missing_models_do_not_load() {
        let dir = tempdir();
        assert!(!load_clip(&dir.path().join("clip.onnx")));
        assert!(!load_ram_plus(dir.path()));
    }

    #[test]
    fn video_keyframe_of_a_non_video_is_none() {
        let dir = tempdir();
        let path = dir.path().join("clip.mov");
        std::fs::write(&path, vec![0u8; 4096]).unwrap();
        assert!(video_keyframe(&path, 512, Duration::from_secs(6)).is_none());
    }
}

