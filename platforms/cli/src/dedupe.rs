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
use std::path::PathBuf;

use anyhow::Result;
use rusqlite::params;

use crate::context::{display_path, human_size, print_json, Ctx};

#[allow(clippy::fn_params_excessive_bools, clippy::too_many_arguments)]
pub fn run(
    ctx: &Ctx,
    exact: bool,
    similar: bool,
    threshold: u32,
    apply: bool,
    dry_run: bool,
    delete: bool,
    yes: bool,
) -> Result<()> {
    ctx.require_db_exists()?;

    // Default to exact when neither flag is given.
    let (do_exact, do_similar) = if !exact && !similar {
        (true, false)
    } else {
        (exact, similar)
    };

    // `--apply` (and a bare `--dry-run` preview) take the destructive path.
    if apply || dry_run {
        return apply_run(ctx, do_exact, do_similar, threshold, dry_run, delete, yes);
    }

    let conn = fileid_engine::db::open_read(&ctx.db)?;
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

// ---- apply (remove duplicates) ----------------------------------------------

/// One file slated for removal (the non-kept members of a duplicate group).
struct Victim {
    id: i64,
    path: String,
    size: i64,
}

/// Outcome of a victim query: whether the underlying dedupe signal even exists
/// in the DB (else: model-free library), plus the files to remove.
struct VictimSet {
    available: bool,
    victims: Vec<Victim>,
}

#[allow(clippy::fn_params_excessive_bools)]
fn apply_run(
    ctx: &Ctx,
    do_exact: bool,
    do_similar: bool,
    threshold: u32,
    dry_run: bool,
    delete: bool,
    yes: bool,
) -> Result<()> {
    let conn = fileid_engine::db::open_read(&ctx.db)?;

    // Destructive apply acts on exactly one signal. Default exact (byte-
    // identical) — the safest; `--similar` opts into perceptual near-dups.
    let use_similar = do_similar && !do_exact;
    let set = if use_similar {
        similar_victims(&conn, threshold)?
    } else {
        exact_victims(&conn)?
    };
    let signal = if use_similar { "perceptual hashes" } else { "content hashes" };

    if !set.available {
        if ctx.json {
            print_json(&serde_json::json!({
                "command": "dedupe", "mode": "apply", "available": false,
                "message": format!("no {signal} in this library"),
                "hint": "run `fileid scan --models` (or a desktop scan) to compute the dedupe signal",
            }));
        } else {
            println!("{}", ctx.bold("Nothing to de-duplicate."));
            println!(
                "  No {signal} in this library — run `fileid scan --models` (or a desktop scan) first."
            );
        }
        return Ok(());
    }

    let victims = set.victims;
    let total_bytes: i64 = victims.iter().map(|v| v.size).sum();

    if victims.is_empty() {
        if ctx.json {
            print_json(&serde_json::json!({
                "command": "dedupe", "mode": "apply", "available": true,
                "removeCount": 0, "reclaimBytes": 0, "dryRun": dry_run,
            }));
        } else {
            println!("{}", ctx.bold("No duplicates found — nothing to remove."));
        }
        return Ok(());
    }

    let method = if delete { "delete permanently" } else { "move to Trash/Recycle Bin" };
    if ctx.json && dry_run {
        print_json(&serde_json::json!({
            "command": "dedupe", "mode": "apply", "dryRun": true, "available": true,
            "method": if delete { "delete" } else { "trash" },
            "removeCount": victims.len(),
            "reclaimBytes": total_bytes,
            "remove": victims.iter().map(|v| serde_json::json!({
                "id": v.id, "path": v.path, "sizeBytes": v.size,
            })).collect::<Vec<_>>(),
        }));
        return Ok(());
    }

    println!(
        "{} {} file(s) would {} ({} reclaimable):",
        if dry_run { ctx.bold("DRY RUN —") } else { ctx.bold("Will") },
        victims.len(),
        method,
        human_size(total_bytes),
    );
    for v in &victims {
        println!("  {}  {}", display_path(&v.path), ctx.dim(&human_size(v.size)));
    }

    if dry_run {
        ctx.progress(&format!("  {}", ctx.dim("dry run — nothing was removed.")));
        return Ok(());
    }

    let prompt = format!(
        "{} {} file(s) ({})? {}",
        if delete { "Permanently delete" } else { "Trash" },
        victims.len(),
        human_size(total_bytes),
        if delete { "This CANNOT be undone." } else { "(recoverable from Trash)" },
    );
    if !ctx.confirm(&prompt, yes) {
        println!("Aborted — no files removed. {}", ctx.dim("(pass --yes to skip the prompt)"));
        return Ok(());
    }

    drop(conn);
    let mut wconn = fileid_engine::db::open_writer(&ctx.db)?;
    let result = remove_victims(&mut wconn, &victims, delete)?;

    if ctx.json {
        print_json(&serde_json::json!({
            "command": "dedupe", "mode": "apply", "dryRun": false,
            "method": if delete { "delete" } else { "trash" },
            "removed": result.removed,
            "failed": result.failed,
            "unsupported": result.unsupported,
            "reclaimBytes": result.reclaimed,
        }));
        return Ok(());
    }

    println!("{}", ctx.bold("Dedupe apply complete."));
    println!("  Removed:     {}", result.removed);
    println!("  Reclaimed:   {}", human_size(result.reclaimed));
    if result.failed > 0 {
        println!("  Failed:      {}", result.failed);
    }
    if result.unsupported > 0 {
        println!(
            "  {} {} file(s) could not be trashed on this platform.",
            ctx.bold("Note:"),
            result.unsupported
        );
        println!(
            "  {}",
            ctx.dim("Trash is unavailable here — re-run with --delete to remove permanently.")
        );
    }
    Ok(())
}

struct ApplyResult {
    removed: usize,
    failed: usize,
    unsupported: usize,
    reclaimed: i64,
}

/// Trash (default) or permanently delete each victim, then drop its `files`
/// row (mirroring the engine's `trashFiles` handler: filesystem op first, DB
/// row removed only for the ones that actually left disk). FTS / embedding rows
/// cascade via the schema's triggers + `ON DELETE CASCADE`.
fn remove_victims(
    conn: &mut rusqlite::Connection,
    victims: &[Victim],
    delete: bool,
) -> Result<ApplyResult> {
    let outcomes: Vec<bool> = if delete {
        victims
            .iter()
            .map(|v| std::fs::remove_file(&v.path).is_ok())
            .collect()
    } else {
        let paths: Vec<PathBuf> = victims.iter().map(|v| PathBuf::from(&v.path)).collect();
        fileid_engine::shell::trash::trash(&paths)
    };

    let mut result = ApplyResult { removed: 0, failed: 0, unsupported: 0, reclaimed: 0 };
    let tx = conn.transaction()?;
    for (v, ok) in victims.iter().zip(outcomes) {
        if ok {
            tx.execute("DELETE FROM files WHERE id = ?1", params![v.id])?;
            result.removed += 1;
            result.reclaimed += v.size;
        } else if delete {
            result.failed += 1;
        } else {
            result.unsupported += 1;
        }
    }
    tx.commit()?;
    Ok(result)
}

/// Exact-duplicate victims: every byte-identical copy beyond the first
/// (kept) one. Keeper = lexicographically-first path (deterministic).
fn exact_victims(conn: &rusqlite::Connection) -> Result<VictimSet> {
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM files WHERE content_hash IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    if total == 0 {
        return Ok(VictimSet { available: false, victims: Vec::new() });
    }
    let mut stmt = conn.prepare(
        "SELECT lower(hex(content_hash)) AS h, id, path_text, size_bytes \
         FROM files WHERE content_hash IS NOT NULL ORDER BY h, path_text",
    )?;
    let mut buckets: BTreeMap<String, Vec<(i64, String, i64)>> = BTreeMap::new();
    let rows = stmt.query_map(params![], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?))
    })?;
    for row in rows.flatten() {
        buckets.entry(row.0).or_default().push((row.1, row.2, row.3));
    }
    let victims = buckets
        .into_values()
        .filter(|v| v.len() > 1)
        .flat_map(|v| v.into_iter().skip(1)) // keep first, remove the rest
        .map(|(id, path, size)| Victim { id, path, size })
        .collect();
    Ok(VictimSet { available: true, victims })
}

/// Near-duplicate victims: every member of each phash-connected component
/// beyond the first (kept). Keeper = lowest file id in the component.
fn similar_victims(conn: &rusqlite::Connection, threshold: u32) -> Result<VictimSet> {
    let set = similar_groups(conn, threshold)?;
    let Some(groups) = set else {
        return Ok(VictimSet { available: false, victims: Vec::new() });
    };
    let mut size_stmt = conn.prepare("SELECT size_bytes FROM files WHERE id = ?1")?;
    let victims = groups
        .into_iter()
        .flat_map(|g| {
            let mut files = g.files;
            files.sort_by_key(|(id, _)| *id);
            files.into_iter().skip(1)
        })
        .map(|(id, path)| {
            let size = size_stmt.query_row(params![id], |r| r.get::<_, i64>(0)).unwrap_or(0);
            Victim { id, path, size }
        })
        .collect();
    Ok(VictimSet { available: true, victims })
}
