//! Folder access the Mac app hands over as security-scoped bookmarks
//! (`grantFolderAccess` and `startScan.rootBookmark`), held the way the Swift
//! engine's `SecurityScopedAccessRegistry` holds it: the granted root until the
//! next grant, a scan's root for the scan. A bookmark that doesn't resolve to
//! the path it names is refused with Swift's `folder_access_denied`. Builds
//! the Mac app doesn't host have no bookmarks and accept every path, as before.

use crate::ipc::{self, sink::Sink};
#[cfg(all(target_os = "macos", feature = "apple-adapters"))]
use crate::ipc::EngineError;

#[cfg(all(target_os = "macos", feature = "apple-adapters"))]
pub(crate) const GRANT_DENIED: &str = "FileID couldn't restore access to the selected folder. Choose it again.";
pub(crate) const SCAN_DENIED: &str = "FileID couldn't access that folder. Choose it again.";

#[cfg(all(target_os = "macos", feature = "apple-adapters"))]
mod mac {
    use std::path::{Component, Path, PathBuf};

    use base64::Engine as _;
    use parking_lot::Mutex;

    use crate::apple::BookmarkLease;

    static ROOT: Mutex<Option<BookmarkLease>> = Mutex::new(None);

    pub(super) fn open(path: &str, bookmark: &str) -> Option<BookmarkLease> {
        let bytes = base64::engine::general_purpose::STANDARD.decode(bookmark.trim()).ok()?;
        let lease = BookmarkLease::open(&bytes)?;
        (standardized(lease.path()) == standardized(Path::new(path))).then_some(lease)
    }

    pub(super) fn replace_root(lease: BookmarkLease) {
        let previous = ROOT.lock().replace(lease);
        drop(previous);
    }

    /// `URL.standardizedFileURL.path`: `.` and `..` resolved lexically, no trailing slash.
    pub(super) fn standardized(path: &Path) -> PathBuf {
        let mut out = PathBuf::new();
        for part in path.components() {
            match part {
                Component::CurDir => {}
                Component::ParentDir => {
                    out.pop();
                }
                other => out.push(other.as_os_str()),
            }
        }
        out
    }
}

/// `grantFolderAccess`: keep access to the app's chosen folder until the next grant.
#[cfg(all(target_os = "macos", feature = "apple-adapters"))]
pub(crate) async fn grant(sink: &Sink, payload: ipc::GrantFolderAccessPayload) {
    match mac::open(&payload.root_path, &payload.root_bookmark) {
        Some(lease) => mac::replace_root(lease),
        None => sink.send(EngineError::event("folder_access_denied", GRANT_DENIED)).await,
    }
}

/// Other builds have no bookmarks; async to share the Mac signature the dispatcher awaits.
#[cfg(not(all(target_os = "macos", feature = "apple-adapters")))]
#[allow(clippy::unused_async)]
pub(crate) async fn grant(_sink: &Sink, _payload: ipc::GrantFolderAccessPayload) {}

/// Access to a scan root, held until dropped.
pub(crate) struct ScanAccess {
    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    _lease: Option<crate::apple::BookmarkLease>,
}

/// The scan root's access; `None` when its bookmark doesn't grant `root_path`.
/// No bookmark is fine outside the sandbox, as in the Swift engine's Developer ID build.
#[cfg(all(target_os = "macos", feature = "apple-adapters"))]
pub(crate) fn scan_access(root_path: &str, bookmark: Option<&str>) -> Option<ScanAccess> {
    match bookmark {
        None => Some(ScanAccess { _lease: None }),
        Some(bookmark) => mac::open(root_path, bookmark).map(|lease| ScanAccess { _lease: Some(lease) }),
    }
}

#[cfg(not(all(target_os = "macos", feature = "apple-adapters")))]
pub(crate) fn scan_access(_root_path: &str, _bookmark: Option<&str>) -> Option<ScanAccess> {
    Some(ScanAccess {})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scan_without_a_bookmark_is_allowed() {
        assert!(scan_access("/tmp", None).is_some());
    }

    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    #[test]
    fn a_bookmark_that_does_not_resolve_is_refused() {
        use base64::Engine as _;
        let garbage = base64::engine::general_purpose::STANDARD.encode(b"not a bookmark");
        assert!(scan_access("/tmp", Some(&garbage)).is_none());
        assert!(scan_access("/tmp", Some("%%% not base64")).is_none());
    }

    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    #[test]
    fn paths_compare_as_foundation_standardizes_them() {
        use std::path::Path;
        assert_eq!(mac::standardized(Path::new("/a/./b/../c/")), Path::new("/a/c"));
        assert_eq!(mac::standardized(Path::new("/a/c")), mac::standardized(Path::new("/a/c/")));
    }
}
