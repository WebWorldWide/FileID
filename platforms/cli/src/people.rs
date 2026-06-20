//! `fileid people` — list person clusters (id, display name, face count).
//! Read-only. People are produced by the engine's face detection + clustering,
//! so this is empty until a full engine scan (with face models) has run.

use anyhow::Result;
use rusqlite::params;

use crate::context::{print_json, Ctx};

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
        if let Some(n) = self.name.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
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

    let mut stmt = conn.prepare(
        "SELECT p.id, p.name, p.first_name, p.last_name, p.is_unknown, p.file_count, \
            (SELECT COUNT(*) FROM face_prints fp WHERE fp.person_id = p.id) AS faces \
         FROM persons p ORDER BY faces DESC, p.id ASC",
    )?;
    let people: Vec<Person> = stmt
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
        .filter_map(Result::ok)
        .collect();

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

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}
