//! `fileid dedupe [--exact|--similar]` — list duplicate / near-duplicate
//! groups. The default listing (and any `--dry-run`) is read-only; `--apply`
//! removes the redundant copies — trashing via `shell::trash` (or unlinking
//! with `--delete`) — behind a confirmation that requires `--yes` on a
//! non-interactive stdin.
//!
//! - `--exact`   fully SHA-256 re-hashes same-size candidates (byte-identical files),
//!   so legacy BLAKE3 and current stored identities group together safely.
//! - `--similar` groups by perceptual-hash Hamming distance (default ≤ 8,
//!   mirroring the engine's near-dup threshold). `--threshold` overrides it.
//!   Similar groups are transitively chained, so `--similar --apply` can
//!   over-delete; it is gated behind an explicit `--yes` (see `apply_run`).
//!
//! Both signals are written by the full engine scan pipeline; the CLI's
//! model-free `scan` does not compute them, so on a CLI-only-indexed library
//! these report "no … in DB" until a full engine scan has run.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::params;

use fileid_engine::util::content_hash::{
    exact_file_sha256, group_exact_duplicates, ExactDuplicateGroup,
};

use crate::context::{display_path, file_size_i64, print_json, Ctx};

use fileid_engine::read::dupes::{self, ExactCaps, MAX_SIMILAR_THRESHOLD};

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
    if do_similar && threshold > MAX_SIMILAR_THRESHOLD {
        anyhow::bail!(
            "similar threshold {threshold} exceeds the supported maximum of {MAX_SIMILAR_THRESHOLD} bits"
        );
    }

    // `--apply` (and a bare `--dry-run` preview) take the destructive path.
    if apply || dry_run {
        return apply_run(ctx, do_exact, do_similar, threshold, dry_run, delete, yes);
    }

    let conn = fileid_engine::db::open_read(&ctx.db)?;
    let mut json_sections = serde_json::Map::new();

    if do_exact {
        let (groups, skipped, partial) = exact_groups(&conn)?;
        if ctx.json {
            json_sections.insert("exact".into(), exact_json(&groups, skipped, partial));
        } else {
            render_exact(ctx, &groups, skipped, partial);
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

/// Files and bytes one pass may re-hash: the listing reports anything beyond as
/// partial; `--apply` refuses to act on a partial set.
const EXACT_CAPS: ExactCaps = ExactCaps {
    max_candidates: 100_000,
    max_bytes: 1 << 40,
};

struct ExactGroup {
    hash: String,
    files: Vec<(String, i64)>, // (path, size)
}

/// How much of the candidate set a bounded listing pass left unverified.
#[derive(Default, Clone, Copy)]
struct ListingPartial {
    skipped_candidates: u64,
    skipped_bytes: u64,
}

impl ListingPartial {
    fn is_partial(&self) -> bool {
        self.skipped_candidates > 0
    }
}

fn exact_buckets(conn: &rusqlite::Connection) -> Result<(Option<Vec<ExactDuplicateGroup>>, usize)> {
    let Some(selected) = dupes::exact_candidates(conn, EXACT_CAPS)? else {
        return Ok((None, 0));
    };
    if selected.is_partial() {
        anyhow::bail!(
            "exact dedupe would leave {} same-size file(s) ({} bytes) unverified; safety budget is {} files / {} bytes",
            selected.skipped_candidates,
            selected.skipped_bytes,
            EXACT_CAPS.max_candidates,
            EXACT_CAPS.max_bytes
        );
    }
    let grouping = group_exact_duplicates(selected.candidates);
    Ok((Some(grouping.groups), grouping.skipped))
}

fn exact_groups(
    conn: &rusqlite::Connection,
) -> Result<(Option<Vec<ExactGroup>>, usize, ListingPartial)> {
    let Some(listing) = dupes::exact_groups(conn, EXACT_CAPS, || false)? else {
        return Ok((None, 0, ListingPartial::default()));
    };
    let groups = listing
        .groups
        .into_iter()
        .map(|group| ExactGroup {
            hash: hex::encode(group.hash),
            files: group
                .files
                .into_iter()
                .map(|file| (file.path.to_string_lossy().into_owned(), file.indexed_size))
                .collect(),
        })
        .collect();
    let partial = ListingPartial {
        skipped_candidates: listing.skipped_candidates,
        skipped_bytes: listing.skipped_bytes,
    };
    Ok((Some(groups), listing.unreadable, partial))
}

fn render_exact(
    ctx: &Ctx,
    groups: &Option<Vec<ExactGroup>>,
    skipped: usize,
    partial: ListingPartial,
) {
    match groups {
        None => {
            println!("{}", ctx.bold("Exact duplicates: none computed."));
            ctx.progress(&format!(
                "  {}",
                ctx.dim("no content hashes in DB — run a full engine scan to populate them")
            ));
        }
        Some(groups) if groups.is_empty() => {
            if partial.is_partial() {
                println!(
                    "{}",
                    ctx.bold("Exact duplicates: none in the verified subset.")
                );
            } else {
                println!("{}", ctx.bold("Exact duplicates: none."));
            }
        }
        Some(groups) => {
            println!("{} exact-duplicate group(s):", groups.len());
            for g in groups {
                println!(
                    "  {} {}",
                    ctx.bold(&format!("[{}]", &g.hash[..g.hash.len().min(12)])),
                    ctx.dim(&format!(
                        "{} copies, {}",
                        g.files.len(),
                        file_size_i64(g.files[0].1)
                    ))
                );
                for (path, _) in &g.files {
                    println!("      {}", display_path(path));
                }
            }
        }
    }
    if skipped > 0 {
        ctx.progress(&format!(
            "  {}",
            ctx.dim(&format!(
                "partial: {skipped} same-size candidate(s) were missing, unreadable, or changed"
            ))
        ));
    }
    if partial.is_partial() {
        ctx.progress(&format!(
            "  {}",
            ctx.dim(&format!(
                "partial: {} candidate file(s) ({}) were beyond the listing's verify budget — narrow the library or use --apply's fail-closed pass",
                partial.skipped_candidates,
                file_size_i64(i64::try_from(partial.skipped_bytes).unwrap_or(i64::MAX))
            ))
        ));
    }
}

fn exact_json(
    groups: &Option<Vec<ExactGroup>>,
    skipped: usize,
    partial: ListingPartial,
) -> serde_json::Value {
    match groups {
        None => serde_json::json!({ "available": false, "skipped": 0, "complete": true }),
        Some(groups) => serde_json::json!({
            "available": true,
            "complete": skipped == 0 && !partial.is_partial(),
            "skipped": skipped,
            "skippedCandidates": partial.skipped_candidates,
            "skippedBytes": partial.skipped_bytes,
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
    files: Vec<(i64, String, i64)>, // (id, path, size)
}

fn similar_groups(
    conn: &rusqlite::Connection,
    threshold: u32,
) -> Result<Option<Vec<SimilarGroup>>> {
    let Some(components) = dupes::similar_groups(conn, threshold, || false)? else {
        return Ok(None);
    };
    let ids: Vec<i64> = components.iter().flatten().copied().collect();
    let mut metadata = load_file_metadata(conn, &ids)?;
    let groups = components
        .into_iter()
        .map(|members| SimilarGroup {
            files: members
                .into_iter()
                .filter_map(|id| metadata.remove(&id).map(|(path, size)| (id, path, size)))
                .collect(),
        })
        .collect();
    Ok(Some(groups))
}

fn load_file_metadata(
    conn: &rusqlite::Connection,
    ids: &[i64],
) -> Result<HashMap<i64, (String, i64)>> {
    let mut metadata = HashMap::with_capacity(ids.len());
    for chunk in ids.chunks(500) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql =
            format!("SELECT id, path_text, size_bytes FROM files WHERE failed = 0 AND id IN ({placeholders})");
        let mut stmt = conn.prepare(&sql)?;
        for (id, path, size) in stmt
            .query_map(rusqlite::params_from_iter(chunk.iter()), |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?
            .flatten()
        {
            metadata.insert(id, (path, size));
        }
    }
    Ok(metadata)
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
            println!(
                "{} near-duplicate group(s) (≤{threshold} bits):",
                groups.len()
            );
            for (i, g) in groups.iter().enumerate() {
                println!(
                    "  {} {}",
                    ctx.bold(&format!("group {}", i + 1)),
                    ctx.dim(&format!("{} files", g.files.len()))
                );
                for (_, path, _) in &g.files {
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
                "files": g.files.iter().map(|(id, p, _)| serde_json::json!({"id": id, "path": p})).collect::<Vec<_>>(),
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
    planned_phash: Option<i64>,
}

struct ExactGroupGuard {
    keeper_path: String,
    size: u64,
    hash: [u8; 32],
}

struct SimilarKeeperGuard {
    keeper_path: String,
    size: i64,
    planned_phash: Option<i64>,
}

struct VictimGroup {
    exact_guard: Option<ExactGroupGuard>,
    similar_guard: Option<SimilarKeeperGuard>,
    victims: Vec<Victim>,
}

struct VictimSet {
    available: bool,
    groups: Vec<VictimGroup>,
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
    // Reject an explicit both-flags apply rather than silently dropping
    // --similar — they select different victim sets.
    if do_exact && do_similar {
        anyhow::bail!(
            "dedupe --apply: choose one of --exact or --similar (they select different victim sets)"
        );
    }
    let use_similar = do_similar && !do_exact;
    let set = if use_similar {
        similar_victims(&conn, threshold)?
    } else {
        exact_victims(&conn)?
    };
    let signal = if use_similar {
        "perceptual hashes"
    } else {
        "content hashes"
    };

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

    let victims: Vec<&Victim> = set
        .groups
        .iter()
        .flat_map(|group| group.victims.iter())
        .collect();
    let total_bytes: i64 = victims.iter().map(|victim| victim.size).sum();

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

    let method = if delete {
        "delete permanently"
    } else {
        "move to Trash/Recycle Bin"
    };
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

    // Human preview only. In `--json` mode stdout must stay a single JSON value
    // (the apply result is emitted below); the `--json` dry-run already returned
    // its payload above, so reaching here under `--json` means a real apply —
    // suppress the preview lines that would otherwise corrupt the JSON output.
    if !ctx.json {
        println!(
            // The template already carries the verb, so the old "Will" prefix
            // produced "Will N file(s) would move to Trash".
            "{}{} file(s) {} {} ({} reclaimable):",
            if dry_run {
                format!("{} ", ctx.bold("DRY RUN —"))
            } else {
                String::new()
            },
            victims.len(),
            if dry_run { "would" } else { "will" },
            method,
            file_size_i64(total_bytes),
        );
        for v in &victims {
            println!(
                "  {}  {}",
                display_path(&v.path),
                ctx.dim(&file_size_i64(v.size))
            );
        }
    }

    if dry_run {
        ctx.progress(&format!("  {}", ctx.dim("dry run — nothing was removed.")));
        return Ok(());
    }

    // `--similar` groups are built by TRANSITIVE chaining of perceptual-hash
    // neighbors: A~B and B~C put {A, B, C} in one group even when A and C are
    // not alike, so a single component can grow large and `--apply` would mark
    // all-but-one of it for removal — an over-delete hazard the byte-identical
    // `--exact` path does not have. Require an explicit `--yes` here (never an
    // interactive guess, never a non-TTY auto-proceed), after a loud warning.
    if use_similar {
        if !ctx.json {
            println!();
            println!(
                "{}",
                ctx.bold("WARNING: --similar --apply can over-delete.")
            );
            println!(
                "  Near-duplicate groups are built by {} of perceptual-hash neighbors:",
                ctx.bold("transitive chaining")
            );
            println!("  A~B and B~C put A, B and C in one group even when A and C are NOT alike,");
            println!("  and apply keeps only one file per group — so a long chain can remove");
            println!("  files you meant to keep.");
            println!(
                "  {}",
                ctx.dim("Review first: `fileid dedupe --similar --dry-run` (or the desktop Cleanup tab).")
            );
        }
        if !yes {
            if ctx.json {
                print_json(&serde_json::json!({
                    "command": "dedupe", "mode": "apply", "aborted": true,
                    "reason": "similar_apply_requires_yes",
                    "warning": "visually-similar groups are transitively chained — members are \
                                not all mutually identical; review with `--similar --dry-run` or \
                                the desktop Cleanup tab, then re-run with --yes",
                }));
            } else {
                println!(
                    "Refusing without {} — re-run with it once you've reviewed the groups.",
                    ctx.bold("--yes")
                );
            }
            return Ok(());
        }
    }

    if !delete && trash_unavailable_on_this_platform() {
        if ctx.json {
            print_json(&serde_json::json!({
                "command": "dedupe", "mode": "apply", "aborted": true,
                "reason": "trash_unavailable_on_platform",
                "message": "recoverable trash is unavailable for the Rust engine on this platform; re-run with --delete for permanent removal",
            }));
        } else {
            println!(
                "{} Recoverable Trash is unavailable for the Rust engine on this platform.",
                ctx.bold("Refusing:")
            );
            println!(
                "  Re-run with {} only if you want permanent deletion.",
                ctx.bold("--delete")
            );
        }
        return Ok(());
    }

    let prompt = format!(
        "{} {} file(s) ({})? {}",
        if delete {
            "Permanently delete"
        } else {
            "Trash"
        },
        victims.len(),
        file_size_i64(total_bytes),
        if delete {
            "This CANNOT be undone."
        } else {
            "(recoverable from Trash)"
        },
    );
    if !ctx.confirm(&prompt, yes) {
        if ctx.json {
            print_json(&serde_json::json!({
                "command": "dedupe", "mode": "apply", "aborted": true,
                "reason": "not_confirmed",
            }));
        } else {
            println!(
                "Aborted — no files removed. {}",
                ctx.dim("(pass --yes to skip the prompt)")
            );
        }
        return Ok(());
    }

    drop(conn);
    let mut wconn = fileid_engine::db::open_writer(&ctx.db)?;
    let result = remove_victim_groups(&mut wconn, &set.groups, delete)?;

    if ctx.json {
        print_json(&serde_json::json!({
            "command": "dedupe", "mode": "apply", "dryRun": false,
            "method": if delete { "delete" } else { "trash" },
            "removed": result.removed,
            "failed": result.failed,
            "unsupported": result.unsupported,
            "reclaimBytes": result.reclaimed,
        }));
        if result.failed > 0 || result.unsupported > 0 {
            anyhow::bail!(
                "dedupe: {} file(s) failed to remove",
                result.failed + result.unsupported
            );
        }
        return Ok(());
    }

    println!("{}", ctx.bold("Dedupe apply complete."));
    println!("  Removed:     {}", result.removed);
    println!("  Reclaimed:   {}", file_size_i64(result.reclaimed));
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
    if result.failed > 0 || result.unsupported > 0 {
        anyhow::bail!(
            "dedupe: {} file(s) failed to remove",
            result.failed + result.unsupported
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

#[cfg(all(not(windows), not(target_os = "linux")))]
fn trash_unavailable_on_this_platform() -> bool {
    true
}

#[cfg(any(windows, target_os = "linux"))]
fn trash_unavailable_on_this_platform() -> bool {
    false
}

/// Trash (default) or permanently delete each victim, then drop its `files`
/// row (mirroring the engine's `trashFiles` handler: filesystem op first, DB
/// row removed only for the ones that actually left disk). FTS / embedding rows
/// cascade via the schema's triggers + `ON DELETE CASCADE`.
fn remove_victim_groups(
    conn: &mut rusqlite::Connection,
    groups: &[VictimGroup],
    delete: bool,
) -> Result<ApplyResult> {
    let mut result = ApplyResult {
        removed: 0,
        failed: 0,
        unsupported: 0,
        reclaimed: 0,
    };
    let mut removed_ids = Vec::new();

    for group in groups {
        if let Some(guard) = &group.exact_guard {
            if !exact_group_still_matches(guard, &group.victims) {
                result.failed += group.victims.len();
                continue;
            }
        }
        for (index, victim) in group.victims.iter().enumerate() {
            if group
                .similar_guard
                .as_ref()
                .is_some_and(|guard| !similar_keeper_still_matches(guard))
            {
                result.failed += group.victims.len() - index;
                break;
            }
            match remove_quarantined_victim(victim, group.exact_guard.as_ref(), delete) {
                RemovalOutcome::Removed => {
                    removed_ids.push(victim.id);
                    result.removed += 1;
                    result.reclaimed += victim.size;
                }
                RemovalOutcome::Failed => result.failed += 1,
                RemovalOutcome::Unsupported => result.unsupported += 1,
            }
        }
    }

    let tx = conn.transaction()?;
    for id in removed_ids {
        tx.execute("DELETE FROM files WHERE id = ?1", params![id])?;
    }
    tx.commit()?;
    Ok(result)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemovalOutcome {
    Removed,
    Failed,
    Unsupported,
}

fn remove_quarantined_victim(
    victim: &Victim,
    exact_guard: Option<&ExactGroupGuard>,
    delete: bool,
) -> RemovalOutcome {
    let original = Path::new(&victim.path);
    let quarantine = match quarantine(original) {
        Ok(path) => path,
        Err(error) => {
            eprintln!(
                "dedupe: could not quarantine {}: {error}",
                display_path(original.to_string_lossy().as_ref())
            );
            return RemovalOutcome::Failed;
        }
    };

    let valid = if let Some(guard) = exact_guard {
        exact_file_sha256(Path::new(&guard.keeper_path), guard.size)
            .is_ok_and(|hash| hash == guard.hash)
            && exact_file_sha256(&quarantine, guard.size).is_ok_and(|hash| hash == guard.hash)
    } else {
        let size_matches = u64::try_from(victim.size).is_ok_and(|size| {
            std::fs::metadata(&quarantine)
                .is_ok_and(|metadata| metadata.is_file() && metadata.len() == size)
        });
        size_matches
            && victim.planned_phash.is_some_and(|planned| {
                fileid_engine::pipeline::tagging::compute_dhash_for_path(&quarantine)
                    .is_ok_and(|live| live == planned)
            })
    };
    if !valid {
        restore_quarantine(&quarantine, original);
        return RemovalOutcome::Failed;
    }

    if delete {
        return if std::fs::remove_file(&quarantine).is_ok() {
            RemovalOutcome::Removed
        } else {
            restore_quarantine(&quarantine, original);
            RemovalOutcome::Failed
        };
    }

    if let Err(error) = fileid_engine::util::rename_no_replace(&quarantine, original) {
        eprintln!(
            "dedupe: validated file could not be restored to its original name before Trash ({error}); recovery copy remains at {}",
            display_path(quarantine.to_string_lossy().as_ref())
        );
        return RemovalOutcome::Failed;
    }
    let restored_is_still_valid = if let Some(guard) = exact_guard {
        exact_file_sha256(Path::new(&guard.keeper_path), guard.size)
            .is_ok_and(|hash| hash == guard.hash)
            && exact_file_sha256(original, guard.size).is_ok_and(|hash| hash == guard.hash)
    } else {
        u64::try_from(victim.size).is_ok_and(|size| {
            std::fs::metadata(original)
                .is_ok_and(|metadata| metadata.is_file() && metadata.len() == size)
                && victim.planned_phash.is_some_and(|planned| {
                    fileid_engine::pipeline::tagging::compute_dhash_for_path(original)
                        .is_ok_and(|live| live == planned)
                })
        })
    };
    if !restored_is_still_valid {
        return RemovalOutcome::Failed;
    }

    let trash_path = original.to_path_buf();
    let removed = fileid_engine::shell::trash::trash(std::slice::from_ref(&trash_path))
        .into_iter()
        .next()
        .unwrap_or(false);
    if removed {
        RemovalOutcome::Removed
    } else if trash_unavailable_on_this_platform() {
        RemovalOutcome::Unsupported
    } else {
        RemovalOutcome::Failed
    }
}

fn quarantine(original: &Path) -> std::io::Result<PathBuf> {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    let parent = original
        .parent()
        .ok_or_else(|| std::io::Error::other("victim has no parent directory"))?;
    let name = original
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    for _ in 0..32 {
        let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".{name}.fileid-quarantine-{}-{sequence}",
            std::process::id()
        ));
        match fileid_engine::util::rename_no_replace(original, &candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a quarantine path",
    ))
}

fn restore_quarantine(quarantine: &Path, original: &Path) {
    if let Err(error) = fileid_engine::util::rename_no_replace(quarantine, original) {
        eprintln!(
            "dedupe: validation failed and automatic restore was blocked ({error}); recovery copy remains at {}",
            display_path(quarantine.to_string_lossy().as_ref())
        );
    }
}

fn similar_keeper_still_matches(guard: &SimilarKeeperGuard) -> bool {
    u64::try_from(guard.size).is_ok_and(|size| {
        std::fs::metadata(&guard.keeper_path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() == size)
            && guard.planned_phash.is_some_and(|planned| {
                fileid_engine::pipeline::tagging::compute_dhash_for_path(Path::new(
                    &guard.keeper_path,
                ))
                .is_ok_and(|live| live == planned)
            })
    })
}

fn exact_group_still_matches(guard: &ExactGroupGuard, victims: &[Victim]) -> bool {
    exact_file_sha256(Path::new(&guard.keeper_path), guard.size)
        .is_ok_and(|hash| hash == guard.hash)
        && victims.iter().all(|victim| {
            u64::try_from(victim.size).is_ok_and(|size| {
                size == guard.size
                    && exact_file_sha256(Path::new(&victim.path), size)
                        .is_ok_and(|hash| hash == guard.hash)
            })
        })
}

/// Exact-duplicate victims: every byte-identical copy beyond the first
/// (kept) one. Keeper = lexicographically-first path (deterministic).
fn exact_victims(conn: &rusqlite::Connection) -> Result<VictimSet> {
    let (buckets, _) = exact_buckets(conn)?;
    let Some(buckets) = buckets else {
        return Ok(VictimSet {
            available: false,
            groups: Vec::new(),
        });
    };
    let groups = buckets
        .into_iter()
        .map(|group| {
            let mut files = group.files.into_iter();
            let keeper = files.next().expect("exact duplicate group has a keeper");
            VictimGroup {
                exact_guard: Some(ExactGroupGuard {
                    keeper_path: keeper.path.to_string_lossy().into_owned(),
                    size: group.size,
                    hash: group.hash,
                }),
                similar_guard: None,
                victims: files
                    .map(|file| Victim {
                        id: file.id,
                        path: file.path.to_string_lossy().into_owned(),
                        size: file.indexed_size,
                        planned_phash: None,
                    })
                    .collect(),
            }
        })
        .collect();
    Ok(VictimSet {
        available: true,
        groups,
    })
}

/// Near-duplicate victims: every member of each phash-connected component
/// beyond the first (kept). Keeper = lowest file id in the component.
fn similar_victims(conn: &rusqlite::Connection, threshold: u32) -> Result<VictimSet> {
    let set = similar_groups(conn, threshold)?;
    let Some(groups) = set else {
        return Ok(VictimSet {
            available: false,
            groups: Vec::new(),
        });
    };
    let phashes: HashMap<i64, i64> = {
        let mut stmt = conn.prepare("SELECT id, phash FROM files WHERE phash IS NOT NULL")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let groups = groups
        .into_iter()
        .map(|group| {
            let mut files = group.files;
            files.sort_by_key(|(id, _, _)| *id);
            let keeper = files.first().cloned();
            VictimGroup {
                exact_guard: None,
                similar_guard: keeper.map(|(id, path, size)| SimilarKeeperGuard {
                    keeper_path: path,
                    size,
                    planned_phash: phashes.get(&id).copied(),
                }),
                victims: files
                    .into_iter()
                    .skip(1)
                    .map(|(id, path, size)| Victim {
                        id,
                        path,
                        size,
                        planned_phash: phashes.get(&id).copied(),
                    })
                    .collect(),
            }
        })
        .collect();
    Ok(VictimSet {
        available: true,
        groups,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str, bytes: &[u8]) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "fileid-dedupe-{name}-{}-{}.bin",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn bmp_gradient(reverse: bool) -> Vec<u8> {
        let row_bytes = 28u32;
        let image_bytes = row_bytes * 8;
        let file_bytes = 54 + image_bytes;
        let mut bytes = Vec::with_capacity(file_bytes as usize);
        bytes.extend_from_slice(b"BM");
        bytes.extend_from_slice(&file_bytes.to_le_bytes());
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(&54u32.to_le_bytes());
        bytes.extend_from_slice(&40u32.to_le_bytes());
        bytes.extend_from_slice(&9i32.to_le_bytes());
        bytes.extend_from_slice(&8i32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&24u16.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&image_bytes.to_le_bytes());
        bytes.extend_from_slice(&[0; 16]);
        for _ in 0..8 {
            for x in 0..9 {
                let value = (if reverse { (8 - x) * 28 } else { x * 28 }) as u8;
                bytes.extend_from_slice(&[value, value, value]);
            }
            bytes.push(0);
        }
        bytes
    }

    #[test]
    fn similar_delete_preserves_same_size_replacement() {
        let original = bmp_gradient(false);
        let replacement = bmp_gradient(true);
        assert_eq!(original.len(), replacement.len());
        let victim_path = temp_file("similar-replaced", &original);
        let planned_phash =
            fileid_engine::pipeline::tagging::compute_dhash_for_path(&victim_path).unwrap();
        std::fs::write(&victim_path, &replacement).unwrap();
        let victim = Victim {
            id: 1,
            path: victim_path.to_string_lossy().into_owned(),
            size: original.len() as i64,
            planned_phash: Some(planned_phash),
        };

        assert_eq!(
            remove_quarantined_victim(&victim, None, true),
            RemovalOutcome::Failed
        );
        assert_eq!(std::fs::read(&victim_path).unwrap(), replacement);
        let _ = std::fs::remove_file(victim_path);
    }

    #[test]
    fn exact_delete_validates_the_quarantined_object() {
        let keeper = temp_file("quarantine-keeper", b"original");
        let victim_path = temp_file("quarantine-victim", b"original");
        let guard = ExactGroupGuard {
            keeper_path: keeper.to_string_lossy().into_owned(),
            size: 8,
            hash: exact_file_sha256(&keeper, 8).unwrap(),
        };
        std::fs::write(&victim_path, b"replaced").unwrap();
        let victim = Victim {
            id: 1,
            path: victim_path.to_string_lossy().into_owned(),
            size: 8,
            planned_phash: None,
        };

        assert_eq!(
            remove_quarantined_victim(&victim, Some(&guard), true),
            RemovalOutcome::Failed
        );
        assert_eq!(std::fs::read(&victim_path).unwrap(), b"replaced");
        let _ = std::fs::remove_file(keeper);
        let _ = std::fs::remove_file(victim_path);
    }

    #[test]
    fn exact_group_revalidation_rejects_changed_keeper_or_victim() {
        let keeper = temp_file("keeper", b"original");
        let victim_path = temp_file("victim", b"original");
        let hash = exact_file_sha256(&keeper, 8).unwrap();
        let guard = ExactGroupGuard {
            keeper_path: keeper.to_string_lossy().into_owned(),
            size: 8,
            hash,
        };
        let victim = Victim {
            id: 1,
            path: victim_path.to_string_lossy().into_owned(),
            size: 8,
            planned_phash: None,
        };
        assert!(exact_group_still_matches(
            &guard,
            std::slice::from_ref(&victim)
        ));
        std::fs::write(&keeper, b"replaced").unwrap();
        assert!(!exact_group_still_matches(
            &guard,
            std::slice::from_ref(&victim)
        ));
        std::fs::write(&keeper, b"original").unwrap();
        std::fs::write(&victim_path, b"replaced").unwrap();
        assert!(!exact_group_still_matches(&guard, &[victim]));
        let _ = std::fs::remove_file(keeper);
        let _ = std::fs::remove_file(victim_path);
    }

    #[test]
    fn exact_apply_changed_keeper_removes_nothing_and_keeps_db_row() {
        let keeper = temp_file("apply-keeper", b"original");
        let victim_path = temp_file("apply-victim", b"original");
        let hash = exact_file_sha256(&keeper, 8).unwrap();
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        fileid_engine::db::migrations::apply(&conn).unwrap();
        conn.execute(
            "INSERT INTO files \
             (id, path_text, path_hash, size_bytes, modified_at, scanned_at, kind, extension) \
             VALUES (1, ?1, 1, 8, 0, 0, 'other', '')",
            params![victim_path.to_string_lossy()],
        )
        .unwrap();
        let groups = vec![VictimGroup {
            exact_guard: Some(ExactGroupGuard {
                keeper_path: keeper.to_string_lossy().into_owned(),
                size: 8,
                hash,
            }),
            similar_guard: None,
            victims: vec![Victim {
                id: 1,
                path: victim_path.to_string_lossy().into_owned(),
                size: 8,
                planned_phash: None,
            }],
        }];
        std::fs::write(&keeper, b"replaced").unwrap();
        let result = remove_victim_groups(&mut conn, &groups, true).unwrap();
        assert_eq!(result.removed, 0);
        assert_eq!(result.failed, 1);
        assert!(victim_path.exists());
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM files WHERE id = 1", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
            1
        );
        let _ = std::fs::remove_file(keeper);
        let _ = std::fs::remove_file(victim_path);
    }

    #[test]
    fn similar_apply_changed_keeper_removes_nothing_and_keeps_db_row() {
        let bytes = bmp_gradient(false);
        let keeper = temp_file("similar-apply-keeper", &bytes);
        let victim_path = temp_file("similar-apply-victim", &bytes);
        let planned_phash =
            fileid_engine::pipeline::tagging::compute_dhash_for_path(&keeper).unwrap();
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        fileid_engine::db::migrations::apply(&conn).unwrap();
        conn.execute(
            "INSERT INTO files \
             (id, path_text, path_hash, size_bytes, modified_at, scanned_at, kind, extension, phash) \
             VALUES (1, ?1, 1, ?2, 0, 0, 'image', 'bmp', ?3)",
            params![victim_path.to_string_lossy(), bytes.len() as i64, planned_phash],
        )
        .unwrap();
        let groups = vec![VictimGroup {
            exact_guard: None,
            similar_guard: Some(SimilarKeeperGuard {
                keeper_path: keeper.to_string_lossy().into_owned(),
                size: bytes.len() as i64,
                planned_phash: Some(planned_phash),
            }),
            victims: vec![Victim {
                id: 1,
                path: victim_path.to_string_lossy().into_owned(),
                size: bytes.len() as i64,
                planned_phash: Some(planned_phash),
            }],
        }];
        std::fs::remove_file(&keeper).unwrap();

        let result = remove_victim_groups(&mut conn, &groups, true).unwrap();
        assert_eq!(result.removed, 0);
        assert_eq!(result.failed, 1);
        assert!(victim_path.exists());
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM files WHERE id = 1", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
            1
        );
        let _ = std::fs::remove_file(victim_path);
    }

    #[test]
    fn exact_buckets_merge_different_stored_hash_recipes() {
        let a = temp_file("mixed-a", b"same bytes");
        let b = temp_file("mixed-b", b"same bytes");
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        fileid_engine::db::migrations::apply(&conn).unwrap();
        for (id, path, stored) in [(1, &a, vec![1u8; 32]), (2, &b, vec![2u8; 32])] {
            conn.execute(
                "INSERT INTO files \
                 (id, path_text, path_hash, size_bytes, modified_at, scanned_at, kind, extension, content_hash) \
                 VALUES (?1, ?2, ?1, 10, 0, 0, 'other', '', ?3)",
                params![id, path.to_string_lossy(), stored],
            )
            .unwrap();
        }
        let (groups, skipped) = exact_buckets(&conn).unwrap();
        let groups = groups.unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].files.len(), 2);
        let _ = std::fs::remove_file(a);
        let _ = std::fs::remove_file(b);
    }

    #[test]
    fn exact_buckets_fail_closed_above_read_budget() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        fileid_engine::db::migrations::apply(&conn).unwrap();
        for id in [1i64, 2] {
            conn.execute(
                "INSERT INTO files \
                 (id, path_text, path_hash, size_bytes, modified_at, scanned_at, kind, extension, content_hash) \
                 VALUES (?1, printf('/huge-%d.bin', ?1), ?1, ?2, 0, 0, 'other', '', X'01')",
                params![id, EXACT_CAPS.max_bytes / 2 + 1],
            )
            .unwrap();
        }
        let error = exact_buckets(&conn).unwrap_err();
        assert!(error.to_string().contains("safety budget"));
    }
}
