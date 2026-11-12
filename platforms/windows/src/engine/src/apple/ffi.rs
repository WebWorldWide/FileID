//! The engine's only Apple C interface: a hand-written mirror of
//! `platforms/apple/adapters/include/fileid_apple.h`, implemented by the
//! FileIDAppleKit dynamic library in `FileID.app/Contents/Frameworks`.
//! Reviewed surface (shared/scripts/check_runtime_egress.py): no adapter opens
//! a URL, socket or HTTP client or launches a process. Keep this file to
//! declarations and the one callback trampoline; safe wrappers live in `super`.
//!
//! Conventions: plain C types; strings are NUL-terminated UTF-8; memory the
//! adapter returns is released with `fileid_apple_free`; handles are nonzero,
//! and 0 means failure.

use std::ffi::{c_char, c_void};

#[link(name = "FileIDAppleKit", kind = "dylib")]
unsafe extern "C" {
    /// Interface version the library implements (`super::ABI_VERSION`).
    pub fn fileid_apple_abi_version() -> u32;
    /// Release adapter-owned state (assertions, leases) before the hard exit.
    pub fn fileid_apple_shutdown();
    /// Free memory the adapter allocated for the caller.
    pub fn fileid_apple_free(pointer: *mut c_void);
    /// Take an IOPM PreventSystemSleep assertion; returns its id, 0 on failure.
    pub fn fileid_apple_sleep_begin(reason: *const c_char) -> u32;
    /// Release an assertion from `fileid_apple_sleep_begin`.
    pub fn fileid_apple_sleep_end(assertion: u32);
    /// Resolve a (security-scoped) bookmark and start accessing it. Returns a
    /// lease id, 0 on failure; `out_path` receives the resolved path (free it
    /// with `fileid_apple_free`), `out_stale` whether the bookmark should be
    /// recreated.
    pub fn fileid_apple_bookmark_open(
        data: *const u8,
        length: usize,
        out_path: *mut *mut c_char,
        out_stale: *mut bool,
    ) -> u64;
    /// Stop accessing a leased bookmark.
    pub fn fileid_apple_bookmark_close(lease: u64);
    /// Move the item at `path` to the Trash (FileManager.trashItem). 0 on
    /// success, with `out_landed` its path in the Trash (may stay null);
    /// otherwise nonzero with `out_error` a message. Free both with
    /// `fileid_apple_free`.
    pub fn fileid_apple_trash_item(
        path: *const c_char,
        out_landed: *mut *mut c_char,
        out_error: *mut *mut c_char,
    ) -> i32;
    /// The item's Finder tags as a JSON array of strings, or null with
    /// `out_error` set. Free the result with `fileid_apple_free`.
    pub fn fileid_apple_tags_read(path: *const c_char, out_error: *mut *mut c_char) -> *mut c_char;
    /// Replace the item's Finder tags with `tags[0..count]`. 0 on success,
    /// otherwise nonzero with `out_error` set.
    pub fn fileid_apple_tags_write(
        path: *const c_char,
        tags: *const *const c_char,
        count: usize,
        out_error: *mut *mut c_char,
    ) -> i32;
    // --- Stage 6: faces ---
    /// Decode a photo for face extraction (FaceClustering.loadCGImage) and,
    /// with `landmarks`, run Vision's face landmarks on it. Returns an image
    /// handle (0 on failure) and `out_json` (free it): width, height,
    /// orientation, faces [{bounds: 4 f64 bit patterns, points: 10 f32 bit
    /// patterns}].
    pub fn fileid_apple_faces_open(path: *const c_char, max_pixel_size: u32, landmarks: bool, out_json: *mut *mut c_char) -> u64;
    /// The open image as tightly packed device-RGB RGBA8 (free `out_rgba`).
    pub fn fileid_apple_faces_pixels(
        image: u64,
        out_rgba: *mut *mut u8,
        out_length: *mut usize,
        out_width: *mut u32,
        out_height: *mut u32,
    ) -> bool;
    /// An integral top-left pixel rect drawn at 112×112 into `out_rgba112`
    /// (112·112·4 bytes) and its JPEG in `out_jpeg` (free it; may stay null).
    pub fn fileid_apple_faces_crop(
        image: u64,
        x: i64,
        y: i64,
        width: i64,
        height: i64,
        out_rgba112: *mut u8,
        out_jpeg: *mut *mut u8,
        out_jpeg_length: *mut usize,
    ) -> bool;
    /// JPEG (quality 0.85) of tightly packed RGBA8; free it; null on failure.
    pub fn fileid_apple_faces_jpeg(rgba: *const u8, width: u32, height: u32, out_length: *mut usize) -> *mut u8;
    /// Release an image handle.
    pub fn fileid_apple_faces_close(image: u64);
    /// Best Takes' photo as packed sRGB RGB8: kind 0 fitted analysis image,
    /// 1 detail image, 2 tile source (free `out_rgb`).
    pub fn fileid_apple_take_decode(
        path: *const c_char,
        kind: u32,
        long_side: u32,
        out_rgb: *mut *mut u8,
        out_length: *mut usize,
        out_width: *mut u32,
        out_height: *mut u32,
        out_source_long_side: *mut u32,
    ) -> bool;
    /// Vision faces (landmarks, capture quality, Face Mesh inputs) and, with
    /// `people`, human rectangles over packed sRGB RGB8, as JSON (free it);
    /// `out_observations` keeps found faces for `fileid_apple_take_mesh`.
    pub fn fileid_apple_take_faces(rgb: *const u8, width: u32, height: u32, people: bool, out_observations: *mut u64) -> *mut c_char;
    /// Face Mesh inputs of kept faces for another decode size, as JSON (free it).
    pub fn fileid_apple_take_mesh(observations: u64, width: u32, height: u32) -> *mut c_char;
    /// Release kept faces.
    pub fn fileid_apple_take_release(observations: u64);
    /// Best Takes' tile source kept as decoded (detail or bounded decode); 0 on failure.
    pub fn fileid_apple_take_tiles_open(path: *const c_char, long_side: u32, out_width: *mut u32, out_height: *mut u32) -> u64;
    /// `fileid_apple_take_faces` (no people) on a rect of a tile source, as JSON
    /// (free it); with `out_rgb`, the crop's packed sRGB RGB8 for Face Mesh.
    pub fn fileid_apple_take_tile_faces(
        tiles: u64,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        out_rgb: *mut *mut u8,
        out_length: *mut usize,
        out_observations: *mut u64,
    ) -> *mut c_char;
    /// Release a tile source.
    pub fn fileid_apple_take_tiles_close(tiles: u64);
    // --- end Stage 6 ---

    // --- Stage 8: timeline, speech and video tools ---
    // `cancel` points at a caller-owned `AtomicBool` (or is null); replies are
    // JSON freed with `fileid_apple_free`, `{"error": text}` on failure.

    /// AVFoundation probe; `strict` applies the media tools' refusals,
    /// `mime_type` (nullable) types a staged file without a media extension.
    pub fn fileid_apple_media_probe(path: *const c_char, mime_type: *const c_char, strict: bool) -> *mut c_char;
    /// One export from `{"recipe", "source", "output"}`: trim/remux/audio/proxy/
    /// rotate, or the fit/pad/stabilize/follow-people video export.
    pub fn fileid_apple_media_export(request_json: *const c_char, cancel: *const bool) -> *mut c_char;
    /// One upright RGB frame of exactly `width`x`height`, null on failure.
    pub fn fileid_apple_video_frame(path: *const c_char, seconds: f64, width: u32, height: u32, out_seconds: *mut f64) -> *mut u8;
    /// Frame pool JPEGs and `manifest.json` in `directory`: 0, 2 invalid, 3 failed, 4 source changed.
    pub fn fileid_apple_video_pool(path: *const c_char, interval: f64, max_side: u32, directory: *const c_char, cancel: *const bool) -> i32;
    /// 1 ready, 0 unavailable or denied, -1 the host cannot ask (no usage description).
    pub fn fileid_apple_speech_status(ask: bool) -> i32;
    /// The Apple Speech model version string.
    pub fn fileid_apple_speech_model_version() -> *mut c_char;
    /// On-device transcription of `[start, end)` seconds: `{"segments": [...]}`.
    pub fn fileid_apple_speech_transcribe(path: *const c_char, start: f64, end: f64, cancel: *const bool) -> *mut c_char;
    // --- end Stage 8 ---

    // --- Stage 5: scan decode and per-file ML ---
    /// The scan's working image; `out_image` receives its handle. JSON info.
    pub fn fileid_apple_image_decode(path: *const c_char, size_bytes: i64, max_pixel_size: u32, out_image: *mut u64) -> *mut c_char;
    /// AVFoundation keyframe at a quarter of the duration. JSON info.
    pub fn fileid_apple_video_keyframe(path: *const c_char, max_pixel_size: u32, timeout_ms: u32, out_image: *mut u64) -> *mut c_char;
    /// The Swift scan's dHash; after Vision and CLIP have read the image.
    pub fn fileid_apple_image_dhash(image: u64) -> u64;
    /// Packed sRGB RGB8 of an image; free `out_pixels` with `fileid_apple_free`.
    pub fn fileid_apple_image_rgb(
        image: u64,
        out_pixels: *mut *mut u8,
        out_length: *mut usize,
        out_width: *mut u32,
        out_height: *mut u32,
    ) -> bool;
    /// Release an image handle.
    pub fn fileid_apple_image_release(image: u64);
    /// Vision primary pass. JSON.
    pub fn fileid_apple_vision_primary(image: u64) -> *mut c_char;
    /// The scan's text read; `request` is JSON. JSON.
    pub fn fileid_apple_text_read(image: u64, request: *const c_char) -> *mut c_char;
    /// OCR of an image-only PDF's first pages. JSON.
    pub fn fileid_apple_pdf_ocr(path: *const c_char) -> *mut c_char;
    /// Compile the accurate text tier once, asynchronously.
    pub fn fileid_apple_ocr_warmup();
    /// Whether the accurate text tier is compiled.
    pub fn fileid_apple_ocr_ready() -> bool;
    /// Document text as the Swift engine extracts it; `out_handled` false for
    /// types the caller extracts itself.
    pub fn fileid_apple_document_text(path: *const c_char, max_chars: usize, out_handled: *mut bool) -> *mut c_char;
    /// Load the MLX CLIP image tower from a verified ONNX file.
    pub fn fileid_apple_clip_load(onnx_path: *const c_char) -> bool;
    /// L2-normalized 512-d CLIP embedding into `out`.
    pub fn fileid_apple_clip_embed(image: u64, out: *mut f32, capacity: usize) -> bool;
    /// Load MLX RAM++ from its model folder.
    pub fn fileid_apple_ramplus_load(directory: *const c_char) -> bool;
    /// RAM++ tags agreed with the primary pass. JSON `[[tag, score], ...]`.
    pub fn fileid_apple_ramplus_tag(image: u64, vision_confidences: *const c_char, face_count: usize) -> *mut c_char;
    // --- end Stage 5 ---

    // --- Stage 7: MLX vision-language models ---
    /// Start a VLM request (UTF-8 JSON with an `op`). Returns a handle, 0 when
    /// refused (the callback is then never called). The callback runs on any
    /// thread and gets exactly one terminal event (`DONE` or `FAILED`).
    pub fn fileid_apple_vlm_submit(
        request: *const u8,
        length: usize,
        callback: VlmCallback,
        context: *mut c_void,
    ) -> u64;
    /// Cancel a running request; it still reports its terminal event.
    pub fn fileid_apple_vlm_cancel(handle: u64);
    // --- end Stage 7 ---
}

// --- Stage 7: MLX vision-language models ---
/// Event callback for `fileid_apple_vlm_submit`; the payload is valid only
/// during the call.
pub type VlmCallback = unsafe extern "C" fn(context: *mut c_void, event: u32, payload: *const u8, length: usize);

/// What a request's events go to: boxed as the callback context by
/// `super::vlm::request`, called once per event.
pub struct VlmReceiver(pub Box<dyn Fn(u32, &[u8]) + Send + Sync>);

/// The callback every VLM request passes: forwards each event to its
/// [`VlmReceiver`] and frees the receiver after the terminal event.
///
/// # Safety
/// `context` must be a `Box<VlmReceiver>` leaked for this request, and the
/// adapter must call back exactly once with `VLM_DONE` or `VLM_FAILED`.
pub unsafe extern "C" fn vlm_callback(context: *mut c_void, event: u32, payload: *const u8, length: usize) {
    if context.is_null() {
        return;
    }
    let bytes: &[u8] = if payload.is_null() || length == 0 {
        &[]
    } else {
        // SAFETY: the adapter passes `length` readable bytes, valid during this call.
        unsafe { std::slice::from_raw_parts(payload, length) }
    };
    // SAFETY: `context` is the receiver boxed for this request (see above).
    unsafe { &*context.cast::<VlmReceiver>() }.0(event, bytes);
    if matches!(event, VLM_DONE | VLM_FAILED) {
        // SAFETY: the terminal event is the last call for this request.
        drop(unsafe { Box::from_raw(context.cast::<VlmReceiver>()) });
    }
}

pub const VLM_TOKEN: u32 = 1;
pub const VLM_PROGRESS: u32 = 2;
pub const VLM_DONE: u32 = 3;
pub const VLM_FAILED: u32 = 4;
// --- end Stage 7 ---
