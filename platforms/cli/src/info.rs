//! `fileid info <path-or-id>` — show a file's metadata, tags, people, and a
//! text snippet. Read-only.

use anyhow::Result;
use rusqlite::{params, OptionalExtension};

use crate::context::{human_size, print_json, terminal_text, Ctx};

struct FileRow {
    id: i64,
    path: String,
    kind: String,
    extension: String,
    size: i64,
    created: Option<f64>,
    modified: Option<f64>,
    scanned: f64,
    has_faces: bool,
    has_text: bool,
    camera: Option<String>,
    lat: Option<f64>,
    lon: Option<f64>,
    failed: bool,
    error: Option<String>,
    vlm_desc: Option<String>,
    vlm_name: Option<String>,
}

const SELECT_COLS: &str = "id, path_text, kind, extension, size_bytes, created_at, modified_at, \
    scanned_at, has_faces, has_text, camera_model, location_lat, location_lon, failed, \
    error_message, vlm_description, vlm_proposed_name";

pub fn run(ctx: &Ctx, target: &str) -> Result<()> {
    ctx.require_db_exists()?;
    let conn = fileid_engine::db::open_read(&ctx.db)?;

    let row = lookup(&conn, target)?;
    let Some(row) = row else {
        if ctx.json {
            print_json(&serde_json::json!({
                "command": "info",
                "error": "not_found",
                "target": target,
            }));
        } else {
            println!("No indexed file matches {}.", ctx.bold(target));
        }
        return Ok(());
    };

    let tags = load_tags(&conn, row.id);
    let people = load_people(&conn, row.id);
    let snippet: Option<String> = conn
        .query_row(
            "SELECT substr(text, 1, 280) FROM doc_text WHERE file_id = ?1",
            params![row.id],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten();

    if ctx.json {
        print_json(&serde_json::json!({
            "command": "info",
            "id": row.id,
            "path": row.path,
            "kind": row.kind,
            "extension": row.extension,
            "sizeBytes": row.size,
            "createdAt": row.created,
            "modifiedAt": row.modified,
            "scannedAt": row.scanned,
            "hasFaces": row.has_faces,
            "hasText": row.has_text,
            "cameraModel": row.camera,
            "location": row.lat.zip(row.lon).map(|(la, lo)| serde_json::json!([la, lo])),
            "failed": row.failed,
            "error": row.error,
            "vlmDescription": row.vlm_desc,
            "vlmProposedName": row.vlm_name,
            "tags": tags.iter().map(|(t, s, sc)| serde_json::json!({"tag": t, "source": s, "score": sc})).collect::<Vec<_>>(),
            "people": people.iter().map(|(id, n)| serde_json::json!({"id": id, "name": n})).collect::<Vec<_>>(),
            "snippet": snippet,
        }));
        return Ok(());
    }

    println!("{}", ctx.bold(&row.path));
    println!("  id:        {}", row.id);
    println!(
        "  kind:      {} (.{})",
        terminal_text(&row.kind),
        terminal_text(&row.extension)
    );
    println!("  size:      {}", human_size(row.size));
    if let Some(c) = row.created {
        println!("  created:   {}", unix_to_date(c));
    }
    if let Some(m) = row.modified {
        println!("  modified:  {}", unix_to_date(m));
    }
    println!("  scanned:   {}", unix_to_date(row.scanned));
    if let Some(cam) = &row.camera {
        println!("  camera:    {}", terminal_text(cam));
    }
    if let (Some(la), Some(lo)) = (row.lat, row.lon) {
        println!("  location:  {la:.5}, {lo:.5}");
    }
    println!(
        "  flags:     {}{}{}",
        if row.has_text { "text " } else { "" },
        if row.has_faces { "faces " } else { "" },
        if row.failed { "FAILED" } else { "" }
    );
    if let Some(e) = &row.error {
        println!("  error:     {}", terminal_text(e));
    }
    if let Some(d) = &row.vlm_desc {
        println!("  caption:   {}", terminal_text(d));
    }
    if let Some(n) = &row.vlm_name {
        println!("  suggested: {}", terminal_text(n));
    }
    if !people.is_empty() {
        let names: Vec<String> = people
            .iter()
            .map(|(id, n)| n.clone().unwrap_or_else(|| format!("#{id}")))
            .collect();
        println!("  people:    {}", terminal_text(&names.join(", ")));
    }
    if !tags.is_empty() {
        println!("  {}", ctx.bold("tags:"));
        for (tag, source, score) in &tags {
            let score_s = score.map(|s| format!(" {s:.2}")).unwrap_or_default();
            println!(
                "    {} {}",
                terminal_text(tag),
                ctx.dim(&format!("({source}{score_s})"))
            );
        }
    }
    if let Some(s) = &snippet {
        let one_line: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
        println!("  {}", ctx.bold("text:"));
        println!("    {}", ctx.dim(&one_line));
    }
    Ok(())
}

fn lookup(conn: &rusqlite::Connection, target: &str) -> Result<Option<FileRow>> {
    let Some(id) = crate::context::resolve_file_id(conn, target) else {
        return Ok(None);
    };
    let sql = format!("SELECT {SELECT_COLS} FROM files WHERE id = ?1");
    Ok(conn
        .query_row(&sql, params![id], |r| {
            Ok(FileRow {
                id: r.get(0)?,
                path: r.get(1)?,
                kind: r.get(2)?,
                extension: r.get(3)?,
                size: r.get(4)?,
                created: r.get(5)?,
                modified: r.get(6)?,
                scanned: r.get(7)?,
                has_faces: r.get::<_, i64>(8)? != 0,
                has_text: r.get::<_, i64>(9)? != 0,
                camera: r.get(10)?,
                lat: r.get(11)?,
                lon: r.get(12)?,
                failed: r.get::<_, i64>(13)? != 0,
                error: r.get(14)?,
                vlm_desc: r.get(15)?,
                vlm_name: r.get(16)?,
            })
        })
        .optional()?)
}

fn load_tags(conn: &rusqlite::Connection, file_id: i64) -> Vec<(String, String, Option<f64>)> {
    let Ok(mut stmt) = conn.prepare(
        "SELECT tag, source, score FROM tags WHERE file_id = ?1 ORDER BY source, score DESC",
    ) else {
        return Vec::new();
    };
    stmt.query_map(params![file_id], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get::<_, Option<f64>>(2)?))
    })
    .map(|rows| rows.flatten().collect())
    .unwrap_or_default()
}

fn load_people(conn: &rusqlite::Connection, file_id: i64) -> Vec<(i64, Option<String>)> {
    let Ok(mut stmt) = conn.prepare(
        "SELECT DISTINCT p.id, p.name FROM face_prints fp \
         JOIN persons p ON p.id = fp.person_id WHERE fp.file_id = ?1",
    ) else {
        return Vec::new();
    };
    stmt.query_map(params![file_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
}

/// Format a Unix timestamp (seconds) as `YYYY-MM-DD HH:MM` UTC, no deps.
/// Howard Hinnant's civil-from-days algorithm.
fn unix_to_date(secs: f64) -> String {
    if !secs.is_finite() || secs <= 0.0 {
        return "—".to_string();
    }
    let total = secs as i64;
    let days = total.div_euclid(86_400);
    let rem = total.rem_euclid(86_400);
    let (hh, mm) = (rem / 3600, (rem % 3600) / 60);

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    format!("{year:04}-{m:02}-{d:02} {hh:02}:{mm:02} UTC")
}
