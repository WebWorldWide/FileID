use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::pipeline::discovery::FileKind;
use crate::pipeline::restructure::{FileForClassify, PlannedIdentity};

/// Files + per-file person names for restructure planning. Person names come
/// from a deduped, ordered correlated subquery — NOT
/// `GROUP_CONCAT(DISTINCT p.name, char(31))`, which SQLite rejects at run with
/// "DISTINCT aggregates must have exactly one argument". `names` (column 8) is a
/// char(31)-separated list; only an unambiguous single-person result enables
/// the People rule so group photos are not auto-filed under an arbitrary name.
///
/// Bind `(root, prefix, upper)` from [`plan_root_bounds`]; an empty root
/// selects every active file.
pub const PLAN_FILES_SQL: &str = "SELECT
   f.id, f.path_text, f.kind, f.modified_at, f.created_at,
   f.location_lat, f.location_lon, f.has_text,
   (SELECT GROUP_CONCAT(name, char(31))
      FROM (SELECT DISTINCT p.name
              FROM persons p
              JOIN face_prints fp ON fp.person_id = p.id
             WHERE fp.file_id = f.id
               AND p.name IS NOT NULL AND TRIM(p.name) <> ''
             ORDER BY p.name)) AS names,
   (SELECT COUNT(*) FROM face_prints fp
     WHERE fp.file_id = f.id AND COALESCE(fp.excluded, 0) = 0) AS face_count,
   (SELECT COUNT(*) FROM face_prints fp
      JOIN persons p ON p.id = fp.person_id
     WHERE fp.file_id = f.id
       AND p.name IS NOT NULL AND TRIM(p.name) <> '') AS named_face_count,
   (SELECT COUNT(DISTINCT fp.person_id) FROM face_prints fp
      JOIN persons p ON p.id = fp.person_id
     WHERE fp.file_id = f.id
       AND p.name IS NOT NULL AND TRIM(p.name) <> '') AS named_person_count,
   f.vlm_proposed_name, f.content_hash, f.size_bytes, f.file_ref
 FROM files f
 WHERE f.failed = 0
   AND (?1 = '' OR f.path_text COLLATE NOCASE = ?1
        OR (f.path_text COLLATE NOCASE >= ?2 AND f.path_text COLLATE NOCASE < ?3))
 ORDER BY f.id";

/// [`PLAN_FILES_SQL`] keyset-paged: also binds `?4` (last id) and `?5` (limit).
pub const PLAN_FILES_PAGE_SQL: &str = "SELECT
   f.id, f.path_text, f.kind, f.modified_at, f.created_at,
   f.location_lat, f.location_lon, f.has_text,
   (SELECT GROUP_CONCAT(name, char(31))
      FROM (SELECT DISTINCT p.name
              FROM persons p
              JOIN face_prints fp ON fp.person_id = p.id
             WHERE fp.file_id = f.id
               AND p.name IS NOT NULL AND TRIM(p.name) <> ''
             ORDER BY p.name)) AS names,
   (SELECT COUNT(*) FROM face_prints fp
     WHERE fp.file_id = f.id AND COALESCE(fp.excluded, 0) = 0) AS face_count,
   (SELECT COUNT(*) FROM face_prints fp
      JOIN persons p ON p.id = fp.person_id
     WHERE fp.file_id = f.id
       AND p.name IS NOT NULL AND TRIM(p.name) <> '') AS named_face_count,
   (SELECT COUNT(DISTINCT fp.person_id) FROM face_prints fp
      JOIN persons p ON p.id = fp.person_id
     WHERE fp.file_id = f.id
       AND p.name IS NOT NULL AND TRIM(p.name) <> '') AS named_person_count,
   f.vlm_proposed_name, f.content_hash, f.size_bytes, f.file_ref
 FROM files f
 WHERE f.failed = 0
   AND (?1 = '' OR f.path_text COLLATE NOCASE = ?1
        OR (f.path_text COLLATE NOCASE >= ?2 AND f.path_text COLLATE NOCASE < ?3))
   AND f.id > ?4
 ORDER BY f.id
 LIMIT ?5";

/// `(root, prefix, upper)` bind values scoping [`PLAN_FILES_SQL`] to `root`.
pub fn plan_root_bounds(root: &str) -> (String, String, String) {
    // Pick the separator from the ORIGINAL root, before trimming. A Windows
    // drive-root like "C:\" trims to "C:" — which contains no backslash — so
    // deriving the separator from the trimmed string chose '/', built the
    // prefix "C:/", and produced a range that matched ZERO backslash-stored
    // files (every real "C:\Users\..." path sorts past the '/'-based upper
    // bound). Result: an empty/incorrect plan scope for a whole-drive library.
    let separator = if root.contains('\\') { '\\' } else { '/' };
    let root = root.trim_end_matches(['/', '\\']).to_string();
    let prefix = format!("{root}{separator}");
    let mut bytes = prefix.as_bytes().to_vec();
    for index in (0..bytes.len()).rev() {
        if bytes[index] != u8::MAX {
            bytes[index] += 1;
            bytes.truncate(index + 1);
            return (root, prefix, String::from_utf8_lossy(&bytes).into_owned());
        }
    }
    (root, prefix, "\u{10ffff}".into())
}

fn single_person_name(names: Option<&str>) -> Option<String> {
    let mut names = names
        .into_iter()
        .flat_map(|names| names.split('\x1F'))
        .map(str::trim)
        .filter(|name| !name.is_empty());
    let name = names.next()?.to_string();
    names.next().is_none().then_some(name)
}

pub fn unambiguous_person_name(
    names: Option<&str>,
    face_count: i64,
    named_face_count: i64,
    named_person_count: i64,
) -> Option<String> {
    (face_count > 0 && face_count == named_face_count && named_person_count == 1)
        .then(|| single_person_name(names))
        .flatten()
}

/// Row mapper for [`PLAN_FILES_SQL`] / [`PLAN_FILES_PAGE_SQL`].
pub fn plan_row_to_file(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileForClassify> {
    let kind: String = row.get(2)?;
    let names: Option<String> = row.get(8)?;
    let person_name = unambiguous_person_name(
        names.as_deref(),
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
    );
    Ok(FileForClassify {
        file_id: row.get(0)?,
        source: PathBuf::from(row.get::<_, String>(1)?),
        kind: FileKind::from_db_str(&kind),
        modified_unix: row.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
        created_unix: row.get(4)?,
        person_name,
        location_lat: row.get(5)?,
        location_lon: row.get(6)?,
        has_text: row.get::<_, Option<i64>>(7)?.unwrap_or(0) != 0,
        vlm_proposed: row.get(12)?,
        identity: Some(PlannedIdentity {
            content_hash: row.get::<_, Option<Vec<u8>>>(13)?.map(hex::encode),
            size_bytes: row.get::<_, Option<u64>>(14)?.unwrap_or(0),
            file_ref: row.get::<_, Option<u64>>(15)?,
            modified_at: row.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
        }),
    })
}

/// Deepest folder containing every active file, plus the active-file count.
/// `None` when the library is empty. The component prefix of a sorted set's
/// first and last members bounds every member, so only two rows are read.
pub fn library_root(conn: &Connection) -> rusqlite::Result<Option<(PathBuf, i64)>> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM files WHERE failed = 0", [], |row| row.get(0))?;
    if count == 0 {
        return Ok(None);
    }
    let edge = |order: &str| -> rusqlite::Result<PathBuf> {
        conn.query_row(
            &format!("SELECT path_text FROM files WHERE failed = 0 ORDER BY path_text {order} LIMIT 1"),
            [],
            |row| row.get::<_, String>(0).map(PathBuf::from),
        )
    };
    let (first, last) = (edge("ASC")?, edge("DESC")?);
    let parents = [&first, &last].map(|path| path.parent().unwrap_or(path).components().collect::<Vec<_>>());
    let shared = parents[0].iter().zip(&parents[1]).take_while(|(a, b)| a == b).count();
    let root: PathBuf = parents[0][..shared].iter().map(|part| part.as_os_str()).collect();
    Ok(Some((root, count)))
}

/// True when `root` is a filesystem root (`/`, `C:\`) — no parent folder.
pub fn is_filesystem_root(root: &Path) -> bool {
    root.parent().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert(conn: &Connection, path: &str) {
        conn.execute(
            "INSERT INTO files (path_text, path_hash, size_bytes, scanned_at, kind, extension) \
             VALUES (?1, 0, 1, 0, 'image', 'jpg')",
            [path],
        )
        .unwrap();
    }

    #[test]
    fn library_root_is_the_shared_parent_folder() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        assert!(library_root(&conn).unwrap().is_none());
        insert(&conn, "/home/u/Pictures/2021/a.jpg");
        assert_eq!(library_root(&conn).unwrap().unwrap(), (PathBuf::from("/home/u/Pictures/2021"), 1));
        insert(&conn, "/home/u/Pictures/2022/b.jpg");
        insert(&conn, "/home/u/Pictures/2021b/c.jpg");
        assert_eq!(library_root(&conn).unwrap().unwrap().0, PathBuf::from("/home/u/Pictures"));
        insert(&conn, "/opt/x.jpg");
        let (root, count) = library_root(&conn).unwrap().unwrap();
        assert_eq!((root.as_path(), count), (Path::new("/"), 4));
        assert!(is_filesystem_root(&root));
    }

    #[test]
    fn plan_row_maps_kind_and_unambiguous_person() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert(&conn, "/lib/a.jpg");
        let (root, prefix, upper) = plan_root_bounds("");
        let files = conn
            .prepare(PLAN_FILES_SQL)
            .unwrap()
            .query_map(rusqlite::params![root, prefix, upper], plan_row_to_file)
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].kind, FileKind::Image);
        assert!(files[0].person_name.is_none());
    }
}
