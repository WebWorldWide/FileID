//! `fileid restructure --plan [root]` — compute and print the proposed reorg.
//!
//! READ-ONLY. This reuses the engine's exact rule-cascade classifier
//! (`pipeline::restructure::classify`, a pure model-free function), so the
//! plan matches what the desktop apps' Restructure tab would propose when no
//! CLIP embeddings are present (the semantic "butler" boost degrades to the
//! same cascade). Applying the plan (`applyRestructure`) is a documented
//! follow-on and is intentionally NOT implemented here.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use fileid_engine::ipc::RestructureMove;
use fileid_engine::pipeline::discovery::FileKind;
use fileid_engine::pipeline::restructure::{
    category_counts, classify, FileForClassify, ProposedMove,
};
use fileid_engine::pipeline::restructure_apply::RestructureApply;
use parking_lot::Mutex;
use rusqlite::params;

use crate::context::{print_json, Ctx};

const MAX_PRINTED_MOVES: usize = 200;

const PLAN_FILES_SQL: &str = "\
SELECT f.id, f.path_text, f.kind, f.modified_at, f.created_at, f.location_lat, f.location_lon, f.has_text,
    (SELECT p.name FROM face_prints fp JOIN persons p ON p.id = fp.person_id
     WHERE fp.file_id = f.id AND p.name IS NOT NULL AND TRIM(p.name) <> '' LIMIT 1) AS person_name
FROM files f WHERE f.failed = 0";

pub fn run(
    ctx: &Ctx,
    plan: bool,
    apply: bool,
    dry_run: bool,
    symlinks: bool,
    yes: bool,
    root: Option<PathBuf>,
) -> Result<()> {
    if plan && apply {
        anyhow::bail!("--plan and --apply are mutually exclusive; choose one");
    }
    if !plan && !apply {
        anyhow::bail!(
            "specify --plan (read-only) or --apply (execute). See `fileid restructure --help`."
        );
    }
    ctx.require_db_exists()?;
    let conn = fileid_engine::db::open_read(&ctx.db)?;

    let mut stmt = conn.prepare(PLAN_FILES_SQL)?;
    let files: Vec<FileForClassify> = stmt
        .query_map(params![], |r| {
            let kind_str: String = r.get(2)?;
            Ok(FileForClassify {
                file_id: r.get(0)?,
                source: PathBuf::from(r.get::<_, String>(1)?),
                kind: kind_from_str(&kind_str),
                modified_unix: r.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
                created_unix: r.get(4)?,
                person_name: r
                    .get::<_, Option<String>>(8)?
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty()),
                location_lat: r.get(5)?,
                location_lon: r.get(6)?,
                has_text: r.get::<_, Option<i64>>(7)?.unwrap_or(0) != 0,
            })
        })?
        .filter_map(Result::ok)
        .collect();

    if files.is_empty() {
        if ctx.json {
            print_json(&serde_json::json!({
                "command": "restructure", "plan": true, "fileCount": 0, "moves": [],
            }));
        } else {
            println!("Library is empty — nothing to restructure. Run `fileid scan <path>` first.");
        }
        return Ok(());
    }

    let library_root = match root {
        Some(r) => std::fs::canonicalize(&r).unwrap_or(r),
        None => common_ancestor(files.iter().map(|f| f.source.as_path()))
            .unwrap_or_else(|| PathBuf::from(".")),
    };

    let moves = classify(&files, &library_root);
    let counts = category_counts(&moves);

    if apply {
        drop(stmt);
        drop(conn);
        return apply_moves(ctx, &library_root, &moves, dry_run, symlinks, yes);
    }

    if ctx.json {
        let moves_json: Vec<serde_json::Value> = moves
            .iter()
            .map(|m| {
                serde_json::json!({
                    "fileId": m.file_id,
                    "source": m.source.to_string_lossy(),
                    "destination": m.destination.to_string_lossy(),
                    "category": m.category,
                    "confidence": m.confidence.as_str(),
                    "reason": m.reason,
                })
            })
            .collect();
        let counts_json: Vec<serde_json::Value> = counts
            .iter()
            .map(|c| serde_json::json!({ "category": c.category, "count": c.count }))
            .collect();
        print_json(&serde_json::json!({
            "command": "restructure",
            "plan": true,
            "libraryRoot": library_root.to_string_lossy(),
            "fileCount": files.len(),
            "moveCount": moves.len(),
            "categories": counts_json,
            "moves": moves_json,
        }));
        return Ok(());
    }

    println!("{}", ctx.bold("Proposed restructure (read-only plan):"));
    println!("  Library root: {}", library_root.display());
    println!("  Files:        {}", files.len());
    println!("  Moves:        {}", moves.len());
    println!("  {}", ctx.bold("By category:"));
    for c in &counts {
        println!("    {:<22} {}", c.category, c.count);
    }
    println!("  {}", ctx.bold("Moves:"));
    for m in moves.iter().take(MAX_PRINTED_MOVES) {
        let src = m
            .source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| m.source.to_string_lossy().into_owned());
        let dest = rel_to(&library_root, &m.destination);
        println!(
            "    {}  {}  {}",
            src,
            ctx.dim("→"),
            dest
        );
        if let Some(reason) = &m.reason {
            println!(
                "        {}",
                ctx.dim(&format!("{} · {}", m.confidence.as_str(), reason))
            );
        }
    }
    if moves.len() > MAX_PRINTED_MOVES {
        println!(
            "    {}",
            ctx.dim(&format!("… and {} more (use --json for the full plan)", moves.len() - MAX_PRINTED_MOVES))
        );
    }
    ctx.progress(&format!(
        "  {}",
        ctx.dim("plan only — nothing moved. Apply (applyRestructure) is a documented follow-on.")
    ));
    Ok(())
}

fn kind_from_str(s: &str) -> FileKind {
    match s {
        "image" => FileKind::Image,
        "video" => FileKind::Video,
        "pdf" => FileKind::Pdf,
        "doc" => FileKind::Doc,
        "audio" => FileKind::Audio,
        "model" => FileKind::Model,
        _ => FileKind::Other,
    }
}

fn rel_to(root: &Path, dest: &Path) -> String {
    dest.strip_prefix(root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| dest.to_string_lossy().into_owned())
}

/// Deepest directory containing every path. Returns the parent of a single
/// file, or the common component prefix of many.
fn common_ancestor<'a>(paths: impl Iterator<Item = &'a Path>) -> Option<PathBuf> {
    let mut prefix: Option<Vec<Component<'a>>> = None;
    let mut count = 0usize;
    for p in paths {
        count += 1;
        let comps: Vec<Component> = p.components().collect();
        prefix = Some(match prefix {
            None => comps,
            Some(prev) => prev
                .into_iter()
                .zip(comps)
                .take_while(|(a, b)| a == b)
                .map(|(a, _)| a)
                .collect(),
        });
    }
    let comps = prefix?;
    let mut out = PathBuf::new();
    for c in comps {
        out.push(c.as_os_str());
    }
    // A single path collapses to itself (a file); use its parent.
    if count == 1 {
        out = out.parent().map(Path::to_path_buf).unwrap_or(out);
    }
    Some(out)
}

// ---- apply -------------------------------------------------------------------

/// Execute (or, with `dry_run`, preview) the proposed moves via the engine's
/// exact `RestructureApply` — the same code path the `applyRestructure` IPC
/// command runs (collision-uniquify, stale-plan + path-traversal guards, undo
/// journal). In-process and cross-platform (MoveFileExW on Windows;
/// `std::fs::rename` elsewhere). `--symlinks` previews the layout without
/// relocating originals.
fn apply_moves(
    ctx: &Ctx,
    library_root: &Path,
    proposed: &[ProposedMove],
    dry_run: bool,
    symlinks: bool,
    yes: bool,
) -> Result<()> {
    if proposed.is_empty() {
        if ctx.json {
            print_json(&serde_json::json!({
                "command": "restructure", "mode": "apply", "moveCount": 0, "dryRun": dry_run,
            }));
        } else {
            println!("{}", ctx.bold("Nothing to move — the library is already organized."));
        }
        return Ok(());
    }

    let moves: Vec<RestructureMove> = proposed.iter().map(to_ipc_move).collect();
    let verb = if symlinks { "symlink" } else { "move" };

    if ctx.json && dry_run {
        print_json(&serde_json::json!({
            "command": "restructure", "mode": "apply", "dryRun": true,
            "useSymlinks": symlinks,
            "libraryRoot": library_root.to_string_lossy(),
            "moveCount": moves.len(),
            "moves": moves.iter().map(|m| serde_json::json!({
                "fileId": m.file_id, "source": m.source, "destination": m.destination,
                "category": m.category, "confidence": m.confidence, "reason": m.reason,
            })).collect::<Vec<_>>(),
        }));
        return Ok(());
    }

    println!(
        "{} {} file(s) into {}:",
        if dry_run {
            ctx.bold("DRY RUN — would")
        } else {
            ctx.bold(&format!("Will {verb}"))
        },
        moves.len(),
        library_root.display(),
    );
    for m in moves.iter().take(MAX_PRINTED_MOVES) {
        let src = Path::new(&m.source)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| m.source.clone());
        let dest = rel_to(library_root, Path::new(&m.destination));
        println!("    {}  {}  {}", src, ctx.dim("→"), dest);
    }
    if moves.len() > MAX_PRINTED_MOVES {
        println!(
            "    {}",
            ctx.dim(&format!("… and {} more", moves.len() - MAX_PRINTED_MOVES))
        );
    }

    if dry_run {
        ctx.progress(&format!("  {}", ctx.dim("dry run — nothing was moved.")));
        return Ok(());
    }

    let prompt = format!(
        "{} {} file(s) under {}?{}",
        if symlinks { "Create symlinks for" } else { "Move" },
        moves.len(),
        library_root.display(),
        if symlinks {
            ""
        } else {
            " Originals are relocated (an undo journal is written)."
        },
    );
    if !ctx.confirm(&prompt, yes) {
        println!(
            "Aborted — nothing moved. {}",
            ctx.dim("(pass --yes to skip the prompt)")
        );
        return Ok(());
    }

    let conn = fileid_engine::db::open_writer(&ctx.db)?;
    let applier =
        RestructureApply::new(Arc::new(Mutex::new(conn)), library_root.to_path_buf(), symlinks);
    let result = applier.apply(&moves)?;

    if ctx.json {
        print_json(&serde_json::json!({
            "command": "restructure", "mode": "apply", "dryRun": false,
            "useSymlinks": symlinks,
            "applied": result.applied,
            "failed": result.failed,
            "privilegeError": result.privilege_error,
        }));
        return Ok(());
    }
    println!("{}", ctx.bold("Restructure apply complete."));
    println!("  Applied:  {}", result.applied);
    if result.failed > 0 {
        println!("  Failed:   {}", result.failed);
    }
    if let Some(pe) = &result.privilege_error {
        println!("  {} {}", ctx.bold("Symlink privilege:"), pe);
    }
    Ok(())
}

fn to_ipc_move(pm: &ProposedMove) -> RestructureMove {
    RestructureMove {
        file_id: pm.file_id,
        source: pm.source.to_string_lossy().into_owned(),
        destination: pm.destination.to_string_lossy().into_owned(),
        category: pm.category.clone(),
        tier: None,
        confidence: pm.confidence.as_str().to_string(),
        reason: pm.reason.clone(),
    }
}
