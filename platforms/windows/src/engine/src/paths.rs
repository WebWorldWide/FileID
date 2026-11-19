//! Per-OS state directory layout; never transmitted off-device.
//! - Windows: `%LOCALAPPDATA%\FileID\` (mirror of macOS `AppSupportPath.swift`).
//! - Linux/BSD, and macOS for the CLI/TUI: `$XDG_DATA_HOME/FileID/` → `~/.local/share/FileID/`.
//! - macOS app-hosted engine (`apple-adapters`): `~/Library/Application Support/FileID/` (the container when sandboxed),
//!   or the folder of `FILEID_DATABASE_PATH` when set, exactly like the Swift engine.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Windows honors `LOCALAPPDATA`, falling back to `%USERPROFILE%\AppData\Local\FileID`; elsewhere `XDG_DATA_HOME`
/// (default `~/.local/share`) joined with `FileID`.
#[cfg(windows)]
fn resolve_root() -> Result<PathBuf> {
    if let Ok(s) = std::env::var("LOCALAPPDATA") && !s.is_empty() {
        return Ok(PathBuf::from(s).join("FileID"));
    }
    if let Ok(home) = std::env::var("USERPROFILE") {
        return Ok(PathBuf::from(home).join("AppData").join("Local").join("FileID"));
    }
    anyhow::bail!("could not resolve %LOCALAPPDATA% or %USERPROFILE% for FileID state dir")
}

/// Its state is the Swift engine's; CLI and TUI builds never enable `apple-adapters`, so their state never merges with
/// the app's.
pub const APP_HOSTED: bool = cfg!(all(target_os = "macos", feature = "apple-adapters"));

/// `FILEID_DATABASE_PATH` (the Swift engine's variable), then `FILEID_DB`; empty values are ignored.
#[cfg_attr(not(all(target_os = "macos", feature = "apple-adapters")), allow(dead_code))]
fn app_database_override() -> Option<PathBuf> {
    ["FILEID_DATABASE_PATH", "FILEID_DB"]
        .iter()
        .filter_map(std::env::var_os)
        .find(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// An explicit database keeps every journal, plan and log beside it, so an isolated engine never touches the user's
/// library.
#[cfg_attr(not(all(target_os = "macos", feature = "apple-adapters")), allow(dead_code))]
fn resolve_app_root() -> Result<PathBuf> {
    if let Some(database) = app_database_override() {
        let parent = database.parent().filter(|parent| !parent.as_os_str().is_empty());
        return parent
            .map(Path::to_path_buf)
            .with_context(|| format!("FILEID_DATABASE_PATH has no parent folder: {}", database.display()));
    }
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .context("could not resolve $HOME for the FileID state folder")?;
    Ok(PathBuf::from(home).join("Library").join("Application Support").join("FileID"))
}

#[cfg(all(target_os = "macos", feature = "apple-adapters"))]
#[cfg_attr(test, allow(dead_code))]
fn resolve_root() -> Result<PathBuf> {
    resolve_app_root()
}

#[cfg(all(not(windows), not(all(target_os = "macos", feature = "apple-adapters"))))]
fn resolve_root() -> Result<PathBuf> {
    if let Ok(s) = std::env::var("XDG_DATA_HOME")
        && !s.is_empty() {
            return Ok(PathBuf::from(s).join("FileID"));
        }
    if let Ok(home) = std::env::var("HOME") {
        return Ok(PathBuf::from(home).join(".local").join("share").join("FileID"));
    }
    anyhow::bail!("could not resolve $XDG_DATA_HOME or $HOME for FileID state dir")
}

#[cfg(not(test))]
pub fn root() -> Result<PathBuf> {
    resolve_root()
}

/// Unit tests never write the owner's real state folder.
#[cfg(test)]
pub fn root() -> Result<PathBuf> {
    Ok(std::env::temp_dir().join(format!("fileid-engine-tests-{}", std::process::id())).join("FileID"))
}

/// `FILEID_DB` is an explicit front-end/test override; empty values are ignored so a misconfigured shell never resolves
/// to the working directory.
pub fn db_path() -> Result<PathBuf> {
    if APP_HOSTED && let Some(path) = app_database_override() {
        return Ok(path);
    }
    if let Some(path) = std::env::var_os("FILEID_DB").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    Ok(root()?.join("fileid.sqlite"))
}
pub fn logs_dir()     -> Result<PathBuf> { Ok(root()?.join("logs")) }
// `models_dir` is the single resolution point for every model artifact (registry destinations, install sentinels,
// per-model loaders).
/// `FILEID_MODELS_DIR` overrides the per-OS default (highest priority). The `fileid` CLI points it at
/// [`engine_models_dir`] so downloads never land in the macOS app's read-only CoreML dir and a spawned engine inherits
/// the same path.
pub fn models_dir() -> Result<PathBuf> {
    if let Some(over) = std::env::var_os("FILEID_MODELS_DIR")
        && !over.is_empty() {
            return Ok(PathBuf::from(over));
        }
    default_models_dir()
}

/// Always `<root>/Models`, ignoring `FILEID_MODELS_DIR` and the macOS desktop-app preference; distinct from the Swift
/// app's read-only CoreML `~/Library/Application Support/FileID/Models`. The CLI exports it as `FILEID_MODELS_DIR`.
/// Used by the CLI (an external lib consumer), never by the engine binary.
pub fn engine_models_dir() -> Result<PathBuf> { Ok(root()?.join("Models")) }

/// Nothing is installed here any more (tract is compiled in); the path stays so a data reset recognises it and keeps
/// what a user put there.
#[allow(dead_code)]
pub fn runtime_dir() -> Result<PathBuf> { Ok(root()?.join("runtime")) }

#[cfg(not(target_os = "macos"))]
fn default_models_dir() -> Result<PathBuf> { Ok(root()?.join("Models")) }

/// macOS with `FILEID_MODELS_DIR` UNSET only: prefer the Swift app's `~/Library/Application Support/FileID/Models` when
/// it exists, else the XDG default. Read-only by construction; the CLI/TUI pin `FILEID_MODELS_DIR` to
/// [`engine_models_dir`] so they never reach this branch.
#[cfg(all(target_os = "macos", feature = "apple-adapters"))]
fn default_models_dir() -> Result<PathBuf> { Ok(root()?.join("Models")) }

#[cfg(all(target_os = "macos", not(feature = "apple-adapters"), not(test)))]
fn default_models_dir() -> Result<PathBuf> {
    if let Some(home) = std::env::var_os("HOME") {
        let app_models =
            PathBuf::from(home).join("Library/Application Support/FileID/Models");
        if app_models.is_dir() {
            return Ok(app_models);
        }
    }
    Ok(root()?.join("Models"))
}

#[cfg(all(target_os = "macos", not(feature = "apple-adapters"), test))]
fn default_models_dir() -> Result<PathBuf> { Ok(root()?.join("Models")) }

/// ONNX weights the Mac app installs under other names (same manifest artifact, same SHA-256): Rust-layout path,
/// Swift-layout path, both relative to the models folder.
pub const SWIFT_MODEL_LAYOUT: &[(&str, &str)] = &[
    ("sface/face_recognition_sface_2021dec.onnx", "face_recognition_sface_2021dec.onnx"),
    ("mobileclip/mobileclip_s2_image.onnx", "mobileclip_image/clip_vitb32_image.onnx"),
];

/// On macOS a model file missing from the layout asked for falls back to the same artifact under the other layout
/// (Rust or Swift), so models installed once serve both engines.
pub fn installed_model_file(path: &Path) -> PathBuf {
    if cfg!(target_os = "macos")
        && !path.exists()
        && let Some(alias) = other_layout_alias(path)
        && alias.is_file()
    {
        return alias;
    }
    path.to_path_buf()
}

fn other_layout_alias(path: &Path) -> Option<PathBuf> {
    let root = models_dir().ok()?;
    let relative = path
        .strip_prefix(&root)
        .ok()?
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    SWIFT_MODEL_LAYOUT
        .iter()
        .find_map(|&(rust, swift)| match relative.as_str() {
            name if name == rust => Some(swift),
            name if name == swift => Some(rust),
            _ => None,
        })
        .map(|other| other.split('/').fold(root, |path, part| path.join(part)))
}

pub fn hf_cache_dir() -> Result<PathBuf> { Ok(models_dir()?.join("HuggingFace")) }
pub fn thumbs_dir()   -> Result<PathBuf> { Ok(root()?.join("thumbs.cache")) }
pub fn faces_dir()    -> Result<PathBuf> { Ok(root()?.join("face_crops")) }
pub fn restructure_plans_dir() -> Result<PathBuf> { Ok(root()?.join("restructure_plans")) }
pub fn restructure_shortcut_undo_dir() -> Result<PathBuf> {
    Ok(root()?.join("restructure_shortcut_undo"))
}
/// Separate from the engine's `settings.json`; read-only from the engine, the app owns writes.
pub fn app_settings_path() -> Result<PathBuf> { Ok(root()?.join("app-settings.json")) }
pub fn trash_log_path() -> Result<PathBuf> { Ok(root()?.join("trash_log.json")) }

pub fn ensure_state_dirs() -> Result<PathBuf> {
    let root = root()?;
    for sub in [
        &root,
        &logs_dir()?,
        &models_dir()?,
        &hf_cache_dir()?,
        &thumbs_dir()?,
        &faces_dir()?,
        &restructure_plans_dir()?,
        &restructure_shortcut_undo_dir()?,
    ] {
        crate::util::read_only::require_writable(sub)?;
        std::fs::create_dir_all(sub)
            .with_context(|| format!("creating {}", sub.display()))?;
    }
    if let Some(parent) = db_path()?.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating database parent {}", parent.display()))?;
    }
    Ok(root)
}

#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    fn restore(key: &str, val: Option<std::ffi::OsString>) {
        match val {
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        }
    }

    /// A set-but-EMPTY `%LOCALAPPDATA%` must fall through to `%USERPROFILE%`, not collapse to the CWD-relative
    /// `FileID`.
    #[cfg(windows)]
    #[test]
    fn root_empty_localappdata_falls_back_to_userprofile() {
        let _g = TEST_ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let saved = std::env::var_os("LOCALAPPDATA");
        let profile = std::env::var_os("USERPROFILE")
            .expect("USERPROFILE must be set to exercise the fallback");

        unsafe { std::env::set_var("LOCALAPPDATA", "") };
        let empty = resolve_root();
        unsafe { std::env::set_var("LOCALAPPDATA", r"D:\State\Local") };
        let set = resolve_root();
        unsafe { std::env::remove_var("LOCALAPPDATA") };
        let unset = resolve_root();

        restore("LOCALAPPDATA", saved);

        let want = PathBuf::from(profile)
            .join("AppData")
            .join("Local")
            .join("FileID");
        let empty = empty.expect("empty LOCALAPPDATA must resolve via USERPROFILE");
        assert!(empty.is_absolute(), "leaked a CWD-relative root: {}", empty.display());
        assert_eq!(empty, want);
        assert_eq!(set.unwrap(), PathBuf::from(r"D:\State\Local\FileID"));
        assert_eq!(unset.unwrap(), want);
    }

    /// The non-Windows behavior the Windows guard mirrors: empty `$XDG_DATA_HOME` falls back to `$HOME`, never a
    /// CWD-relative path.
    #[cfg(all(not(windows), not(all(target_os = "macos", feature = "apple-adapters"))))]
    #[test]
    fn root_empty_xdg_data_home_falls_back_to_home() {
        let _g = TEST_ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let saved = std::env::var_os("XDG_DATA_HOME");
        let home = std::env::var_os("HOME")
            .expect("HOME must be set to exercise the fallback");

        unsafe { std::env::set_var("XDG_DATA_HOME", "") };
        let empty = resolve_root();
        unsafe { std::env::set_var("XDG_DATA_HOME", "/data/xdg") };
        let set = resolve_root();
        unsafe { std::env::remove_var("XDG_DATA_HOME") };
        let unset = resolve_root();

        restore("XDG_DATA_HOME", saved);

        let want = PathBuf::from(home).join(".local").join("share").join("FileID");
        let empty = empty.expect("empty XDG_DATA_HOME must resolve via HOME");
        assert!(empty.is_absolute(), "leaked a CWD-relative root: {}", empty.display());
        assert_eq!(empty, want);
        assert_eq!(set.unwrap(), PathBuf::from("/data/xdg/FileID"));
        assert_eq!(unset.unwrap(), want);
    }

    #[test]
    fn db_override_is_exact_and_empty_value_falls_back() {
        let _g = TEST_ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let saved = std::env::var_os("FILEID_DB");
        let default = {
            unsafe { std::env::remove_var("FILEID_DB") };
            db_path().unwrap()
        };

        let explicit = std::env::temp_dir().join("fileid-explicit.sqlite");
        unsafe { std::env::set_var("FILEID_DB", &explicit) };
        assert_eq!(db_path().unwrap(), explicit);

        unsafe { std::env::set_var("FILEID_DB", "") };
        assert_eq!(db_path().unwrap(), default);
        restore("FILEID_DB", saved);
    }

    #[test]
    fn app_hosted_root_matches_the_swift_engine() {
        let _g = TEST_ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let saved: Vec<_> = ["FILEID_DATABASE_PATH", "FILEID_DB", "HOME"]
            .into_iter()
            .map(|key| (key, std::env::var_os(key)))
            .collect();

        unsafe {
            std::env::remove_var("FILEID_DATABASE_PATH");
            std::env::remove_var("FILEID_DB");
            std::env::set_var("HOME", "/Users/someone");
        }
        assert_eq!(
            resolve_app_root().unwrap(),
            PathBuf::from("/Users/someone/Library/Application Support/FileID")
        );
        unsafe { std::env::set_var("FILEID_DB", "/scratch/b/fileid.sqlite") };
        assert_eq!(resolve_app_root().unwrap(), PathBuf::from("/scratch/b"));
        unsafe { std::env::set_var("FILEID_DATABASE_PATH", "/scratch/a/fileid.sqlite") };
        assert_eq!(resolve_app_root().unwrap(), PathBuf::from("/scratch/a"));
        assert_eq!(app_database_override(), Some(PathBuf::from("/scratch/a/fileid.sqlite")));
        if APP_HOSTED {
            assert_eq!(db_path().unwrap(), PathBuf::from("/scratch/a/fileid.sqlite"));
        }
        unsafe { std::env::set_var("FILEID_DATABASE_PATH", "") };
        assert_eq!(resolve_app_root().unwrap(), PathBuf::from("/scratch/b"));

        for (key, value) in saved {
            restore(key, value);
        }
    }

    #[test]
    fn rust_model_layout_is_found_when_the_swift_path_is_asked_for_on_macos() {
        let _g = TEST_ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let saved = std::env::var_os("FILEID_MODELS_DIR");
        let dir = std::env::temp_dir().join(format!("fileid-rust-layout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sface")).unwrap();
        unsafe { std::env::set_var("FILEID_MODELS_DIR", &dir) };
        let rust = dir.join("sface").join("face_recognition_sface_2021dec.onnx");
        let swift = dir.join("face_recognition_sface_2021dec.onnx");
        std::fs::write(&rust, b"sface").unwrap();
        let expected = if cfg!(target_os = "macos") { &rust } else { &swift };
        assert_eq!(&installed_model_file(&swift), expected);
        restore("FILEID_MODELS_DIR", saved);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn swift_model_layout_is_found_on_macos() {
        let _g = TEST_ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let saved = std::env::var_os("FILEID_MODELS_DIR");
        let dir = std::env::temp_dir().join(format!("fileid-swift-layout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("mobileclip_image")).unwrap();
        unsafe { std::env::set_var("FILEID_MODELS_DIR", &dir) };

        let rust = dir.join("mobileclip").join("mobileclip_s2_image.onnx");
        let swift = dir.join("mobileclip_image").join("clip_vitb32_image.onnx");
        assert_eq!(installed_model_file(&rust), rust);
        std::fs::write(&swift, b"clip").unwrap();
        let expected = if cfg!(target_os = "macos") { &swift } else { &rust };
        assert_eq!(&installed_model_file(&rust), expected);
        std::fs::create_dir_all(rust.parent().unwrap()).unwrap();
        std::fs::write(&rust, b"clip").unwrap();
        assert_eq!(installed_model_file(&rust), rust);
        let unrelated = dir.join("clip_text").join("clip_text.onnx");
        assert_eq!(installed_model_file(&unrelated), unrelated);

        restore("FILEID_MODELS_DIR", saved);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn model_override_also_owns_the_huggingface_cache() {
        let _g = TEST_ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let saved = std::env::var_os("FILEID_MODELS_DIR");
        let custom = std::env::temp_dir().join("fileid-model-root");
        unsafe { std::env::set_var("FILEID_MODELS_DIR", &custom) };

        assert_eq!(models_dir().unwrap(), custom);
        assert_eq!(hf_cache_dir().unwrap(), custom.join("HuggingFace"));

        restore("FILEID_MODELS_DIR", saved);
    }
}
