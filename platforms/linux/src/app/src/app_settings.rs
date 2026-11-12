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

/// The page last used in a sidebar area (macOS `lastTab.<Area>`).
pub fn area_page(area: &str) -> Option<String> {
    get_string(&format!("lastTab.{area}"))
}

pub fn remember_area_page(area: &str, page: &str) {
    set_entries(&[(&format!("lastTab.{area}"), Value::String(page.to_owned()))]);
}

pub fn forget_folder() {
    let Some(path) = settings_path() else { return };
    let mut map = load_map(&path);
    map.remove("lastFolderPath");
    map.remove("lastFolderDisplay");
    save_map(&path, &map);
}

/// The library location (listRoots id) Library, Search & Moments and Chat are
/// scoped to; `None` for "All locations" (Windows `libraryScopeRootId`).
pub fn library_scope_root_id() -> Option<i64> {
    let path = settings_path()?;
    load_map(&path)
        .get("libraryScopeRootId")
        .and_then(Value::as_i64)
        .filter(|id| *id > 0)
}

pub fn remember_library_scope_root_id(id: Option<i64>) {
    let Some(path) = settings_path() else { return };
    let mut map = load_map(&path);
    match id {
        Some(id) => map.insert("libraryScopeRootId".into(), Value::from(id)),
        None => map.remove("libraryScopeRootId"),
    };
    save_map(&path, &map);
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

// ── Cleanup auto-tag and Chat summaries (parity-gtk-shell) ───────────────────

fn get_bool(key: &str, default: bool) -> bool {
    let Some(path) = settings_path() else {
        return default;
    };
    load_map(&path)
        .get(key)
        .and_then(Value::as_bool)
        .unwrap_or(default)
}

/// Tag the copies kept after Cleanup moves extras to Trash. Windows
/// `CleanupAutoTagKept` / macOS `cleanup.autoTagKeepers`; default on.
pub const CLEANUP_AUTO_TAG_NAME: &str = "duplicate-resolved";

pub fn cleanup_auto_tag() -> bool {
    get_bool("cleanupAutoTagKept", true)
}

pub fn remember_cleanup_auto_tag(on: bool) {
    set_entries(&[("cleanupAutoTagKept", Value::Bool(on))]);
}

/// Chat "Summarize with local AI" preference (macOS `chatUseModel`); default on.
pub fn chat_use_model() -> bool {
    get_bool("chatUseModel", true)
}

pub fn remember_chat_use_model(on: bool) {
    set_entries(&[("chatUseModel", Value::Bool(on))]);
}

/// Describe Files ▸ Name style, stored under the Windows `naming*` keys
/// (macOS `naming.*`). Defaults: short, 40 characters, people and event on,
/// date off, learning from renames on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamingSettings {
    pub style: String,
    pub max_length: u32,
    pub include_people: bool,
    pub include_event: bool,
    pub include_date: bool,
    pub learn: bool,
}

impl Default for NamingSettings {
    fn default() -> Self {
        Self {
            style: "short".to_owned(),
            max_length: 40,
            include_people: true,
            include_event: true,
            include_date: false,
            learn: true,
        }
    }
}

fn naming_from_map(map: &Map<String, Value>) -> NamingSettings {
    let defaults = NamingSettings::default();
    let flag = |key: &str, default: bool| map.get(key).and_then(Value::as_bool).unwrap_or(default);
    NamingSettings {
        style: map
            .get("namingStyle")
            .and_then(Value::as_str)
            .filter(|style| matches!(*style, "short" | "descriptive" | "dateFirst"))
            .unwrap_or(&defaults.style)
            .to_owned(),
        max_length: map
            .get("namingMaxLength")
            .and_then(Value::as_i64)
            .map_or(defaults.max_length, |length| length.clamp(4, 120) as u32),
        include_people: flag("namingIncludePeople", defaults.include_people),
        include_event: flag("namingIncludeEvent", defaults.include_event),
        include_date: flag("namingIncludeDate", defaults.include_date),
        learn: flag("namingLearn", defaults.learn),
    }
}

pub fn naming() -> NamingSettings {
    settings_path().map_or_else(NamingSettings::default, |path| naming_from_map(&load_map(&path)))
}

pub fn remember_naming(naming: &NamingSettings) {
    set_entries(&[
        ("namingStyle", Value::String(naming.style.clone())),
        ("namingMaxLength", Value::from(naming.max_length)),
        ("namingIncludePeople", Value::Bool(naming.include_people)),
        ("namingIncludeEvent", Value::Bool(naming.include_event)),
        ("namingIncludeDate", Value::Bool(naming.include_date)),
        ("namingLearn", Value::Bool(naming.learn)),
    ]);
}

/// Absolute folder paths to skip when running Deep Analyze over the whole
/// library (mirrors Windows `AppSettings.DeepAnalyzeExcludedFolders`).
/// Separate from the scan exclusion list — a folder can be fine to
/// catalog/tag/search but too slow or private to run the VLM over. `None`
/// (key absent, matching every settings.json written before this feature
/// existed) means no exclusions, same as an empty list. Sent fresh with
/// every deepAnalyzeAll; an explicit file selection is never filtered.
/// Trim trailing separators, drop blanks, dedupe, cap the list (matches the
/// schema's `deepAnalyzeAll.excludedFolders` maxItems). Mirrors the Windows
/// `SanitizeExcludedFolders` except for case: Windows folds case because NTFS
/// does, but Linux filesystems are case-sensitive, so `~/Photos` and
/// `~/photos` are genuinely different directories. Folding here would silently
/// drop one of them from a privacy-motivated control — the user would believe a
/// folder was excluded when it wasn't.
#[cfg(test)]
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
    fn naming_settings_default_and_sanitize_like_windows() {
        assert_eq!(naming_from_map(&Map::new()), NamingSettings::default());
        let Value::Object(map) = serde_json::json!({
            "namingStyle": "dateFirst", "namingMaxLength": 64, "namingIncludePeople": false,
            "namingIncludeEvent": false, "namingIncludeDate": true, "namingLearn": false
        }) else { unreachable!() };
        assert_eq!(
            naming_from_map(&map),
            NamingSettings {
                style: "dateFirst".into(),
                max_length: 64,
                include_people: false,
                include_event: false,
                include_date: true,
                learn: false
            }
        );
        let Value::Object(bad) = serde_json::json!({"namingStyle": "loud", "namingMaxLength": 900})
        else { unreachable!() };
        let sanitized = naming_from_map(&bad);
        assert_eq!((sanitized.style.as_str(), sanitized.max_length), ("short", 120));
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
