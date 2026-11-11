//! Apple adapters for the engine the macOS app hosts (`Contents/MacOS/fileidd`),
//! compiled only with `--features apple-adapters` on macOS. Apple-framework
//! mechanics (IOPM, security-scoped bookmarks, later Vision, ImageIO,
//! AVFoundation and MLX) run in the Swift FileIDAppleKit library behind the
//! C interface in `ffi.rs`; this module wraps it in safe types. Catalog
//! writes, job policy and ranking stay in Rust.

pub mod faces;
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

/// Takes ownership of an adapter-allocated string.
fn take_string(pointer: *mut c_char) -> Option<Vec<u8>> {
    (!pointer.is_null()).then(|| {
        // SAFETY: the adapter returned a NUL-terminated string it allocated for us.
        let bytes = unsafe { CStr::from_ptr(pointer) }.to_bytes().to_vec();
        // SAFETY: released exactly once, with the adapter's allocator.
        unsafe { ffi::fileid_apple_free(pointer.cast()) };
        bytes
    })
}

fn c_path(path: &Path) -> anyhow::Result<CString> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(path.as_os_str().as_bytes()).map_err(|_| anyhow::anyhow!("path contains a NUL byte"))
}

fn adapter_error(error: *mut c_char, fallback: &str) -> anyhow::Error {
    let message = take_string(error).map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
    anyhow::anyhow!(message.unwrap_or_else(|| fallback.to_owned()))
}

/// Moves `path` to the user's Trash with FileManager.trashItem; returns where
/// it landed when Foundation says. Callers check identity before and after
/// (`shell::macos::trash`).
pub fn trash_item(path: &Path) -> anyhow::Result<Option<PathBuf>> {
    let path = c_path(path)?;
    let mut landed: *mut c_char = std::ptr::null_mut();
    let mut error: *mut c_char = std::ptr::null_mut();
    // SAFETY: `path` is NUL-terminated; the out-pointers point at locals.
    let status = unsafe { ffi::fileid_apple_trash_item(path.as_ptr(), &raw mut landed, &raw mut error) };
    let landed = take_string(landed).map(|bytes| PathBuf::from(OsString::from_vec(bytes)));
    if status != 0 {
        return Err(adapter_error(error, "the Trash refused the file"));
    }
    drop(take_string(error));
    Ok(landed)
}

/// The item's Finder tags (`URLResourceKey.tagNamesKey`).
pub fn finder_tags(path: &Path) -> anyhow::Result<Vec<String>> {
    let path = c_path(path)?;
    let mut error: *mut c_char = std::ptr::null_mut();
    // SAFETY: `path` is NUL-terminated; the out-pointer points at a local.
    let json = unsafe { ffi::fileid_apple_tags_read(path.as_ptr(), &raw mut error) };
    let Some(json) = take_string(json) else {
        return Err(adapter_error(error, "could not read Finder tags"));
    };
    drop(take_string(error));
    Ok(serde_json::from_slice(&json)?)
}

/// Replaces the item's Finder tags with `tags` (empty clears them).
pub fn set_finder_tags(path: &Path, tags: &[String]) -> anyhow::Result<()> {
    let path = c_path(path)?;
    let owned = tags
        .iter()
        .map(|tag| CString::new(tag.as_str()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| anyhow::anyhow!("a tag contains a NUL byte"))?;
    let pointers: Vec<*const c_char> = owned.iter().map(|tag| tag.as_ptr()).collect();
    let mut error: *mut c_char = std::ptr::null_mut();
    // SAFETY: `pointers` holds `len` NUL-terminated strings that outlive the call.
    let status = unsafe {
        ffi::fileid_apple_tags_write(path.as_ptr(), pointers.as_ptr(), pointers.len(), &raw mut error)
    };
    if status != 0 {
        return Err(adapter_error(error, "could not write Finder tags"));
    }
    drop(take_string(error));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finder_tags_cross_the_interface() {
        let dir = std::env::temp_dir().join(format!("fileid-apple-tags-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("ünïcode photo.jpg");
        std::fs::write(&file, b"x").unwrap();
        set_finder_tags(&file, &["Beach".into(), "Grandma Ö".into()]).unwrap();
        assert_eq!(finder_tags(&file).unwrap(), vec!["Beach", "Grandma Ö"]);
        assert!(finder_tags(&dir.join("missing.jpg")).is_err());
        assert!(set_finder_tags(&dir.join("missing.jpg"), &["x".into()]).is_err());
        std::fs::remove_dir_all(dir).ok();
    }

    // Trash only through the refusal path: a missing file never reaches the
    // user's Trash, and the adapter reports why.
    #[test]
    fn trash_of_a_missing_file_reports_the_error() {
        let missing = std::env::temp_dir().join(format!("fileid-apple-missing-{}", uuid::Uuid::new_v4()));
        let error = trash_item(&missing).unwrap_err();
        assert!(!error.to_string().is_empty());
    }

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
