//! Per-OS state directory layout.
//!
//! - Windows: `%LOCALAPPDATA%\FileID\` (mirror of macOS `AppSupportPath.swift`).
//! - Linux/BSD: `$XDG_DATA_HOME/FileID/` → `~/.local/share/FileID/`.
//! - macOS (if engine ever runs natively here): `~/Library/Application Support/FileID/`.
//!
//! The engine writes only inside these directories; the app reads from
//! the same paths. None of these are ever transmitted off-device.

use std::path::PathBuf;

use anyhow::{Context, Result};

/// Root state directory.
///
/// On Windows we honor `LOCALAPPDATA` first (the canonical envvar); fall
/// back to `%USERPROFILE%\AppData\Local\FileID` if it's missing.
///
/// On non-Windows platforms we follow the XDG Base Directory spec:
/// `XDG_DATA_HOME` (defaults to `~/.local/share`) joined with `FileID`.
/// macOS-native deployments override XDG_DATA_HOME → `~/Library/Application Support`
/// at the system level if you set it, but otherwise default to the
/// XDG path which is also the natural location for a cross-platform engine.
#[cfg(windows)]
pub fn root() -> Result<PathBuf> {
    if let Ok(s) = std::env::var("LOCALAPPDATA") {
        return Ok(PathBuf::from(s).join("FileID"));
    }
    if let Ok(home) = std::env::var("USERPROFILE") {
        return Ok(PathBuf::from(home).join("AppData").join("Local").join("FileID"));
    }
    anyhow::bail!("could not resolve %LOCALAPPDATA% or %USERPROFILE% for FileID state dir")
}

#[cfg(not(windows))]
pub fn root() -> Result<PathBuf> {
    if let Ok(s) = std::env::var("XDG_DATA_HOME") {
        if !s.is_empty() {
            return Ok(PathBuf::from(s).join("FileID"));
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        return Ok(PathBuf::from(home).join(".local").join("share").join("FileID"));
    }
    anyhow::bail!("could not resolve $XDG_DATA_HOME or $HOME for FileID state dir")
}

pub fn db_path()      -> Result<PathBuf> { Ok(root()?.join("fileid.sqlite")) }
pub fn logs_dir()     -> Result<PathBuf> { Ok(root()?.join("logs")) }
pub fn models_dir()   -> Result<PathBuf> { Ok(root()?.join("Models")) }
pub fn hf_cache_dir() -> Result<PathBuf> { Ok(root()?.join("Models").join("HuggingFace")) }
pub fn thumbs_dir()   -> Result<PathBuf> { Ok(root()?.join("thumbs.cache")) }
pub fn faces_dir()    -> Result<PathBuf> { Ok(root()?.join("face_crops")) }
#[allow(dead_code)]
pub fn settings_path() -> Result<PathBuf> { Ok(root()?.join("settings.json")) }
/// The C# app's settings file (separate from the engine's probe-cache
/// `settings.json`). Read-only from the engine; the app owns writes.
pub fn app_settings_path() -> Result<PathBuf> { Ok(root()?.join("app-settings.json")) }
pub fn trash_log_path() -> Result<PathBuf> { Ok(root()?.join("trash_log.json")) }
#[allow(dead_code)]
pub fn merge_log_path() -> Result<PathBuf> { Ok(root()?.join("merge_log.json")) }

/// Create the full state-directory layout. Idempotent; safe on every launch.
pub fn ensure_state_dirs() -> Result<PathBuf> {
    let root = root()?;
    for sub in [&root, &logs_dir()?, &models_dir()?, &hf_cache_dir()?, &thumbs_dir()?, &faces_dir()?] {
        std::fs::create_dir_all(sub)
            .with_context(|| format!("creating {}", sub.display()))?;
    }
    Ok(root)
}
