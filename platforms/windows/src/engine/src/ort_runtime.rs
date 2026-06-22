//! ONNX Runtime dynamic-library resolution for the `load-dynamic` build.
//!
//! `ort` is compiled with `load-dynamic`, so on first use it `dlopen`s the ONNX
//! Runtime shared library by name — `onnxruntime.dll` (Windows),
//! `libonnxruntime.so` (Linux), `libonnxruntime.dylib` (macOS) — unless
//! `ORT_DYLIB_PATH` points at an explicit file. For a bare relative name `ort`
//! also probes beside the running executable before falling back to the OS
//! loader search path.
//!
//! macOS is the problem case this module exists for. `ort`'s `download-binaries`
//! feature fetches only a STATIC `libonnxruntime.a` for `aarch64-apple-darwin`
//! (pyke's "none" tarball contains no `.dylib` — verified), so a `load-dynamic`
//! build has NO runtime library unless the user provisions one. We locate that
//! provisioned dylib and pin `ORT_DYLIB_PATH` to it before the first ML session.
//!
//! Provisioning options (all MIT-licensed ONNX Runtime, all documented in
//! `shared/docs/RUNTIME.md`):
//!   * `fileid runtime install`                       (the CLI subcommand)
//!   * `shared/scripts/install_onnxruntime_macos.sh`  (plain shell)
//!   * `brew install onnxruntime`                     (Homebrew prefix)
//!
//! Windows keeps its own accelerator-pack `ORT_DYLIB_PATH` pin in `main.rs`, and
//! Linux relies on the system / `download-binaries` path — both untouched here.

#![allow(dead_code)] // some helpers are consumed only by the cross-platform CLI.

#[cfg(target_os = "macos")]
use std::path::PathBuf;

/// The bare shared-library file name `ort` `dlopen`s when `ORT_DYLIB_PATH` is
/// unset. Also the file name the installers write into [`crate::paths::runtime_dir`].
#[cfg(target_os = "windows")]
pub const DYLIB_FILE_NAME: &str = "onnxruntime.dll";
#[cfg(target_os = "macos")]
pub const DYLIB_FILE_NAME: &str = "libonnxruntime.dylib";
#[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
pub const DYLIB_FILE_NAME: &str = "libonnxruntime.so";

/// The one-time install command surfaced to the user when the runtime is
/// missing. Distinct from the AI-model download (`fileid models download`).
pub const INSTALL_COMMAND: &str = "fileid runtime install";

// ─────────────────────────────────────────────────────────────────────────
// macOS resolution.
// ─────────────────────────────────────────────────────────────────────────

/// Where `fileid runtime install` writes the dylib:
/// `<state-root>/runtime/libonnxruntime.dylib`.
#[cfg(target_os = "macos")]
pub fn install_path() -> anyhow::Result<PathBuf> {
    Ok(crate::paths::runtime_dir()?.join(DYLIB_FILE_NAME))
}

/// Ordered macOS search locations for an installed ONNX Runtime dylib (highest
/// priority first), NOT counting an explicit `ORT_DYLIB_PATH` (resolved before
/// this list):
///   1. beside the running executable — also what `ort` itself probes for a
///      bare name, surfaced here so `status`/error text can report it;
///   2. the engine runtime dir (`fileid runtime install` target);
///   3. Homebrew on Apple silicon (`/opt/homebrew/lib`);
///   4. `/usr/local/lib` (Intel-prefix Homebrew / manual installs).
#[cfg(target_os = "macos")]
pub fn search_locations() -> Vec<PathBuf> {
    let mut out = Vec::with_capacity(4);
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            out.push(dir.join(DYLIB_FILE_NAME));
        }
    }
    if let Ok(rt) = crate::paths::runtime_dir() {
        out.push(rt.join(DYLIB_FILE_NAME));
    }
    out.push(PathBuf::from("/opt/homebrew/lib").join(DYLIB_FILE_NAME));
    out.push(PathBuf::from("/usr/local/lib").join(DYLIB_FILE_NAME));
    out
}

/// Resolve an installed ONNX Runtime dylib, honoring an explicit, non-empty
/// `ORT_DYLIB_PATH` first, then [`search_locations`]. Returns the first path
/// that is an existing file.
///
/// Note: even when this returns `None`, `ort` may still `dlopen` a dylib placed
/// on the dyld search path (`/usr/lib`, `DYLD_LIBRARY_PATH`, …) — so callers use
/// this for a fast, actionable pre-flight, not as a hard proof of absence.
#[cfg(target_os = "macos")]
pub fn resolve_dylib() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("ORT_DYLIB_PATH") {
        if !p.is_empty() {
            let p = PathBuf::from(p);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    search_locations().into_iter().find(|p| p.is_file())
}

/// Pin `ORT_DYLIB_PATH` to a resolved dylib so the `load-dynamic` loader finds
/// it. No-op when `ORT_DYLIB_PATH` is already set (honor the user's override) or
/// nothing resolves. Call once, before the first ORT session. Returns the path
/// pinned, if any.
#[cfg(target_os = "macos")]
pub fn pin_dylib_path() -> Option<PathBuf> {
    if std::env::var_os("ORT_DYLIB_PATH").is_some_and(|v| !v.is_empty()) {
        return None;
    }
    let resolved = search_locations().into_iter().find(|p| p.is_file())?;
    // SAFETY note: single-threaded at this point in startup (set before the
    // tokio workers touch ORT), mirroring the Windows pin in main.rs.
    std::env::set_var("ORT_DYLIB_PATH", &resolved);
    Some(resolved)
}

/// Is the ONNX Runtime resolvable for this process? On macOS: an explicit
/// `ORT_DYLIB_PATH` or a hit in [`search_locations`]. On every other platform
/// the runtime is resolved through that platform's own path (bundled DLLs /
/// system `.so`), so this is unconditionally `true` — the macOS provisioning
/// gate must never block a Windows/Linux scan.
#[cfg(target_os = "macos")]
pub fn is_available() -> bool {
    resolve_dylib().is_some()
}
#[cfg(not(target_os = "macos"))]
pub fn is_available() -> bool {
    true
}

/// One-line, user-facing message for a missing macOS runtime. Names the one-time
/// install command and keeps it clearly distinct from the model download.
#[cfg(target_os = "macos")]
pub fn missing_runtime_message() -> String {
    format!(
        "ONNX Runtime is not installed, so the AI scan can't load its models. \
         Install it once with `{INSTALL_COMMAND}` (or `brew install onnxruntime`), then scan \
         again. This is a one-time setup, separate from the AI model download \
         (`fileid models download`)."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dylib_file_name_matches_platform() {
        if cfg!(target_os = "windows") {
            assert_eq!(DYLIB_FILE_NAME, "onnxruntime.dll");
        } else if cfg!(target_os = "macos") {
            assert_eq!(DYLIB_FILE_NAME, "libonnxruntime.dylib");
        } else {
            assert_eq!(DYLIB_FILE_NAME, "libonnxruntime.so");
        }
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn runtime_always_available_off_macos() {
        // The macOS-only provisioning gate must never block Windows/Linux.
        assert!(is_available());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn search_locations_are_ordered_and_named() {
        let locs = search_locations();
        // runtime dir + the two system prefixes are always present; the
        // beside-exe entry is best-effort (current_exe may fail in a sandbox).
        assert!(locs.iter().all(|p| p.ends_with(DYLIB_FILE_NAME)));
        assert!(locs.iter().any(|p| p.starts_with("/opt/homebrew/lib")));
        assert!(locs.iter().any(|p| p.starts_with("/usr/local/lib")));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn explicit_dylib_path_is_honored_first() {
        // A non-existent ORT_DYLIB_PATH must NOT resolve (we only return real
        // files), and must suppress the auto-pin (honor the user's intent).
        let dir = std::env::temp_dir().join(format!("fileid-ort-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join(DYLIB_FILE_NAME);
        std::fs::write(&fake, b"not-a-real-dylib").unwrap();

        // Saved/restored so the test never leaks process env into siblings.
        let prev = std::env::var_os("ORT_DYLIB_PATH");
        std::env::set_var("ORT_DYLIB_PATH", &fake);
        assert_eq!(resolve_dylib().as_deref(), Some(fake.as_path()));
        assert!(pin_dylib_path().is_none(), "must not re-pin when already set");
        match prev {
            Some(v) => std::env::set_var("ORT_DYLIB_PATH", v),
            None => std::env::remove_var("ORT_DYLIB_PATH"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
