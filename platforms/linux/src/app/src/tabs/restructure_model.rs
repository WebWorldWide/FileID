// Restructure page model: outcomes (keep / tidy / reorganize), buckets, the
// three step cells and the library reads behind the metric row, CLIP banner
// and Staying put list. Pure where it can be, so it is tested headless.

use fileid_engine::ipc::RestructureMove;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Outcome {
    Tidy,
    Reorganize,
}

/// macOS `kind(forTier:)`: Mixed folders contribute misplaced files (Tidy);
/// Junk, unknown or missing tiers dissolve (Reorganize).
pub fn outcome(movement: &RestructureMove) -> Outcome {
    match movement.tier.as_deref() {
        Some(tier) if tier.eq_ignore_ascii_case("mixed") => Outcome::Tidy,
        _ => Outcome::Reorganize,
    }
}

pub fn default_selected(confidence: &str) -> bool {
    confidence.eq_ignore_ascii_case("auto")
}

/// Destination folder relative to the library root ("Photos/2024").
pub fn bucket(movement: &RestructureMove, root: &str) -> String {
    let destination = std::path::Path::new(&movement.destination);
    let folder = destination.parent().unwrap_or(destination);
    let relative = folder
        .strip_prefix(root.trim_end_matches('/'))
        .unwrap_or(folder);
    let label = relative
        .to_string_lossy()
        .trim_start_matches('/')
        .to_owned();
    if label.is_empty() {
        movement.category.clone()
    } else {
        label
    }
}

pub fn source_folder(movement: &RestructureMove) -> String {
    std::path::Path::new(&movement.source)
        .parent()
        .and_then(|parent| parent.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepState {
    Upcoming,
    Current,
    Done,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub planning: bool,
    pub has_plan: bool,
    pub selected: usize,
    pub moving: bool,
    pub moved: bool,
}

/// Preview · Review · Move cell states from DESIGN_V3_PORT "Restructure".
pub fn steps(p: Progress) -> [StepState; 3] {
    use StepState::*;
    let preview = if p.planning || (!p.has_plan && !p.moved) {
        Current
    } else {
        Done
    };
    let review = if p.moved || (p.has_plan && p.selected > 0) {
        Done
    } else if p.has_plan {
        Current
    } else {
        Upcoming
    };
    let mv = if p.moved && !p.moving {
        Done
    } else if p.moving || (p.has_plan && p.selected > 0) {
        Current
    } else {
        Upcoming
    };
    [preview, review, mv]
}

pub fn plural(n: usize, noun: &str) -> String {
    format!(
        "{} {noun}{}",
        crate::shell::grouped(n as u64),
        if n == 1 { "" } else { "s" }
    )
}

pub fn zero_move_status(anchors: u32) -> String {
    match anchors {
        0 => "No moves were proposed for this folder.".to_owned(),
        n => format!(
            "Everything here is already organized. {n} folder{} kept.",
            if n == 1 { "" } else { "s" }
        ),
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Library {
    pub indexed: u64,
    pub images_without_clip: u64,
    /// (folder relative to the root, files) for folders no move touches.
    pub staying: Vec<(String, u64)>,
}

/// Indexed files under `root`, photos without CLIP, and the folders that keep
/// all of their files (no source of a planned move).
pub fn library_under(conn: &rusqlite::Connection, root: &str, moved_from: &[String]) -> Library {
    let prefix = format!("{}/", root.trim_end_matches('/'));
    let (indexed, images_without_clip) = conn
        .query_row(
            "SELECT COUNT(*), \
             COALESCE(SUM(CASE WHEN f.kind = 'image' \
                 AND NOT EXISTS (SELECT 1 FROM clip_embeddings c WHERE c.file_id = f.id) THEN 1 ELSE 0 END), 0) \
             FROM files f WHERE f.failed = 0 AND substr(f.path_text, 1, length(?1)) = ?1",
            rusqlite::params![prefix],
            |row| Ok((row.get::<_, i64>(0)?.max(0) as u64, row.get::<_, i64>(1)?.max(0) as u64)),
        )
        .unwrap_or((0, 0));
    let mut folders: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT path_text FROM files WHERE failed = 0 AND substr(path_text, 1, length(?1)) = ?1",
    ) {
        if let Ok(rows) = stmt.query_map([&prefix], |row| row.get::<_, String>(0)) {
            for path in rows.flatten() {
                let folder = std::path::Path::new(&path)
                    .parent()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                *folders.entry(folder).or_default() += 1;
            }
        }
    }
    let moved: std::collections::HashSet<&str> = moved_from.iter().map(String::as_str).collect();
    let staying = folders
        .into_iter()
        .filter(|(folder, _)| !moved.contains(folder.as_str()))
        .map(|(folder, count)| {
            let relative = folder
                .strip_prefix(root.trim_end_matches('/'))
                .unwrap_or(&folder)
                .trim_start_matches('/');
            (
                if relative.is_empty() {
                    "(library root)".to_owned()
                } else {
                    relative.to_owned()
                },
                count,
            )
        })
        .collect();
    Library {
        indexed,
        images_without_clip,
        staying,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn movement(source: &str, destination: &str, tier: Option<&str>) -> RestructureMove {
        RestructureMove {
            source: source.into(),
            destination: destination.into(),
            category: "Photos".into(),
            tier: tier.map(str::to_owned),
            confidence: "auto".into(),
            ..RestructureMove::default()
        }
    }

    #[test]
    fn tiers_map_to_tidy_or_reorganize() {
        assert_eq!(
            outcome(&movement("/l/a.jpg", "/l/b/a.jpg", Some("Mixed"))),
            Outcome::Tidy
        );
        assert_eq!(
            outcome(&movement("/l/a.jpg", "/l/b/a.jpg", Some("Junk"))),
            Outcome::Reorganize
        );
        assert_eq!(
            outcome(&movement("/l/a.jpg", "/l/b/a.jpg", None)),
            Outcome::Reorganize
        );
        assert!(default_selected("AUTO") && !default_selected("review"));
    }

    #[test]
    fn buckets_are_relative_destination_folders() {
        let m = movement("/lib/Untitled/a.jpg", "/lib/Photos/2024/a.jpg", None);
        assert_eq!(bucket(&m, "/lib"), "Photos/2024");
        assert_eq!(bucket(&m, "/lib/"), "Photos/2024");
        assert_eq!(source_folder(&m), "Untitled");
        assert_eq!(
            bucket(&movement("/lib/x/a.jpg", "/lib/a.jpg", None), "/lib"),
            "Photos"
        );
    }

    #[test]
    fn step_cells_follow_the_plan() {
        use StepState::*;
        let none = Progress::default();
        assert_eq!(steps(none), [Current, Upcoming, Upcoming]);
        assert_eq!(
            steps(Progress {
                planning: true,
                ..none
            }),
            [Current, Upcoming, Upcoming]
        );
        let planned = Progress {
            has_plan: true,
            ..none
        };
        assert_eq!(steps(planned), [Done, Current, Upcoming]);
        let chosen = Progress {
            selected: 4,
            ..planned
        };
        assert_eq!(steps(chosen), [Done, Done, Current]);
        assert_eq!(
            steps(Progress {
                moving: true,
                ..chosen
            }),
            [Done, Done, Current]
        );
        assert_eq!(
            steps(Progress {
                moved: true,
                ..none
            }),
            [Done, Done, Done]
        );
        assert_eq!(
            zero_move_status(4),
            "Everything here is already organized. 4 folders kept."
        );
        assert_eq!(plural(1200, "file"), "1,200 files");
    }

    #[test]
    fn library_counts_this_root_and_lists_untouched_folders() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE files (id INTEGER PRIMARY KEY, path_text TEXT, kind TEXT, failed INTEGER);
             CREATE TABLE clip_embeddings (file_id INTEGER PRIMARY KEY);
             INSERT INTO files VALUES (1, '/lib/Trips/a.jpg', 'image', 0), (2, '/lib/Trips/b.jpg', 'image', 0),
                 (3, '/lib/Untitled/c.pdf', 'pdf', 0), (4, '/library2/d.jpg', 'image', 0), (5, '/lib/e.jpg', 'image', 1);
             INSERT INTO clip_embeddings VALUES (1);",
        )
        .unwrap();
        let library = library_under(&conn, "/lib", &["/lib/Untitled".to_owned()]);
        assert_eq!((library.indexed, library.images_without_clip), (3, 1));
        assert_eq!(library.staying, vec![("Trips".to_owned(), 2)]);
    }
}
