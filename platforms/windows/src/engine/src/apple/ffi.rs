//! The engine's only Apple C interface: a hand-written mirror of
//! `platforms/apple/adapters/include/fileid_apple.h`, implemented by the
//! FileIDAppleKit dynamic library in `FileID.app/Contents/Frameworks`.
//! Reviewed surface (shared/scripts/check_runtime_egress.py): no adapter opens
//! a URL, socket or HTTP client or launches a process. Keep this file to
//! declarations; safe wrappers live in `super`.
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
}
