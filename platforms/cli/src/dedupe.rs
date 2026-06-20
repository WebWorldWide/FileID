//! `fileid dedupe [--exact|--similar]` — list duplicate / near-duplicate
//! groups. Read-only (never trashes anything).
//!
//! - `--exact`   groups by BLAKE3 `content_hash` (byte-identical files).
//! - `--similar` groups by perceptual-hash Hamming distance (default ≤ 8,
//!   mirroring the engine's near-dup threshold). `--threshold` overrides it.
//!
//! Both signals are written by the full engine scan pipeline; the CLI's
//! model-free `scan` does not compute them, so on a CLI-only-indexed library
//! these report "no … in DB" until a full engine scan has run.

use std::collections::BTreeMap;

use anyhow::Result;
use rusqlite::params;

use crate::context::{display_path, human_size, print_json, Ctx};

pub fn run(ctx: &Ctx, exact: bool, similar: bool, threshold: u32) -> Result<()> {
    ctx.require_db_exists()?;
    let conn = fileid_engine::db::open_read(&ctx.db)?;

    // Default to exact when neither flag is given.
    let (do_exact, do_similar) = if !exact && !similar {
        (true, false)
    } else {
        (exact, similar)
    };

    let mut json_sections = serde_json::Map::new();

    if do_exact {
        let groups = exact_groups(&conn)?;
        if ctx.json {
            json_sections.insert("exact".into(), exact_json(&groups));
        } else {
            render_exact(ctx, &groups);
        }
    }
    if do_similar {
        let groups = similar_groups(&conn, threshold)?;
        if ctx.json {
            json_sections.insert("similar".into(), similar_json(&groups, threshold));
        } else {
            render_similar(ctx, &groups, threshold);
        }
    }

    if ctx.json {
        print_json(&serde_json::json!({
            "command": "dedupe",
            "groups": json_sections,
        }));
    }
    Ok(())
}

// ---- exact (content_hash) ----------------------------------------------------

struct ExactGroup {
    hash: String,
    files: Vec<(String, i64)>, // (path, size)
}

fn exact_groups(conn: &rusqlite::Connection) -> Result<Option<Vec<ExactGroup>>> {
    let total: i64 =
        conn.query_row("SELECT COUNT(*) FROM files WHERE content_hash IS NOT NULL", [], |r| {
            r.get(0)
        })?;
    if total == 0 {
        return Ok(None);
    }
    let mut stmt = conn.prepare(
        "SELECT lower(hex(content_hash)) AS h, path_text, size_bytes \
         FROM files WHERE content_hash IS NOT NULL ORDER BY h, path_text",
    )?;
    let mut buckets: BTreeMap<String, Vec<(String, i64)>> = BTreeMap::new();
    let rows = stmt.query_map(params![], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
    })?;
    for row in rows.flatten() {
        buckets.entry(row.0).or_default().push((row.1, row.2));
    }
    let groups = buckets
        .into_iter()
        .filter(|(_, v)| v.len() > 1)
        .map(|(hash, files)| ExactGroup { hash, files })
        .collect();
    Ok(Some(groups))
}

fn render_exact(ctx: &Ctx, groups: &Option<Vec<ExactGroup>>) {
    match groups {
        None => {
            println!("{}", ctx.bold("Exact duplicates: none computed."));
            ctx.progress(&format!(
                "  {}",
                ctx.dim("no content hashes in DB — run a full engine scan to populate them")
            ));
        }
        Some(groups) if groups.is_empty() => {
            println!("{}", ctx.bold("Exact duplicates: none."));
        }
        Some(groups) => {
            println!("{} exact-duplicate group(s):", groups.len());
            for g in groups {
                println!(
                    "  {} {}",
                    ctx.bold(&format!("[{}]", &g.hash[..g.hash.len().min(12)])),
                    ctx.dim(&format!("{} copies, {}", g.files.len(), human_size(g.files[0].1)))
                );
                for (path, _) in &g.files {
                    println!("      {}", display_path(path));
                }
            }
        }
    }
}

fn exact_json(groups: &Option<Vec<ExactGroup>>) -> serde_json::Value {
    match groups {
        None => serde_json::json!({ "available": false }),
        Some(groups) => serde_json::json!({
            "available": true,
            "count": groups.len(),
            "groups": groups.iter().map(|g| serde_json::json!({
                "contentHash": g.hash,
                "copies": g.files.len(),
                "sizeBytes": g.files[0].1,
                "files": g.files.iter().map(|(p, _)| p).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        }),
    }
}

// ---- near-duplicate (phash Hamming) -----------------------------------------

struct SimilarGroup {
    files: Vec<(i64, String)>, // (id, path)
}

fn similar_groups(conn: &rusqlite::Connection, threshold: u32) -> Result<Option<Vec<SimilarGroup>>> {
    let mut stmt =
        conn.prepare("SELECT id, path_text, phash FROM files WHERE phash IS NOT NULL")?;
    let rows: Vec<(i64, String, i64)> = stmt
        .query_map(params![], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .filter_map(Result::ok)
        .collect();
    if rows.is_empty() {
        return Ok(None);
    }

    // Union-find: union any pair within `threshold` Hamming distance; report
    // connected components of size > 1.
    let n = rows.len();
    let mut parent: Vec<usize> = (0..n).collect();
    for i in 0..n {
        for j in (i + 1)..n {
            let dist = (rows[i].2 ^ rows[j].2).count_ones();
            if dist <= threshold {
                union(&mut parent, i, j);
            }
        }
    }
    let mut comps: BTreeMap<usize, Vec<(i64, String)>> = BTreeMap::new();
    for (idx, row) in rows.iter().enumerate() {
        let root = find(&mut parent, idx);
        comps.entry(root).or_default().push((row.0, row.1.clone()));
    }
    let groups = comps
        .into_values()
        .filter(|v| v.len() > 1)
        .map(|files| SimilarGroup { files })
        .collect();
    Ok(Some(groups))
}

fn find(parent: &mut [usize], mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]];
        x = parent[x];
    }
    x
}

fn union(parent: &mut [usize], a: usize, b: usize) {
    let ra = find(parent, a);
    let rb = find(parent, b);
    if ra != rb {
        parent[ra] = rb;
    }
}

fn render_similar(ctx: &Ctx, groups: &Option<Vec<SimilarGroup>>, threshold: u32) {
    match groups {
        None => {
            println!("{}", ctx.bold("Near-duplicates: none computed."));
            ctx.progress(&format!(
                "  {}",
                ctx.dim("no perceptual hashes in DB — run a full engine scan to populate them")
            ));
        }
        Some(groups) if groups.is_empty() => {
            println!("Near-duplicates (≤{threshold} bits): none.");
        }
        Some(groups) => {
            println!("{} near-duplicate group(s) (≤{threshold} bits):", groups.len());
            for (i, g) in groups.iter().enumerate() {
                println!("  {} {}", ctx.bold(&format!("group {}", i + 1)), ctx.dim(&format!("{} files", g.files.len())));
                for (_, path) in &g.files {
                    println!("      {}", display_path(path));
                }
            }
        }
    }
}

fn similar_json(groups: &Option<Vec<SimilarGroup>>, threshold: u32) -> serde_json::Value {
    match groups {
        None => serde_json::json!({ "available": false, "threshold": threshold }),
        Some(groups) => serde_json::json!({
            "available": true,
            "threshold": threshold,
            "count": groups.len(),
            "groups": groups.iter().map(|g| serde_json::json!({
                "size": g.files.len(),
                "files": g.files.iter().map(|(id, p)| serde_json::json!({"id": id, "path": p})).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        }),
    }
}
