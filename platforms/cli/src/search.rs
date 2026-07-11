//! `fileid search <query>` — FTS keyword search (model-free).
//!
//! Matches the engine's `doc_fts` (document text) and `ocr_fts` (image OCR
//! text) FTS5 indexes, plus a filename substring fallback.
//!
//! `--similar <path-or-id>` is the visual / semantic nearest-neighbor path: it
//! reads the seed file's stored CLIP image embedding and ranks every other
//! embedded file by cosine similarity — the same `clip_embeddings` the engine's
//! "Find similar" action uses. Those embeddings are written only by a full
//! engine scan (`scan --models` / desktop); on a model-free library the command
//! reports that none are present.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};

use anyhow::Result;
use rusqlite::params;

use crate::context::{display_path, human_size, print_json, resolve_file_id, Ctx};

struct Hit {
    id: i64,
    path: String,
    kind: String,
    size: i64,
    source: &'static str,
    snippet: Option<String>,
    /// Rank position within this hit's source tier (0 = best). The FTS queries
    /// `ORDER BY rank`, so capturing the row index preserves relevance order
    /// through the id-keyed dedup map and the truncation to `limit`.
    ordinal: usize,
}

struct ScoredHit {
    score: f32,
    hit: Hit,
}

impl PartialEq for ScoredHit {
    fn eq(&self, other: &Self) -> bool {
        self.score.to_bits() == other.score.to_bits() && self.hit.id == other.hit.id
    }
}

impl Eq for ScoredHit {}

impl PartialOrd for ScoredHit {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ScoredHit {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.score
            .total_cmp(&other.score)
            .then_with(|| self.hit.id.cmp(&other.hit.id))
    }
}

fn keep_top_k(heap: &mut BinaryHeap<Reverse<ScoredHit>>, candidate: ScoredHit, limit: usize) {
    if limit == 0 {
        return;
    }
    if heap.len() < limit {
        heap.push(Reverse(candidate));
    } else if heap.peek().is_some_and(|worst| candidate > worst.0) {
        let _ = heap.pop();
        heap.push(Reverse(candidate));
    }
}

pub fn run(ctx: &Ctx, terms: &[String], similar: Option<&str>, limit: usize) -> Result<()> {
    if let Some(seed) = similar {
        return run_similar(ctx, seed, limit);
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
    rows.sort_by_key(|h| (source_rank(h.source), h.ordinal, h.id));
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
    println!("{} match(es) for {}:", rows.len(), ctx.bold(raw));
    for h in &rows {
        println!(
            "  {}  {}",
            ctx.bold(&display_path(&h.path)),
            ctx.dim(&format!(
                "[{}, {}, {}]",
                h.kind,
                human_size(h.size),
                h.source
            ))
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
            ordinal: 0,
        })
    });
    if let Ok(rows) = rows {
        // Rows arrive in `ORDER BY rank` (best first) — record the position so the
        // id-keyed dedup + final sort preserve FTS relevance within this tier.
        for (i, mut h) in rows.flatten().enumerate() {
            h.ordinal = i;
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
            ordinal: 0,
        })
    });
    if let Ok(rows) = rows {
        // Filename LIKE has no relevance rank — keep query (rowid) order as the
        // tier's natural order (this is the lowest-priority tier anyway).
        for (i, mut h) in rows.flatten().enumerate() {
            h.ordinal = i;
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

/// `--similar <path-or-id>`: rank embedded files by cosine similarity to the
/// seed file's CLIP image embedding.
fn run_similar(ctx: &Ctx, seed: &str, limit: usize) -> Result<()> {
    ctx.require_db_exists()?;
    let conn = fileid_engine::db::open_read(&ctx.db)?;

    let Some(seed_id) = resolve_file_id(&conn, seed) else {
        return absent(
            ctx,
            "not_found",
            &format!("no indexed file matches {seed}"),
            None,
        );
    };

    let seed_vec: Option<Vec<f32>> = conn
        .query_row(
            "SELECT embedding FROM clip_embeddings WHERE file_id = ?1",
            params![seed_id],
            |r| r.get::<_, Vec<u8>>(0),
        )
        .ok()
        .map(|b| decode_embedding(&b));

    let Some(seed_vec) = seed_vec.filter(|v| !v.is_empty()) else {
        // Distinguish "no embeddings at all" (model-free library) from "this
        // particular file wasn't embedded".
        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM clip_embeddings", [], |r| r.get(0))
            .unwrap_or(0);
        let (kind, msg) = if total == 0 {
            (
                "no_embeddings",
                "no CLIP embeddings in this library — run `fileid scan --models` (or a desktop \
                 scan) to populate them, then retry"
                    .to_string(),
            )
        } else {
            (
                "seed_not_embedded",
                format!("file #{seed_id} has no CLIP embedding (it may be a non-image, or predate the model scan)"),
            )
        };
        return absent(ctx, kind, &msg, Some(seed_id));
    };
    let seed_norm = norm(&seed_vec);
    if seed_norm == 0.0 {
        return absent(
            ctx,
            "seed_not_embedded",
            "the seed embedding is degenerate (zero vector)",
            Some(seed_id),
        );
    }

    let mut stmt = conn.prepare(
        "SELECT f.id, f.path_text, f.kind, f.size_bytes, e.embedding \
         FROM clip_embeddings e JOIN files f ON f.id = e.file_id \
         WHERE e.file_id <> ?1",
    )?;
    let rows = stmt.query_map(params![seed_id], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, Vec<u8>>(4)?,
        ))
    })?;
    let mut top = BinaryHeap::with_capacity(limit.saturating_add(1));
    for (id, path, kind, size, blob) in rows.flatten() {
        let v = decode_embedding(&blob);
        let n = norm(&v);
        if v.len() != seed_vec.len() || n == 0.0 {
            continue;
        }
        let score = dot(&seed_vec, &v) / (seed_norm * n);
        if !score.is_finite() {
            continue;
        }
        keep_top_k(
            &mut top,
            ScoredHit {
                score,
                hit: Hit {
                    id,
                    path,
                    kind,
                    size,
                    source: "similar",
                    snippet: None,
                    ordinal: 0,
                },
            },
            limit,
        );
    }
    let mut scored: Vec<(f32, Hit)> = top.into_iter().map(|Reverse(v)| (v.score, v.hit)).collect();

    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.id.cmp(&b.1.id)));

    if ctx.json {
        let arr: Vec<serde_json::Value> = scored
            .iter()
            .map(|(cos, h)| {
                serde_json::json!({
                    "id": h.id,
                    "path": h.path,
                    "kind": h.kind,
                    "sizeBytes": h.size,
                    "similarity": cos,
                })
            })
            .collect();
        print_json(&serde_json::json!({
            "command": "search",
            "mode": "similar",
            "seedId": seed_id,
            "count": arr.len(),
            "results": arr,
        }));
        return Ok(());
    }

    if scored.is_empty() {
        println!("No other embedded files to compare against.");
        return Ok(());
    }
    println!(
        "{} file(s) most similar to {}:",
        scored.len(),
        ctx.bold(seed)
    );
    for (cos, h) in &scored {
        println!(
            "  {}  {}",
            ctx.bold(&display_path(&h.path)),
            ctx.dim(&format!(
                "[{}, {}, cos {:.3}]",
                h.kind,
                human_size(h.size),
                cos
            ))
        );
    }
    Ok(())
}

/// CLIP embeddings are stored as little-endian f32 BLOBs (mirror of the
/// engine's `floats_to_le_bytes`).
fn decode_embedding(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn norm(v: &[f32]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
}

fn absent(ctx: &Ctx, kind: &str, msg: &str, seed_id: Option<i64>) -> Result<()> {
    if ctx.json {
        print_json(&serde_json::json!({
            "command": "search",
            "mode": "similar",
            "error": kind,
            "message": msg,
            "seedId": seed_id,
        }));
    } else {
        println!("{}", ctx.bold("Similarity search unavailable."));
        println!("  {msg}.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_k_similarity_memory_is_bounded_and_ordered() {
        let mut heap = BinaryHeap::new();
        for id in 0..100_000 {
            keep_top_k(
                &mut heap,
                ScoredHit {
                    score: id as f32,
                    hit: Hit {
                        id,
                        path: String::new(),
                        kind: String::new(),
                        size: 0,
                        source: "similar",
                        snippet: None,
                        ordinal: 0,
                    },
                },
                25,
            );
            assert!(heap.len() <= 25);
        }
        let mut scores: Vec<f32> = heap.into_iter().map(|Reverse(v)| v.score).collect();
        scores.sort_by(|a, b| b.total_cmp(a));
        assert_eq!(scores.first().copied(), Some(99_999.0));
        assert_eq!(scores.last().copied(), Some(99_975.0));
    }
}
