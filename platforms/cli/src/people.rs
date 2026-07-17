//! `fileid people` — list person clusters (id, display name, face count).
//! Read-only. People are produced by the engine's face detection + clustering,
//! so this is empty until a full engine scan (with face models) has run.

use anyhow::Result;
use rusqlite::params;

use crate::context::{print_json, truncate, Ctx};

struct Person {
    id: i64,
    name: Option<String>,
    first: Option<String>,
    last: Option<String>,
    is_unknown: bool,
    file_count: i64,
    faces: i64,
}

impl Person {
    fn display_name(&self) -> String {
        if let Some(n) = self
            .name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return n.to_string();
        }
        let composed = [self.first.as_deref(), self.last.as_deref()]
            .into_iter()
            .flatten()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if !composed.is_empty() {
            composed
        } else if self.is_unknown {
            "Unknown".to_string()
        } else {
            "Unnamed".to_string()
        }
    }
}

pub fn run(ctx: &Ctx) -> Result<()> {
    ctx.require_db_exists()?;
    let conn = fileid_engine::db::open_read(&ctx.db)?;
    let people = load_people(&conn)?;

    if ctx.json {
        let arr: Vec<serde_json::Value> = people
            .iter()
            .map(|p| {
                serde_json::json!({
                    "id": p.id,
                    "name": p.display_name(),
                    "isUnknown": p.is_unknown,
                    "faceCount": p.faces,
                    "fileCount": p.file_count,
                })
            })
            .collect();
        print_json(&serde_json::json!({
            "command": "people",
            "count": arr.len(),
            "people": arr,
        }));
        return Ok(());
    }

    if people.is_empty() {
        println!("No person clusters.");
        ctx.progress(&format!(
            "  {}",
            ctx.dim("people come from face clustering — run a full engine scan with face models")
        ));
        return Ok(());
    }
    println!("{} person cluster(s):", people.len());
    println!(
        "  {:<6} {:<28} {:>6} {:>6}",
        ctx.bold("id"),
        ctx.bold("name"),
        ctx.bold("faces"),
        ctx.bold("files")
    );
    for p in &people {
        println!(
            "  {:<6} {:<28} {:>6} {:>6}",
            p.id,
            truncate(&p.display_name(), 28),
            p.faces,
            p.file_count
        );
    }
    Ok(())
}

fn load_people(conn: &rusqlite::Connection) -> Result<Vec<Person>> {
    let mut stmt = conn.prepare(
        "SELECT p.id, p.name, p.first_name, p.last_name, p.is_unknown, \
            (SELECT COUNT(DISTINCT fp.file_id) FROM face_prints fp \
             JOIN files f ON f.id = fp.file_id \
             WHERE fp.person_id = p.id AND f.failed = 0) AS files, \
            (SELECT COUNT(*) FROM face_prints fp JOIN files f ON f.id = fp.file_id \
             WHERE fp.person_id = p.id AND f.failed = 0) AS faces \
         FROM persons p WHERE EXISTS ( \
             SELECT 1 FROM face_prints fp JOIN files f ON f.id = fp.file_id \
             WHERE fp.person_id = p.id AND f.failed = 0 \
         ) ORDER BY faces DESC, p.id ASC",
    )?;
    let people = stmt
        .query_map(params![], |r| {
            Ok(Person {
                id: r.get(0)?,
                name: r.get(1)?,
                first: r.get(2)?,
                last: r.get(3)?,
                is_unknown: r.get::<_, Option<i64>>(4)?.unwrap_or(0) != 0,
                file_count: r.get(5)?,
                faces: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(people)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn people_counts_only_active_files_and_hides_inactive_clusters() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        fileid_engine::db::migrations::apply(&conn).unwrap();
        conn.execute(
            "INSERT INTO files (id, path_text, path_hash, size_bytes, scanned_at, kind, extension, failed) VALUES \
             (1, '/active.jpg', 1, 1, 0, 'image', 'jpg', 0), \
             (2, '/missing.jpg', 2, 1, 0, 'image', 'jpg', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO persons (id, name, file_count, created_at) VALUES \
             (10, 'Visible', 99, 0), (20, 'Hidden', 99, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO face_prints (file_id, person_id, print_data, bbox) VALUES \
             (1, 10, X'00', '0,0,1,1'), (1, 10, X'00', '0,0,1,1'), \
             (2, 10, X'00', '0,0,1,1'), (2, 20, X'00', '0,0,1,1')",
            [],
        )
        .unwrap();

        let people = load_people(&conn).unwrap();
        assert_eq!(people.len(), 1);
        assert_eq!(people[0].id, 10);
        assert_eq!(people[0].file_count, 1);
        assert_eq!(people[0].faces, 2);
    }
}
