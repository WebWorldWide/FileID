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
pub const VLM_TOKEN: u32 = 1;
pub const VLM_PROGRESS: u32 = 2;
pub const VLM_DONE: u32 = 3;
pub const VLM_FAILED: u32 = 4;
// --- end Stage 7 ---
