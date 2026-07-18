// app_settings — Linux mirror of Windows `AppSettings.cs` / macOS `@AppStorage`.
//
// Reads and writes the SAME `app-settings.json` the other platforms use
// (`fileid_engine::paths::app_settings_path()`), with the same camelCase keys,
// so a shared/synced data dir round-trips. Only the keys this app owns are
// touched; every other key is preserved verbatim (forward compatibility with
// the Windows schema). Writes are atomic (temp file + rename) so a crash
// mid-save can't corrupt the file.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

fn settings_path() -> Option<PathBuf> {
    fileid_engine::paths::app_settings_path().ok()
}

fn load_map(path: &Path) -> Map<String, Value> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|value| match value {
            Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default()
}

fn save_map(path: &Path, map: &Map<String, Value>) {
    let Ok(bytes) = serde_json::to_vec_pretty(&Value::Object(map.clone())) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let temporary = path.with_extension("json.tmp");
    if std::fs::write(&temporary, bytes).is_ok() && std::fs::rename(&temporary, path).is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
}

fn get_string(key: &str) -> Option<String> {
    let path = settings_path()?;
    load_map(&path)
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn set_entries(entries: &[(&str, Value)]) {
    let Some(path) = settings_path() else { return };
    let mut map = load_map(&path);
    for (key, value) in entries {
        map.insert((*key).to_owned(), value.clone());
    }
    save_map(&path, &map);
}

/// The persisted last-picked library folder, only when it still exists on
/// disk (mirrors Windows `LibraryRootRecovery`: a deleted folder is treated
/// as never-picked instead of resurrecting a dead path into the UI).
pub fn last_folder() -> Option<PathBuf> {
    let path = PathBuf::from(get_string("lastFolderPath")?);
    path.is_dir().then_some(path)
}

/// Persist the picked folder (path + leaf display label, matching the
/// Windows `lastFolderPath` / `lastFolderDisplay` pair).
pub fn remember_folder(path: &Path) {
    let display = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    set_entries(&[
        (
            "lastFolderPath",
            Value::String(path.to_string_lossy().into_owned()),
        ),
        ("lastFolderDisplay", Value::String(display)),
    ]);
}

/// The persisted active tab id (`library` / `people` / …), matching the
/// Windows `activeTab` key. Unknown/empty values fall back to the default.
pub fn active_tab() -> Option<String> {
    get_string("activeTab").filter(|tab| !tab.trim().is_empty())
}

pub fn remember_active_tab(tab: &str) {
    set_entries(&[("activeTab", Value::String(tab.to_owned()))]);
}

/// Sidebar visibility across launches (Windows `sidebarVisible`).
pub fn sidebar_visible() -> bool {
    let Some(path) = settings_path() else {
        return true;
    };
    load_map(&path)
        .get("sidebarVisible")
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

pub fn remember_sidebar_visible(visible: bool) {
    set_entries(&[("sidebarVisible", Value::Bool(visible))]);
}

/// Has the user dismissed the first-launch Welcome sheet? Mirrors the Windows
/// `welcomeSheetSeen` key / macOS `@AppStorage("welcomeSheetSeen")`.
pub fn welcome_sheet_seen() -> bool {
    let Some(path) = settings_path() else {
        return false;
    };
    load_map(&path)
        .get("welcomeSheetSeen")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

pub fn remember_welcome_sheet_seen() {
    set_entries(&[("welcomeSheetSeen", Value::Bool(true))]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_preserves_unknown_keys() {
        let dir = std::env::temp_dir().join(format!(
            "fileid-linux-settings-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("app-settings.json");
        std::fs::write(
            &path,
            br#"{"gpuExecutionProviderOverride":"cuda","schemaVersion":6}"#,
        )
        .unwrap();

        let mut map = load_map(&path);
        map.insert("lastFolderPath".into(), Value::String("/tmp".into()));
        save_map(&path, &map);

        let reloaded = load_map(&path);
        assert_eq!(
            reloaded.get("gpuExecutionProviderOverride"),
            Some(&Value::String("cuda".into()))
        );
        assert_eq!(reloaded.get("schemaVersion"), Some(&Value::from(6)));
        assert_eq!(
            reloaded.get("lastFolderPath"),
            Some(&Value::String("/tmp".into()))
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn corrupt_settings_file_yields_empty_map_not_a_panic() {
        let dir = std::env::temp_dir().join(format!(
            "fileid-linux-settings-corrupt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("app-settings.json");
        std::fs::write(&path, b"{not json").unwrap();
        assert!(load_map(&path).is_empty());
        std::fs::remove_dir_all(dir).ok();
    }
}
