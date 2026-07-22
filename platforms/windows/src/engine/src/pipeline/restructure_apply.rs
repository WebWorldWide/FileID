// Restructure apply — execute a `Vec<ProposedMove>` on disk.
//
// Two modes:
//   * Real move (default): `MoveFileExW(MOVEFILE_COPY_ALLOWED)` — NO
//     `MOVEFILE_REPLACE_EXISTING`, so an occupied destination fails the move
//     instead of silently overwriting whatever is already there (B3). Atomic
//     when same volume; copy+delete across volumes. The DB row's `path_text`
//     is updated by a SEPARATE statement AFTER the move returns — this is NOT
//     one transaction with the filesystem op (it can't be). A crash in the
//     move→update window leaves the file relocated with `path_text` stale; the
//     next scan self-heals it via rename-heal on the NTFS `file_ref`, and a
//     failed update is also recorded to a recovery sidecar.
//   * Symlink (advanced): `CreateSymbolicLinkW`. Requires either
//     SeCreateSymbolicLinkPrivilege (admin) OR Developer Mode enabled.
//     Lets the user preview the proposed structure without committing
//     to actual moves.
//
// COLLISION SAFETY (B3): many distinct sources share a basename and the rule
// cascade funnels them into one folder, so two planned moves can target the
// same path. Each real-move destination is uniquified within its parent
// (`name (2).ext`, …) so both files survive; nothing is ever clobbered.
//
// STALE-PLAN / IDENTITY GUARD (B4): a plan is built from a DB snapshot, then
// applied after an arbitrary delay. Before each move the live DB row for
// `file_id` is re-read and required to still name `source`, so a plan that
// went stale (the file was renamed/moved/replaced meanwhile) can't move the
// wrong bytes — the payload `source` string is not authoritative on its own.
//
// PATH-TRAVERSAL GUARD: every destination MUST canonicalize to a path
// inside `library_root`. We refuse to write outside the user's chosen
// library — even if the planner is buggy or someone forges a payload.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Lines, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::ipc::{RestructureApplyResult, RestructureMove};
use crate::pipeline::restructure_feedback;

type ClaimedDestination = [u8; 16];

#[derive(serde::Deserialize)]
struct UndoEntry {
    file_id: i64,
    from: String,
    to: String,
}

struct UndoJournalIter {
    lines: Lines<BufReader<File>>,
}

impl Iterator for UndoJournalIter {
    type Item = Result<UndoEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        self.lines.next().map(|line| {
            let line = line.context("reading restructure undo journal")?;
            serde_json::from_str(&line).context("parsing restructure undo journal entry")
        })
    }
}

/// Write-ahead undo journal: every inverse entry is appended, flushed, and
/// fsync'd BEFORE the move it describes executes, and rolled back to the prior
/// offset if that move then fails. The journal therefore never claims a move
/// that didn't happen and never misses one that did — closing the two crash
/// windows the previous write-behind (fsync-every-500) design left open.
/// Mirrors the macOS engine's journal discipline. (audit 2026-07-14)
struct UndoJournal {
    file: File,
    len: u64,
}

impl UndoJournal {
    /// Open fresh, truncating the previous run's journal. Called lazily right
    /// before the FIRST journaled move, so an apply that never journals
    /// (symlink mode, all no-ops) preserves the prior journal, and an open
    /// failure aborts before anything moves. Fail-closed: undo protection is a
    /// precondition of a recorded apply, not best-effort.
    fn open_truncating(path: Option<PathBuf>) -> Result<UndoJournal> {
        let path = path.context("no undo journal location available")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("creating undo journal dir {}", dir.display()))?;
        }
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .with_context(|| format!("opening undo journal {}", path.display()))?;
        file.sync_all()
            .with_context(|| format!("syncing truncated undo journal {}", path.display()))?;
        Ok(UndoJournal { file, len: 0 })
    }

    /// Durably append one inverse entry; returns the pre-append offset so a
    /// failed move can roll the entry back.
    fn append_ahead(&mut self, file_id: i64, from: &str, to: &str) -> Result<u64> {
        let prev = self.len;
        let mut line =
            serde_json::json!({ "file_id": file_id, "from": from, "to": to }).to_string();
        line.push('\n');
        self.file
            .write_all(line.as_bytes())
            .context("appending undo journal entry")?;
        self.file.sync_data().context("syncing undo journal entry")?;
        self.len = prev + line.len() as u64;
        Ok(prev)
    }

    /// The move this entry described never happened — truncate it away so undo
    /// can't replay a phantom. Prior entries stay durable. Best-effort: a
    /// failed rollback leaves a phantom entry whose replay stale-skips on the
    /// identity checks.
    fn rollback_to(&mut self, prev: u64) {
        use std::io::Seek as _;
        let _ = self.file.set_len(prev);
        let _ = self.file.seek(std::io::SeekFrom::Start(prev));
        let _ = self.file.sync_data();
        self.len = prev;
    }
}

/// One forward pass over the journal collecting each entry's byte span and
/// validating that it parses. Returns None if the journal does not exist.
/// Tolerates exactly a torn TRAILING entry: under write-ahead ordering an
/// entry is fsync'd before its move starts, so a torn tail means that move
/// never executed and skipping it is safe. Corruption anywhere earlier fails
/// closed — an explicit error beats a partial undo that reorders dependents.
fn scan_undo_journal_spans(path: &Path) -> Result<Option<Vec<(u64, u32)>>> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| format!("opening undo journal {}", path.display()))
        }
    };
    let mut reader = BufReader::new(file);
    let mut spans: Vec<(u64, u32)> = Vec::new();
    let mut offset = 0u64;
    let mut buf: Vec<u8> = Vec::new();
    loop {
        buf.clear();
        let n = std::io::BufRead::read_until(&mut reader, b'\n', &mut buf)
            .context("reading undo journal")?;
        if n == 0 {
            break;
        }
        let had_newline = buf.last() == Some(&b'\n');
        let body_len = if had_newline { n - 1 } else { n };
        let parses = serde_json::from_slice::<UndoEntry>(&buf[..body_len]).is_ok();
        if parses {
            spans.push((offset, u32::try_from(body_len).context("journal entry too large")?));
            offset += n as u64;
            if !had_newline {
                break; // final entry, newline lost to a crash — content intact
            }
        } else {
            // Only a torn FINAL entry is acceptable.
            let mut probe = [0u8; 1];
            let at_eof = std::io::Read::read(&mut reader, &mut probe)
                .context("probing undo journal tail")?
                == 0;
            if at_eof && !had_newline {
                tracing::warn!(
                    offset,
                    "[RESTRUCTURE] dropping torn trailing undo entry (its move never executed)"
                );
                break;
            }
            anyhow::bail!(
                "undo journal corrupt at byte {offset}: refusing a partial undo of {} valid entries",
                spans.len()
            );
        }
    }
    Ok(Some(spans))
}

/// Streams journal entries NEWEST-FIRST via pre-scanned byte spans. Dependent
/// moves (A→X then B→A) must be restored newest-first or the older inverse
/// (X→A) finds A occupied by B and uniquifies into "A (2)" — silent
/// corruption. Holds only the span table, never the journal contents.
struct ReverseUndoIter {
    file: File,
    spans: Vec<(u64, u32)>,
    next: usize,
}

impl Iterator for ReverseUndoIter {
    type Item = Result<RestructureMove>;

    fn next(&mut self) -> Option<Self::Item> {
        use std::io::{Read as _, Seek as _};
        if self.next == 0 {
            return None;
        }
        self.next -= 1;
        let (start, len) = self.spans[self.next];
        let mut buf = vec![0u8; len as usize];
        let entry = (|| -> Result<RestructureMove> {
            self.file
                .seek(std::io::SeekFrom::Start(start))
                .context("seeking undo journal entry")?;
            self.file
                .read_exact(&mut buf)
                .context("reading undo journal entry")?;
            let e: UndoEntry = serde_json::from_slice(&buf)
                .context("parsing restructure undo journal entry")?;
            Ok(RestructureMove {
                file_id: e.file_id,
                source: e.from,
                destination: e.to,
                category: String::new(),
                tier: None,
                confidence: String::new(),
                reason: None,
            })
        })();
        Some(entry)
    }
}

fn claimed_destination_key(path: &Path) -> ClaimedDestination {
    let folded = path.to_string_lossy().to_lowercase();
    let digest = blake3::hash(folded.as_bytes());
    let mut key = [0_u8; 16];
    key.copy_from_slice(&digest.as_bytes()[..16]);
    key
}

#[cfg(windows)]
use windows::core::PCWSTR;
#[cfg(windows)]
use windows::Win32::Storage::FileSystem::{
    CreateSymbolicLinkW, MoveFileExW, MOVEFILE_COPY_ALLOWED, MOVEFILE_WRITE_THROUGH,
    SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE, SYMBOLIC_LINK_FLAGS,
};

pub struct RestructureApply {
    db_conn: Arc<Mutex<Connection>>,
    library_root: PathBuf,
    use_symlinks: bool,
    // F-C6-013: cooperative cancel polled between moves. Defaults to a fresh,
    // never-set flag; the dispatcher injects a shared flag via `with_cancel` so
    // a user "stop" aborts a 100k-move apply between moves (each completed move
    // is already durable, so stopping mid-batch preserves per-move atomicity).
    cancel: Arc<AtomicBool>,
    // Test seam: journal location override so concurrent tests never share (or
    // clobber) the real user journal. None → the app-data location.
    undo_journal_override: Option<PathBuf>,
}

impl RestructureApply {
    pub fn new(db_conn: Arc<Mutex<Connection>>, library_root: PathBuf, use_symlinks: bool) -> Self {
        Self {
            db_conn,
            library_root,
            use_symlinks,
            cancel: Arc::new(AtomicBool::new(false)),
            undo_journal_override: None,
        }
    }

    #[cfg(test)]
    fn with_undo_journal_path(mut self, path: PathBuf) -> Self {
        self.undo_journal_override = Some(path);
        self
    }

    /// Inject a shared cancellation flag. `handle_apply_restructure` passes the
    /// flag that the CancelScan dispatch arm sets; `apply` polls it at the top of
    /// each move so a long apply is stoppable. (F-C6-013)
    pub fn with_cancel(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = cancel;
        self
    }

    /// Apply every proposed move. Stops on first hard error; returns the
    /// applied + failed counts. A privilege error in symlink mode short-
    /// circuits with a friendly message instead of partial writes.
    // The engine package compiles this module once for the library and again
    // for the binary. External library callers and unit tests use the slice
    // convenience method; the binary uses `apply_iter` directly.
    #[allow(dead_code)]
    pub fn apply(&self, moves: &[RestructureMove]) -> Result<RestructureApplyResult> {
        self.apply_iter_with(
            moves.iter().cloned().map(Ok),
            Some(moves.len()),
            true,
        )
    }

    /// Apply a move stream without materializing the complete plan. This is the
    /// million-file path used by the CLI and persisted GUI plans; all per-run
    /// state (undo journal, collision set, cancellation, and feedback) remains
    /// shared across the stream exactly as it is for `apply(&[...])`.
    pub fn apply_iter<I>(
        &self,
        moves: I,
        total_hint: Option<usize>,
    ) -> Result<RestructureApplyResult>
    where
        I: IntoIterator<Item = Result<RestructureMove>>,
    {
        self.apply_iter_with(moves, total_hint, true)
    }

    fn apply_iter_with<I>(
        &self,
        moves: I,
        total_hint: Option<usize>,
        record_undo: bool,
    ) -> Result<RestructureApplyResult>
    where
        I: IntoIterator<Item = Result<RestructureMove>>,
    {
        let canonical_root = canonicalize_safely(&self.library_root)
            .with_context(|| format!("library root {}", self.library_root.display()))?;

        let mut applied = 0u32;
        let mut failed = 0u32;
        // WRITE-AHEAD undo journal (macOS parity, audit 2026-07-14): each
        // inverse entry is appended + fsync'd BEFORE its move executes and
        // rolled back if the move then fails, so the journal never claims a
        // move that didn't happen and never misses one that did. Opened
        // LAZILY at the first journaled move: a batch that never journals
        // (symlink mode, all no-ops) can't truncate the prior run's journal,
        // and an unopenable journal aborts before ANY file moves — undo
        // protection is a precondition now, not best-effort.
        let mut journal: Option<UndoJournal> = None;
        // L1: a recorded SYMLINK run journals nothing (it creates links, not
        // move-based inverses), but a prior REAL-move run's journal would
        // survive — so "Undo last run" after a symlink run would reverse that
        // older, unrelated real run (data movement the user didn't ask to
        // undo). Clear any stale journal at the start of a recorded symlink
        // run so undo_last is a truthful no-op afterward. Best-effort: a
        // failure to remove it only risks the pre-existing mis-undo, so it
        // must not abort the symlink apply.
        if record_undo && self.use_symlinks {
            if let Some(path) = self.undo_journal_path() {
                match std::fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => tracing::warn!(
                        error = %e,
                        "[RESTRUCTURE] could not clear stale undo journal before symlink run"
                    ),
                }
            }
        }
        // (source, final destination) of every successful real move, fed to the
        // learn-from-corrections memory in ONE lock acquisition after the loop so a
        // future plan can boost a move toward a folder the user has filed here
        // before. Populated alongside the undo journal, so it is forward-applies-only
        // (empty on an undo run, record_undo=false). (R3 → learn-your-style)
        let mut applied_pairs: Vec<(String, PathBuf)> =
            Vec::with_capacity(APPLY_PROGRESS_INTERVAL);
        // B3: destinations claimed earlier in THIS batch, so two distinct
        // sources that map to the same basename don't collide before either
        // touches disk. Keyed by the LOWERCASED path string: NTFS (and APFS)
        // are case-insensitive by default, so "Photo.jpg" and "photo.jpg" name
        // the same file — case-folding the key makes the second move uniquify
        // instead of silently clobbering the first (data loss). Mirrors the
        // `ci_starts_with` full-Unicode fold and the macOS `Restructure.swift`
        // lowercased claimed set, so a library round-trips identically.
        let mut claimed: HashSet<ClaimedDestination> = HashSet::new();

        // F-C6-013: the apply loop was a silent, unstoppable serial walk — at
        // 100k+ moves the user got no feedback and no stop.
        let total = total_hint.unwrap_or(0);
        for (idx, m) in moves.into_iter().enumerate() {
            // A failed stream read (corrupt / vanished spooled plan) must NOT
            // discard the partial result via `?`: every move already applied is
            // real and journaled, and an Err reply makes the app report "your
            // files are unchanged" with no Undo affordance. Stop, count the
            // unread remainder as failed, and return the truthful partial.
            let m = match m {
                Ok(m) => m,
                Err(err) => {
                    tracing::warn!(
                        ?err,
                        applied,
                        failed,
                        processed = idx,
                        total,
                        "[RESTRUCTURE] plan stream read failed mid-apply; stopping"
                    );
                    let remainder = total.saturating_sub(idx).max(1);
                    failed = failed.saturating_add(u32::try_from(remainder).unwrap_or(u32::MAX));
                    break;
                }
            };
            // Poll the cancel flag at the TOP of every iteration. Every move
            // already completed is durable (per-move FS op + DB update), so
            // stopping BETWEEN moves is safe and preserves per-move atomicity.
            if self.cancel.load(Ordering::Relaxed) {
                tracing::info!(applied, failed, processed = idx, total, "[RESTRUCTURE] apply cancelled by user");
                break;
            }
            let processed = idx + 1;
            if should_emit_apply_progress(processed, total, APPLY_PROGRESS_INTERVAL) {
                tracing::info!(applied, failed, processed, total, "[RESTRUCTURE] apply progress");
            }

            // B4/S6/S7: bind the move to the planned file identity. The
            // payload `source` is not authoritative on its own — re-read the
            // live DB row for `file_id` and require it still names this
            // source. A stale plan (file renamed/moved/replaced since
            // planning) is skipped so we never move the wrong bytes or stamp
            // the row with a path that never held this file.
            let db_file_ref = match current_identity_in_db(&self.db_conn, m.file_id) {
                Ok(Some((db_path, db_ref))) if paths_equal(&db_path, &m.source) => db_ref,
                // A cancelled/partially failed undo is intentionally resumable.
                // Entries already restored by the first attempt remain in the
                // journal; recognize their live DB + on-disk destination as an
                // idempotent success so a retry can finish and clear the journal.
                Ok(Some((db_path, db_ref)))
                    if !record_undo
                        && paths_equal(&db_path, &m.destination)
                        && Path::new(&m.destination).try_exists().unwrap_or(false)
                        && !file_ref_swapped(
                            db_ref,
                            crate::platform::file_ref(Path::new(&m.destination)),
                        ) =>
                {
                    continue;
                }
                // Undo fallback on journal evidence: the forward apply can
                // succeed the on-disk move but FAIL update_path_in_db (a live
                // UNIQUE path_text conflict, or a kill in the move→update
                // window). Then the file is physically at the journaled
                // final_dest (= this undo move's `source`) while path_text
                // still names the original (= this undo move's `destination`),
                // so neither DB-derived arm above matches and the file would
                // be stale-skipped and stranded forever. Trust the journal: if
                // the file is physically at `source` and `destination` is
                // free, and the file identity still matches the DB row, move
                // it back. Undo-only (record_undo=false) so a forward apply
                // never takes this path.
                Ok(Some((db_path, db_ref)))
                    if !record_undo
                        && paths_equal(&db_path, &m.destination)
                        && Path::new(&m.source).try_exists().unwrap_or(false)
                        && !Path::new(&m.destination).try_exists().unwrap_or(false)
                        && !file_ref_swapped(
                            db_ref,
                            crate::platform::file_ref(Path::new(&m.source)),
                        ) =>
                {
                    db_ref
                }
                _ => {
                    tracing::warn!(
                        file_id = m.file_id,
                        "[RESTRUCTURE] skipping stale move: source no longer matches the DB row"
                    );
                    failed += 1;
                    continue;
                }
            };

            // R-#14 same-path SWAP guard: the path check above only proves the DB row
            // still NAMES this source — not that the file currently AT that path is the
            // one we planned to move. If a different file was dropped at the same path
            // in the plan→apply window (a sync client re-downloading, an app re-saving),
            // moving it would relocate the wrong bytes and stamp this file_id onto an
            // unrelated file. Compare the planned file's stored file_ref to the one on
            // disk now; skip on positive mismatch. Conservative — a NULL stored ref or a
            // platform/file with no readable ref (non-NTFS, or the non-Windows stub)
            // leaves the move to proceed, so a legitimate move is never falsely skipped.
            if file_ref_swapped(db_file_ref, crate::platform::file_ref(Path::new(&m.source))) {
                tracing::warn!(
                    file_id = m.file_id,
                    "[RESTRUCTURE] skipping swapped move: a different file now occupies the planned source path"
                );
                failed += 1;
                continue;
            }

            let dest = PathBuf::from(&m.destination);
            // Path-traversal guard. The destination's parent must exist
            // OR be createable under library_root. Canonicalize the
            // closest existing ancestor and verify containment.
            //
            // D1: skip this on an UNDO replay (record_undo=false). Undo
            // destinations are the ORIGINAL scanned paths the engine itself
            // journaled at apply time — inherently trusted, and not
            // necessarily under the caller-supplied library_root (undo carries
            // whatever root the app currently has selected, which may differ
            // from the applied root). Re-gating journaled restores by that
            // root made undo reject EVERY file and silently no-op, leaving the
            // library reorganized with the journal retained. The traversal
            // guard exists to contain forward, plan-generated destinations
            // (possibly VLM-named); it must not block reversal of the engine's
            // own recorded moves.
            if record_undo {
                if let Err(err) = ensure_inside_root(&dest, &canonical_root) {
                    tracing::warn!(?err, dest=%crate::platform::redact_path_for_log(&dest), "rejecting move outside library root");
                    failed += 1;
                    continue;
                }
            }

            if let Some(parent) = dest.parent() {
                // SEC-5: TOCTOU defense, pass 1. Check the EXISTING ancestor
                // chain BEFORE create_dir_all extends it — an attacker may
                // have planted a junction in a pre-existing folder under
                // library_root that would silently redirect the write
                // outside the root the moment we resolve through it.
                if has_reparse_point_in_chain(parent, &canonical_root) {
                    tracing::warn!(
                        parent=%crate::platform::redact_path_for_log(parent),
                        "rejecting move: pre-existing reparse point in destination parent chain"
                    );
                    failed += 1;
                    continue;
                }
                if let Err(err) = std::fs::create_dir_all(parent) {
                    tracing::warn!(?err, parent=%crate::platform::redact_path_for_log(parent), "create_dir_all failed");
                    failed += 1;
                    continue;
                }
                // SEC-5: TOCTOU defense, pass 2. Re-check after
                // create_dir_all. The window between the pre-check and
                // here is small but non-zero; defense in depth is cheap.
                if has_reparse_point_in_chain(parent, &canonical_root) {
                    tracing::warn!(
                        parent=%crate::platform::redact_path_for_log(parent),
                        "rejecting move: reparse point appeared after create_dir_all"
                    );
                    failed += 1;
                    continue;
                }
            }

            // Skip a no-op (the file already sits at its PLANNED destination)
            // BEFORE uniquifying. If we uniquified first, `unique_destination`
            // would see the file itself occupying `dest`, bump it to a ` (2)`
            // sibling, and we'd rename an already-correctly-placed file —
            // churning an organized library, silently in auto-file mode. (ENG-42)
            if !self.use_symlinks && paths_equal(&m.source, &dest.to_string_lossy()) {
                applied += 1;
                continue;
            }

            // B3: real moves never clobber. `move_file` drops
            // MOVEFILE_REPLACE_EXISTING, and we additionally resolve a
            // collision-free name within the SAME parent (so containment +
            // the reparse checks above still hold) — both distinct files
            // survive. Symlink mode keeps the requested name and fails
            // naturally if it's taken (CreateSymbolicLinkW won't overwrite).
            let final_dest = if self.use_symlinks {
                dest.clone()
            } else {
                let d = unique_destination(&dest, &claimed);
                claimed.insert(claimed_destination_key(&d));
                d
            };

            // WRITE-AHEAD: the inverse entry (final → original) is durable
            // BEFORE the move executes. If the journal cannot open, abort now —
            // no file has moved yet (lazy open fires on the first real move).
            // If a later append fails, stop BEFORE the unrecorded move: every
            // completed move stays undoable, the remainder is reported failed.
            let mut journal_entry_offset: Option<u64> = None;
            if record_undo && !self.use_symlinks {
                if journal.is_none() {
                    journal = Some(
                        UndoJournal::open_truncating(self.undo_journal_path())
                            .context("undo journal unavailable; aborting before any file moves")?,
                    );
                }
                let j = journal.as_mut().expect("journal just opened");
                match j.append_ahead(m.file_id, &final_dest.to_string_lossy(), &m.source) {
                    Ok(prev) => journal_entry_offset = Some(prev),
                    Err(err) => {
                        tracing::error!(
                            ?err,
                            applied,
                            failed,
                            "[RESTRUCTURE] undo journal append failed; stopping before the unrecorded move"
                        );
                        let remainder = total.saturating_sub(idx).max(1);
                        failed = failed.saturating_add(u32::try_from(remainder).unwrap_or(u32::MAX));
                        break;
                    }
                }
            }

            let result = if self.use_symlinks {
                make_symlink(&m.source, &final_dest)
            } else {
                move_file(&m.source, &final_dest)
            };
            match result {
                Ok(()) => {
                    if !self.use_symlinks {
                        // Only update DB on real moves. Symlinks leave
                        // `path_text` pointing at the original.
                        let db_updated = match update_path_in_db(&self.db_conn, m.file_id, &final_dest) {
                            Ok(()) => true,
                            Err(err) => {
                                tracing::error!(
                                    ?err,
                                    file_id = m.file_id,
                                    dst = %crate::platform::redact_path_for_log(&final_dest),
                                    "[RESTRUCTURE] moved on disk but DB path update failed; recorded for recovery"
                                );
                                record_path_update_failure(m.file_id, &m.source, &final_dest);
                                false
                            }
                        };
                        crate::shell::tags::move_sidecar(
                            std::path::Path::new(&m.source),
                            &final_dest,
                        );
                        if !db_updated {
                            failed += 1;
                            continue;
                        }
                        if record_undo {
                            applied_pairs.push((m.source.clone(), final_dest.clone()));
                            if applied_pairs.len() >= APPLY_PROGRESS_INTERVAL {
                                record_feedback_batch(&self.db_conn, &mut applied_pairs);
                            }
                        }
                    }
                    applied += 1;
                }
                Err(ApplyError::Privilege(msg)) => {
                    // The journaled entry describes a move that never happened —
                    // roll it back so undo can't replay a phantom.
                    if let (Some(j), Some(prev)) = (journal.as_mut(), journal_entry_offset) {
                        j.rollback_to(prev);
                    }
                    return Ok(RestructureApplyResult {
                        applied,
                        failed,
                        privilege_error: Some(msg),
                    });
                }
                Err(ApplyError::Other(err)) => {
                    tracing::warn!(
                        ?err,
                        src=%crate::platform::redact_path_for_log(&m.source),
                        dst=%crate::platform::redact_path_for_log(&final_dest),
                        "move failed"
                    );
                    if let (Some(j), Some(prev)) = (journal.as_mut(), journal_entry_offset) {
                        j.rollback_to(prev);
                    }
                    // D4: the move never happened, so release the reservation —
                    // otherwise a later move whose natural destination equals
                    // this (now-free) path is needlessly uniquified to " (2)".
                    if !self.use_symlinks {
                        claimed.remove(&claimed_destination_key(&final_dest));
                    }
                    failed += 1;
                }
            }
        }

        // Every entry is already individually durable (write-ahead); one final
        // sync_all covers file metadata on a clean finish. (None during an undo
        // run, record_undo=false, so a CANCELLED undo leaves the ORIGINAL
        // journal intact and the user can re-run undo for the remainder.)
        if let Some(j) = journal {
            let _ = j.file.sync_all();
        }

        // Learn-from-corrections: each applied move is an approved example, so credit
        // its filename tokens toward its destination folder for future plans. One lock
        // acquisition for the whole batch; best-effort, never fails an apply. Forward
        // applies only — `applied_pairs` is empty on an undo run (record_undo=false).
        if record_undo {
            record_feedback_batch(&self.db_conn, &mut applied_pairs);
        }
        Ok(RestructureApplyResult { applied, failed, privilege_error: None })
    }

    // ── Undo (R2 — reversible "Undo last run") ──────────────────────────────

    fn undo_journal_path(&self) -> Option<PathBuf> {
        self.undo_journal_override.clone().or_else(|| {
            crate::paths::trash_log_path()
                .ok()
                .and_then(|t| t.parent().map(|d| d.join("restructure_undo.ndjson")))
        })
    }

    fn open_undo_journal(&self) -> Result<Option<UndoJournalIter>> {
        let Some(path) = self.undo_journal_path() else {
            return Ok(None);
        };
        match File::open(&path) {
            Ok(file) => Ok(Some(UndoJournalIter {
                lines: BufReader::new(file).lines(),
            })),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err).with_context(|| format!("opening undo journal {}", path.display())),
        }
    }

    /// Undo the most recent `apply`: replay the inverse moves through `apply`
    /// itself (so the identical stale-check / containment / no-clobber / DB-update
    /// safety applies), then clear the journal so a run can't be undone twice.
    ///
    /// Replay is NEWEST-FIRST (reverse journal order): with dependent moves
    /// (A→X then B→A) the forward order restores A into the slot B currently
    /// occupies and uniquifies it into "A (2)" — silent corruption. Reverse
    /// order first gives B its home back, then A. A torn TRAILING entry (crash
    /// mid-append, before its fsync — so its move never executed under the
    /// write-ahead ordering) is skipped; torn data anywhere else fails closed.
    /// (RESTRUCTURE.md §6 reversibility; macOS parity, audit 2026-07-14)
    pub fn undo_last(&self) -> Result<RestructureApplyResult> {
        let Some(path) = self.undo_journal_path() else {
            return Ok(RestructureApplyResult { applied: 0, failed: 0, privilege_error: None });
        };
        // One forward pass collects byte spans + validates entries; replay then
        // seeks backward through the spans. No journal-sized String/Vec is
        // retained even for a million-move apply (16 B/entry of offsets).
        let Some(spans) = scan_undo_journal_spans(&path)? else {
            return Ok(RestructureApplyResult { applied: 0, failed: 0, privilege_error: None });
        };
        let total = spans.len();
        if total == 0 {
            return Ok(RestructureApplyResult { applied: 0, failed: 0, privilege_error: None });
        }
        let file = File::open(&path)
            .with_context(|| format!("reopening undo journal {}", path.display()))?;
        let inverse = ReverseUndoIter { file, spans, next: total };
        // record_undo:false so the undo's own moves DON'T overwrite the journal — a
        // cancelled undo must leave the original intact so the user can re-run it and
        // put the REMAINING files back (already-restored ones stale-skip on the
        // retry). Only a fully-completed (non-cancelled) undo clears it.
        let result = self.apply_iter_with(
            inverse,
            Some(total),
            false,
        )?;
        // Clear the journal ONLY on a fully-completed undo: not cancelled AND
        // every inverse move succeeded. A partial failure (a file locked by
        // another process, a privilege error) keeps the journal so the user can
        // re-run undo and put the REMAINING files back — the already-restored
        // ones stale-skip on the retry, exactly like the cancel path. Deleting
        // it on partial failure permanently stranded the un-restored files in
        // their group folders with no inverse-move record. (audit 2026-07-08)
        if !self.cancel.load(Ordering::Relaxed) && result.failed == 0 {
            // Re-read the journal for bounded-memory empty-directory cleanup
            // before deleting it. Repeated parents are harmless: remove_dir is
            // empty-only, and later entries simply observe an absent directory.
            self.cleanup_empty_dirs_from_journal();
            let _ = std::fs::remove_file(&path);
        }
        Ok(result)
    }

    /// Remove the empty group folders an apply created, after its undo restored the
    /// files. `std::fs::remove_dir` only succeeds on an EMPTY dir, so user files are
    /// never at risk; we additionally stay strictly inside the library root and never
    /// touch the root itself. Deepest-first so nested empties fully collapse.
    /// Best-effort. (R2 → reversibility completeness)
    fn cleanup_empty_dirs_from_journal(&self) {
        let root = self.library_root.as_path();
        let Ok(Some(entries)) = self.open_undo_journal() else {
            return;
        };
        for entry in entries.flatten() {
            let Some(dir) = Path::new(&entry.from).parent() else {
                continue;
            };
            let mut cur = dir.to_path_buf();
            while cur.as_path() != root && cur.starts_with(root) && std::fs::remove_dir(&cur).is_ok()
            {
                match cur.parent() {
                    Some(p) => cur = p.to_path_buf(),
                    None => break,
                }
            }
        }
    }
}

const APPLY_PROGRESS_INTERVAL: usize = 500;

/// Apply-progress throttle: emit on the first move, on the last, and once per
/// `interval` processed moves, so a 100k-move apply logs ~total/interval lines
/// instead of none (silent) or one-per-move (flood). Pure → the cadence is
/// unit-assertable. (F-C6-013)
fn should_emit_apply_progress(processed: usize, total: usize, interval: usize) -> bool {
    if interval == 0 || processed == 0 {
        return false;
    }
    processed == 1 || processed == total || processed % interval == 0
}

#[derive(Debug)]
#[cfg_attr(not(windows), allow(dead_code))]
enum ApplyError {
    Privilege(String),
    Other(anyhow::Error),
}

#[cfg(windows)]
fn move_file(src: &str, dst: &Path) -> std::result::Result<(), ApplyError> {
    use std::os::windows::ffi::OsStrExt;
    // Win32 file APIs silently fail past MAX_PATH (260) unless the operand
    // carries the \\?\ extended-length prefix — the engine .exe has no
    // longPathAware manifest. Every other FS site wraps via to_extended_length
    // (bulk.rs rename, platform.rs, discovery.rs, dbwriter.rs, …); restructure
    // routes files into deep semantic group folders (root + up to 200-char
    // group name + filename) that trivially exceed 260, so without the prefix
    // the move just fails (failed++) where bulk-rename of the same path works.
    let src_ext = crate::util::path_safety::to_extended_length(Path::new(src));
    let dst_ext = crate::util::path_safety::to_extended_length(dst);
    let src_w: Vec<u16> = src_ext
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let dst_w: Vec<u16> = dst_ext
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        // B3: NO MOVEFILE_REPLACE_EXISTING. An occupied destination must fail
        // the move (→ ApplyError::Other → failed++), never overwrite. The
        // caller has already resolved a collision-free `dst`, so a remaining
        // collision here means an unexpected race — fail safe rather than
        // destroy data. MOVEFILE_COPY_ALLOWED still permits cross-volume moves.
        //
        // MOVEFILE_WRITE_THROUGH: for a cross-volume move Windows performs a
        // copy-to-destination + delete-source. Without WRITE_THROUGH the call
        // can return (and the source delete become durable) before the
        // destination bytes are flushed to disk — a crash/power-loss in that
        // window leaves the source gone and the destination absent/partial,
        // an unrecoverable loss the write-ahead undo journal cannot restore
        // (it recorded the move as done). WRITE_THROUGH flushes the copy
        // before the source is deleted, restoring the source-intact-XOR-
        // destination-durable invariant every recovery path assumes. No-op
        // for same-volume moves (an atomic metadata rename).
        MoveFileExW(
            PCWSTR(src_w.as_ptr()),
            PCWSTR(dst_w.as_ptr()),
            MOVEFILE_COPY_ALLOWED | MOVEFILE_WRITE_THROUGH,
        )
        .map_err(|e| ApplyError::Other(anyhow::Error::msg(e.to_string())))
    }
}

#[cfg(windows)]
fn make_symlink(src: &str, dst: &Path) -> std::result::Result<(), ApplyError> {
    use std::os::windows::ffi::OsStrExt;
    // \\?\ prefix both operands so the link can be created (and its target
    // resolved) past MAX_PATH (260) — same rationale as move_file.
    let src_ext = crate::util::path_safety::to_extended_length(Path::new(src));
    let dst_ext = crate::util::path_safety::to_extended_length(dst);
    let src_w: Vec<u16> = src_ext
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let dst_w: Vec<u16> = dst_ext
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let flags = SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE;
    let r = unsafe {
        CreateSymbolicLinkW(
            PCWSTR(dst_w.as_ptr()),
            PCWSTR(src_w.as_ptr()),
            SYMBOLIC_LINK_FLAGS(flags.0),
        )
    };
    if r.as_bool() {
        Ok(())
    } else {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(1314) {
            // ERROR_PRIVILEGE_NOT_HELD
            Err(ApplyError::Privilege(
                "Symlink mode needs Developer Mode enabled \
                 (Settings → Privacy & security → For developers) \
                 OR an elevated FileID. Try the default 'real move' mode instead."
                    .into(),
            ))
        } else {
            Err(ApplyError::Other(anyhow::Error::msg(err.to_string())))
        }
    }
}

#[cfg(not(windows))]
fn move_file(src: &str, dst: &Path) -> std::result::Result<(), ApplyError> {
    // Portable (Linux/macOS) mirror of the Windows MoveFileExW path. The caller
    // already created the destination parent and resolved a collision-free name;
    // we re-assert both guarantees here so a standalone call is just as safe:
    //   • parent created on demand (mirrors the create_dir_all the Windows path
    //     relies on the caller for),
    //   • NEVER clobber — an occupied destination fails the move (parity with
    //     MoveFileExW dropping MOVEFILE_REPLACE_EXISTING); a remaining collision
    //     means an unexpected race, so fail safe rather than destroy data,
    //   • cross-device (EXDEV — std::fs::rename can't span filesystems, common
    //     with a NAS mount → local disk) falls back to copy + delete so the file
    //     is preserved, like MOVEFILE_COPY_ALLOWED.
    let src_path = Path::new(src);
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ApplyError::Other(anyhow::Error::msg(e.to_string())))?;
    }
    match crate::util::rename_no_replace(src_path, dst) {
        Ok(()) => Ok(()),
        Err(error) if error.raw_os_error() == Some(libc::EXDEV) => {
            let mut source = std::fs::File::open(src_path)
                .map_err(|e| ApplyError::Other(anyhow::Error::msg(e.to_string())))?;
            let permissions = source
                .metadata()
                .map_err(|e| ApplyError::Other(anyhow::Error::msg(e.to_string())))?
                .permissions();
            let mut destination = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(dst)
                .map_err(|e| ApplyError::Other(anyhow::Error::msg(e.to_string())))?;
            // Flush the destination FILE, then its PARENT DIRECTORY, before
            // unlinking the source. fsync of a file does not make its new
            // dirent durable; without the parent-dir fsync a crash after the
            // source unlink can leave the file in neither location (the source
            // unlink committed, the never-flushed destination dirent lost) —
            // an unrecoverable loss the write-ahead journal cannot restore.
            // Mirrors the Trash cross-filesystem path (shell/mod.rs
            // copy_claimed_external) and MOVEFILE_WRITE_THROUGH on Windows.
            let copied = std::io::copy(&mut source, &mut destination)
                .and_then(|_| std::fs::set_permissions(dst, permissions))
                .and_then(|_| destination.sync_all())
                .and_then(|_| {
                    if let Some(parent) = dst.parent() {
                        std::fs::File::open(parent)?.sync_all()?;
                    }
                    Ok(())
                });
            if let Err(error) = copied {
                let _ = std::fs::remove_file(dst);
                return Err(ApplyError::Other(anyhow::Error::msg(error.to_string())));
            }
            if let Err(error) = std::fs::remove_file(src_path) {
                let _ = std::fs::remove_file(dst);
                return Err(ApplyError::Other(anyhow::Error::msg(error.to_string())));
            }
            Ok(())
        }
        Err(error) => Err(ApplyError::Other(anyhow::Error::msg(error.to_string()))),
    }
}

#[cfg(not(windows))]
fn make_symlink(src: &str, dst: &Path) -> std::result::Result<(), ApplyError> {
    // The app's "use shortcuts/symlinks instead of moving" option. `dst` is the
    // link to create, `src` the existing target it points at — same operand
    // order as the Windows CreateSymbolicLinkW(dst, src) path. A pre-existing
    // `dst` makes symlink() fail naturally (no clobber). Unix symlink creation
    // is unprivileged, so there is no ApplyError::Privilege arm here.
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ApplyError::Other(anyhow::Error::msg(e.to_string())))?;
    }
    std::os::unix::fs::symlink(src, dst)
        .map_err(|e| ApplyError::Other(anyhow::Error::msg(e.to_string())))
}

fn update_path_in_db(conn: &Arc<Mutex<Connection>>, file_id: i64, new_path: &Path) -> Result<()> {
    let conn = conn.lock();
    // ENG-91: keep path_hash in sync with path_text (same as the rename command
    // + every dbwriter insert) so the column stays consistent for lookups/dedup
    // and cross-platform DB parity — a move that updated only path_text left a
    // stale hash.
    let path_text = new_path.to_string_lossy();
    let path_hash = crate::util::path_safety::stable_path_hash(&path_text);
    // prepare_cached: a plan can issue thousands of moves, so cache the parse on
    // the long-lived writer connection (codebase idiom — see bulk.rs/dbwriter.rs).
    // NFC-normalize path_search like the dbwriter insert + macOS do, so an
    // NFD-accented name stays findable by the app's NFC-normalized search query
    // (the v16 contract). Without this, a moved file is unsearchable by its
    // accented name until the next rescan re-stamps it. (audit parity fix)
    let path_search = crate::pipeline::dbwriter::nfc_path_search(&path_text);
    // OR ABORT is load-bearing: `path_text` is UNIQUE ON CONFLICT REPLACE, so a
    // PLAIN update that collides with a LIVE row already at the new path (a
    // transient earlier update failure left this file's on-disk move done but its
    // DB path stale, then a later move routes another file here; or an external
    // rename desynced the row) would silently REPLACE-delete that row and cascade
    // its user tags/person assignments. OR ABORT raises instead, and the caller's
    // record_path_update_failure recovery arm reconciles it on the next scan.
    // (audit 2026-07: rename-heal ON CONFLICT REPLACE sibling)
    let changed = conn
        .prepare_cached("UPDATE OR ABORT files SET path_text = ?1, path_hash = ?2, path_search = ?4 WHERE id = ?3")?
        .execute(params![path_text, path_hash, file_id, path_search])
        .context("DB UPDATE files.path_text")?;
    if changed != 1 {
        anyhow::bail!("DB UPDATE files.path_text affected {changed} rows (expected 1)");
    }
    Ok(())
}

/// B4 + R-#14: the current `(path_text, file_ref)` the DB holds for `file_id`, or None
/// if the row is gone. `path_text` is the authoritative name; `file_ref` (NTFS MFT
/// reference, stored `u64 as i64`) is the planned-file identity the swap guard checks
/// against the on-disk ref. `file_ref` is None for a row scanned before v8 or on a
/// volume with no readable ref.
fn current_identity_in_db(
    conn: &Arc<Mutex<Connection>>,
    file_id: i64,
) -> Result<Option<(String, Option<i64>)>> {
    let conn = conn.lock();
    let mut stmt = conn.prepare_cached("SELECT path_text, file_ref FROM files WHERE id = ?1")?;
    stmt.query_row(params![file_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?))
    })
    .optional()
    .context("DB SELECT files.path_text,file_ref")
}

/// R-#14 positive-evidence swap detector. True ONLY when both the DB's stored file_ref
/// and the on-disk file_ref are known AND differ — a different file now occupies the
/// planned source path. Any missing input (NULL stored ref; a file/volume/platform with
/// no readable ref, incl. the non-Windows stub) returns false so a legitimate move is
/// never wrongly skipped. The stored ref is read back `i64 as u64` to undo the
/// dbwriter's `u64 as i64` cast. NTFS file_ref carries a sequence number so even an MFT
/// entry reuse is caught; an APFS/HFS inode can false-MATCH on reuse (rare), which only
/// ever fails OPEN (the move proceeds), never closed.
fn file_ref_swapped(db_ref: Option<i64>, current_ref: Option<u64>) -> bool {
    match (db_ref, current_ref) {
        (Some(d), Some(c)) => (d as u64) != c,
        _ => false,
    }
}

/// Path equality that tolerates separator/case differences. Fast path is a
/// string compare (the normal case — both came from the same DB row at plan
/// time); otherwise compare canonical forms (a non-existent path canonicalizes
/// to Err and is treated as not-equal, so a vanished source is a mismatch).
fn paths_equal(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
    }
}

/// B3: resolve a destination that collides with neither an on-disk file nor a
/// destination already claimed by an earlier move in this batch, by appending
/// ` (2)`, ` (3)`, … before the extension — within the same parent so the
/// containment/reparse checks already performed on `dest` still hold.
fn unique_destination(dest: &Path, claimed: &HashSet<ClaimedDestination>) -> PathBuf {
    let occupied = |p: &Path| {
        // \\?\ prefix so a deep already-occupied destination is detected rather
        // than mis-probed as free (std::fs silently fails past MAX_PATH).
        // `claimed` is keyed by the lowercased path string so a case-only
        // difference (NTFS/APFS are case-insensitive) still registers as taken.
        claimed.contains(&claimed_destination_key(p))
            || std::fs::symlink_metadata(crate::util::path_safety::to_extended_length(p)).is_ok()
    };
    if !occupied(dest) {
        return dest.to_path_buf();
    }
    let parent = dest.parent().unwrap_or_else(|| Path::new(""));
    let stem = dest
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = dest.extension().map(|e| e.to_string_lossy().into_owned());
    for n in 2..=9999u32 {
        let name = match &ext {
            Some(e) => format!("{stem} ({n}).{e}"),
            None => format!("{stem} ({n})"),
        };
        let candidate = parent.join(name);
        if !occupied(&candidate) {
            return candidate;
        }
    }
    // Exhausted — return the original; the no-REPLACE move then fails safely.
    dest.to_path_buf()
}

/// Persist one bounded feedback batch, then release its path strings. Keeping
/// every successful pair until a million-file run completed duplicated the
/// whole plan in memory even though feedback recording itself is append-only.
fn record_feedback_batch(
    db: &Arc<Mutex<Connection>>,
    pairs: &mut Vec<(String, PathBuf)>,
) {
    if pairs.is_empty() {
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    restructure_feedback::record(
        db,
        pairs.iter().map(|(s, d)| (Path::new(s), d.as_path())),
        now,
    );
    pairs.clear();
}

/// Consume `restructure_recover.ndjson` once at engine startup: for each
/// recorded (file_id → dst) whose file is physically present at `dst`, realign
/// the stale `path_text` to `dst` (fail-closed via `update_path_in_db`'s
/// UPDATE OR ABORT, so a live conflicting row is never clobbered). This is the
/// reader the record's "recoverable even if the next scan never runs" contract
/// promised — without it, the durable record was inert and a moved-but-DB-
/// update-failed file with no `file_ref`/`content_hash` (exFAT/network volumes)
/// would strand its tags on the next scan. The file is cleared after one pass;
/// records that can't heal (file gone, or a conflict) are dropped as
/// best-effort, leaving rename-heal / undo as the remaining recovery routes.
/// Returns the number of rows realigned.
pub fn reconcile_pending_path_updates(db: &Arc<Mutex<Connection>>) -> usize {
    let Ok(trash) = crate::paths::trash_log_path() else {
        return 0;
    };
    let Some(dir) = trash.parent() else {
        return 0;
    };
    let path = dir.join("restructure_recover.ndjson");
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return 0; // no record file → nothing to do
    };
    let mut healed = 0usize;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(rec) = serde_json::from_str::<serde_json::Value>(line) else {
            continue; // torn/partial line — skip
        };
        let (Some(file_id), Some(dst)) = (
            rec.get("file_id").and_then(|v| v.as_i64()),
            rec.get("dst").and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        // Only heal when the file is actually where the record says it is.
        if !Path::new(dst).is_file() {
            continue;
        }
        // Already aligned? (a prior scan's rename-heal beat us here) — skip.
        let current: Option<String> = {
            let conn = db.lock();
            conn.query_row(
                "SELECT path_text FROM files WHERE id = ?1",
                rusqlite::params![file_id],
                |r| r.get(0),
            )
            .ok()
        };
        match current {
            Some(p) if paths_equal(&p, dst) => continue,
            None => continue, // row gone
            Some(_) => {}
        }
        if update_path_in_db(db, file_id, Path::new(dst)).is_ok() {
            healed += 1;
        }
    }
    // Best-effort single-pass consumption: clear the record file regardless so
    // it can't grow unbounded or re-heal a since-moved file.
    let _ = std::fs::remove_file(&path);
    if healed > 0 {
        tracing::info!(healed, "[RESTRUCTURE] reconciled stale path_text from recovery record");
    }
    healed
}

/// B5: best-effort durable record of a successful on-disk move whose DB
/// path-update failed, so the stale `path_text` is recoverable even if the
/// next scan (which self-heals via rename-heal on the NTFS `file_ref`) never
/// runs — reconciled at startup by [`reconcile_pending_path_updates`]. NDJSON,
/// append-only; a recovery hint, not a restore authority like `trash_log`, so
/// no HMAC. Written beside the trash log.
fn record_path_update_failure(file_id: i64, src: &str, dst: &Path) {
    let Ok(trash) = crate::paths::trash_log_path() else {
        return;
    };
    let Some(dir) = trash.parent() else {
        return;
    };
    let path = dir.join("restructure_recover.ndjson");
    let line = serde_json::json!({
        "file_id": file_id,
        "src": src,
        "dst": dst.to_string_lossy(),
    })
    .to_string();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        use std::io::Write;
        let _ = writeln!(f, "{line}");
        let _ = f.sync_all();
    }
}

/// Canonicalize a path, treating a missing target as "exists in spirit".
/// Walks up to the closest existing ancestor and canonicalizes that —
/// the unresolved tail is appended back. Lets us containment-check
/// destinations that don't exist yet (we're about to create them).
fn canonicalize_safely(p: &Path) -> Result<PathBuf> {
    if let Ok(c) = std::fs::canonicalize(p) {
        return Ok(c);
    }
    let mut cur = p.to_path_buf();
    let mut tail = PathBuf::new();
    while !cur.exists() {
        if let Some(name) = cur.file_name() {
            tail = if tail.as_os_str().is_empty() {
                PathBuf::from(name)
            } else {
                Path::new(name).join(tail)
            };
        }
        if !cur.pop() {
            break;
        }
    }
    let mut canonical = std::fs::canonicalize(&cur)
        .with_context(|| format!("canonicalize ancestor {}", cur.display()))?;
    canonical.push(tail);
    Ok(canonical)
}

fn ensure_inside_root(dest: &Path, canonical_root: &Path) -> Result<()> {
    let canonical_dest = canonicalize_safely(dest)?;
    if !canonical_dest.starts_with(canonical_root) {
        anyhow::bail!(
            "destination {} is outside library root {}",
            canonical_dest.display(),
            canonical_root.display()
        );
    }
    Ok(())
}

/// SEC-5: walk every ancestor of `path` up to (but not including) `root`
/// and return true if any of them is a reparse point (junction or
/// symlink). Used as a TOCTOU defense before MoveFileExW: even if the
/// CANONICAL path checks out, an attacker who plants a junction in the
/// destination's parent BETWEEN the canonicalize call and the MoveFileExW
/// call would redirect the write outside library_root. Refusing moves
/// that pass through reparse points eliminates that surface.
#[cfg(windows)]
fn has_reparse_point_in_chain(parent: &Path, root: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    use crate::util::path_safety::strip_extended_length;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    // `parent` is the raw (non-verbatim) destination parent from the IPC plan,
    // but `root` arrives canonicalized — on Windows that is a verbatim `\\?\C:\…`
    // path. Comparing the two prefix forms made `cur.starts_with(root)` false on
    // the FIRST iteration, so the walk broke after checking only the leaf parent
    // and never inspected intermediate ancestors — silently reducing the SEC-5
    // junction-TOCTOU defense to one level. Normalize BOTH operands with
    // strip_extended_length, which removes the `\\?\` prefix WITHOUT resolving the
    // link (std::fs::canonicalize must NOT be used here: it follows the junction
    // and defeats detection), so the ancestor walk runs up to the real root.
    let root_norm = strip_extended_length(root);
    let mut cur = parent.to_path_buf();
    loop {
        if let Ok(meta) = std::fs::symlink_metadata(&cur) {
            if (meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
                return true;
            }
        }
        // Stop once we reach (or pass) the root. Compared CASE-INSENSITIVELY and
        // component-wise: NTFS is case-insensitive, so a raw IPC parent that
        // differs only in casing from the canonical root (e.g. `d:\library\…` vs
        // canonical `D:\Library`) must still be recognized as inside it. Plain
        // `Path::starts_with` is case-sensitive and broke this walk after a single
        // level on any casing mismatch — silently reducing SEC-5 to one ancestor.
        // (audit F-A2)
        let cur_norm = strip_extended_length(&cur);
        let under = ci_starts_with(&cur_norm, &root_norm);
        let at_root = under && ci_starts_with(&root_norm, &cur_norm);
        if at_root || !under {
            break;
        }
        if !cur.pop() { break; }
    }
    false
}

/// Component-wise, case-insensitive prefix test (Windows NTFS is
/// case-insensitive). Unlike a lowercased-string `starts_with`, this respects
/// path-component boundaries so a sibling like `…\PhotosBackup` cannot
/// prefix-match `…\Photos`. (audit F-A2)
///
/// Folds with full Unicode `to_lowercase`, not `eq_ignore_ascii_case`: an
/// ASCII-only fold left a non-ASCII component (e.g. `Café` vs `CAFÉ`) compared
/// byte-exact, so a library root with a case-differing accented component made
/// `under` false on the first iteration and the SEC-5 reparse walk broke after
/// inspecting only the leaf parent — leaving every intermediate ancestor
/// unchecked. Unicode folding keeps the component-wise structure (siblings
/// still can't prefix-match) and only ever makes the walk continue further,
/// the conservative/safe direction. (audit R3-18)
#[cfg(windows)]
fn ci_starts_with(p: &Path, prefix: &Path) -> bool {
    let mut pc = p.components();
    for pre in prefix.components() {
        match pc.next() {
            Some(c)
                if c.as_os_str().to_string_lossy().to_lowercase()
                    == pre.as_os_str().to_string_lossy().to_lowercase() =>
            {
                continue
            }
            _ => return false,
        }
    }
    true
}

#[cfg(not(windows))]
fn has_reparse_point_in_chain(_parent: &Path, _root: &Path) -> bool { false }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_emit_apply_progress_cadence() {
        // Never on the zeroth processed item or with a zero interval.
        assert!(!should_emit_apply_progress(0, 1000, 500));
        assert!(!should_emit_apply_progress(500, 1000, 0));
        // First move (immediate feedback), every `interval`, and the last move.
        assert!(should_emit_apply_progress(1, 1000, 500));
        assert!(should_emit_apply_progress(500, 1000, 500));
        assert!(should_emit_apply_progress(1000, 1000, 500));
        // Silent on the in-between indices (so 100k moves → ~200 lines, not 100k).
        assert!(!should_emit_apply_progress(2, 1000, 500));
        assert!(!should_emit_apply_progress(499, 1000, 500));
        assert!(!should_emit_apply_progress(501, 1000, 500));
    }

    /// F-C6-013: a pre-cancelled apply must break before touching the filesystem
    /// — no move, and a cancel is NOT counted as a failure. Cross-platform: the
    /// cancel poll sits at the top of the loop, ahead of the (Windows-only)
    /// move_file, so the loop exits without reaching it.
    #[test]
    fn apply_honors_cancel_before_moving_any_file() {
        let root = std::env::temp_dir().join(format!("fileid-apply-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join("a.jpg");
        std::fs::write(&src, b"data").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &src.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));

        // Already cancelled before apply runs.
        let cancel = Arc::new(AtomicBool::new(true));
        let apply = RestructureApply::new(db, root.clone(), false).with_cancel(cancel);
        let dest = root.join("Sorted").join("a.jpg").to_string_lossy().into_owned();
        let res = apply
            .apply(&[move_fixture(1, &src.to_string_lossy(), &dest)])
            .unwrap();

        assert_eq!(res.applied, 0, "cancelled before any move applies");
        assert_eq!(res.failed, 0, "a cancel is not a failure");
        assert!(src.exists(), "source untouched by a cancelled apply");
        assert!(!root.join("Sorted").join("a.jpg").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A mid-stream plan read error (corrupt/vanished spool during a paged
    /// apply) must return the truthful PARTIAL result — moves already applied
    /// stay counted (so the app surfaces Undo) and the unread remainder is
    /// reported as failed — instead of aborting with Err, which the app maps
    /// to "your files are unchanged".
    #[test]
    fn stream_error_mid_apply_returns_partial_result() {
        let root = std::env::temp_dir().join(format!(
            "fileid-apply-stream-err-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join("a.jpg");
        std::fs::write(&src, b"data").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &src.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));

        let apply = RestructureApply::new(db, root.clone(), false);
        let dest = root.join("Sorted").join("a.jpg").to_string_lossy().into_owned();
        let stream = vec![
            Ok(move_fixture(1, &src.to_string_lossy(), &dest)),
            Err(anyhow::anyhow!("spooled plan truncated")),
        ];
        let res = apply.apply_iter(stream, Some(3)).unwrap();

        assert_eq!(res.applied, 1, "the completed move stays counted");
        assert_eq!(res.failed, 2, "unread remainder (total 3 - 1 processed) reported as failed");
        assert!(!src.exists(), "first move really happened on disk");
        assert!(root.join("Sorted").join("a.jpg").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Regression (audit 2026-07 — rename-heal ON CONFLICT REPLACE, sibling site):
    /// `update_path_in_db` must NOT REPLACE-delete a LIVE row already occupying the
    /// destination path. `path_text` is UNIQUE ON CONFLICT REPLACE, so before the
    /// `UPDATE OR ABORT` fix a plain UPDATE onto an occupied path silently deleted
    /// the occupant + FK-cascaded its user data. This can happen mid-restructure
    /// after a transient earlier update failure desyncs a row.
    #[test]
    fn update_path_in_db_aborts_instead_of_clobbering_a_live_row() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, "C:/lib/A.jpg"); // row to be moved
        insert_file_row(&conn, 2, "C:/lib/B.jpg"); // LIVE row occupying the target
        // Give row 2 a user tag so a cascade would be observable.
        conn.execute(
            "INSERT INTO tags (file_id, tag, source, score) VALUES (2, 'Grandma', 'user', 1.0)",
            [],
        )
        .unwrap();
        let db = Arc::new(Mutex::new(conn));

        // Move row 1 onto B.jpg, which row 2 still owns → must error, not clobber.
        let res = update_path_in_db(&db, 1, Path::new("C:/lib/B.jpg"));
        assert!(res.is_err(), "colliding path update must abort, not silently REPLACE");

        let g = db.lock();
        let rows: i64 = g.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 2, "both rows must survive the aborted update");
        let tag: i64 = g
            .query_row(
                "SELECT COUNT(*) FROM tags WHERE file_id = 2 AND tag = 'Grandma'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tag, 1, "the live row's user tag must not be FK-cascade-deleted");
    }

    #[test]
    fn update_path_in_db_rejects_a_disappeared_row() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        let db = Arc::new(Mutex::new(conn));

        let error = update_path_in_db(&db, 404, Path::new("C:/lib/moved.jpg")).unwrap_err();
        assert!(error.to_string().contains("affected 0 rows"));
    }

    #[test]
    fn apply_reports_db_path_update_failure_and_keeps_undo() {
        let root = undo_fixture_root("db-update-failure");
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.jpg");
        let destination = root.join("Sorted").join("source.jpg");
        std::fs::write(&source, b"source").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &source.to_string_lossy());
        insert_file_row(&conn, 2, &destination.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));
        let journal = root.join("undo.ndjson");
        let apply = RestructureApply::new(db.clone(), root.clone(), false)
            .with_undo_journal_path(journal.clone());

        let result = apply
            .apply(&[move_fixture(
                1,
                &source.to_string_lossy(),
                &destination.to_string_lossy(),
            )])
            .unwrap();

        assert_eq!((result.applied, result.failed), (0, 1));
        assert!(!source.exists(), "the filesystem move already completed");
        assert!(destination.exists());
        assert!(journal.exists(), "the recovery boundary must remain available");
        let stored_path: String = db
            .lock()
            .query_row("SELECT path_text FROM files WHERE id = 1", [], |row| row.get(0))
            .unwrap();
        assert_eq!(stored_path, source.to_string_lossy());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// M2: after an apply moves the file on disk but FAILS the DB path update
    /// (a live UNIQUE conflict, as in the test above), undo must still restore
    /// the file to its original location using the journal's physical evidence
    /// — the DB-derived arms can't, because path_text still names the original.
    /// Before the fix, undo stale-skipped the entry and stranded the file.
    #[test]
    fn undo_restores_a_moved_but_db_update_failed_file() {
        let root = undo_fixture_root("undo-db-update-failure");
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.jpg");
        let destination = root.join("Sorted").join("source.jpg");
        std::fs::write(&source, b"source").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &source.to_string_lossy());
        // A live row already occupies the destination path → apply's
        // update_path_in_db(1, destination) aborts on the UNIQUE conflict.
        insert_file_row(&conn, 2, &destination.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));
        let journal = root.join("undo.ndjson");
        let apply = RestructureApply::new(db.clone(), root.clone(), false)
            .with_undo_journal_path(journal.clone());

        // Forward apply: on-disk move succeeds, DB update fails.
        let fwd = apply
            .apply(&[move_fixture(
                1,
                &source.to_string_lossy(),
                &destination.to_string_lossy(),
            )])
            .unwrap();
        assert_eq!((fwd.applied, fwd.failed), (0, 1));
        assert!(!source.exists() && destination.exists());

        // The destination row (id 2) was only a fixture to force the conflict;
        // drop it so undo's move-back to `source` isn't itself blocked, then
        // undo the run.
        db.lock()
            .execute("DELETE FROM files WHERE id = 2", [])
            .unwrap();
        let undo = RestructureApply::new(db.clone(), root.clone(), false)
            .with_undo_journal_path(journal.clone())
            .undo_last()
            .unwrap();

        assert_eq!(undo.applied, 1, "undo must restore the stranded file");
        assert!(source.exists(), "file is back at its original path");
        assert!(!destination.exists(), "file left the post-move location");
        let restored: String = db
            .lock()
            .query_row("SELECT path_text FROM files WHERE id = 1", [], |row| row.get(0))
            .unwrap();
        assert_eq!(restored, source.to_string_lossy(), "DB path realigned to disk");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn ensure_inside_root_accepts_canonical_descendant() {
        let tmp = std::env::temp_dir();
        let root = tmp.join("fileid-test-root");
        let _ = std::fs::create_dir_all(&root);
        let inside = root.join("Photos").join("2024").join("a.jpg");
        let canonical_root = canonicalize_safely(&root).unwrap();
        assert!(ensure_inside_root(&inside, &canonical_root).is_ok());
    }

    #[test]
    fn unique_destination_disambiguates_collisions() {
        let tmp = std::env::temp_dir().join("fileid-uniq-dest-test");
        let _ = std::fs::create_dir_all(&tmp);
        let dest = tmp.join("audio.mp3");
        // Nothing assigned, file absent → original name.
        let assigned0: HashSet<ClaimedDestination> = HashSet::new();
        assert_eq!(unique_destination(&dest, &assigned0), dest);
        // A second move targeting the same name in-batch → " (2)".
        let mut assigned1: HashSet<ClaimedDestination> = HashSet::new();
        assigned1.insert(claimed_destination_key(&dest));
        let d2 = unique_destination(&dest, &assigned1);
        assert_eq!(d2, tmp.join("audio (2).mp3"));
        assert_ne!(d2, dest);
        // A file already on disk also forces disambiguation.
        std::fs::write(&dest, b"x").unwrap();
        let d3 = unique_destination(&dest, &assigned0);
        assert_eq!(d3, tmp.join("audio (2).mp3"));
        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn ensure_inside_root_rejects_traversal() {
        let tmp = std::env::temp_dir();
        let root = tmp.join("fileid-test-root2");
        let _ = std::fs::create_dir_all(&root);
        let canonical_root = canonicalize_safely(&root).unwrap();
        let outside = canonical_root.parent().unwrap().join("evil.jpg");
        assert!(ensure_inside_root(&outside, &canonical_root).is_err());
    }

    #[test]
    fn unique_destination_avoids_disk_and_claimed_collisions() {
        let dir = std::env::temp_dir().join(format!("fileid-uniqdest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("IMG.jpg");

        // Free → returned as-is.
        let empty = HashSet::new();
        assert_eq!(unique_destination(&dest, &empty), dest);

        // On disk → bumped to " (2)".
        std::fs::write(&dest, b"x").unwrap();
        assert_eq!(unique_destination(&dest, &empty), dir.join("IMG (2).jpg"));

        // " (2)" also claimed this batch → bumped to " (3)".
        let mut claimed = HashSet::new();
        claimed.insert(claimed_destination_key(&dir.join("IMG (2).jpg")));
        assert_eq!(unique_destination(&dest, &claimed), dir.join("IMG (3).jpg"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// DATA-INTEGRITY: NTFS/APFS are case-insensitive by default, so a target
    /// claimed earlier in the batch as "photo.jpg" and a later move to
    /// "Photo.jpg" name the SAME file. The case-folded `claimed` key must catch
    /// this so the second move uniquifies instead of silently clobbering the
    /// first. Parity with `Restructure.swift`'s lowercased claimed set.
    #[test]
    fn unique_destination_detects_case_only_claimed_collision() {
        let dir = std::env::temp_dir().join(format!("fileid-uniqdest-ci-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // First move claimed "photo.jpg" (stored lowercased, as `apply` does).
        let mut claimed = HashSet::new();
        claimed.insert(claimed_destination_key(&dir.join("photo.jpg")));

        // Second move targets the case-variant "Photo.jpg" — same file on a
        // case-insensitive FS → must be detected and bumped to " (2)".
        assert_eq!(
            unique_destination(&dir.join("Photo.jpg"), &claimed),
            dir.join("Photo (2).jpg")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn move_fixture(file_id: i64, source: &str, destination: &str) -> RestructureMove {
        RestructureMove {
            file_id,
            source: source.to_string(),
            destination: destination.to_string(),
            category: "Sorted".to_string(),
            tier: None,
            confidence: String::new(),
            reason: None,
        }
    }

    fn insert_file_row(conn: &Connection, id: i64, path: &str) {
        conn.execute(
            "INSERT INTO files (id, path_text, path_hash, size_bytes, scanned_at, kind, extension, failed) \
             VALUES (?1, ?2, 0, 4, 0.0, 'image', 'jpg', 0)",
            params![id, path],
        )
        .unwrap();
    }

    #[test]
    fn undo_retry_treats_an_already_restored_entry_as_idempotent() {
        let root = std::env::temp_dir().join(format!(
            "fileid-undo-retry-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let original_dir = root.join("incoming");
        std::fs::create_dir_all(&original_dir).unwrap();
        let original = original_dir.join("photo.jpg");
        std::fs::write(&original, b"photo").unwrap();
        let already_vacated = root.join("Photos").join("photo.jpg");

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &original.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));
        let apply = RestructureApply::new(db, root.clone(), false);
        let inverse = move_fixture(
            1,
            &already_vacated.to_string_lossy(),
            &original.to_string_lossy(),
        );

        let result = apply
            .apply_iter_with(std::iter::once(Ok(inverse)), Some(1), false)
            .unwrap();
        assert_eq!(result.applied, 0);
        assert_eq!(result.failed, 0, "retry must not become permanently stale");
        assert!(original.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    fn undo_fixture_root(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "fileid-{tag}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ))
    }

    /// Dependent moves (A→Albums/X, then B→A's vacated slot) must undo
    /// NEWEST-FIRST: forward replay restores A into the slot B still occupies
    /// and uniquifies it into "A (2).txt" — silent corruption. (audit 2026-07-14)
    #[test]
    fn undo_restores_dependent_moves_in_reverse_order() {
        let root = undo_fixture_root("undo-reverse");
        std::fs::create_dir_all(&root).unwrap();
        let a = root.join("A.txt");
        let b = root.join("B.txt");
        std::fs::write(&a, b"AAAA").unwrap();
        std::fs::write(&b, b"BBBB").unwrap();
        let a_new = root.join("Albums").join("X.txt");

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &a.to_string_lossy());
        insert_file_row(&conn, 2, &b.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));
        let journal = root.join("undo.ndjson");
        let apply = RestructureApply::new(db, root.clone(), false)
            .with_undo_journal_path(journal.clone());

        let moves = vec![
            move_fixture(1, &a.to_string_lossy(), &a_new.to_string_lossy()),
            move_fixture(2, &b.to_string_lossy(), &a.to_string_lossy()),
        ];
        let res = apply.apply(&moves).unwrap();
        assert_eq!((res.applied, res.failed), (2, 0));
        assert_eq!(std::fs::read(&a).unwrap(), b"BBBB", "B took A's vacated slot");
        assert_eq!(std::fs::read(&a_new).unwrap(), b"AAAA");

        let undo = apply.undo_last().unwrap();
        assert_eq!((undo.applied, undo.failed), (2, 0), "undo must fully restore");
        assert_eq!(std::fs::read(&a).unwrap(), b"AAAA", "A restored to its own slot");
        assert_eq!(std::fs::read(&b).unwrap(), b"BBBB", "B restored home");
        assert!(!a_new.exists());
        assert!(
            !root.join("A (2).txt").exists(),
            "forward-order replay corruption: A was uniquified instead of restored"
        );
        assert!(!journal.exists(), "completed undo clears the journal");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A torn trailing journal entry (crash mid-append, before its fsync — so
    /// its move never executed) must not abort the undo of every valid,
    /// durable entry before it. (audit 2026-07-14)
    #[test]
    fn undo_tolerates_a_torn_trailing_journal_entry() {
        let root = undo_fixture_root("undo-torn");
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join("photo.jpg");
        std::fs::write(&src, b"PIC").unwrap();
        let dst = root.join("Sorted").join("photo.jpg");

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &src.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));
        let journal = root.join("undo.ndjson");
        let apply = RestructureApply::new(db, root.clone(), false)
            .with_undo_journal_path(journal.clone());

        let res = apply
            .apply(&[move_fixture(1, &src.to_string_lossy(), &dst.to_string_lossy())])
            .unwrap();
        assert_eq!((res.applied, res.failed), (1, 0));

        {
            use std::io::Write as _;
            let mut f = OpenOptions::new().append(true).open(&journal).unwrap();
            f.write_all(b"{\"file_id\":9,\"fro").unwrap();
        }

        let undo = apply.undo_last().unwrap();
        assert_eq!((undo.applied, undo.failed), (1, 0), "valid entry still undone");
        assert!(src.exists(), "file restored despite the torn tail");
        assert!(!journal.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Fail-closed: if the undo journal cannot open, a recorded apply must
    /// abort BEFORE any file moves — undo protection is a precondition, not
    /// best-effort. (audit 2026-07-14; macOS parity)
    #[test]
    fn unopenable_undo_journal_aborts_apply_before_any_move() {
        let root = undo_fixture_root("undo-noopen");
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join("doc.txt");
        std::fs::write(&src, b"DOC").unwrap();
        let dst = root.join("Docs").join("doc.txt");

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &src.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));
        // A DIRECTORY at the journal path makes the file open fail.
        let journal = root.join("undo.ndjson");
        std::fs::create_dir_all(&journal).unwrap();
        let apply = RestructureApply::new(db, root.clone(), false)
            .with_undo_journal_path(journal.clone());

        let res = apply.apply(&[move_fixture(1, &src.to_string_lossy(), &dst.to_string_lossy())]);
        assert!(res.is_err(), "apply must fail closed without a journal");
        assert!(src.exists(), "nothing may move without undo protection");
        assert!(!dst.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A move that fails AFTER its write-ahead entry landed must roll the
    /// entry back, so undo never replays a phantom; later entries continue
    /// cleanly at the rolled-back offset. (audit 2026-07-14)
    #[test]
    fn failed_move_rolls_back_its_journal_entry() {
        let root = undo_fixture_root("undo-rollback");
        std::fs::create_dir_all(&root).unwrap();
        let missing = root.join("ghost.txt"); // DB row exists, file does not
        let real = root.join("real.txt");
        std::fs::write(&real, b"REAL").unwrap();
        let dst_missing = root.join("Sorted").join("ghost.txt");
        let dst_real = root.join("Sorted").join("real.txt");

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &missing.to_string_lossy());
        insert_file_row(&conn, 2, &real.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));
        let journal = root.join("undo.ndjson");
        let apply = RestructureApply::new(db, root.clone(), false)
            .with_undo_journal_path(journal.clone());

        let moves = vec![
            move_fixture(1, &missing.to_string_lossy(), &dst_missing.to_string_lossy()),
            move_fixture(2, &real.to_string_lossy(), &dst_real.to_string_lossy()),
        ];
        let res = apply.apply(&moves).unwrap();
        assert_eq!((res.applied, res.failed), (1, 1));

        let journal_text = std::fs::read_to_string(&journal).unwrap();
        assert_eq!(
            journal_text.lines().count(),
            1,
            "phantom entry must be rolled back: {journal_text:?}"
        );
        assert!(journal_text.contains("real.txt"));

        let undo = apply.undo_last().unwrap();
        assert_eq!((undo.applied, undo.failed), (1, 0), "no phantom replay");
        assert!(real.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An apply that journals nothing (here: a pure no-op move) must NOT
    /// truncate the previous run's journal — that undo history is the user's
    /// only path back. (audit 2026-07-14)
    #[test]
    fn non_journaling_apply_preserves_the_prior_journal() {
        let root = undo_fixture_root("undo-preserve");
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join("song.mp3");
        std::fs::write(&src, b"MP3").unwrap();
        let dst = root.join("Music").join("song.mp3");

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &src.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));
        let journal = root.join("undo.ndjson");
        let apply = RestructureApply::new(db, root.clone(), false)
            .with_undo_journal_path(journal.clone());

        let res = apply
            .apply(&[move_fixture(1, &src.to_string_lossy(), &dst.to_string_lossy())])
            .unwrap();
        assert_eq!((res.applied, res.failed), (1, 0));
        let first_journal = std::fs::read_to_string(&journal).unwrap();
        assert_eq!(first_journal.lines().count(), 1);

        // Second run: the file is already exactly where the plan wants it — a
        // no-op that journals nothing and must leave run 1's journal intact.
        let res2 = apply
            .apply(&[move_fixture(1, &dst.to_string_lossy(), &dst.to_string_lossy())])
            .unwrap();
        assert_eq!(res2.failed, 0);
        assert_eq!(
            std::fs::read_to_string(&journal).unwrap(),
            first_journal,
            "a non-journaling apply truncated the prior undo journal"
        );

        let undo = apply.undo_last().unwrap();
        assert_eq!((undo.applied, undo.failed), (1, 0));
        assert!(src.exists(), "run 1 still undoable after the no-op run");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// B3: two distinct sources sharing a basename, funnelled to the same
    /// destination, must BOTH survive — the second is uniquified, never
    /// clobbered. Windows-only: exercises the real MoveFileExW path; the
    /// portable std::fs move path is covered by the not(windows) tests below.
    #[test]
    #[cfg(windows)]
    fn apply_two_same_basename_sources_keeps_both() {
        let root = std::env::temp_dir().join(format!("fileid-apply-both-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let a_dir = root.join("a");
        let b_dir = root.join("b");
        let dest_dir = root.join("Sorted");
        std::fs::create_dir_all(&a_dir).unwrap();
        std::fs::create_dir_all(&b_dir).unwrap();
        let src_a = a_dir.join("IMG_0001.jpg");
        let src_b = b_dir.join("IMG_0001.jpg");
        std::fs::write(&src_a, b"AAAA").unwrap();
        std::fs::write(&src_b, b"BBBB").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &src_a.to_string_lossy());
        insert_file_row(&conn, 2, &src_b.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));

        let apply = RestructureApply::new(db, root.clone(), false);
        let dest = dest_dir.join("IMG_0001.jpg").to_string_lossy().into_owned();
        let moves = vec![
            move_fixture(1, &src_a.to_string_lossy(), &dest),
            move_fixture(2, &src_b.to_string_lossy(), &dest),
        ];
        let res = apply.apply(&moves).unwrap();

        assert_eq!(res.applied, 2, "both moves applied");
        assert_eq!(res.failed, 0);
        let first = dest_dir.join("IMG_0001.jpg");
        let second = dest_dir.join("IMG_0001 (2).jpg");
        assert!(first.exists() && second.exists(), "both files survived under distinct names");
        // No clobber: the two original payloads are both present.
        let mut bodies = std::collections::HashSet::new();
        bodies.insert(std::fs::read(&first).unwrap());
        bodies.insert(std::fs::read(&second).unwrap());
        assert!(bodies.contains(b"AAAA".as_slice()) && bodies.contains(b"BBBB".as_slice()));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// R3-18: ci_starts_with must fold NON-ASCII case (NTFS is case-insensitive
    /// for accented letters too), or the SEC-5 reparse-point walk breaks early
    /// on a library root with a case-differing accented component. The
    /// component-wise structure must still reject a sibling prefix.
    #[test]
    #[cfg(windows)]
    fn ci_starts_with_folds_non_ascii_and_respects_boundaries() {
        use std::path::Path;
        assert!(
            ci_starts_with(Path::new(r"D:\Photos\CAFÉ\2024"), Path::new(r"D:\Photos\café")),
            "non-ASCII case must fold (NTFS is case-insensitive for accented letters)"
        );
        assert!(
            !ci_starts_with(Path::new(r"D:\PhotosBackup"), Path::new(r"D:\Photos")),
            "a sibling must not prefix-match (component boundaries respected)"
        );
    }

    /// B4: a move whose source no longer matches the live DB row for its
    /// file_id is a stale plan and must be skipped, not executed.
    #[test]
    fn apply_skips_stale_move_when_source_mismatches_db() {
        let root = std::env::temp_dir().join(format!("fileid-apply-stale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let real = root.join("real.jpg");
        std::fs::write(&real, b"data").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        // The DB says file 1 lives at `real`, but the (stale) plan claims a
        // different source path.
        insert_file_row(&conn, 1, &real.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));

        let apply = RestructureApply::new(db, root.clone(), false);
        let stale_src = root.join("vanished.jpg").to_string_lossy().into_owned();
        let dest = root.join("Sorted").join("x.jpg").to_string_lossy().into_owned();
        let res = apply.apply(&[move_fixture(1, &stale_src, &dest)]).unwrap();

        assert_eq!(res.applied, 0, "stale move must not apply");
        assert_eq!(res.failed, 1);
        assert!(real.exists(), "the real file must be untouched");
        assert!(!root.join("Sorted").join("x.jpg").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// R-#14: the swap detector fires ONLY on a both-known mismatch — any missing
    /// input must leave the move to proceed (no false skips). Pins the i64<->u64
    /// bit-cast round-trip too (a high-bit NTFS ref stored as a negative i64).
    #[test]
    fn file_ref_swapped_only_on_positive_mismatch() {
        assert!(file_ref_swapped(Some(100), Some(200)), "both known + differ → swapped");
        assert!(!file_ref_swapped(Some(100), Some(100)), "both known + equal → not swapped");
        assert!(!file_ref_swapped(Some(-1), Some(u64::MAX)), "-1i64 as u64 == u64::MAX → equal");
        assert!(!file_ref_swapped(None, Some(200)), "no stored ref → proceed");
        assert!(!file_ref_swapped(Some(100), None), "no on-disk ref → proceed");
        assert!(!file_ref_swapped(None, None), "neither known → proceed");
    }

    /// R-#14: a real same-path swap — the DB recorded one file_ref for the planned
    /// file, but a DIFFERENT file now occupies that exact path — must be skipped, not
    /// moved. Windows-only: needs a live NTFS file_ref (the non-Windows
    /// `platform::file_ref` stub returns None, leaving the guard inert — the macOS
    /// engine's inode-based mirror has its own integration test).
    #[test]
    #[cfg(windows)]
    fn apply_skips_move_when_file_ref_swapped() {
        let root = std::env::temp_dir().join(format!("fileid-apply-swap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join("doc.pdf");
        std::fs::write(&src, b"SWAPPED-IN").unwrap();
        // The file actually on disk now. If the volume has no readable ref the guard
        // can't engage — skip the assertion rather than fail spuriously.
        let Some(real_ref) = crate::platform::file_ref(&src) else {
            let _ = std::fs::remove_dir_all(&root);
            return;
        };

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        // DB row names the SAME path but a DIFFERENT file_ref — the file we planned to
        // move, since replaced on disk by another. `real_ref ^ 1` is guaranteed != real.
        conn.execute(
            "INSERT INTO files (id, path_text, path_hash, size_bytes, scanned_at, kind, extension, failed, file_ref) \
             VALUES (1, ?1, 0, 10, 0.0, 'doc', 'pdf', 0, ?2)",
            params![src.to_string_lossy(), (real_ref ^ 1) as i64],
        )
        .unwrap();
        let db = Arc::new(Mutex::new(conn));

        let apply = RestructureApply::new(db, root.clone(), false);
        let dest = root.join("Sorted").join("doc.pdf").to_string_lossy().into_owned();
        let res = apply.apply(&[move_fixture(1, &src.to_string_lossy(), &dest)]).unwrap();

        assert_eq!(res.applied, 0, "a swapped file must not be moved");
        assert_eq!(res.failed, 1);
        assert!(src.exists(), "the swapped-in file must be left untouched");
        assert!(!root.join("Sorted").join("doc.pdf").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Portable (Linux/macOS) coverage for the std::fs move path: a real move
    /// relocates the file, creates a missing destination parent on demand, and
    /// an occupied destination is refused rather than clobbered — parity with
    /// the Windows MoveFileExW-without-REPLACE_EXISTING contract.
    #[test]
    #[cfg(not(windows))]
    fn move_file_relocates_creates_parent_and_refuses_clobber() {
        let root = std::env::temp_dir().join(format!("fileid-movefile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join("src.bin");
        std::fs::write(&src, b"PAYLOAD").unwrap();

        // Parent ("nested") does not exist yet — move_file must create it.
        let dst = root.join("nested").join("out.bin");
        move_file(&src.to_string_lossy(), &dst).expect("move succeeds");
        assert!(!src.exists(), "source removed after a successful move");
        assert_eq!(std::fs::read(&dst).unwrap(), b"PAYLOAD");

        // No clobber: a second move onto the now-occupied destination must fail
        // and leave both the existing file and the new source untouched.
        let src2 = root.join("src2.bin");
        std::fs::write(&src2, b"OTHER").unwrap();
        assert!(
            move_file(&src2.to_string_lossy(), &dst).is_err(),
            "an occupied destination must not be clobbered"
        );
        assert_eq!(std::fs::read(&dst).unwrap(), b"PAYLOAD", "existing file preserved");
        assert!(src2.exists(), "source preserved when the move is refused");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Portable coverage for the symlink ("use shortcuts instead of moving")
    /// option: the link is created pointing at the original, the parent is made
    /// on demand, and the original is left in place (symlink mode never moves).
    #[test]
    #[cfg(not(windows))]
    fn make_symlink_creates_link_to_original() {
        let root = std::env::temp_dir().join(format!("fileid-symlink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("real.bin");
        std::fs::write(&target, b"REAL").unwrap();
        let link = root.join("links").join("alias.bin");

        make_symlink(&target.to_string_lossy(), &link).expect("symlink created");
        assert!(target.exists(), "original left in place (symlink mode does not move)");
        let meta = std::fs::symlink_metadata(&link).unwrap();
        assert!(meta.file_type().is_symlink(), "a real symlink was created");
        assert_eq!(std::fs::read(&link).unwrap(), b"REAL", "link resolves to the original payload");

        let _ = std::fs::remove_dir_all(&root);
    }
}
