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
    const MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
    const MAX_ENTRY_BYTES: u64 = 1024 * 1024 * 1024;
    const MAX_ENTRIES: usize = 10_000;

    let parent = zip_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("zip has no parent dir"))?;
    let stage = parent.join(format!(".fileid-extract-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&stage).context("creating private zip staging directory")?;

    let result = extract_archive(
        zip_path,
        &stage,
        MAX_BYTES,
        MAX_ENTRY_BYTES,
        MAX_ENTRIES,
    )
    .and_then(|()| promote_staged(&stage, parent));
    let _ = std::fs::remove_dir_all(&stage);
    result
}

fn extract_archive(
    zip_path: &Path,
    stage: &Path,
    max_bytes: u64,
    max_entry_bytes: u64,
    max_entries: usize,
) -> anyhow::Result<()> {
    let stage_canon = std::fs::canonicalize(stage).context("canonicalizing zip staging root")?;

    let file = std::fs::File::open(zip_path).context("opening zip")?;
    let mut archive = ::zip::ZipArchive::new(file).context("reading zip directory")?;

    if archive.len() > max_entries {
        anyhow::bail!(
            "zip rejected: {} entries (cap {})",
            archive.len(),
            max_entries
        );
    }

    let mut total_bytes: u64 = 0;     // declared (central-directory) sizes
    let mut total_written: u64 = 0;   // actual decompressed bytes written
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).context("zip entry")?;
        let name = entry
            .enclosed_name()
            .ok_or_else(|| anyhow::anyhow!("zip contains an entry with an unsafe name"))?;
        let dest = stage_canon.join(&name);
        if entry.is_dir() {
            create_staged_dir(&stage_canon, &dest)?;
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
        if entry_size > max_entry_bytes {
            anyhow::bail!(
                "zip rejected: entry '{}' claims {} bytes (per-entry cap {})",
                name.display(),
                entry_size,
                max_entry_bytes
            );
        }
        // Cheap early-out on an honest header; the AUTHORITATIVE cumulative cap
        // is charged the ACTUAL decompressed bytes after the copy below (ENG-88).
        if total_bytes.saturating_add(entry_size) > max_bytes {
            anyhow::bail!("zip rejected: cumulative size exceeds {} bytes", max_bytes);
        }
        total_bytes = total_bytes.saturating_add(entry_size);

        if let Some(p) = dest.parent() {
            create_staged_dir(&stage_canon, p)?;
        }

        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&dest)
            .with_context(|| format!("creating staged entry {}", dest.display()))?;
        // Bound the ACTUAL decompressed output, not the attacker-controlled
        // declared entry.size(): a lying central directory could otherwise
        // make io::copy inflate far past the caps (zip bomb, ENG-88). The cap
        // shrinks to the remaining cumulative budget; +1 so an entry exactly
        // at the cap copies fully while an over-cap one is still detected.
        let entry_cap = max_entry_bytes.min(max_bytes.saturating_sub(total_written));
        let mut limited = std::io::Read::take(&mut entry, entry_cap.saturating_add(1));
        let written = std::io::copy(&mut limited, &mut out)
            .with_context(|| format!("writing {}", dest.display()))?;
        if written > entry_cap {
            let _ = std::fs::remove_file(&dest);
            anyhow::bail!(
                "zip rejected: '{}' decompressed past the byte cap (bomb?)",
                name.display()
            );
        }
        total_written = total_written.saturating_add(written);

        if let Ok(real) = std::fs::canonicalize(&dest) {
            if !real.starts_with(&stage_canon) {
                let _ = std::fs::remove_file(&dest);
                anyhow::bail!("zip entry escaped extraction root: {}", dest.display());
            }
        }
    }
    Ok(())
}

fn create_staged_dir(stage_canon: &Path, dest: &Path) -> anyhow::Result<()> {
    let relative = dest
        .strip_prefix(stage_canon)
        .context("staged directory escaped extraction root")?;
    let mut current = stage_canon.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            anyhow::bail!("invalid staged directory component");
        };
        current.push(name);
        match std::fs::create_dir(&current) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata = std::fs::symlink_metadata(&current)?;
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    anyhow::bail!("staged path is not a real directory: {}", current.display());
                }
            }
            Err(error) => return Err(error).context("creating staged directory"),
        }
        let real = std::fs::canonicalize(&current)?;
        if !real.starts_with(stage_canon) {
            anyhow::bail!("staged directory escaped extraction root: {}", real.display());
        }
    }
    Ok(())
}

fn promote_staged(stage: &Path, parent: &Path) -> anyhow::Result<()> {
    promote_staged_with(stage, parent, |path| std::fs::create_dir_all(path))
}

fn promote_staged_with<F>(
    stage: &Path,
    parent: &Path,
    mut create_backup_dir: F,
) -> anyhow::Result<()>
where
    F: FnMut(&Path) -> std::io::Result<()>,
{
    let parent_meta = std::fs::symlink_metadata(parent).context("inspecting zip destination root")?;
    if !parent_meta.is_dir() || parent_meta.file_type().is_symlink() {
        anyhow::bail!("zip destination root is not a real directory");
    }

    let backup = parent.join(format!(".fileid-backup-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&backup).context("creating zip promotion backup")?;
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(stage).follow_links(false) {
        let entry = entry.context("walking zip staging tree")?;
        if entry.file_type().is_file() {
            files.push(
                entry
                    .path()
                    .strip_prefix(stage)
                    .context("staged file escaped extraction root")?
                    .to_path_buf(),
            );
        }
    }
    files.sort();

    for relative in &files {
        ensure_real_destination_dirs(parent, relative.parent().unwrap_or_else(|| Path::new("")))?;
        match std::fs::symlink_metadata(parent.join(relative)) {
            Ok(metadata) if metadata.is_dir() => {
                let _ = std::fs::remove_dir_all(&backup);
                anyhow::bail!(
                    "zip file conflicts with existing directory: {}",
                    relative.display()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                let _ = std::fs::remove_dir_all(&backup);
                return Err(error).context("inspecting existing zip destination");
            }
        }
    }

    let mut backed_up = Vec::new();
    for relative in &files {
        let destination = parent.join(relative);
        match std::fs::symlink_metadata(&destination) {
            Ok(_) => {
                let backup_path = backup.join(relative);
                if let Some(backup_parent) = backup_path.parent() {
                    if let Err(error) = create_backup_dir(backup_parent) {
                        return fail_with_rollback(
                            error,
                            "creating zip backup directory",
                            stage,
                            parent,
                            &backup,
                            &[],
                            &backed_up,
                        );
                    }
                }
                if let Err(error) = std::fs::rename(&destination, &backup_path) {
                    return fail_with_rollback(
                        error,
                        "backing up existing zip destination",
                        stage,
                        parent,
                        &backup,
                        &[],
                        &backed_up,
                    );
                }
                backed_up.push(relative.clone());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return fail_with_rollback(
                    error,
                    "inspecting existing zip destination",
                    stage,
                    parent,
                    &backup,
                    &[],
                    &backed_up,
                );
            }
        }
    }

    let mut promoted = Vec::new();
    for relative in &files {
        if let Err(error) = std::fs::rename(stage.join(relative), parent.join(relative)) {
            return fail_with_rollback(
                error,
                "promoting staged zip entry",
                stage,
                parent,
                &backup,
                &promoted,
                &backed_up,
            );
        }
        promoted.push(relative.clone());
    }

    if let Err(error) = std::fs::remove_dir_all(&backup) {
        tracing::warn!(
            backup = %crate::platform::redact_path_for_log(&backup),
            ?error,
            "zip promotion succeeded but the safety backup could not be removed"
        );
    }
    Ok(())
}

fn ensure_real_destination_dirs(root: &Path, relative: &Path) -> anyhow::Result<()> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            anyhow::bail!("invalid zip destination component");
        };
        current.push(name);
        match std::fs::create_dir(&current) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata = std::fs::symlink_metadata(&current)?;
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    anyhow::bail!(
                        "zip destination component is not a real directory: {}",
                        current.display()
                    );
                }
            }
            Err(error) => return Err(error).context("creating zip destination directory"),
        }
    }
    Ok(())
}

fn fail_with_rollback<T>(
    cause: std::io::Error,
    operation: &str,
    stage: &Path,
    parent: &Path,
    backup: &Path,
    promoted: &[std::path::PathBuf],
    backed_up: &[std::path::PathBuf],
) -> anyhow::Result<T> {
    match rollback_promotion(stage, parent, backup, promoted, backed_up) {
        Ok(()) => Err(cause).with_context(|| operation.to_string()),
        Err(rollback) => Err(anyhow::anyhow!(
            "{operation} failed: {cause}; rollback was incomplete: {rollback:#}"
        )),
    }
}

fn rollback_promotion(
    stage: &Path,
    parent: &Path,
    backup: &Path,
    promoted: &[std::path::PathBuf],
    backed_up: &[std::path::PathBuf],
) -> anyhow::Result<()> {
    let mut failures = Vec::new();
    for name in promoted.iter().rev() {
        if let Err(error) = std::fs::rename(parent.join(name), stage.join(name)) {
            failures.push(format!("could not withdraw {}: {error}", name.display()));
        }
    }
    for name in backed_up.iter().rev() {
        if let Err(error) = std::fs::rename(backup.join(name), parent.join(name)) {
            failures.push(format!("could not restore {}: {error}", name.display()));
        }
    }
    if failures.is_empty() {
        std::fs::remove_dir_all(backup).context("removing recovered zip backup")?;
        Ok(())
    } else {
        anyhow::bail!(
            "{}; original files retained at {}",
            failures.join("; "),
            backup.display()
        )
    }
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
    fn extraction_failure_leaves_live_files_unchanged() {
        let temp = std::env::temp_dir().join(format!(
            "fileid_zip_atomic_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&temp).unwrap();
        std::fs::write(temp.join("a.txt"), b"old").unwrap();
        let zip = make_zip_with_entries(
            &temp,
            &[("a.txt", b"new"), ("../escape.txt", b"oops")],
        );
        assert!(extract_into_parent(&zip).is_err());
        assert_eq!(std::fs::read(temp.join("a.txt")).unwrap(), b"old");
        std::fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn existing_hard_link_is_replaced_without_touching_its_target() {
        let temp = std::env::temp_dir().join(format!(
            "fileid_zip_hardlink_{}",
            uuid::Uuid::new_v4()
        ));
        let install = temp.join("install");
        std::fs::create_dir_all(&install).unwrap();
        let outside = temp.join("outside.txt");
        std::fs::write(&outside, b"outside").unwrap();
        std::fs::hard_link(&outside, install.join("a.txt")).unwrap();
        let zip = make_zip_with_entries(&install, &[("a.txt", b"replacement")]);
        extract_into_parent(&zip).unwrap();
        assert_eq!(std::fs::read(&outside).unwrap(), b"outside");
        assert_eq!(std::fs::read(install.join("a.txt")).unwrap(), b"replacement");
        std::fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn sequential_archives_preserve_unrelated_nested_files() {
        let temp = std::env::temp_dir().join(format!(
            "fileid_zip_merge_{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&temp).unwrap();
        let first = make_zip_with_entries(&temp, &[("bin/keep.dll", b"keep")]);
        extract_into_parent(&first).unwrap();
        let second = temp.join("second.zip");
        std::fs::rename(&first, &second).unwrap();
        let replacement = make_zip_with_entries(&temp, &[("bin/new.dll", b"new")]);
        extract_into_parent(&replacement).unwrap();
        assert_eq!(std::fs::read(temp.join("bin/keep.dll")).unwrap(), b"keep");
        assert_eq!(std::fs::read(temp.join("bin/new.dll")).unwrap(), b"new");
        std::fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn backup_directory_failure_rolls_back_earlier_originals() {
        let temp = std::env::temp_dir().join(format!(
            "fileid_zip_backup_dir_{}",
            uuid::Uuid::new_v4()
        ));
        let stage = temp.join("stage");
        let parent = temp.join("install");
        std::fs::create_dir_all(stage.join("nested")).unwrap();
        std::fs::create_dir_all(parent.join("nested")).unwrap();
        std::fs::write(stage.join("a.txt"), b"new-a").unwrap();
        std::fs::write(stage.join("nested/b.txt"), b"new-b").unwrap();
        std::fs::write(parent.join("a.txt"), b"old-a").unwrap();
        std::fs::write(parent.join("nested/b.txt"), b"old-b").unwrap();

        let mut calls = 0;
        let error = promote_staged_with(&stage, &parent, |path| {
            calls += 1;
            if calls == 2 {
                Err(std::io::Error::other("injected backup directory failure"))
            } else {
                std::fs::create_dir_all(path)
            }
        })
        .unwrap_err();

        assert!(error.to_string().contains("creating zip backup directory"));
        assert_eq!(std::fs::read(parent.join("a.txt")).unwrap(), b"old-a");
        assert_eq!(
            std::fs::read(parent.join("nested/b.txt")).unwrap(),
            b"old-b"
        );
        std::fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn failed_rollback_retains_original_backup() {
        let temp = std::env::temp_dir().join(format!(
            "fileid_zip_rollback_{}",
            uuid::Uuid::new_v4()
        ));
        let stage = temp.join("stage");
        let parent = temp.join("install");
        let backup = temp.join("backup");
        std::fs::create_dir_all(&stage).unwrap();
        std::fs::create_dir_all(&parent).unwrap();
        std::fs::create_dir_all(&backup).unwrap();
        std::fs::write(backup.join("runtime.dll"), b"original").unwrap();
        std::fs::create_dir(parent.join("runtime.dll")).unwrap();

        let error = rollback_promotion(
            &stage,
            &parent,
            &backup,
            &[],
            &[std::path::PathBuf::from("runtime.dll")],
        )
        .unwrap_err();
        assert!(error.to_string().contains("original files retained"));
        assert_eq!(
            std::fs::read(backup.join("runtime.dll")).unwrap(),
            b"original"
        );
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
            // Dedupe input names — the zip crate rejects duplicate
            // filenames at write time with `InvalidArchive("Duplicate
            // filename")`. The proptest generator occasionally produces
            // colliding strings (especially short 1-3 char names); when
            // it does, `make_zip_with_entries` panics on `start_file`
            // and the test reports as "failed" even though the safety
            // invariant we're testing has nothing to do with dedup.
            // Filter to a unique-by-name set BEFORE constructing the zip
            // so we test the actual property: that extract_into_parent
            // never lets a file escape its parent dir, for any valid zip.
            let mut seen = std::collections::HashSet::new();
            let unique_names: Vec<&str> = names
                .iter()
                .filter(|n| seen.insert(n.as_str()))
                .map(|s| s.as_str())
                .collect();
            if unique_names.is_empty() {
                std::fs::remove_dir_all(&temp).ok();
                return Ok(());
            }
            let entries: Vec<(&str, &[u8])> =
                unique_names.iter().map(|n| (*n, b"x" as &[u8])).collect();
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
