//! Apple adapters for the engine the macOS app hosts (`Contents/MacOS/fileidd`),
//! compiled only with `--features apple-adapters` on macOS. Apple-framework
//! mechanics (IOPM, security-scoped bookmarks, later Vision, ImageIO,
//! AVFoundation and MLX) run in the Swift FileIDAppleKit library behind the
//! C interface in `ffi.rs`; this module wraps it in safe types. Catalog
//! writes, job policy and ranking stay in Rust.

mod ffi;

use std::ffi::{CStr, CString, OsString, c_char};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

/// Interface version this engine was built against.
pub const ABI_VERSION: u32 = 1;

/// Exit status when the bundled FileIDAppleKit implements another interface.
const EXIT_ABI_MISMATCH: i32 = 78;

/// Interface version of the loaded FileIDAppleKit.
pub fn adapter_abi_version() -> u32 {
    // SAFETY: no arguments; the library is linked at load time.
    unsafe { ffi::fileid_apple_abi_version() }
}

/// Run as the Mac app's engine: events on fd 2 (the app reads the engine's
/// stderr), state in the Swift engine's folder, then a hard exit.
pub fn run_hosted() -> ! {
    match crate::paths::logs_dir() {
        Ok(logs) => {
            if let Err(error) = crate::ipc::sink::route_events_to_stderr(&logs) {
                eprintln!("FileIDEngine: engine-stderr.log unavailable ({error}); events share fd 2");
            }
        }
        Err(error) => eprintln!("FileIDEngine: no logs folder ({error:#}); events share fd 2"),
    }
    let abi = adapter_abi_version();
    if abi != ABI_VERSION {
        eprintln!("FileIDEngine: FileIDAppleKit interface {abi}, engine expects {ABI_VERSION}");
        hard_exit(EXIT_ABI_MISMATCH);
    }
    let code = match crate::server::run() {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("FileIDEngine: {error:#}");
            1
        }
    };
    hard_exit(code)
}

/// Release adapter state and leave without running atexit handlers or static
/// destructors, as the Swift engine does: MLX's destructors crash during normal
/// teardown. The catalog is already checkpointed and closed by `server::run`.
pub fn hard_exit(code: i32) -> ! {
    // SAFETY: no arguments; idempotent in the adapter.
    unsafe { ffi::fileid_apple_shutdown() };
    let _ = std::io::Write::flush(&mut std::io::stderr());
    // SAFETY: terminates the process; nothing runs afterwards.
    unsafe { libc::_exit(code) }
}

/// An IOPM PreventSystemSleep assertion (the display may still sleep), held
/// until dropped. Lid-closed runs on Apple silicon still need AC power.
#[derive(Debug)]
pub struct SleepAssertion(u32);

impl SleepAssertion {
    /// `None` when the assertion could not be taken (best effort, like the Swift engine).
    pub fn begin(reason: &str) -> Option<Self> {
        let reason = CString::new(reason.replace('\0', " ")).ok()?;
        // SAFETY: `reason` is a valid NUL-terminated string for the call.
        let id = unsafe { ffi::fileid_apple_sleep_begin(reason.as_ptr()) };
        (id != 0).then_some(Self(id))
    }
}

impl Drop for SleepAssertion {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from fileid_apple_sleep_begin and is released once.
        unsafe { ffi::fileid_apple_sleep_end(self.0) };
    }
}

/// Access to a folder the user granted through a security-scoped bookmark,
/// held until dropped.
#[derive(Debug)]
pub struct BookmarkLease {
    id: u64,
    path: PathBuf,
    stale: bool,
}

impl BookmarkLease {
    /// Resolve `bookmark` and start accessing it; `None` when it no longer resolves.
    pub fn open(bookmark: &[u8]) -> Option<Self> {
        let mut path: *mut c_char = std::ptr::null_mut();
        let mut stale = false;
        // SAFETY: the buffer is valid for `len` bytes; out-pointers point at locals.
        let id = unsafe { ffi::fileid_apple_bookmark_open(bookmark.as_ptr(), bookmark.len(), &raw mut path, &raw mut stale) };
        let resolved = (!path.is_null()).then(|| {
            // SAFETY: the adapter returned a NUL-terminated string it allocated for us.
            let bytes = unsafe { CStr::from_ptr(path) }.to_bytes().to_vec();
            // SAFETY: released exactly once, with the adapter's allocator.
            unsafe { ffi::fileid_apple_free(path.cast()) };
            PathBuf::from(OsString::from_vec(bytes))
        });
        match (id, resolved) {
            (0, _) => None,
            (id, Some(path)) => Some(Self { id, path, stale }),
            (id, None) => {
                // SAFETY: a lease without a path is unusable; close it once.
                unsafe { ffi::fileid_apple_bookmark_close(id) };
                None
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The bookmark resolved but should be recreated by the app.
    pub fn is_stale(&self) -> bool {
        self.stale
    }
}

impl Drop for BookmarkLease {
    fn drop(&mut self) {
        // SAFETY: `self.id` came from fileid_apple_bookmark_open and is closed once.
        unsafe { ffi::fileid_apple_bookmark_close(self.id) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_implements_this_interface() {
        assert_eq!(adapter_abi_version(), ABI_VERSION);
    }

    #[test]
    fn sleep_assertion_is_taken_and_released() {
        let assertion = SleepAssertion::begin("FileID engine test").expect("IOPM assertion");
        assert_ne!(assertion.0, 0);
        drop(assertion);
    }

    #[test]
    fn garbage_bookmark_does_not_resolve() {
        assert!(BookmarkLease::open(b"not a bookmark").is_none());
        assert!(BookmarkLease::open(&[]).is_none());
    }
}
