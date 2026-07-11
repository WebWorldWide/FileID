//! `fileid restructure --plan [root]` — compute the proposed reorg; with
//! `--apply`, execute it.
//!
//! `--plan` is READ-ONLY: it reuses the engine's exact rule-cascade classifier
//! (`pipeline::restructure::classify`, a pure model-free function), so the
//! plan matches what the desktop apps' Restructure tab would propose when no
//! CLIP embeddings are present (the semantic "butler" boost degrades to the
//! same cascade). `--apply` executes that plan in-process via the engine's
//! `RestructureApply` (the same code path the `applyRestructure` IPC command
//! runs: collision-uniquify, stale-plan + path-traversal guards, undo journal);
//! it prompts unless `--yes`, and `--apply --dry-run` previews without moving.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
#[cfg(test)]
use std::path::Component;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use fileid_engine::ipc::RestructureMove;
use fileid_engine::pipeline::discovery::FileKind;
use fileid_engine::pipeline::restructure::{classify, FileForClassify, ProposedMove};
use fileid_engine::pipeline::restructure_apply::RestructureApply;
use parking_lot::Mutex;
use rusqlite::params;
use serde::ser::{Error as _, SerializeSeq};
use serde::{Serialize, Serializer};

use crate::context::{print_json, Ctx};

const MAX_PRINTED_MOVES: usize = 200;
const CLASSIFY_CHUNK: usize = 4_096;

const PLAN_FILES_SQL: &str = "\
SELECT f.id, f.path_text, f.kind, f.modified_at, f.created_at, f.location_lat, f.location_lon, f.has_text,
    (SELECT p.name FROM face_prints fp JOIN persons p ON p.id = fp.person_id
     WHERE fp.file_id = f.id AND p.name IS NOT NULL AND TRIM(p.name) <> '' LIMIT 1) AS person_name
FROM files f WHERE f.failed = 0";

#[derive(Debug)]
struct PlanSpool {
    path: PathBuf,
}

impl PlanSpool {
    fn create() -> Result<(Self, BufWriter<File>)> {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fileid-restructure-{}-{nonce}-{seq}.ndjson",
            std::process::id()
        ));
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("creating restructure spool {}", path.display()))?;
        Ok((Self { path }, BufWriter::new(file)))
    }

    fn iter(&self) -> Result<SpoolMoveIter> {
        let file = File::open(&self.path)
            .with_context(|| format!("opening restructure spool {}", self.path.display()))?;
        Ok(SpoolMoveIter {
            lines: BufReader::new(file).lines(),
        })
    }
}

impl Drop for PlanSpool {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

struct SpoolMoveIter {
    lines: std::io::Lines<BufReader<File>>,
}

impl Iterator for SpoolMoveIter {
    type Item = Result<RestructureMove>;

    fn next(&mut self) -> Option<Self::Item> {
        self.lines.next().map(|line| {
            let line = line.context("reading restructure spool")?;
            serde_json::from_str(&line).context("decoding restructure spool row")
        })
    }
}

struct SpoolMoves<'a>(&'a PlanSpool);

impl Serialize for SpoolMoves<'_> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let iter = self.0.iter().map_err(S::Error::custom)?;
        let mut seq = serializer.serialize_seq(None)?;
        for move_result in iter {
            let m = move_result.map_err(S::Error::custom)?;
            let item = serde_json::json!({
                "fileId": m.file_id,
                "source": m.source,
                "destination": m.destination,
                "category": m.category,
                "confidence": m.confidence,
                "reason": m.reason,
            });
            seq.serialize_element(&item)?;
        }
        seq.end()
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CategoryReport {
    category: String,
    count: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PlanReport<'a> {
    command: &'static str,
    plan: bool,
    library_root: String,
    file_count: u64,
    move_count: u64,
    categories: &'a [CategoryReport],
    moves: SpoolMoves<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DryRunReport<'a> {
    command: &'static str,
    mode: &'static str,
    dry_run: bool,
    use_symlinks: bool,
    library_root: String,
    move_count: u64,
    moves: SpoolMoves<'a>,
}

#[derive(Clone, Copy)]
struct ApplyOptions {
    dry_run: bool,
    symlinks: bool,
    yes: bool,
}

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

    let file_count = conn
        .query_row("SELECT COUNT(*) FROM files WHERE failed = 0", [], |r| {
            r.get::<_, i64>(0)
        })?
        .max(0) as u64;
    if file_count == 0 {
        if ctx.json {
            print_json(&serde_json::json!({
                "command": "restructure", "plan": true, "fileCount": 0, "moves": [],
            }));
        } else {
            println!("Library is empty — nothing to restructure. Run `fileid scan <path>` first.");
        }
        return Ok(());
    }

    let library_root = resolve_library_root(&conn, root)?;
    let needs_spool = apply || ctx.json;
    let (spool, mut spool_writer) = if needs_spool {
        let (spool, writer) = PlanSpool::create()?;
        (Some(spool), Some(writer))
    } else {
        (None, None)
    };
    let mut counts: HashMap<String, u64> = HashMap::new();
    let mut sample = Vec::with_capacity(MAX_PRINTED_MOVES);
    let mut move_count = 0_u64;
    let mut chunk = Vec::with_capacity(CLASSIFY_CHUNK);
    let mut stmt = conn.prepare(PLAN_FILES_SQL)?;
    let mut rows = stmt.query(params![])?;
    while let Some(row) = rows.next()? {
        chunk.push(row_to_file(row)?);
        if chunk.len() >= CLASSIFY_CHUNK {
            absorb_chunk(
                &mut chunk,
                &library_root,
                &mut counts,
                &mut sample,
                &mut move_count,
                &mut spool_writer,
            )?;
        }
    }
    absorb_chunk(
        &mut chunk,
        &library_root,
        &mut counts,
        &mut sample,
        &mut move_count,
        &mut spool_writer,
    )?;
    if let Some(mut writer) = spool_writer {
        writer.flush().context("flushing restructure spool")?;
    }
    drop(rows);
    drop(stmt);
    drop(conn);

    let mut counts: Vec<CategoryReport> = counts
        .into_iter()
        .map(|(category, count)| CategoryReport { category, count })
        .collect();
    counts.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| a.category.cmp(&b.category))
    });

    if apply {
        return apply_spooled(
            ctx,
            &library_root,
            spool.as_ref().expect("apply always creates a spool"),
            &sample,
            move_count,
            ApplyOptions {
                dry_run,
                symlinks,
                yes,
            },
        );
    }

    if ctx.json {
        let report = PlanReport {
            command: "restructure",
            plan: true,
            library_root: library_root.to_string_lossy().into_owned(),
            file_count,
            move_count,
            categories: &counts,
            moves: SpoolMoves(spool.as_ref().expect("JSON plan always creates a spool")),
        };
        write_json(&report)?;
        return Ok(());
    }

    println!("{}", ctx.bold("Proposed restructure (read-only plan):"));
    println!("  Library root: {}", library_root.display());
    println!("  Files:        {file_count}");
    println!("  Moves:        {move_count}");
    println!("  {}", ctx.bold("By category:"));
    for c in &counts {
        println!("    {:<22} {}", c.category, c.count);
    }
    println!("  {}", ctx.bold("Moves:"));
    for m in &sample {
        let src = m
            .source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| m.source.to_string_lossy().into_owned());
        let dest = rel_to(&library_root, &m.destination);
        println!("    {}  {}  {}", src, ctx.dim("→"), dest);
        if let Some(reason) = &m.reason {
            println!(
                "        {}",
                ctx.dim(&format!("{} · {}", m.confidence.as_str(), reason))
            );
        }
    }
    if move_count > MAX_PRINTED_MOVES as u64 {
        println!(
            "    {}",
            ctx.dim(&format!(
                "… and {} more (use --json for the full plan)",
                move_count - MAX_PRINTED_MOVES as u64
            ))
        );
    }
    ctx.progress(&format!(
        "  {}",
        ctx.dim("plan only — nothing moved. Run with --apply to execute (or --apply --dry-run to preview).")
    ));
    Ok(())
}

fn row_to_file(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileForClassify> {
    let kind_str: String = row.get(2)?;
    Ok(FileForClassify {
        file_id: row.get(0)?,
        source: PathBuf::from(row.get::<_, String>(1)?),
        kind: kind_from_str(&kind_str),
        modified_unix: row.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
        created_unix: row.get(4)?,
        person_name: row
            .get::<_, Option<String>>(8)?
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        location_lat: row.get(5)?,
        location_lon: row.get(6)?,
        has_text: row.get::<_, Option<i64>>(7)?.unwrap_or(0) != 0,
    })
}

fn absorb_chunk(
    files: &mut Vec<FileForClassify>,
    library_root: &Path,
    counts: &mut HashMap<String, u64>,
    sample: &mut Vec<ProposedMove>,
    move_count: &mut u64,
    spool: &mut Option<BufWriter<File>>,
) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    for proposed in classify(files, library_root) {
        *counts.entry(proposed.category.clone()).or_default() += 1;
        *move_count += 1;
        if sample.len() < MAX_PRINTED_MOVES {
            sample.push(proposed.clone());
        }
        if let Some(writer) = spool.as_mut() {
            serde_json::to_writer(&mut *writer, &to_ipc_move(&proposed))
                .context("encoding restructure spool row")?;
            writer
                .write_all(b"\n")
                .context("writing restructure spool row")?;
        }
    }
    files.clear();
    Ok(())
}

fn resolve_library_root(conn: &rusqlite::Connection, root: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(root) = root {
        return Ok(std::fs::canonicalize(&root).unwrap_or(root));
    }

    let mut stmt = conn.prepare("SELECT path_text FROM files WHERE failed = 0")?;
    let mut rows = stmt.query([])?;
    let mut common: Option<PathBuf> = None;
    let mut count = 0_u64;
    while let Some(row) = rows.next()? {
        let path = PathBuf::from(row.get::<_, String>(0)?);
        count += 1;
        common = Some(match common {
            None => path,
            Some(prefix) => common_prefix(&prefix, &path),
        });
    }
    let mut common = common.context("library has no indexed files")?;
    if count == 1 {
        common = common.parent().map(Path::to_path_buf).unwrap_or(common);
    }
    if common.parent().is_none() {
        anyhow::bail!(
            "this library's files span multiple top-level locations with no \
             shared parent folder, so there's no safe root to organize into. \
             Re-run with an explicit destination, e.g. `fileid restructure <ROOT>`."
        );
    }
    Ok(common)
}

fn common_prefix(a: &Path, b: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for (left, right) in a.components().zip(b.components()) {
        if left != right {
            break;
        }
        out.push(left.as_os_str());
    }
    out
}

fn write_json(value: &impl Serialize) -> Result<()> {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    serde_json::to_writer(&mut lock, value).context("writing JSON output")?;
    lock.write_all(b"\n").context("finishing JSON output")?;
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
#[cfg(test)]
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
fn apply_spooled(
    ctx: &Ctx,
    library_root: &Path,
    spool: &PlanSpool,
    sample: &[ProposedMove],
    move_count: u64,
    options: ApplyOptions,
) -> Result<()> {
    let ApplyOptions {
        dry_run,
        symlinks,
        yes,
    } = options;
    if move_count == 0 {
        if ctx.json {
            print_json(&serde_json::json!({
                "command": "restructure", "mode": "apply", "moveCount": 0, "dryRun": dry_run,
            }));
        } else {
            println!(
                "{}",
                ctx.bold("Nothing to move — the library is already organized.")
            );
        }
        return Ok(());
    }

    let verb = if symlinks { "symlink" } else { "move" };

    if ctx.json && dry_run {
        write_json(&DryRunReport {
            command: "restructure",
            mode: "apply",
            dry_run: true,
            use_symlinks: symlinks,
            library_root: library_root.to_string_lossy().into_owned(),
            move_count,
            moves: SpoolMoves(spool),
        })?;
        return Ok(());
    }

    // Human preview only. In --json mode stdout must carry nothing but the
    // final result object (the confirm prompt + progress lines already go to
    // stderr); printing this header/loop there corrupts the JSON for the real
    // `--apply --json` path, which falls through to the result print below.
    if !ctx.json {
        println!(
            "{} {} file(s) into {}:",
            if dry_run {
                ctx.bold("DRY RUN — would")
            } else {
                ctx.bold(&format!("Will {verb}"))
            },
            move_count,
            library_root.display(),
        );
        for m in sample {
            let src = m
                .source
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| m.source.to_string_lossy().into_owned());
            let dest = rel_to(library_root, &m.destination);
            println!("    {}  {}  {}", src, ctx.dim("→"), dest);
        }
        if move_count > MAX_PRINTED_MOVES as u64 {
            println!(
                "    {}",
                ctx.dim(&format!(
                    "… and {} more",
                    move_count - MAX_PRINTED_MOVES as u64
                ))
            );
        }
    }

    if dry_run {
        ctx.progress(&format!("  {}", ctx.dim("dry run — nothing was moved.")));
        return Ok(());
    }

    let prompt = format!(
        "{} {} file(s) under {}?{}",
        if symlinks {
            "Create symlinks for"
        } else {
            "Move"
        },
        move_count,
        library_root.display(),
        if symlinks {
            ""
        } else {
            " Originals are relocated (an undo journal is written)."
        },
    );
    if !ctx.confirm(&prompt, yes) {
        if ctx.json {
            print_json(&serde_json::json!({
                "command": "restructure", "mode": "apply", "aborted": true,
                "reason": "not_confirmed",
            }));
        } else {
            println!(
                "Aborted — nothing moved. {}",
                ctx.dim("(pass --yes to skip the prompt)")
            );
        }
        return Ok(());
    }

    let conn = fileid_engine::db::open_writer(&ctx.db)?;
    let applier = RestructureApply::new(
        Arc::new(Mutex::new(conn)),
        library_root.to_path_buf(),
        symlinks,
    );
    let result = applier.apply_iter(
        spool.iter()?,
        Some(usize::try_from(move_count).context("restructure plan is too large")?),
    )?;

    if ctx.json {
        print_json(&serde_json::json!({
            "command": "restructure", "mode": "apply", "dryRun": false,
            "useSymlinks": symlinks,
            "applied": result.applied,
            "failed": result.failed,
            "privilegeError": result.privilege_error,
        }));
        if result.failed > 0 {
            anyhow::bail!("restructure: {} move(s) failed", result.failed);
        }
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
    if result.failed > 0 {
        anyhow::bail!("restructure: {} move(s) failed", result.failed);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn ca(paths: &[&str]) -> Option<PathBuf> {
        common_ancestor(paths.iter().map(Path::new))
    }

    // A meaningful shared folder has a parent → the caller organizes into it.
    #[test]
    fn shared_folder_has_a_parent() {
        let a = ca(&["/home/u/Pics/2019/a.jpg", "/home/u/Pics/2020/b.jpg"]).unwrap();
        assert_eq!(a, PathBuf::from("/home/u/Pics"));
        assert!(
            a.parent().is_some(),
            "a real root must have a parent so apply proceeds"
        );
    }

    // Disparate absolute paths share only the filesystem root, which has NO
    // parent — the signal the apply path uses to refuse (else it would organize
    // a split library straight into `/Photos`, `/Documents`, …).
    #[test]
    fn disparate_unix_paths_collapse_to_parentless_root() {
        let a = ca(&["/home/u/Pictures/x.jpg", "/mnt/ext/Photos/y.jpg"]).unwrap();
        assert_eq!(a, PathBuf::from("/"));
        assert!(
            a.parent().is_none(),
            "'/' must be parentless so apply refuses"
        );
    }

    // A single file collapses to its parent directory (unchanged behavior).
    #[test]
    fn single_file_uses_its_parent() {
        assert_eq!(
            ca(&["/home/u/Pics/only.jpg"]).unwrap(),
            PathBuf::from("/home/u/Pics")
        );
    }

    #[test]
    #[ignore = "explicit million-file bounded-memory regression"]
    fn million_file_plan_keeps_chunks_and_preview_bounded() {
        let root = Path::new("/library");
        let mut chunk = Vec::with_capacity(CLASSIFY_CHUNK);
        let mut counts = HashMap::new();
        let mut sample = Vec::new();
        let mut move_count = 0_u64;
        let mut spool = None;

        for id in 0..1_000_000_i64 {
            chunk.push(FileForClassify {
                file_id: id,
                source: root.join("incoming").join(format!("photo-{id}.jpg")),
                kind: FileKind::Image,
                modified_unix: 1_704_067_200.0,
                created_unix: None,
                person_name: None,
                location_lat: None,
                location_lon: None,
                has_text: false,
            });
            assert!(chunk.len() <= CLASSIFY_CHUNK);
            if chunk.len() == CLASSIFY_CHUNK {
                absorb_chunk(
                    &mut chunk,
                    root,
                    &mut counts,
                    &mut sample,
                    &mut move_count,
                    &mut spool,
                )
                .unwrap();
            }
        }
        absorb_chunk(
            &mut chunk,
            root,
            &mut counts,
            &mut sample,
            &mut move_count,
            &mut spool,
        )
        .unwrap();

        assert!(chunk.is_empty());
        assert_eq!(sample.len(), MAX_PRINTED_MOVES);
        assert_eq!(move_count, 1_000_000);
        assert_eq!(counts.get("photo"), Some(&1_000_000));
    }
}
