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

/// Absolute folder paths to skip when running Deep Analyze over the whole
/// library (mirrors Windows `AppSettings.DeepAnalyzeExcludedFolders`).
/// Separate from the scan exclusion list — a folder can be fine to
/// catalog/tag/search but too slow or private to run the VLM over. `None`
/// (key absent, matching every settings.json written before this feature
/// existed) means no exclusions, same as an empty list. Sent fresh with
/// every deepAnalyzeAll; an explicit file selection is never filtered.
pub fn deep_analyze_excluded_folders() -> Option<Vec<String>> {
    let path = settings_path()?;
    let list = load_map(&path)
        .get("deepAnalyzeExcludedFolders")?
        .as_array()?
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    // Sanitize on READ too, not just on write: settings.json is shared with the
    // other platforms and hand-editable, so this is the path that would
    // otherwise hand an unbounded/relative list straight to the engine.
    let list = sanitize_deep_analyze_excluded_folders(&list);
    (!list.is_empty()).then_some(list)
}

/// Trim trailing separators, drop blanks, dedupe, cap the list (matches the
/// schema's `deepAnalyzeAll.excludedFolders` maxItems). Mirrors the Windows
/// `SanitizeExcludedFolders` except for case: Windows folds case because NTFS
/// does, but Linux filesystems are case-sensitive, so `~/Photos` and
/// `~/photos` are genuinely different directories. Folding here would silently
/// drop one of them from a privacy-motivated control — the user would believe a
/// folder was excluded when it wasn't.
pub fn sanitize_deep_analyze_excluded_folders(raw: &[String]) -> Vec<String> {
    const MAX: usize = 256;
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for entry in raw {
        if out.len() >= MAX {
            break;
        }
        let trimmed = entry.trim().trim_end_matches(['\\', '/']);
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed.to_owned()) {
            out.push(trimmed.to_owned());
        }
    }
    out
}

pub fn remember_deep_analyze_excluded_folders(folders: &[String]) {
    let sanitized = sanitize_deep_analyze_excluded_folders(folders);
    let value = Value::Array(sanitized.into_iter().map(Value::String).collect());
    set_entries(&[("deepAnalyzeExcludedFolders", value)]);
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
    fn sanitize_deep_analyze_excluded_folders_trims_dedupes_and_drops_blanks() {
        let raw = vec![
            "/home/adam/Photos/ ".to_string(),
            "/home/adam/photos".to_string(), // NOT a dup: case-sensitive FS
            "   ".to_string(),               // blank
            "/home/adam/Photos/".to_string(), // dup by trailing sep
            "/home/adam/Private".to_string(),
        ];
        let result = sanitize_deep_analyze_excluded_folders(&raw);
        assert_eq!(
            result,
            vec![
                "/home/adam/Photos".to_string(),
                "/home/adam/photos".to_string(),
                "/home/adam/Private".to_string(),
            ],
            "Linux paths are case-sensitive — /Photos and /photos are different \
             directories and both must survive; only the trailing-separator \
             duplicate collapses"
        );
    }

    #[test]
    fn sanitize_deep_analyze_excluded_folders_caps_at_bound() {
        let many: Vec<String> = (0..400).map(|i| format!("/x/{i}")).collect();
        let result = sanitize_deep_analyze_excluded_folders(&many);
        assert_eq!(result.len(), 256);
    }

    #[test]
    fn deep_analyze_excluded_folders_round_trips_through_the_settings_file() {
        // Exercises the same load_map/save_map path deep_analyze_excluded_folders
        // and remember_deep_analyze_excluded_folders use, on an isolated temp
        // file — settings_path() itself resolves a real, non-test-isolated
        // location, so this mirrors settings_round_trip_preserves_unknown_keys
        // above rather than calling the public getters/setters directly.
        let dir = std::env::temp_dir().join(format!(
            "fileid-linux-settings-da-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("app-settings.json");

        let mut map = load_map(&path);
        let sanitized = sanitize_deep_analyze_excluded_folders(&[
            "/home/adam/Private".to_string(),
            "/home/adam/Private".to_string(), // dup, must collapse
        ]);
        map.insert(
            "deepAnalyzeExcludedFolders".into(),
            Value::Array(sanitized.into_iter().map(Value::String).collect()),
        );
        save_map(&path, &map);

        let reloaded = load_map(&path);
        let folders: Vec<String> = reloaded
            .get("deepAnalyzeExcludedFolders")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
        assert_eq!(folders, vec!["/home/adam/Private".to_string()]);
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
