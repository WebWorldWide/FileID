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

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashSet;
use std::sync::Arc;

use crate::ipc::{RestructureApplyResult, RestructureMove};

#[cfg(windows)]
use windows::core::PCWSTR;
#[cfg(windows)]
use windows::Win32::Storage::FileSystem::{
    CreateSymbolicLinkW, MoveFileExW, MOVEFILE_COPY_ALLOWED,
    SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE, SYMBOLIC_LINK_FLAGS,
};

pub struct RestructureApply {
    db_conn: Arc<Mutex<Connection>>,
    library_root: PathBuf,
    use_symlinks: bool,
}

impl RestructureApply {
    pub fn new(db_conn: Arc<Mutex<Connection>>, library_root: PathBuf, use_symlinks: bool) -> Self {
        Self { db_conn, library_root, use_symlinks }
    }

    /// Apply every proposed move. Stops on first hard error; returns the
    /// applied + failed counts. A privilege error in symlink mode short-
    /// circuits with a friendly message instead of partial writes.
    pub fn apply(&self, moves: &[RestructureMove]) -> Result<RestructureApplyResult> {
        let canonical_root = canonicalize_safely(&self.library_root)
            .with_context(|| format!("library root {}", self.library_root.display()))?;

        let mut applied = 0u32;
        let mut failed = 0u32;
        // B3: destinations claimed earlier in THIS batch, so two distinct
        // sources that map to the same basename don't collide before either
        // touches disk.
        let mut claimed: HashSet<PathBuf> = HashSet::new();

        for m in moves {
            // B4/S6/S7: bind the move to the planned file identity. The
            // payload `source` is not authoritative on its own — re-read the
            // live DB row for `file_id` and require it still names this
            // source. A stale plan (file renamed/moved/replaced since
            // planning) is skipped so we never move the wrong bytes or stamp
            // the row with a path that never held this file.
            match current_path_in_db(&self.db_conn, m.file_id) {
                Ok(Some(db_path)) if paths_equal(&db_path, &m.source) => {}
                _ => {
                    tracing::warn!(
                        file_id = m.file_id,
                        "[RESTRUCTURE] skipping stale move: source no longer matches the DB row"
                    );
                    failed += 1;
                    continue;
                }
            }

            let dest = PathBuf::from(&m.destination);
            // Path-traversal guard. The destination's parent must exist
            // OR be createable under library_root. Canonicalize the
            // closest existing ancestor and verify containment.
            if let Err(err) = ensure_inside_root(&dest, &canonical_root) {
                tracing::warn!(?err, dest=%crate::platform::redact_path_for_log(&dest), "rejecting move outside library root");
                failed += 1;
                continue;
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
                claimed.insert(d.clone());
                d
            };

            // Skip a no-op (the file already sits at the destination) rather
            // than spuriously renaming it to a ` (2)` sibling.
            if !self.use_symlinks && paths_equal(&m.source, &final_dest.to_string_lossy()) {
                applied += 1;
                continue;
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
                        if let Err(err) = update_path_in_db(&self.db_conn, m.file_id, &final_dest) {
                            // B5: the file is already relocated; do NOT silently
                            // swallow. Record it durably for recovery and log at
                            // error. (It also self-heals on the next scan via
                            // rename-heal on the NTFS file_ref.)
                            tracing::error!(
                                ?err,
                                file_id = m.file_id,
                                dst = %crate::platform::redact_path_for_log(&final_dest),
                                "[RESTRUCTURE] moved on disk but DB path update failed; recorded for recovery"
                            );
                            record_path_update_failure(m.file_id, &m.source, &final_dest);
                        }
                    }
                    applied += 1;
                }
                Err(ApplyError::Privilege(msg)) => {
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
                    failed += 1;
                }
            }
        }

        Ok(RestructureApplyResult { applied, failed, privilege_error: None })
    }
}

#[derive(Debug)]
enum ApplyError {
    Privilege(String),
    Other(anyhow::Error),
}

#[cfg(windows)]
fn move_file(src: &str, dst: &Path) -> std::result::Result<(), ApplyError> {
    use std::os::windows::ffi::OsStrExt;
    let src_w: Vec<u16> = std::ffi::OsStr::new(src)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let dst_w: Vec<u16> = dst
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
        MoveFileExW(
            PCWSTR(src_w.as_ptr()),
            PCWSTR(dst_w.as_ptr()),
            MOVEFILE_COPY_ALLOWED,
        )
        .map_err(|e| ApplyError::Other(anyhow::Error::msg(e.to_string())))
    }
}

#[cfg(windows)]
fn make_symlink(src: &str, dst: &Path) -> std::result::Result<(), ApplyError> {
    use std::os::windows::ffi::OsStrExt;
    let src_w: Vec<u16> = std::ffi::OsStr::new(src)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let dst_w: Vec<u16> = dst
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
fn move_file(_src: &str, _dst: &Path) -> std::result::Result<(), ApplyError> {
    Err(ApplyError::Other(anyhow::anyhow!("move_file requires Windows")))
}

#[cfg(not(windows))]
fn make_symlink(_src: &str, _dst: &Path) -> std::result::Result<(), ApplyError> {
    Err(ApplyError::Other(anyhow::anyhow!("symlink requires Windows")))
}

fn update_path_in_db(conn: &Arc<Mutex<Connection>>, file_id: i64, new_path: &Path) -> Result<()> {
    let conn = conn.lock();
    conn.execute(
        "UPDATE files SET path_text = ?1 WHERE id = ?2",
        params![new_path.to_string_lossy(), file_id],
    )
    .context("DB UPDATE files.path_text")?;
    Ok(())
}

/// B4: the current `path_text` the DB holds for `file_id`, or None if the row
/// is gone. The single authoritative source for what `file_id` actually names.
fn current_path_in_db(conn: &Arc<Mutex<Connection>>, file_id: i64) -> Result<Option<String>> {
    let conn = conn.lock();
    conn.query_row(
        "SELECT path_text FROM files WHERE id = ?1",
        params![file_id],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .context("DB SELECT files.path_text")
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
fn unique_destination(dest: &Path, claimed: &HashSet<PathBuf>) -> PathBuf {
    let occupied = |p: &Path| claimed.contains(p) || std::fs::symlink_metadata(p).is_ok();
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

/// B5: best-effort durable record of a successful on-disk move whose DB
/// path-update failed, so the stale `path_text` is recoverable even if the
/// next scan (which self-heals via rename-heal on the NTFS `file_ref`) never
/// runs. NDJSON, append-only; a recovery hint, not a restore authority like
/// `trash_log`, so no HMAC. Written beside the trash log.
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
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    let mut cur = parent.to_path_buf();
    loop {
        if let Ok(meta) = std::fs::symlink_metadata(&cur) {
            if (meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
                return true;
            }
        }
        // Stop once we reach (or pass) the root.
        if cur == root || !cur.starts_with(root) {
            break;
        }
        if !cur.pop() { break; }
    }
    false
}

#[cfg(not(windows))]
fn has_reparse_point_in_chain(_parent: &Path, _root: &Path) -> bool { false }

#[cfg(test)]
mod tests {
    use super::*;

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
        claimed.insert(dir.join("IMG (2).jpg"));
        assert_eq!(unique_destination(&dest, &claimed), dir.join("IMG (3).jpg"));

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

    /// B3: two distinct sources sharing a basename, funnelled to the same
    /// destination, must BOTH survive — the second is uniquified, never
    /// clobbered.
    #[test]
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
}
