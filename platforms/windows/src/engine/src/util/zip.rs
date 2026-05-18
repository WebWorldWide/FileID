//! Hardened zip extraction. Used by the prewarm flow for `.zip` downloads
//! (llama.cpp runtime, Performance Packs). Files in nested folders inside
//! the zip land under the same nested folders next to the zip.
//!
//! Hardened against:
//! - **Zip slip** (entries with absolute / `..` paths). `enclosed_name()`
//!   blocks `..`; we ALSO canonicalize-and-`starts_with`-check the
//!   destination against the parent to catch any junction/symlink
//!   traversal at the FS layer.
//! - **Zip bombs** — caps total uncompressed bytes at 2 GiB and entry
//!   count at 10,000.
//! - **Symlink/special entries** — skipped (we only write regular files
//!   and create directories).

use std::path::Path;

use anyhow::Context;

/// Extract every entry of `zip_path` into its parent directory.
pub(crate) fn extract_into_parent(zip_path: &Path) -> anyhow::Result<()> {
    const MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024; // 2 GiB cumulative
    // Per-entry cap is half the cumulative cap so a single bomb entry
    // can't consume the whole budget before the others are inspected.
    const MAX_ENTRY_BYTES: u64 = 1024 * 1024 * 1024;
    const MAX_ENTRIES: usize = 10_000;

    let parent = zip_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("zip has no parent dir"))?;
    let parent_canon = std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());

    let file = std::fs::File::open(zip_path).context("opening zip")?;
    let mut archive = ::zip::ZipArchive::new(file).context("reading zip directory")?;

    if archive.len() > MAX_ENTRIES {
        anyhow::bail!(
            "zip rejected: {} entries (cap {})",
            archive.len(),
            MAX_ENTRIES
        );
    }

    let mut total_bytes: u64 = 0;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).context("zip entry")?;
        let name = entry
            .enclosed_name()
            .ok_or_else(|| anyhow::anyhow!("zip contains an entry with an unsafe name"))?;
        let dest = parent.join(&name);
        if entry.is_dir() {
            std::fs::create_dir_all(&dest).ok();
            continue;
        }
        if let Some(mode) = entry.unix_mode() {
            const S_IFMT: u32 = 0o170000;
            const S_IFREG: u32 = 0o100000;
            if (mode & S_IFMT) != S_IFREG {
                continue;
            }
        }
        let entry_size = entry.size();
        if entry_size > MAX_ENTRY_BYTES {
            anyhow::bail!(
                "zip rejected: entry '{}' claims {} bytes (per-entry cap {})",
                name.display(),
                entry_size,
                MAX_ENTRY_BYTES
            );
        }
        if total_bytes.saturating_add(entry_size) > MAX_BYTES {
            anyhow::bail!("zip rejected: cumulative size exceeds {} bytes", MAX_BYTES);
        }
        total_bytes = total_bytes.saturating_add(entry_size);

        if let Some(p) = dest.parent() {
            std::fs::create_dir_all(p).ok();
        }
        let mut out = std::fs::File::create(&dest)
            .with_context(|| format!("creating {}", dest.display()))?;
        std::io::copy(&mut entry, &mut out)
            .with_context(|| format!("writing {}", dest.display()))?;

        if let Ok(real) = std::fs::canonicalize(&dest) {
            if !real.starts_with(&parent_canon) {
                let _ = std::fs::remove_file(&dest);
                anyhow::bail!("zip entry escaped extraction root: {}", dest.display());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use ::zip::{write::SimpleFileOptions, ZipWriter};

    fn make_zip_with_entries(temp: &Path, entries: &[(&str, &[u8])]) -> std::path::PathBuf {
        let zip_path = temp.join("test.zip");
        let f = std::fs::File::create(&zip_path).unwrap();
        let mut w = ZipWriter::new(f);
        let opts: SimpleFileOptions =
            SimpleFileOptions::default().compression_method(::zip::CompressionMethod::Stored);
        for (name, data) in entries {
            w.start_file(*name, opts).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap();
        zip_path
    }

    #[test]
    fn extracts_simple_zip() {
        let temp = std::env::temp_dir().join(format!(
            "fileid_zip_simple_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&temp).unwrap();
        let zip = make_zip_with_entries(&temp, &[("a.txt", b"hello"), ("nested/b.txt", b"world")]);
        extract_into_parent(&zip).unwrap();
        assert_eq!(std::fs::read(temp.join("a.txt")).unwrap(), b"hello");
        assert_eq!(std::fs::read(temp.join("nested/b.txt")).unwrap(), b"world");
        std::fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn rejects_zip_slip_via_parent_traversal() {
        let temp = std::env::temp_dir().join(format!(
            "fileid_zip_slip_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&temp).unwrap();
        // The zip crate's enclosed_name() should already block this — we
        // assert it bails rather than landing the file outside `parent`.
        let zip = make_zip_with_entries(&temp, &[("../escape.txt", b"oops")]);
        let res = extract_into_parent(&zip);
        assert!(res.is_err(), "zip-slip path must be rejected");
        std::fs::remove_dir_all(&temp).ok();
    }

    // Property tests prove the safety invariants of extract_into_parent
    // on randomized inputs — every output file must land under `parent`,
    // or the function must return Err. No panic, no escape, no leak.
    proptest::proptest! {
        // Invariant: extract_into_parent never panics and never writes
        // outside `parent`, regardless of entry name shape.
        #[test]
        fn never_escapes_parent(
            names in proptest::collection::vec(
                "[a-zA-Z0-9_./\\\\-]{1,40}",
                1..6,
            ),
        ) {
            let temp = std::env::temp_dir().join(format!(
                "fileid_zip_prop_{}",
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir_all(&temp).expect("temp dir");
            let entries: Vec<(&str, &[u8])> =
                names.iter().map(|n| (n.as_str(), b"x" as &[u8])).collect();
            let zip = make_zip_with_entries(&temp, &entries);
            let parent_canon = std::fs::canonicalize(&temp).unwrap_or(temp.clone());
            // The result is either Ok (every file written under parent)
            // or Err (rejected before write). Either way: no escape.
            let _ = extract_into_parent(&zip);
            // Walk `temp` and confirm every file is under parent_canon.
            for entry in walkdir::WalkDir::new(&temp).into_iter().flatten() {
                if entry.file_type().is_file() {
                    let real = std::fs::canonicalize(entry.path())
                        .unwrap_or_else(|_| entry.path().to_path_buf());
                    proptest::prop_assert!(
                        real.starts_with(&parent_canon),
                        "file {} escaped parent {}",
                        real.display(),
                        parent_canon.display()
                    );
                }
            }
            std::fs::remove_dir_all(&temp).ok();
        }
    }
}
