// macOS Trash and Finder tags for the engine the Mac app hosts (`fileidd`,
// `--features apple-adapters`). FileManager.trashItem and Finder tag names run
// in the Swift FileIDAppleKit library (`crate::apple`); identity checks, the
// write-ahead journal and inode recovery stay here, as in the Swift engine's
// `trashFiles` and `TrashJournal`.
//
// Test-only Trash root: builds with the `test-trash-root` feature (never
// shipped; `assemble_app.sh` refuses such a binary), and unit tests, move
// items into `$FILEID_TEST_TRASH_ROOT` instead of the user's Trash, and only
// when the catalog is an explicit scratch one (`FILEID_DATABASE_PATH` or
// `FILEID_DB`). Without that they refuse: a test build never reaches the
// real Trash.

pub mod trash {
    use anyhow::{Context, Result};
    use std::path::{Path, PathBuf};

    use crate::platform::FileIdentity;

    enum Backend {
        /// FileManager.trashItem through FileIDAppleKit.
        #[cfg_attr(any(test, feature = "test-trash-root"), allow(dead_code))]
        Finder,
        /// A plain folder standing in for the Trash (test builds only).
        #[cfg_attr(not(any(test, feature = "test-trash-root")), allow(dead_code))]
        Folder(PathBuf),
    }

    #[cfg(not(any(test, feature = "test-trash-root")))]
    fn backend() -> Result<Backend> {
        Ok(Backend::Finder)
    }

    #[cfg(any(test, feature = "test-trash-root"))]
    fn backend() -> Result<Backend> {
        let root = std::env::var_os("FILEID_TEST_TRASH_ROOT")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .context("this test build moves files only into FILEID_TEST_TRASH_ROOT, which is not set")?;
        let scratch_catalog = ["FILEID_DATABASE_PATH", "FILEID_DB"]
            .iter()
            .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()));
        anyhow::ensure!(scratch_catalog, "the test Trash root needs a scratch catalog (FILEID_DATABASE_PATH)");
        anyhow::ensure!(root.is_absolute(), "FILEID_TEST_TRASH_ROOT must be absolute");
        std::fs::create_dir_all(&root).context("create the test Trash root")?;
        Ok(Backend::Folder(root))
    }

    /// Folders an item trashed from `original` can be in: the user's Trash and,
    /// for another volume, its `.Trashes/<uid>` (as `TrashJournal.trashFolders`).
    pub(crate) fn trash_folders(original: &Path) -> Vec<PathBuf> {
        match backend() {
            Ok(Backend::Folder(root)) => vec![root],
            Ok(Backend::Finder) => {
                let mut folders = Vec::new();
                if let Some(home) = user_home() {
                    folders.push(home.join(".Trash"));
                }
                if let Some(volume) = original.parent().and_then(mount_point)
                    && volume != Path::new("/")
                {
                    // SAFETY: getuid has no preconditions.
                    folders.push(volume.join(".Trashes").join(unsafe { libc::getuid() }.to_string()));
                }
                folders
            }
            Err(_) => Vec::new(),
        }
    }

    /// The real home folder: inside the App Sandbox `HOME` is the container.
    fn user_home() -> Option<PathBuf> {
        if let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty())
            && !home.to_string_lossy().contains("/Library/Containers/")
        {
            return Some(PathBuf::from(home));
        }
        let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
        let mut buffer = vec![0 as libc::c_char; 4096];
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: every pointer refers to a live local for the duration of the call.
        let status = unsafe {
            libc::getpwuid_r(libc::getuid(), &raw mut entry, buffer.as_mut_ptr(), buffer.len(), &raw mut result)
        };
        if status != 0 || result.is_null() || entry.pw_dir.is_null() {
            return None;
        }
        // SAFETY: getpwuid_r filled `pw_dir` with a NUL-terminated string inside `buffer`.
        let dir = unsafe { std::ffi::CStr::from_ptr(entry.pw_dir) };
        Some(PathBuf::from(std::ffi::OsStr::new(&*dir.to_string_lossy())))
    }

    fn mount_point(path: &Path) -> Option<PathBuf> {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
        let mut stats: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: `path` is NUL-terminated and `stats` is a live local.
        if unsafe { libc::statfs(path.as_ptr(), &raw mut stats) } != 0 {
            return None;
        }
        // SAFETY: the kernel NUL-terminates f_mntonname.
        let name = unsafe { std::ffi::CStr::from_ptr(stats.f_mntonname.as_ptr()) };
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(name.to_bytes())))
    }

    /// Batch wrapper for the CLI's `dedupe`: one bool per input, true = trashed.
    pub fn trash(paths: &[PathBuf]) -> Vec<bool> {
        paths
            .iter()
            .map(|path| {
                crate::platform::file_identity(path)
                    .is_some_and(|identity| trash_in_place(path, identity).is_ok())
            })
            .collect()
    }

    /// Moves `path` to the Trash if it is still the file with `expected`
    /// identity, and returns where it landed. An item that turns out to be
    /// another file is moved back and the call fails.
    pub(crate) fn trash_in_place(path: &Path, expected: FileIdentity) -> Result<PathBuf> {
        crate::util::read_only::require_source_mutation(path)?;
        anyhow::ensure!(
            crate::platform::file_identity(path) == Some(expected),
            "the file changed since it was checked; nothing was moved"
        );
        let landed = match backend()? {
            Backend::Finder => crate::apple::trash_item(path)?,
            Backend::Folder(root) => Some(move_into_folder(path, &root)?),
        };
        let landed = match landed {
            Some(landed) => landed,
            None => locate_inode(&trash_folders(path), path, expected.file)
                .context("the Trash did not say where it put the file")?,
        };
        if crate::platform::file_identity(&landed) != Some(expected) {
            let put_back = crate::util::rename_no_replace(&landed, path);
            anyhow::bail!(
                "another file was at {} when it moved to the Trash; {}",
                path.display(),
                if put_back.is_ok() { "it was put back" } else { "it is still in the Trash" }
            );
        }
        Ok(landed)
    }

    /// Finder's naming for a taken Trash name: "photo.jpg", then "photo 2.jpg".
    fn move_into_folder(path: &Path, root: &Path) -> Result<PathBuf> {
        let name = path.file_name().context("Trash source has no file name")?;
        let stem = Path::new(name).file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
        let extension = Path::new(name).extension().map(|ext| format!(".{}", ext.to_string_lossy()));
        for attempt in 1..10_000u32 {
            let candidate = if attempt == 1 {
                root.join(name)
            } else {
                root.join(format!("{stem} {attempt}{}", extension.as_deref().unwrap_or("")))
            };
            match crate::util::rename_no_replace(path, &candidate) {
                Ok(()) => return Ok(candidate),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error).context("move into the test Trash root"),
            }
        }
        anyhow::bail!("no free name in the test Trash root for {}", path.display())
    }

    /// The Trash entry with `inode`, looking only at names that start with the
    /// original's stem (Finder keeps the name or appends to it).
    fn locate_inode(folders: &[PathBuf], original: &Path, inode: u64) -> Option<PathBuf> {
        let stem = original.file_stem()?.to_string_lossy().into_owned();
        folders.iter().find_map(|folder| {
            std::fs::read_dir(folder).ok()?.flatten().find_map(|entry| {
                let path = entry.path();
                (entry.file_name().to_string_lossy().starts_with(&stem)
                    && crate::platform::file_ref(&path) == Some(inode))
                .then_some(path)
            })
        })
    }

    /// Where a journaled item is in the Trash now: its recorded location while
    /// it still holds the same file, else the entry with its inode.
    pub(crate) fn locate(item: &crate::commands::trash_log::TrashLogItem) -> Option<PathBuf> {
        let inode = item.expected_inode()?;
        let original = Path::new(&item.original_path);
        let matches = |path: &Path| match item.source_identity {
            Some(identity) => crate::platform::file_identity(path) == Some(identity),
            None => crate::platform::file_ref(path) == Some(inode),
        };
        if let Some(recorded) = item.trashed_path.as_deref().map(Path::new)
            && matches(recorded)
        {
            return Some(recorded.to_path_buf());
        }
        locate_inode(&trash_folders(original), original, inode).filter(|path| matches(path))
    }

    /// Moves a journaled item from the Trash back to `original`, never
    /// replacing what is there. The caller has checked `original` is inside an
    /// authorized library root. Returns the restored file's identity.
    pub(crate) fn restore(item: &crate::commands::trash_log::TrashLogItem, original: &Path) -> Result<FileIdentity> {
        crate::util::read_only::require_source_mutation(original)?;
        let trashed = locate(item).context("the file is no longer in the Trash")?;
        let identity = crate::platform::file_identity(&trashed).context("the Trash item is unreadable")?;
        if let Some(parent) = original.parent() {
            std::fs::create_dir_all(parent).context("recreate the original folder")?;
        }
        crate::util::rename_no_replace(&trashed, original).context("move the file back from the Trash")?;
        anyhow::ensure!(
            crate::platform::file_identity(original) == Some(identity),
            "the restored file changed while it moved back"
        );
        Ok(identity)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn scratch(tag: &str) -> PathBuf {
            let dir = std::env::temp_dir().join(format!("fileid-mac-trash-{tag}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        #[test]
        fn unit_tests_never_reach_the_real_trash() {
            // No test sets FILEID_TEST_TRASH_ROOT: every Trash call refuses.
            let dir = scratch("refuse");
            let file = dir.join("keep.txt");
            std::fs::write(&file, b"keep").unwrap();
            let identity = crate::platform::file_identity(&file).unwrap();
            assert!(trash_in_place(&file, identity).is_err());
            assert_eq!(trash(std::slice::from_ref(&file)), vec![false]);
            assert!(trash_folders(&file).is_empty());
            assert_eq!(std::fs::read(&file).unwrap(), b"keep");
            std::fs::remove_dir_all(dir).ok();
        }

        #[test]
        fn folder_trash_uses_finder_names_and_restores_by_inode() {
            let dir = scratch("folder");
            let root = dir.join("Trash");
            std::fs::create_dir_all(&root).unwrap();
            let first = dir.join("a").join("photo.jpg");
            let second = dir.join("b").join("photo.jpg");
            for (path, bytes) in [(&first, b"one"), (&second, b"two")] {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, bytes).unwrap();
            }
            let landed_first = move_into_folder(&first, &root).unwrap();
            let landed_second = move_into_folder(&second, &root).unwrap();
            assert_eq!(landed_first, root.join("photo.jpg"));
            assert_eq!(landed_second, root.join("photo 2.jpg"));

            let inode = crate::platform::file_ref(&landed_second).unwrap();
            assert_eq!(locate_inode(&[root.clone()], &second, inode), Some(landed_second.clone()));
            assert_eq!(locate_inode(&[root.clone()], &dir.join("other.jpg"), inode), None);
            std::fs::remove_dir_all(dir).ok();
        }

        #[test]
        fn mount_point_of_a_temporary_folder_is_a_volume_root() {
            let dir = scratch("mount");
            let mount = mount_point(&dir).unwrap();
            assert!(dir.starts_with(&mount) || dir.canonicalize().unwrap().starts_with(&mount));
            std::fs::remove_dir_all(dir).ok();
        }
    }
}

pub mod tags {
    use anyhow::Result;
    use std::path::Path;

    /// Adds `tags` to the file's Finder tags and never removes one: Finder tags
    /// are the user's, and the app's own tag undo removes only what it added
    /// (`TagWriter`). Case-insensitive; existing spelling wins.
    pub fn write_tags(path: &Path, tags: &[String]) -> Result<()> {
        crate::util::read_only::require_source_mutation(path)?;
        let existing = crate::apple::finder_tags(path)?;
        let merged = merge_tags(&existing, tags);
        if merged != existing {
            crate::apple::set_finder_tags(path, &merged)?;
        }
        Ok(())
    }

    pub fn read_tags(path: &Path) -> Result<Vec<String>> {
        crate::apple::finder_tags(path)
    }

    /// Finder tags are extended attributes; they move with the file.
    pub fn move_sidecar(_old: &Path, _new: &Path) {}

    /// `TagWriter.mergeTags`: keep `existing` in order, append new trimmed tags.
    pub(crate) fn merge_tags(existing: &[String], adding: &[String]) -> Vec<String> {
        let mut out = existing.to_vec();
        let mut seen: std::collections::HashSet<String> = existing.iter().map(|tag| tag.to_lowercase()).collect();
        for tag in adding {
            let trimmed = tag.trim();
            if !trimmed.is_empty() && seen.insert(trimmed.to_lowercase()) {
                out.push(trimmed.to_owned());
            }
        }
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn merge_keeps_existing_spelling_and_order() {
            let existing = vec!["Mom".to_string(), "Important".to_string()];
            let merged = merge_tags(&existing, &["mom".into(), " Beach ".into(), "".into(), "beach".into()]);
            assert_eq!(merged, vec!["Mom", "Important", "Beach"]);
        }

        #[test]
        fn finder_tags_round_trip_and_keep_the_users_own() {
            let dir = std::env::temp_dir().join(format!("fileid-finder-tags-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let file = dir.join("photo.jpg");
            std::fs::write(&file, b"x").unwrap();
            crate::apple::set_finder_tags(&file, &["Important".to_string()]).unwrap();
            write_tags(&file, &["Beach".into(), "important".into()]).unwrap();
            assert_eq!(read_tags(&file).unwrap(), vec!["Important", "Beach"]);
            crate::apple::set_finder_tags(&file, &[]).unwrap();
            assert!(read_tags(&file).unwrap().is_empty());
            std::fs::remove_dir_all(dir).ok();
        }
    }
}
