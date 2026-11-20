//! `persons.file_count`: distinct files holding a counted face (`excluded` unset); same expressions as Swift
//! `Storage/PersonCounts.swift`.

use rusqlite::{Connection, params};

const COUNT: &str = "(SELECT COUNT(DISTINCT file_id) FROM face_prints \
                      WHERE person_id = persons.id AND COALESCE(excluded,0)=0)";

/// Corrects every stored count that differs from the live one, including a person left with no counted face. Returns the number of rows changed.
pub fn recount_stale(conn: &Connection) -> rusqlite::Result<usize> {
    conn.execute(
        &format!("UPDATE persons SET file_count = {COUNT} WHERE file_count IS NOT {COUNT}"),
        [],
    )
}

/// Read these before the files go.
pub fn persons_of_files(conn: &Connection, file_ids: &[i64]) -> rusqlite::Result<Vec<i64>> {
    let mut stmt = conn.prepare_cached(
        "SELECT DISTINCT person_id FROM face_prints WHERE file_id = ?1 AND person_id IS NOT NULL",
    )?;
    let mut out = Vec::new();
    for id in file_ids {
        for row in stmt.query_map(params![id], |r| r.get::<_, i64>(0))? {
            out.push(row?);
        }
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

pub fn reconcile(conn: &Connection, person_ids: &[i64]) -> rusqlite::Result<()> {
    for pid in person_ids {
        conn.execute(
            &format!("UPDATE persons SET file_count = {COUNT} WHERE id = ?1"),
            params![pid],
        )?;
        conn.execute(
            "UPDATE persons SET representative_face_id = \
             (SELECT id FROM face_prints WHERE person_id = ?1 ORDER BY id LIMIT 1) \
             WHERE id = ?1 AND (representative_face_id IS NULL OR representative_face_id NOT IN \
             (SELECT id FROM face_prints WHERE person_id = ?1))",
            params![pid],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{insert_file, memory_db};

    fn face(conn: &Connection, id: i64, file: i64, person: Option<i64>, excluded: i64) {
        conn.execute(
            "INSERT INTO face_prints(id,file_id,person_id,print_data,bbox,excluded) VALUES(?1,?2,?3,X'00','[]',?4)",
            params![id, file, person, excluded],
        )
        .unwrap();
    }

    fn counts(conn: &Connection) -> Vec<(i64, i64)> {
        conn.prepare("SELECT id, file_count FROM persons ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    fn seed() -> Connection {
        let conn = memory_db();
        for (id, path) in [(1, "/a/1.jpg"), (2, "/a/2.jpg"), (3, "/a/3.jpg")] {
            insert_file(&conn, id, path, "image");
        }
        for id in 1..=4 {
            conn.execute("INSERT INTO persons(id,created_at,file_count) VALUES(?1,0,?2)", params![id, 99]).unwrap();
        }
        face(&conn, 1, 1, Some(1), 0);
        face(&conn, 2, 1, Some(1), 0);
        face(&conn, 3, 2, Some(1), 0);
        face(&conn, 4, 3, Some(1), 1);
        face(&conn, 5, 3, Some(2), 1);
        face(&conn, 6, 3, Some(4), 0);
        conn
    }

    #[test]
    fn recount_stale_corrects_wrong_excluded_and_empty_counts() {
        let conn = seed();
        assert_eq!(recount_stale(&conn).unwrap(), 4);
        assert_eq!(counts(&conn), vec![(1, 2), (2, 0), (3, 0), (4, 1)]);
        assert_eq!(recount_stale(&conn).unwrap(), 0, "a second pass changes nothing");
    }

    #[test]
    fn recount_stale_leaves_correct_rows_alone() {
        let conn = seed();
        conn.execute("UPDATE persons SET file_count = 1 WHERE id = 4", []).unwrap();
        assert_eq!(recount_stale(&conn).unwrap(), 3);
        assert_eq!(counts(&conn)[3], (4, 1));
    }

    #[test]
    fn reconcile_recounts_only_the_named_persons_and_repairs_the_representative() {
        let conn = seed();
        conn.execute("UPDATE persons SET representative_face_id = 77 WHERE id = 1", []).unwrap();
        assert_eq!(persons_of_files(&conn, &[3]).unwrap(), vec![1, 2, 4]);
        reconcile(&conn, &[1]).unwrap();
        assert_eq!(counts(&conn), vec![(1, 2), (2, 99), (3, 99), (4, 99)]);
        let rep: Option<i64> = conn.query_row("SELECT representative_face_id FROM persons WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(rep, Some(1));
    }
}
