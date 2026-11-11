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
    /// `people`, human rectangles over packed sRGB RGB8, as JSON (free it).
    pub fn fileid_apple_take_faces(rgb: *const u8, width: u32, height: u32, people: bool, detail_width: u32, detail_height: u32) -> *mut c_char;
    // --- end Stage 6 ---
}
