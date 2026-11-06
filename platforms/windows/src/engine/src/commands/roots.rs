//! `listRoots`: the library locations the catalog has scanned, with whether each is
//! reachable right now and how many files it holds (a nested location's files count only
//! there). Read-only; never scans.

use std::sync::Arc;

use parking_lot::Mutex;
use rusqlite::Connection;

use crate::ipc::sink::Sink;
use crate::ipc::{EngineError, EventPayload, IpcEvent, LibraryRoot, ListRootsPayload, RootsList, Wrap};

/// A folder counts as online when it exists, is a directory and has an entry. An unplugged
/// drive often leaves an empty mount folder behind, which must not read as online.
pub(crate) fn is_online(path: &str) -> bool {
    let path = std::path::Path::new(path);
    path.is_dir() && std::fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_some())
}

/// Half-open range matching every stored path under `root`: `root/` up to `root0`
/// (`0` is the character after `/`; `]` follows `\`).
/// Scans record the full path as the display name; show the folder's own name instead.
fn display_name(path: &str, stored: String) -> String {
    if stored != path {
        return stored;
    }
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty())
        .map_or(stored.clone(), str::to_owned)
}

fn subtree_bounds(root: &str) -> (String, String, String) {
    let (separator, next) = if root.contains('\\') && !root.contains('/') { ('\\', ']') } else { ('/', '0') };
    let trimmed = root.trim_end_matches(separator);
    (trimmed.to_owned(), format!("{trimmed}{separator}"), format!("{trimmed}{next}"))
}

/// The files one library location holds: its path and everything under it, minus any deeper
/// location, because the most specific location owns a file (the apps' `OfflineRoots` rule).
#[derive(Clone, Debug, PartialEq)]
pub struct RootScope {
    bounds: (String, String, String),
    deeper: Vec<(String, String, String)>,
    param: String,
}

fn within((exact, low, high): &(String, String, String), path: &str) -> bool {
    path == exact || (path >= low.as_str() && path < high.as_str())
}

impl RootScope {
    pub fn new(root: &str, others: &[String]) -> Self {
        let bounds = subtree_bounds(root);
        let mut deeper: Vec<_> = others
            .iter()
            .map(|other| subtree_bounds(other))
            .filter(|other| other.0 != bounds.0 && within(&bounds, &other.0))
            .collect();
        deeper.sort();
        deeper.dedup();
        let nested: Vec<[&String; 3]> = deeper.iter().map(|(exact, low, high)| [exact, low, high]).collect();
        let param = serde_json::json!([bounds.0, bounds.1, bounds.2, nested]).to_string();
        Self { bounds, deeper, param }
    }

    /// `None` without an id; an error for an id the catalog doesn't know.
    pub fn load(conn: &Connection, id: Option<i64>) -> anyhow::Result<Option<Self>> {
        let Some(id) = id else { return Ok(None) };
        anyhow::ensure!(id > 0, "Invalid library location");
        let paths: Vec<(i64, String)> = conn
            .prepare("SELECT id,path FROM library_roots")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let root = paths
            .iter()
            .find(|(candidate, _)| *candidate == id)
            .map(|(_, path)| path.clone())
            .ok_or_else(|| anyhow::anyhow!("That location is no longer in the library."))?;
        let others: Vec<String> = paths.into_iter().filter(|(candidate, _)| *candidate != id).map(|(_, path)| path).collect();
        Ok(Some(Self::new(&root, &others)))
    }

    pub fn contains(&self, path: &str) -> bool {
        within(&self.bounds, path) && !self.deeper.iter().any(|deeper| within(deeper, path))
    }

    /// The JSON bound to [`clause`]'s parameter.
    pub fn param(&self) -> &str {
        &self.param
    }
}

/// SQL that is true when `column` lies in the scope bound to `param` (a [`RootScope::param`]),
/// or when `param` is NULL. Binds the parameter once.
pub fn clause(column: &str, param: &str) -> String {
    let inside = |bounds: &str| {
        format!("({column}=json_extract({bounds},'$[0]') OR ({column}>=json_extract({bounds},'$[1]') AND {column}<json_extract({bounds},'$[2]')))")
    };
    format!(
        "(SELECT s.v IS NULL OR ({} AND NOT EXISTS (SELECT 1 FROM json_each(s.v,'$[3]') d WHERE {})) FROM (SELECT {param} AS v) s)",
        inside("s.v"),
        inside("d.value")
    )
}

pub fn list(conn: &Connection) -> rusqlite::Result<Vec<LibraryRoot>> {
    let mut statement = conn.prepare("SELECT id,path,display,kind,enabled,last_scan_at,last_status FROM library_roots ORDER BY added_at,id")?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)? != 0,
                row.get::<_, Option<f64>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut roots = Vec::with_capacity(rows.len());
    let paths: Vec<(i64, String)> = rows.iter().map(|row| (row.0, row.1.clone())).collect();
    let totals = format!(
        "SELECT COUNT(*),COALESCE(SUM(size_bytes),0) FROM files WHERE failed=0 AND (path_text=?1 OR (path_text>=?2 AND path_text<?3)) AND {}",
        clause("path_text", "?4")
    );
    for (id, path, display, kind, enabled, last_scan_at, last_status) in rows {
        let (exact, low, high) = subtree_bounds(&path);
        let others: Vec<String> = paths.iter().filter(|(other, _)| *other != id).map(|(_, other)| other.clone()).collect();
        let scope = RootScope::new(&path, &others);
        let (file_count, total_bytes): (i64, i64) = conn.query_row(
            &totals,
            rusqlite::params![exact, low, high, scope.param()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let online = is_online(&path);
        let display = display_name(&path, display);
        roots.push(LibraryRoot { id, path, display, kind, enabled, online, file_count, total_bytes, last_scan_at, last_status });
    }
    Ok(roots)
}

pub async fn handle(sink: Sink, database: Option<Arc<Mutex<Connection>>>, payload: ListRootsPayload) {
    if !payload.is_valid() {
        sink.send(EngineError::event(
            "invalid_list_roots_request",
            "listRoots needs a 1–128 character ASCII request ID.",
        ))
        .await;
        return;
    }
    let request_id = payload.request_id;
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<LibraryRoot>> {
        let Some(database) = database else { return Ok(vec![]) };
        let path = database.lock().path().filter(|path| !path.is_empty()).map(std::path::PathBuf::from);
        match path {
            Some(path) => Ok(list(&crate::db::open_read(&path)?)?),
            None => Ok(list(&database.lock())?),
        }
    })
    .await;
    match result {
        Ok(Ok(roots)) => sink.send(IpcEvent::now(EventPayload::RootsList(Wrap::new(RootsList { request_id, roots })))).await,
        Ok(Err(error)) => sink.send(EngineError::event("list_roots_failed", error.to_string())).await,
        Err(error) => sink.send(EngineError::event("list_roots_failed", error.to_string())).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        conn
    }

    #[test]
    fn counts_files_under_each_root_and_not_in_lookalike_siblings() {
        let conn = conn();
        conn.execute_batch(
            "INSERT INTO library_roots(path,display,kind,added_at,last_scan_at,last_status) VALUES('/Volumes/__fileid_test_absent__/Camera','Camera','external',1,5,'completed'),('/Volumes/__fileid_test_absent__/Camera2','Camera2','external',2,NULL,NULL);
             INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension,failed) VALUES
              (1,'/Volumes/__fileid_test_absent__/Camera/a.jpg',1,100,0,'image','jpg',0),
              (2,'/Volumes/__fileid_test_absent__/Camera/sub/b.jpg',2,200,0,'image','jpg',0),
              (3,'/Volumes/__fileid_test_absent__/Camera/c.jpg',3,400,0,'image','jpg',1),
              (4,'/Volumes/__fileid_test_absent__/Camera2/d.jpg',4,800,0,'image','jpg',0),
              (5,'/Volumes/__fileid_test_absent__/Cameras/e.jpg',5,1600,0,'image','jpg',0);",
        )
        .unwrap();
        let roots = list(&conn).unwrap();
        assert_eq!(roots.len(), 2);
        assert_eq!((roots[0].file_count, roots[0].total_bytes), (2, 300), "failed rows and sibling folders are excluded");
        assert_eq!((roots[1].file_count, roots[1].total_bytes), (1, 800));
        assert_eq!(roots[0].last_status.as_deref(), Some("completed"));
        assert_eq!(roots[1].last_scan_at, None);
        assert!(!roots[0].online, "a folder that is not on this machine reads as offline");
    }

    #[test]
    fn a_nested_location_counts_its_own_files_and_its_parent_does_not() {
        let conn = conn();
        conn.execute_batch(
            "INSERT INTO library_roots(path,display,kind,added_at) VALUES('/Volumes/__fileid_test_absent__/Camera','Camera','external',1),('/Volumes/__fileid_test_absent__/Camera/Synced','Synced','external',2);
             INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES
              (1,'/Volumes/__fileid_test_absent__/Camera/a.jpg',1,100,0,'image','jpg'),
              (2,'/Volumes/__fileid_test_absent__/Camera/Synced/b.jpg',2,200,0,'image','jpg'),
              (3,'/Volumes/__fileid_test_absent__/Camera/Synced/x/c.jpg',3,400,0,'image','jpg'),
              (4,'/Volumes/__fileid_test_absent__/Camera/Synced2/d.jpg',4,800,0,'image','jpg');",
        )
        .unwrap();
        let roots = list(&conn).unwrap();
        assert_eq!((roots[0].file_count, roots[0].total_bytes), (2, 900), "the parent leaves the nested location's files out");
        assert_eq!((roots[1].file_count, roots[1].total_bytes), (2, 600));
    }

    #[test]
    fn a_missing_or_empty_folder_is_offline_and_a_populated_one_online() {
        let dir = std::env::temp_dir().join(format!("fileid-roots-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!is_online(dir.to_str().unwrap()), "an empty mount folder is offline");
        std::fs::write(dir.join("a.jpg"), b"x").unwrap();
        assert!(is_online(dir.to_str().unwrap()));
        assert!(!is_online(dir.join("missing").to_str().unwrap()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn display_name_is_the_folder_name_unless_one_was_chosen() {
        assert_eq!(display_name("/Volumes/Adlon", "/Volumes/Adlon".into()), "Adlon");
        assert_eq!(display_name("/Users/a/Pictures/", "/Users/a/Pictures/".into()), "Pictures");
        assert_eq!(display_name(r"D:\Photos\2024", r"D:\Photos\2024".into()), "2024");
        assert_eq!(display_name("/Volumes/Adlon", "Camera".into()), "Camera");
        assert_eq!(display_name("/", "/".into()), "/");
    }

    #[test]
    fn a_scope_keeps_its_own_files_and_leaves_lookalikes_and_deeper_locations_out() {
        let others = ["/m/Camera2".to_owned(), "/m/Camera/Synced/".to_owned(), "/elsewhere".to_owned()];
        let scope = RootScope::new("/m/Camera/", &others);
        for inside in ["/m/Camera", "/m/Camera/a.jpg", "/m/Camera/2020/b.jpg", "/m/Camera/Synced2/c.jpg"] {
            assert!(scope.contains(inside), "{inside}");
        }
        for outside in ["/m/Camera2/a.jpg", "/m/Cameras/a.jpg", "/m/Camera/Synced", "/m/Camera/Synced/d.jpg", "/m/Cam", "/elsewhere/e.jpg"] {
            assert!(!scope.contains(outside), "{outside}");
        }
        assert!(RootScope::new("/", &others).contains("/x/e.jpg"));
        assert!(!RootScope::new("/", &others).contains("/elsewhere/e.jpg"));
        assert!(!RootScope::new("/", &others).contains("/m/Camera2/a.jpg"));
        let windows = RootScope::new(r"D:\Photos", &[r"D:\Photos\Old".to_owned()]);
        assert!(windows.contains(r"D:\Photos\a.jpg") && !windows.contains(r"D:\Photos2\a.jpg") && !windows.contains(r"D:\Photos\Old\a.jpg"));
    }

    #[test]
    fn the_sql_clause_agrees_with_contains_and_unknown_ids_are_errors() {
        let conn = conn();
        conn.execute_batch(
            "INSERT INTO library_roots(id,path,display,kind,added_at) VALUES(1,'/m/Camera','Camera','external',1),(2,'/m/Camera2','Camera2','external',2),(3,'/m/Camera/Synced','Synced','external',3);
             INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES
              (1,'/m/Camera/a.jpg',1,1,0,'image','jpg'),(2,'/m/Camera/x/b.jpg',2,1,0,'image','jpg'),(3,'/m/Camera2/c.jpg',3,1,0,'image','jpg'),
              (4,'/m/Cameras/d.jpg',4,1,0,'image','jpg'),(5,'/m/Camera/Synced/e.jpg',5,1,0,'image','jpg');",
        )
        .unwrap();
        let ids = |scope: Option<&RootScope>| -> Vec<i64> {
            let sql = format!("SELECT id FROM files f WHERE {} ORDER BY id", clause("f.path_text", "?1"));
            conn.prepare(&sql).unwrap().query_map([scope.map(RootScope::param)], |row| row.get(0)).unwrap().map(Result::unwrap).collect()
        };
        assert_eq!(ids(None), [1, 2, 3, 4, 5]);
        for (root, expected) in [(1, vec![1, 2]), (2, vec![3]), (3, vec![5])] {
            let scope = RootScope::load(&conn, Some(root)).unwrap().unwrap();
            assert_eq!(ids(Some(&scope)), expected);
            let paths: Vec<String> = conn.prepare("SELECT path_text FROM files ORDER BY id").unwrap().query_map([], |row| row.get(0)).unwrap().map(Result::unwrap).collect();
            let kept: Vec<i64> = paths.iter().zip(1..).filter(|(path, _)| scope.contains(path)).map(|(_, id)| id).collect();
            assert_eq!(kept, expected, "contains matches the SQL for root {root}");
        }
        assert!(RootScope::load(&conn, None).unwrap().is_none());
        assert!(RootScope::load(&conn, Some(99)).is_err());
        assert!(RootScope::load(&conn, Some(0)).is_err());
    }

    #[test]
    fn windows_style_roots_use_backslash_bounds() {
        let (exact, low, high) = subtree_bounds(r"D:\Photos\");
        assert_eq!((exact.as_str(), low.as_str(), high.as_str()), (r"D:\Photos", r"D:\Photos\", r"D:\Photos]"));
        let (_, low, high) = subtree_bounds("/Volumes/X/");
        assert_eq!((low.as_str(), high.as_str()), ("/Volumes/X/", "/Volumes/X0"));
    }
}
