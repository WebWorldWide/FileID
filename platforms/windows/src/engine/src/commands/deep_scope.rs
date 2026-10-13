use std::collections::HashSet;
use std::fmt;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScopeError {
    TooManyFiles,
    InvalidFileId,
    TooManyFolders,
    InvalidFolder,
    UnavailableFolder,
    InvalidFilePath,
    UnresolvedFile,
    ExcludedTarget,
    Cancelled,
    #[cfg(windows)]
    PathComparison,
}

impl fmt::Display for ScopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TooManyFiles => "Select at most 100 files for Deep Analyze.",
            Self::InvalidFileId => "Selected file IDs must be positive.",
            Self::TooManyFolders => "Configure at most 100 Deep Analyze exclusion folders.",
            Self::InvalidFolder => "Exclusions must be absolute directory paths without parent traversal or NUL characters.",
            Self::UnavailableFolder => "An exclusion folder could not be resolved as an accessible directory. Restore access before whole-library analysis.",
            Self::InvalidFilePath => "A catalog target does not have a valid absolute path.",
            Self::UnresolvedFile => "A catalog target could not be resolved safely against the exclusions. Restore access before whole-library analysis.",
            Self::ExcludedTarget => "A target is now excluded. Analysis stopped before opening it.",
            Self::Cancelled => "Deep scope planning cancelled.",
            #[cfg(windows)]
            Self::PathComparison => "A target path could not be compared safely against the exclusions.",
        })
    }
}

impl std::error::Error for ScopeError {}

#[derive(Debug)]
struct ExcludedFolder {
    original: PathBuf,
    resolved: PathBuf,
}

#[derive(Debug)]
pub(crate) struct DeepScope {
    selected: Option<Vec<i64>>,
    excluded: Vec<ExcludedFolder>,
}

impl DeepScope {
    #[cfg(test)]
    pub(crate) fn from_fields(
        file_ids: Option<&[i64]>,
        excluded_folders: Option<&[String]>,
    ) -> Result<Self, ScopeError> {
        Self::from_fields_cancelable(file_ids, excluded_folders, &|| false)
    }

    pub(crate) fn from_fields_cancelable(
        file_ids: Option<&[i64]>,
        excluded_folders: Option<&[String]>,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Self, ScopeError> {
        if cancelled() { return Err(ScopeError::Cancelled); }
        let folders = excluded_folders.unwrap_or_default();
        if folders.len() > 100 {
            return Err(ScopeError::TooManyFolders);
        }
        if folders
            .iter()
            .any(|raw| raw.is_empty() || raw.chars().count() > 32_768)
        {
            return Err(ScopeError::InvalidFolder);
        }
        if let Some(ids) = file_ids {
            if ids.len() > 100 {
                return Err(ScopeError::TooManyFiles);
            }
            if ids.iter().any(|id| *id <= 0) {
                return Err(ScopeError::InvalidFileId);
            }
            let mut seen = HashSet::new();
            return Ok(Self {
                selected: Some(ids.iter().copied().filter(|id| seen.insert(*id)).collect()),
                excluded: Vec::new(),
            });
        }
        let mut excluded = Vec::with_capacity(folders.len());
        for raw in folders {
            if cancelled() { return Err(ScopeError::Cancelled); }
            let original = Path::new(raw);
            if raw.contains('\0')
                || !original.is_absolute()
                || original
                    .components()
                    .any(|c| matches!(c, Component::ParentDir))
            {
                return Err(ScopeError::InvalidFolder);
            }
            let resolved = original
                .canonicalize()
                .map_err(|_| ScopeError::UnavailableFolder)?;
            if !resolved.is_dir() {
                return Err(ScopeError::UnavailableFolder);
            }
            excluded.push(ExcludedFolder {
                original: original.to_path_buf(),
                resolved,
            });
        }
        if cancelled() { return Err(ScopeError::Cancelled); }
        Ok(Self {
            selected: None,
            excluded,
        })
    }

    pub(crate) fn selected_ids(&self) -> Option<&[i64]> {
        self.selected.as_deref()
    }

    pub(crate) fn validate_current_roots(&self) -> Result<(), ScopeError> {
        for folder in &self.excluded {
            let current = folder.original.canonicalize().map_err(|_| ScopeError::UnavailableFolder)?;
            if !current.is_dir()
                || !contains_path(&folder.resolved, &current)?
                || !contains_path(&current, &folder.resolved)?
            {
                return Err(ScopeError::UnavailableFolder);
            }
        }
        Ok(())
    }

    pub(crate) fn allows_file(&self, id: i64, path: &Path) -> Result<bool, ScopeError> {
        Ok(self.source_path(id, path)?.is_some())
    }

    pub(crate) fn source_path(&self, id: i64, path: &Path) -> Result<Option<PathBuf>, ScopeError> {
        if let Some(ids) = &self.selected {
            return Ok(ids.contains(&id).then(|| path.to_path_buf()));
        }
        self.validate_current_roots()?;
        if self.excluded.is_empty() {
            return Ok(Some(path.to_path_buf()));
        }
        if !path.is_absolute() || path.as_os_str().as_encoded_bytes().contains(&0) {
            return Err(ScopeError::InvalidFilePath);
        }
        if !path.components().any(|c| matches!(c, Component::ParentDir)) {
            for folder in &self.excluded {
                if contains_path(&folder.original, path)? || contains_path(&folder.resolved, path)?
                {
                    return Ok(None);
                }
            }
        }
        let resolved = path
            .canonicalize()
            .map_err(|_| ScopeError::UnresolvedFile)?;
        self.validate_current_roots()?;
        for folder in &self.excluded {
            if contains_path(&folder.resolved, &resolved)? {
                return Ok(None);
            }
        }
        Ok(Some(resolved))
    }
}

#[cfg(not(windows))]
fn contains_path(root: &Path, target: &Path) -> Result<bool, ScopeError> {
    Ok(target.starts_with(root))
}

#[cfg(windows)]
fn contains_path(root: &Path, target: &Path) -> Result<bool, ScopeError> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn CompareStringOrdinal(
            a: *const u16,
            a_len: i32,
            b: *const u16,
            b_len: i32,
            ignore_case: i32,
        ) -> i32;
    }
    fn normalized(path: &Path) -> Vec<u16> {
        let mut wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .map(|c| if c == 47 { 92 } else { c })
            .collect();
        let unc: Vec<u16> = r"\\?\UNC\".encode_utf16().collect();
        let verbatim: Vec<u16> = r"\\?\".encode_utf16().collect();
        if wide.starts_with(&unc) {
            wide.splice(..unc.len(), [92, 92]);
        } else if wide.starts_with(&verbatim) {
            wide.drain(..verbatim.len());
        }
        while wide.last() == Some(&92) {
            wide.pop();
        }
        wide
    }
    let root = normalized(root);
    let target = normalized(target);
    if root.is_empty() {
        return Err(ScopeError::PathComparison);
    }
    if target.len() < root.len() || (target.len() > root.len() && target[root.len()] != 92) {
        return Ok(false);
    }
    let length = i32::try_from(root.len()).map_err(|_| ScopeError::PathComparison)?;
    // Kernel ordinal folding avoids disagreement between Rust Unicode casing and Windows path casing.
    let comparison =
        unsafe { CompareStringOrdinal(root.as_ptr(), length, target.as_ptr(), length, 1) };
    match comparison {
        0 => Err(ScopeError::PathComparison),
        2 => Ok(true),
        _ => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "fileid-deep-scope-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn folder(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::create_dir(&path).unwrap();
            path
        }
        fn file(&self, directory: &Path, name: &str) -> PathBuf {
            assert!(directory.starts_with(&self.0));
            let path = directory.join(name);
            std::fs::write(&path, b"generated scope fixture").unwrap();
            path
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn exclusions(path: &Path) -> Vec<String> {
        vec![path.to_str().unwrap().to_owned()]
    }

    #[test]
    fn explicit_selection_deduplicates_in_caller_order_and_ignores_exclusions() {
        let scope =
            DeepScope::from_fields(Some(&[4, 2, 4, 7]), Some(&["unavailable/relative".into()]))
                .unwrap();
        assert_eq!(scope.selected_ids(), Some([4, 2, 7].as_slice()));
        assert!(scope.allows_file(2, Path::new("missing")).unwrap());
        assert!(!scope.allows_file(3, Path::new("missing")).unwrap());
    }

    #[test]
    fn empty_selection_is_never_whole_library() {
        let scope = DeepScope::from_fields(Some(&[]), None).unwrap();
        assert_eq!(scope.selected_ids(), Some([].as_slice()));
        assert!(!scope.allows_file(1, Path::new("missing")).unwrap());
        let whole = DeepScope::from_fields(None, None).unwrap();
        assert_eq!(whole.selected_ids(), None);
        assert!(whole.allows_file(1, Path::new("missing")).unwrap());
        assert!(DeepScope::from_fields(None, Some(&[]))
            .unwrap()
            .allows_file(1, Path::new("missing"))
            .unwrap());
    }

    #[test]
    fn selections_are_bounded_and_positive() {
        assert_eq!(
            DeepScope::from_fields(Some(&[1; 101]), None).unwrap_err(),
            ScopeError::TooManyFiles
        );
        for id in [0, -1, i64::MIN] {
            assert_eq!(
                DeepScope::from_fields(Some(&[id]), None).unwrap_err(),
                ScopeError::InvalidFileId
            );
        }
        assert!(DeepScope::from_fields(Some(&[i64::MAX]), None).is_ok());
        assert_eq!(
            DeepScope::from_fields(Some(&[1]), Some(&vec!["relative".into(); 101])).unwrap_err(),
            ScopeError::TooManyFolders
        );
        assert_eq!(
            DeepScope::from_fields(Some(&[1]), Some(&[String::new()])).unwrap_err(),
            ScopeError::InvalidFolder
        );
    }

    #[test]
    fn whole_library_rejects_malformed_or_unavailable_exclusions() {
        let fixture = Fixture::new();
        for folder in [
            String::new(),
            "relative".to_owned(),
            "file:///folder".to_owned(),
            format!("{}\0bad", fixture.0.display()),
            "x".repeat(32_769),
        ] {
            assert_eq!(
                DeepScope::from_fields(None, Some(&[folder])).unwrap_err(),
                ScopeError::InvalidFolder
            );
        }
        assert_eq!(
            DeepScope::from_fields(None, Some(&vec!["relative".into(); 101])).unwrap_err(),
            ScopeError::TooManyFolders
        );
        let parent = fixture.0.join("..");
        assert_eq!(
            DeepScope::from_fields(None, Some(&exclusions(&parent))).unwrap_err(),
            ScopeError::InvalidFolder
        );
        let missing = fixture.0.join("missing");
        assert_eq!(
            DeepScope::from_fields(None, Some(&exclusions(&missing))).unwrap_err(),
            ScopeError::UnavailableFolder
        );
        let file = fixture.file(&fixture.0, "not-a-directory");
        assert_eq!(
            DeepScope::from_fields(None, Some(&exclusions(&file))).unwrap_err(),
            ScopeError::UnavailableFolder
        );
    }

    #[test]
    fn folder_boundaries_are_literal_and_do_not_exclude_siblings() {
        let fixture = Fixture::new();
        let folder = fixture.folder("Photos%_雪");
        let sibling = fixture.folder("Photos%_雪Backup");
        let inside = fixture.file(&folder, "inside.png");
        let outside = fixture.file(&sibling, "outside.png");
        let scope = DeepScope::from_fields(None, Some(&exclusions(&folder))).unwrap();
        assert!(!scope.allows_file(1, &inside).unwrap());
        assert!(!scope.allows_file(1, &folder).unwrap());
        assert!(scope.allows_file(2, &outside).unwrap());
        assert!(!scope.allows_file(3, &folder.join("offline.png")).unwrap());
        assert_eq!(
            scope
                .allows_file(4, &sibling.join("offline.png"))
                .unwrap_err(),
            ScopeError::UnresolvedFile
        );
        assert_eq!(
            scope.allows_file(5, Path::new("relative.png")).unwrap_err(),
            ScopeError::InvalidFilePath
        );
        let trailing = format!("{}{}", folder.display(), std::path::MAIN_SEPARATOR);
        assert!(!DeepScope::from_fields(None, Some(&[trailing]))
            .unwrap()
            .allows_file(1, &inside)
            .unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn resolved_targets_and_aliases_cannot_bypass_exclusions() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let protected = fixture.folder("protected");
        let outside = fixture.folder("outside");
        let source = fixture.file(&protected, "source.png");
        let alias = fixture.0.join("alias");
        symlink(&protected, &alias).unwrap();
        let file_alias = fixture.0.join("file-alias.png");
        symlink(&source, &file_alias).unwrap();
        let scope = DeepScope::from_fields(None, Some(&exclusions(&protected))).unwrap();
        assert!(!scope.allows_file(1, &alias.join("source.png")).unwrap());
        assert!(!scope.allows_file(2, &file_alias).unwrap());
        let alias_scope = DeepScope::from_fields(None, Some(&exclusions(&alias))).unwrap();
        assert!(!alias_scope.allows_file(3, &source).unwrap());
        let moved_alias = fixture.0.join("changed-alias");
        symlink(&outside, &moved_alias).unwrap();
        let outside_source = fixture.file(&outside, "source.png");
        assert!(scope
            .allows_file(4, &moved_alias.join("source.png"))
            .unwrap());
        std::fs::remove_file(&moved_alias).unwrap();
        symlink(&protected, &moved_alias).unwrap();
        assert!(!scope
            .allows_file(4, &moved_alias.join("source.png"))
            .unwrap());
        assert!(scope.allows_file(5, &outside_source).unwrap());
    }

    #[cfg(windows)]
    #[test]
    fn windows_path_boundaries_use_kernel_case_comparison_and_verbatim_normalization() {
        assert!(
            contains_path(Path::new(r"C:\PhotosÉ"), Path::new(r"c:/photosé/file.png")).unwrap()
        );
        assert!(!contains_path(
            Path::new(r"C:\Photos"),
            Path::new(r"C:\PhotosBackup\file.png")
        )
        .unwrap());
        assert!(contains_path(Path::new(r"C:\"), Path::new(r"\\?\C:\folder\file.png")).unwrap());
        assert!(contains_path(
            Path::new(r"\\server\share\Photos"),
            Path::new(r"\\?\UNC\SERVER\share\photos\file.png")
        )
        .unwrap());
        assert!(!contains_path(
            Path::new(r"\\server\share"),
            Path::new(r"\\server\shareBackup\file.png")
        )
        .unwrap());
    }
}
