// File tags via sidecar JSON (with IPropertyStore embedded path planned).
//
// On macOS, tags live in `xattr com.apple.metadata:_kMDItemUserTags`. On
// Windows, the canonical location is `IPropertyStore` →
// `PKEY_Keywords` (System.Keywords). Explorer's Details column reads
// from there, the search index picks it up, etc.
//
// Two-tier strategy:
//   1. Sidecar (`.fileid-tags.json` next to the file): always works,
//      every file type, no Win32 interop, no schema risk on weird
//      property handlers (Office / RAW / .HEIC etc.). This is the
//      tier we ship today.
//   2. Embedded IPropertyStore (V14.x): writes PKEY_Keywords through
//      the system handler so Explorer's Details column shows them
//      natively. Needs windows-rs Win32_System_Variant + careful
//      PROPVARIANT vector marshalling that the windows-rs 0.58
//      surface routes via SafeArray; verify on a corpus of file types
//      (Office handlers reject unknown properties; .swift / .md
//      handlers don't exist; etc.).
//
// Reads always merge both tiers (embedded + sidecar) so the user sees
// a consistent tag set regardless of which writer landed first.

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

/// Replace the file's tag list. Empty `tags` clears the property +
/// removes the sidecar.
pub fn write_tags(path: &Path, tags: &[String]) -> Result<()> {
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

/// Read the file's tag list. Today: sidecar only. V14.x: also probes
/// IPropertyStore PKEY_Keywords and de-duplicates the union.
#[allow(dead_code)]
pub fn read_tags(path: &Path) -> Result<Vec<String>> {
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
        assert_eq!(read, vec!["holiday".to_string(), "2024".to_string()]);
        // Cleanup.
        let _ = std::fs::remove_file(sidecar_path(&f));
    }

    #[test]
    fn write_empty_clears_existing() {
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
        assert!(read_tags(&f).unwrap().is_empty());
    }
}
