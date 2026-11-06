// Which cataloged paths sit under a location that is offline right now
// (macOS `OfflineRoots`). The most specific location wins, so an online folder
// inside an offline drive's path stays online.

use fileid_engine::ipc::LibraryRoot;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct OfflineRoots {
    entries: Vec<(String, bool)>,
}

impl OfflineRoots {
    pub fn new(roots: &[LibraryRoot]) -> Self {
        if roots.iter().all(|root| root.online) {
            return Self::default();
        }
        let mut entries: Vec<(String, bool)> = roots
            .iter()
            .map(|root| (trimmed(&root.path).to_owned(), root.online))
            .collect();
        entries.sort_by_key(|(path, _)| std::cmp::Reverse(path.len()));
        Self { entries }
    }

    pub fn is_offline(&self, path: &str) -> bool {
        self.entries
            .iter()
            .find(|(root, _)| contains(root, path))
            .is_some_and(|(_, online)| !online)
    }
}

fn trimmed(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() && !path.is_empty() {
        "/"
    } else {
        trimmed
    }
}

fn contains(root: &str, path: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|rest| root == "/" || rest.starts_with('/'))
}

/// The same root listing Settings → Locations reads, off the main loop. An
/// unreadable catalog reads as "nothing offline".
pub(super) fn read() -> async_channel::Receiver<OfflineRoots> {
    crate::engine_client::read_db(|conn| {
        fileid_engine::commands::roots::list(conn)
            .map(|roots| OfflineRoots::new(&roots))
            .unwrap_or_default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(id: i64, path: &str, online: bool) -> LibraryRoot {
        LibraryRoot {
            id,
            path: path.into(),
            display: path.into(),
            kind: "external".into(),
            enabled: true,
            online,
            file_count: 1,
            total_bytes: 1,
            last_scan_at: None,
            last_status: None,
        }
    }

    #[test]
    fn files_under_an_offline_root_are_offline_and_neighbours_are_not() {
        let roots = OfflineRoots::new(&[
            root(1, "/media/ada/Camera", false),
            root(2, "/home/ada/Pictures", true),
        ]);
        assert!(roots.is_offline("/media/ada/Camera/2024/a.jpg"));
        assert!(roots.is_offline("/media/ada/Camera"));
        assert!(!roots.is_offline("/media/ada/Camera2/a.jpg"));
        assert!(!roots.is_offline("/media/ada/Cameras/a.jpg"));
        assert!(!roots.is_offline("/home/ada/Pictures/b.jpg"));
        assert!(!roots.is_offline("/elsewhere/c.jpg"));
    }

    #[test]
    fn the_most_specific_location_decides() {
        let roots = OfflineRoots::new(&[
            root(1, "/mnt/nas", false),
            root(2, "/mnt/nas/Synced", true),
        ]);
        assert!(roots.is_offline("/mnt/nas/Old/a.jpg"));
        assert!(!roots.is_offline("/mnt/nas/Synced/a.jpg"));
        let flipped = OfflineRoots::new(&[
            root(1, "/mnt/nas", true),
            root(2, "/mnt/nas/Gone/", false),
        ]);
        assert!(flipped.is_offline("/mnt/nas/Gone/a.jpg"));
        assert!(!flipped.is_offline("/mnt/nas/Here/a.jpg"));
    }

    #[test]
    fn all_online_means_nothing_to_check() {
        let roots = OfflineRoots::new(&[root(1, "/media/ada/Camera", true)]);
        assert_eq!(roots, OfflineRoots::default());
        assert!(!roots.is_offline("/media/ada/Camera/a.jpg"));
        assert_eq!(OfflineRoots::new(&[]), OfflineRoots::default());
    }

    #[test]
    fn a_filesystem_root_covers_everything_not_claimed_by_a_deeper_root() {
        let roots = OfflineRoots::new(&[root(1, "/", false), root(2, "/home/ada", true)]);
        assert!(roots.is_offline("/media/x.jpg"));
        assert!(!roots.is_offline("/home/ada/x.jpg"));
    }
}
