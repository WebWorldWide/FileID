use rusqlite::{params, Connection};

/// A person cluster with at least one face on an active file.
#[derive(Clone, Debug)]
pub struct Person {
    pub id: i64,
    pub name: String,
    pub is_unknown: bool,
    pub files: i64,
    pub faces: i64,
}

/// Display name: explicit name, else "first last", else Unknown/Unnamed.
pub fn display_name(
    name: Option<&str>,
    first: Option<&str>,
    last: Option<&str>,
    is_unknown: bool,
) -> String {
    if let Some(name) = name.map(str::trim).filter(|s| !s.is_empty()) {
        return name.to_string();
    }
    let composed = [first, last]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if !composed.is_empty() {
        composed
    } else if is_unknown {
        "Unknown".to_string()
    } else {
        "Unnamed".to_string()
    }
}

/// People with faces on active files, most faces first. `limit` caps the rows.
pub fn active_people(conn: &Connection, limit: Option<usize>) -> rusqlite::Result<Vec<Person>> {
    let mut stmt = conn.prepare(
        "SELECT p.id, p.name, p.first_name, p.last_name, p.is_unknown, \
            (SELECT COUNT(DISTINCT fp.file_id) FROM face_prints fp JOIN files f ON f.id = fp.file_id \
             WHERE fp.person_id = p.id AND f.failed = 0) AS files, \
            (SELECT COUNT(*) FROM face_prints fp JOIN files f ON f.id = fp.file_id \
             WHERE fp.person_id = p.id AND f.failed = 0) AS faces \
         FROM persons p WHERE EXISTS ( \
             SELECT 1 FROM face_prints fp JOIN files f ON f.id = fp.file_id \
             WHERE fp.person_id = p.id AND f.failed = 0 \
         ) ORDER BY faces DESC, p.id ASC LIMIT ?1",
    )?;
    let limit = limit.map_or(-1, |limit| i64::try_from(limit).unwrap_or(i64::MAX));
    let rows = stmt.query_map(params![limit], |row| {
        let name: Option<String> = row.get(1)?;
        let first: Option<String> = row.get(2)?;
        let last: Option<String> = row.get(3)?;
        let is_unknown = row.get::<_, Option<i64>>(4)?.unwrap_or(0) != 0;
        Ok(Person {
            id: row.get(0)?,
            name: display_name(name.as_deref(), first.as_deref(), last.as_deref(), is_unknown),
            is_unknown,
            files: row.get(5)?,
            faces: row.get(6)?,
        })
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_name_prefers_name_then_parts_then_status() {
        assert_eq!(display_name(Some(" Ada "), Some("X"), None, false), "Ada");
        assert_eq!(display_name(Some(" "), Some("Ada"), Some(" Lovelace"), false), "Ada Lovelace");
        assert_eq!(display_name(None, None, None, true), "Unknown");
        assert_eq!(display_name(None, Some(""), None, false), "Unnamed");
    }

    #[test]
    fn people_count_only_active_files_and_hide_inactive_clusters() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO files (id, path_text, path_hash, size_bytes, scanned_at, kind, extension, failed) VALUES \
             (1, '/active.jpg', 1, 1, 0, 'image', 'jpg', 0), (2, '/missing.jpg', 2, 1, 0, 'image', 'jpg', 1); \
             INSERT INTO persons (id, name, file_count, created_at) VALUES (10, 'Visible', 99, 0), (20, 'Hidden', 99, 0); \
             INSERT INTO face_prints (file_id, person_id, print_data, bbox) VALUES \
             (1, 10, X'00', '0,0,1,1'), (1, 10, X'00', '0,0,1,1'), (2, 10, X'00', '0,0,1,1'), (2, 20, X'00', '0,0,1,1');",
        )
        .unwrap();
        let people = active_people(&conn, None).unwrap();
        assert_eq!(people.len(), 1);
        assert_eq!((people[0].id, people[0].files, people[0].faces), (10, 1, 2));
        assert_eq!(people[0].name, "Visible");
        assert!(active_people(&conn, Some(0)).unwrap().is_empty());
    }
}
