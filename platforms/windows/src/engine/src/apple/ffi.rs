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
}
