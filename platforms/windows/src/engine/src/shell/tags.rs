// File tags via sidecar JSON + IPropertyStore PKEY_Keywords.
//
// On macOS, tags live in `xattr com.apple.metadata:_kMDItemUserTags`. On
// Windows, the canonical location is `IPropertyStore` →
// `PKEY_Keywords` (System.Keywords). Explorer's Details column reads
// from there, the search index picks it up, etc.
//
// Two-tier strategy (both written on every `write_tags` call):
//   1. Sidecar (`.fileid-tags.json` next to the file): always works,
//      every file type, no Win32 interop, no schema risk on weird
//      property handlers (Office / RAW / .HEIC etc.).
//   2. Embedded IPropertyStore: writes PKEY_Keywords through the system
//      property handler so Explorer's Details column shows the tags
//      natively. Silently skipped when the file type has no registered
//      property handler (.swift / .md / unknown extensions).
//
// Multi-tag PKEY_Keywords is canonically a VT_VECTOR | VT_LPWSTR
// (SafeArray of wide strings). windows-rs 0.58 doesn't expose the SDK
// helper `InitPropVariantFromStringVector` for that construction, so we
// fall back to writing one VT_LPWSTR per tag joined by "; " — the
// convention Windows Explorer / search index use when a user types
// multiple keywords into the Details pane. Tools that read PKEY_Keywords
// expect either shape; ours roundtrips cleanly.
//
// `read_tags` merges both tiers (deduped case-insensitively, sidecar
// wins on case) so the user sees a consistent tag set regardless of
// which writer landed first or whether Explorer / Photos has added
// keywords out-of-band.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Default)]
struct SidecarTags {
    tags: Vec<String>,
}

fn sidecar_path(file: &Path) -> PathBuf {
    let parent = file.parent().unwrap_or_else(|| Path::new("."));
    let stem = file
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("untitled");
    parent.join(format!(".{stem}.fileid-tags.json"))
}

/// Replace the file's tag list. Writes both tiers: the sidecar (always)
/// and IPropertyStore PKEY_Keywords (best-effort — silently skipped if
/// the file type has no registered property handler). Empty `tags`
/// clears both: VT_EMPTY into PKEY_Keywords + sidecar removal.
pub fn write_tags(path: &Path, tags: &[String]) -> Result<()> {
    write_sidecar(path, tags)?;
    // IPropertyStore write is best-effort. Many extensions (.swift,
    // .md, source files, archives) don't have a property handler at
    // all; SHGetPropertyStoreFromParsingName errors with E_FAIL /
    // REGDB_E_CLASSNOTREG / similar. We log nothing and fall through
    // so the sidecar carries the tags alone.
    #[cfg(target_os = "windows")]
    {
        let _ = windows_ipropertystore::write_keywords(path, tags);
    }
    Ok(())
}

fn write_sidecar(path: &Path, tags: &[String]) -> Result<()> {
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
    // Atomic write: temp file + rename. Avoids partial files on crash.
    let tmp = sidecar.with_extension("json.tmp");
    std::fs::write(&tmp, &json)
        .with_context(|| format!("writing temp sidecar {}", tmp.display()))?;
    std::fs::rename(&tmp, &sidecar)
        .with_context(|| format!("rename {} -> {}", tmp.display(), sidecar.display()))?;
    Ok(())
}

/// Read the file's tag list. Merges the IPropertyStore PKEY_Keywords
/// (on Windows) with the sidecar JSON, deduped case-insensitively. The
/// sidecar's casing wins when both tiers carry the same tag spelled
/// differently — sidecar is the FileID-authored canonical source.
///
/// Currently exposed for IPC handlers that need to surface the merged
/// view (search / Library card chip); no in-tree caller wires it yet.
#[allow(dead_code)]
pub fn read_tags(path: &Path) -> Result<Vec<String>> {
    let sidecar_tags = read_sidecar(path)?;
    #[cfg(target_os = "windows")]
    let store_tags: Vec<String> = windows_ipropertystore::read_keywords(path).unwrap_or_default();
    #[cfg(not(target_os = "windows"))]
    let store_tags: Vec<String> = Vec::new();

    let mut out = Vec::with_capacity(sidecar_tags.len() + store_tags.len());
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for t in sidecar_tags.into_iter().chain(store_tags.into_iter()) {
        let key = t.to_lowercase();
        if seen.insert(key) {
            out.push(t);
        }
    }
    Ok(out)
}

#[allow(dead_code)]
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

// ────────────────────────────────────────────────────────────────────
// IPropertyStore (Windows only).
// ────────────────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
mod windows_ipropertystore {
    use anyhow::{Context, Result};
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows::core::{PCWSTR, PROPVARIANT};
    use windows::Win32::System::Com::{
        CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::PropertiesSystem::{
        IPropertyStore, SHGetPropertyStoreFromParsingName, GETPROPERTYSTOREFLAGS, GPS_DEFAULT,
        GPS_READWRITE, PROPERTYKEY,
    };

    /// PKEY_Keywords (System.Keywords): {F29F85E0-4FF9-1068-AB91-08002B27B3D9} pid 5.
    const PKEY_KEYWORDS: PROPERTYKEY = PROPERTYKEY {
        fmtid: windows::core::GUID::from_u128(0xF29F85E0_4FF9_1068_AB91_08002B27B3D9),
        pid: 5,
    };

    /// Open IPropertyStore in the requested mode. Returns Err for any
    /// failure — callers treat it as "this file type has no property
    /// handler" and fall through to sidecar-only behavior.
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

    /// Run `body` inside a transient APARTMENTTHREADED COM context.
    /// Matches shell/trash.rs's pattern (per-call init + uninit on the
    /// engine's tag-writer thread). If COM is already initialized on
    /// this thread (S_FALSE), we skip the matching CoUninitialize.
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
            // windows-rs 0.58 doesn't expose InitPropVariantFromStringVector.
            // Fall back to a single VT_LPWSTR with "; "-joined tags — the
            // Explorer Details pane writes the same shape when a user
            // types multiple keywords, and downstream readers (search
            // index, Photos app, FileID itself) parse the separator.
            // Empty tags → VT_EMPTY via PROPVARIANT::default().
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

    #[allow(dead_code)]
    pub fn read_keywords(path: &Path) -> Result<Vec<String>> {
        with_com(|| unsafe {
            let store = open_store(path, GPS_DEFAULT)?;
            let pv = store
                .GetValue(&PKEY_KEYWORDS)
                .context("IPropertyStore::GetValue(PKEY_Keywords)")?;
            // PROPVARIANT::to_string() handles VT_LPWSTR, VT_BSTR and a
            // few sibling string variants. For VT_VECTOR | VT_LPWSTR
            // (which we never write but a third-party tool might),
            // windows-rs 0.58 returns the empty string — degrading
            // gracefully so the sidecar tier still owns those tags.
            let joined = pv.to_string();
            if joined.is_empty() {
                return Ok(Vec::new());
            }
            // Accept both "; " and ";" as separators since different
            // writers produce different spacings.
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
        let dir = std::env::temp_dir().join("fileid-tags-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(name);
        std::fs::write(&path, b"hello").unwrap();
        path
    }

    #[test]
    fn write_then_read_round_trip() {
        let f = temp_file_with_name("rt.txt");
        write_tags(&f, &["holiday".into(), "2024".into()]).unwrap();
        let read = read_tags(&f).unwrap();
        // .txt may or may not roundtrip through IPropertyStore depending
        // on the system property handler, but sidecar must always
        // produce the original two tags.
        assert!(read.contains(&"holiday".to_string()));
        assert!(read.contains(&"2024".to_string()));
        // Cleanup.
        let _ = std::fs::remove_file(sidecar_path(&f));
        let _ = write_tags(&f, &[]);
    }

    #[test]
    fn write_empty_clears_existing_sidecar() {
        let f = temp_file_with_name("clear.txt");
        write_tags(&f, &["x".into()]).unwrap();
        assert!(sidecar_path(&f).exists());
        write_tags(&f, &[]).unwrap();
        assert!(!sidecar_path(&f).exists());
    }

    #[test]
    fn read_missing_sidecar_returns_empty() {
        let f = temp_file_with_name("nofile.txt");
        let _ = std::fs::remove_file(sidecar_path(&f));
        let _ = write_tags(&f, &[]);
        assert!(read_tags(&f).unwrap().is_empty());
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
        let _ = write_tags(&f, &[]);
    }
}
