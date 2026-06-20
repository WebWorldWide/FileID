//! `fileid search <query>` — FTS keyword search (model-free).
//!
//! Matches the engine's `doc_fts` (document text) and `ocr_fts` (image OCR
//! text) FTS5 indexes, plus a filename substring fallback. `--similar`
//! (semantic / CLIP) needs models the CLI MVP doesn't load.

use std::collections::BTreeMap;

use anyhow::Result;
use rusqlite::params;

use crate::context::{display_path, human_size, print_json, Ctx};

struct Hit {
    id: i64,
    path: String,
    kind: String,
    size: i64,
    source: &'static str,
    snippet: Option<String>,
}

pub fn run(ctx: &Ctx, terms: &[String], similar: bool, limit: usize) -> Result<()> {
    if similar {
        return needs_models(ctx);
    }
    let raw = terms.join(" ");
    let raw = raw.trim();
    if raw.is_empty() {
        anyhow::bail!("empty search query");
    }
    ctx.require_db_exists()?;
    let conn = fileid_engine::db::open_read(&ctx.db)?;
    let fts_expr = to_fts_expr(raw);

    // id → Hit, insertion order preserved within source tier by BTreeMap on id;
    // we re-sort by source tier for display so content matches lead.
    let mut hits: BTreeMap<i64, Hit> = BTreeMap::new();

    collect_fts(&conn, "doc_fts", &fts_expr, limit, "content", &mut hits);
    collect_fts(&conn, "ocr_fts", &fts_expr, limit, "ocr", &mut hits);
    collect_filename(&conn, raw, limit, &mut hits);

    let mut rows: Vec<Hit> = hits.into_values().collect();
    rows.sort_by_key(|h| (source_rank(h.source), h.id));
    rows.truncate(limit);

    if ctx.json {
        let arr: Vec<serde_json::Value> = rows
            .iter()
            .map(|h| {
                serde_json::json!({
                    "id": h.id,
                    "path": h.path,
                    "kind": h.kind,
                    "sizeBytes": h.size,
                    "matchedOn": h.source,
                    "snippet": h.snippet,
                })
            })
            .collect();
        print_json(&serde_json::json!({
            "command": "search",
            "query": raw,
            "count": arr.len(),
            "results": arr,
        }));
        return Ok(());
    }

    if rows.is_empty() {
        println!("No matches for {}.", ctx.bold(raw));
        return Ok(());
    }
    println!(
        "{} match(es) for {}:",
        rows.len(),
        ctx.bold(raw)
    );
    for h in &rows {
        println!(
            "  {}  {}",
            ctx.bold(&display_path(&h.path)),
            ctx.dim(&format!("[{}, {}, {}]", h.kind, human_size(h.size), h.source))
        );
        if let Some(s) = &h.snippet {
            let one_line: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
            if !one_line.is_empty() {
                println!("      {}", ctx.dim(&one_line));
            }
        }
    }
    Ok(())
}

fn collect_fts(
    conn: &rusqlite::Connection,
    table: &str,
    expr: &str,
    limit: usize,
    source: &'static str,
    out: &mut BTreeMap<i64, Hit>,
) {
    let sql = format!(
        "SELECT f.id, f.path_text, f.kind, f.size_bytes, \
         snippet({table}, 0, '', '', '…', 10) \
         FROM {table} JOIN files f ON f.id = {table}.rowid \
         WHERE {table} MATCH ?1 ORDER BY rank LIMIT ?2"
    );
    let Ok(mut stmt) = conn.prepare(&sql) else {
        return;
    };
    let rows = stmt.query_map(params![expr, limit as i64], |r| {
        Ok(Hit {
            id: r.get(0)?,
            path: r.get(1)?,
            kind: r.get(2)?,
            size: r.get(3)?,
            source,
            snippet: r.get::<_, Option<String>>(4)?,
        })
    });
    if let Ok(rows) = rows {
        for h in rows.flatten() {
            out.entry(h.id).or_insert(h);
        }
    }
}

fn collect_filename(
    conn: &rusqlite::Connection,
    raw: &str,
    limit: usize,
    out: &mut BTreeMap<i64, Hit>,
) {
    let like = format!("%{}%", raw.to_lowercase());
    let Ok(mut stmt) = conn.prepare(
        "SELECT id, path_text, kind, size_bytes FROM files \
         WHERE lower(path_text) LIKE ?1 LIMIT ?2",
    ) else {
        return;
    };
    let rows = stmt.query_map(params![like, limit as i64], |r| {
        Ok(Hit {
            id: r.get(0)?,
            path: r.get(1)?,
            kind: r.get(2)?,
            size: r.get(3)?,
            source: "filename",
            snippet: None,
        })
    });
    if let Ok(rows) = rows {
        for h in rows.flatten() {
            out.entry(h.id).or_insert(h);
        }
    }
}

fn source_rank(source: &str) -> u8 {
    match source {
        "content" => 0,
        "ocr" => 1,
        _ => 2,
    }
}

/// Turn a free-text query into a safe FTS5 expression: each whitespace token
/// becomes a quoted phrase, AND-ed together. Quoting neutralizes FTS operator
/// characters so an arbitrary user string can't be a syntax error.
fn to_fts_expr(raw: &str) -> String {
    raw.split_whitespace()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

fn needs_models(ctx: &Ctx) -> Result<()> {
    let msg = "semantic / similarity search needs the CLIP models, which the CLI MVP does not load";
    if ctx.json {
        print_json(&serde_json::json!({
            "command": "search",
            "error": "models_required",
            "message": msg,
            "hint": "use the desktop app, or run a full engine scan with models installed",
        }));
    } else {
        println!("{}", ctx.bold("Semantic search unavailable."));
        println!("  {msg}.");
        println!("  Keyword (FTS) search works model-free: `fileid search <words>`.");
    }
    Ok(())
}
