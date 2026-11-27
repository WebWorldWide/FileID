// File tags via sidecar JSON + IPropertyStore PKEY_Keywords (System.Keywords, shown in Explorer's Details column and indexed by search).
// Two tiers, both written on every `write_tags`:
// 1. Sidecar (`.fileid-tags.json` next to the file): works for every file type with no Win32 interop or property-handler risk (Office / RAW / HEIC).
// 2. IPropertyStore: writes PKEY_Keywords through the system property handler; silently skipped when the file type has none (.swift / .md / unknown).
// PKEY_Keywords is canonically a VT_VECTOR of VT_LPWSTR, but windows-rs 0.58 lacks `InitPropVariantFromStringVector`, so one VT_LPWSTR of tags joined by "; " is written (the shape Explorer writes).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Default)]
struct SidecarTags {
    tags: Vec<String>,
}

fn sidecar_path(file: &Path) -> PathBuf {
    let parent = file.parent().unwrap_or_else(|| Path::new("."));
    // Built from the OS name: a lossy stem would let two files with non-Unicode names share one sidecar.
    let mut name = std::ffi::OsString::from(".");
    name.push(file.file_name().unwrap_or_else(|| std::ffi::OsStr::new("untitled")));
    name.push(".fileid-tags.json");
    parent.join(name)
}

/// Replace the file's tag list in both tiers. Empty `tags` clears both. The
/// sidecar write is mandatory (returns Err on failure); the IPropertyStore
/// write is best-effort, since many file types have no property handler.
pub fn write_tags(path: &Path, tags: &[String]) -> Result<()> {
    write_sidecar(path, tags)?;
    #[cfg(target_os = "windows")]
    {
        let _ = windows_ipropertystore::write_keywords(path, tags);
    }
    Ok(())
}

fn write_sidecar(path: &Path, tags: &[String]) -> Result<()> {
    crate::util::read_only::require_source_mutation(path)?;
    crate::util::read_only::require_writable(&sidecar_path(path))?;
    let sidecar = sidecar_path(path);
    if tags.is_empty() {
        if sidecar.exists() {
            std::fs::remove_file(&sidecar)
                .with_context(|| format!("removing sidecar {}", sidecar.display()))?;
        }
        return Ok(());
    }
    let payload = SidecarTags { tags: tags.to_vec() };
    let json = serde_json::to_vec_pretty(&payload).context("serializing sidecar tags")?;
    let tmp = sidecar.with_extension("json.tmp");
    std::fs::write(&tmp, &json)
        .with_context(|| format!("writing temp sidecar {}", tmp.display()))?;
    std::fs::rename(&tmp, &sidecar)
        .with_context(|| format!("rename {} -> {}", tmp.display(), sidecar.display()))?;
    Ok(())
}

/// Moves the tags sidecar to follow a renamed/moved file. Best-effort: a missing sidecar is a no-op and rename errors are swallowed so they never turn a successful move into a failure.
pub fn move_sidecar(old: &Path, new: &Path) {
    if crate::util::read_only::require_source_mutation(old).is_err() || crate::util::read_only::require_source_mutation(new).is_err() { return; }
    let from = sidecar_path(old);
    if std::fs::symlink_metadata(&from).is_err() {
        return;
    }
    let _ = std::fs::rename(&from, sidecar_path(new));
}

/// Read back both tiers, deduped case-insensitively with the sidecar's casing
/// winning. Verifies the write path in tests.
#[cfg(test)]
fn read_tags(path: &Path) -> Result<Vec<String>> {
    let sidecar_tags = read_sidecar(path)?;
    #[cfg(target_os = "windows")]
    let store_tags: Vec<String> = windows_ipropertystore::read_keywords(path).unwrap_or_default();
    #[cfg(not(target_os = "windows"))]
    let store_tags: Vec<String> = Vec::new();

    let mut out = Vec::with_capacity(sidecar_tags.len() + store_tags.len());
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for t in sidecar_tags.into_iter().chain(store_tags) {
        let key = t.to_lowercase();
        if seen.insert(key) {
            out.push(t);
        }
    }
    Ok(out)
}

#[cfg(test)]
fn read_sidecar(path: &Path) -> Result<Vec<String>> {
    let sidecar = sidecar_path(path);
    if !sidecar.exists() {
        return Ok(Vec::new());
    }
    let bytes = std::fs::read(&sidecar)
        .with_context(|| format!("reading sidecar {}", sidecar.display()))?;
    let payload: SidecarTags = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing sidecar {}", sidecar.display()))?;
    Ok(payload.tags)
}

#[cfg(target_os = "windows")]
mod windows_ipropertystore {
    use anyhow::{Context, Result};
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::PROPERTYKEY;
    use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
    use windows::Win32::System::Com::{
        CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::PropertiesSystem::{
        IPropertyStore, SHGetPropertyStoreFromParsingName, GETPROPERTYSTOREFLAGS, GPS_READWRITE,
    };
    #[cfg(test)]
    use windows::Win32::UI::Shell::PropertiesSystem::GPS_DEFAULT;

    const PKEY_KEYWORDS: PROPERTYKEY = PROPERTYKEY {
        fmtid: windows::core::GUID::from_u128(0xF29F85E0_4FF9_1068_AB91_08002B27B3D9),
        pid: 5,
    };

    /// Any failure means "this file type has no property handler"; callers fall back to sidecar-only.
    unsafe fn open_store(path: &Path, flags: GETPROPERTYSTOREFLAGS) -> Result<IPropertyStore> {
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let store: IPropertyStore = unsafe {
            SHGetPropertyStoreFromParsingName(PCWSTR(wide.as_ptr()), None, flags)
                .context("SHGetPropertyStoreFromParsingName")?
        };
        Ok(store)
    }

    fn with_com<R>(body: impl FnOnce() -> R) -> R {
        unsafe {
            let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let must_uninit = hr.is_ok();
            let result = body();
            if must_uninit {
                CoUninitialize();
            }
            result
        }
    }

    pub fn write_keywords(path: &Path, tags: &[String]) -> Result<()> {
        with_com(|| unsafe {
            let store = open_store(path, GPS_READWRITE)?;
            let pv: PROPVARIANT = if tags.is_empty() {
                PROPVARIANT::default()
            } else {
                PROPVARIANT::from(tags.join("; ").as_str())
            };
            store
                .SetValue(&PKEY_KEYWORDS, &pv)
                .context("IPropertyStore::SetValue(PKEY_Keywords)")?;
            store.Commit().context("IPropertyStore::Commit")?;
            Ok(())
        })
    }

    #[cfg(test)]
    pub fn read_keywords(path: &Path) -> Result<Vec<String>> {
        with_com(|| unsafe {
            let store = open_store(path, GPS_DEFAULT)?;
            let pv = store
                .GetValue(&PKEY_KEYWORDS)
                .context("IPropertyStore::GetValue(PKEY_Keywords)")?;
            // windows-rs 0.58 returns an empty string for VT_VECTOR of VT_LPWSTR, so the sidecar tier owns those tags.
            let joined = pv.to_string();
            if joined.is_empty() {
                return Ok(Vec::new());
            }
            let tags: Vec<String> = joined
                .split(';')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect();
            Ok(tags)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file_with_name(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fileid-tags-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, b"hello").unwrap();
        path
    }

    fn cleanup_temp_file(path: &Path) {
        let _ = write_tags(path, &[]);
        let _ = std::fs::remove_file(sidecar_path(path));
        let _ = std::fs::remove_file(path);
        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }

    #[test]
    fn write_then_read_round_trip() {
        let f = temp_file_with_name("rt.txt");
        write_tags(&f, &["holiday".into(), "2024".into()]).unwrap();
        let read = read_tags(&f).unwrap();
        // .txt may or may not roundtrip through IPropertyStore, but the sidecar must always produce the original two tags.
        assert!(read.contains(&"holiday".to_string()));
        assert!(read.contains(&"2024".to_string()));
        cleanup_temp_file(&f);
    }

    #[test]
    fn write_empty_clears_existing_sidecar() {
        let f = temp_file_with_name("clear.txt");
        write_tags(&f, &["x".into()]).unwrap();
        assert!(sidecar_path(&f).exists());
        write_tags(&f, &[]).unwrap();
        assert!(!sidecar_path(&f).exists());
        cleanup_temp_file(&f);
    }

    #[test]
    fn read_missing_sidecar_returns_empty() {
        let f = temp_file_with_name("nofile.txt");
        let _ = std::fs::remove_file(sidecar_path(&f));
        let _ = write_tags(&f, &[]);
        assert!(read_tags(&f).unwrap().is_empty());
        cleanup_temp_file(&f);
    }

    #[test]
    fn read_dedups_case_insensitive() {
        let f = temp_file_with_name("dedup.txt");
        write_tags(&f, &["Holiday".into()]).unwrap();
        let read = read_tags(&f).unwrap();
        let occurrences = read
            .iter()
            .filter(|t| t.eq_ignore_ascii_case("holiday"))
            .count();
        assert_eq!(occurrences, 1);
        cleanup_temp_file(&f);
    }
}
