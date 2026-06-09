// Restructure apply — execute a `Vec<ProposedMove>` on disk.
//
// Two modes:
//   * Real move (default): `MoveFileExW(MOVEFILE_COPY_ALLOWED)` — NON-
//     overwriting. Atomic when same volume; copy+delete across volumes.
//     Colliding destination names are disambiguated ("name (2).ext") so a
//     move can never silently destroy an existing file. The filesystem move
//     and the DB `path_text` update are SEPARATE, non-atomic steps; crash-
//     consistency comes from the rename/move heal (content_hash / file_ref)
//     reconciliation on the next scan, not from a shared transaction.
//   * Symlink (advanced): `CreateSymbolicLinkW`. Requires either
//     SeCreateSymbolicLinkPrivilege (admin) OR Developer Mode enabled.
//     Lets the user preview the proposed structure without committing
//     to actual moves.
//
// PATH-TRAVERSAL GUARD: every destination MUST canonicalize to a path
// inside `library_root`. We refuse to write outside the user's chosen
// library — even if the planner is buggy or someone forges a payload.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection};
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
        // Destinations already claimed by an earlier move in THIS batch, so two
        // NEW colliding files don't both target the same path before either is
        // written.
        let mut assigned: HashSet<PathBuf> = HashSet::new();

        for m in moves {
            let dest = PathBuf::from(&m.destination);
            // Path-traversal guard. The destination's parent must exist
            // OR be createable under library_root. Canonicalize the
            // closest existing ancestor and verify containment.
            if let Err(err) = ensure_inside_root(&dest, &canonical_root) {
                tracing::warn!(?err, dest=%dest.display(), "rejecting move outside library root");
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
                        parent=%parent.display(),
                        "rejecting move: pre-existing reparse point in destination parent chain"
                    );
                    failed += 1;
                    continue;
                }
                if let Err(err) = std::fs::create_dir_all(parent) {
                    tracing::warn!(?err, parent=%parent.display(), "create_dir_all failed");
                    failed += 1;
                    continue;
                }
                // SEC-5: TOCTOU defense, pass 2. Re-check after
                // create_dir_all. The window between the pre-check and
                // here is small but non-zero; defense in depth is cheap.
                if has_reparse_point_in_chain(parent, &canonical_root) {
                    tracing::warn!(
                        parent=%parent.display(),
                        "rejecting move: reparse point appeared after create_dir_all"
                    );
                    failed += 1;
                    continue;
                }
            }

            // Pick a non-colliding destination — never overwrite an existing
            // file or a sibling move in this batch. The move APIs are also
            // non-overwriting (no MOVEFILE_REPLACE_EXISTING), so this is the
            // sole place a collision is resolved.
            let dest = unique_destination(&dest, &assigned);
            assigned.insert(dest.clone());

            let result = if self.use_symlinks {
                make_symlink(&m.source, &dest)
            } else {
                move_file(&m.source, &dest)
            };
            match result {
                Ok(()) => {
                    if !self.use_symlinks {
                        // Only update DB on real moves. Symlinks leave
                        // `path_text` pointing at the original.
                        if let Err(err) = update_path_in_db(&self.db_conn, m.file_id, &dest) {
                            tracing::warn!(?err, file_id = m.file_id, "DB path update failed");
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
                    tracing::warn!(?err, src=%m.source, dst=%m.destination, "move failed");
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
    // Extended-length (\\?\) form so deep (>MAX_PATH) moves don't silently
    // fail — matches every other FS-access site in the engine.
    let src_p = crate::util::path_safety::to_extended_length(Path::new(src));
    let dst_p = crate::util::path_safety::to_extended_length(dst);
    let src_w: Vec<u16> = src_p
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let dst_w: Vec<u16> = dst_p
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        // NO MOVEFILE_REPLACE_EXISTING: collisions are pre-resolved by
        // unique_destination, and without the flag MoveFileExW refuses to
        // clobber (ERROR_ALREADY_EXISTS) as a last-resort safety net rather
        // than destroying an existing file.
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
    let src_p = crate::util::path_safety::to_extended_length(Path::new(src));
    let dst_p = crate::util::path_safety::to_extended_length(dst);
    let src_w: Vec<u16> = src_p
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let dst_w: Vec<u16> = dst_p
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

/// Pick a destination that collides with neither an existing file on disk nor
/// a destination already claimed by an earlier move in this batch. Appends
/// " (2)", " (3)", … before the extension. This is what prevents two distinct
/// source files that share a basename and route to the same bucket (e.g. two
/// `IMG_0001.jpg` into Photos/2024/06, or every `audio.mp3` into a flat Audio/)
/// from clobbering one another — the old MOVEFILE_REPLACE_EXISTING destroyed
/// the first file irrecoverably.
fn unique_destination(dest: &Path, assigned: &HashSet<PathBuf>) -> PathBuf {
    if !dest.exists() && !assigned.contains(dest) {
        return dest.to_path_buf();
    }
    let parent = dest.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = dest
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = dest.extension().map(|s| s.to_string_lossy().into_owned());
    let mut n: u32 = 2;
    loop {
        let name = match &ext {
            Some(e) => format!("{stem} ({n}).{e}"),
            None => format!("{stem} ({n})"),
        };
        let candidate = parent.join(name);
        // Cap defensively; if we somehow can't find a free name, return the
        // last candidate — the non-overwriting move then fails for that file
        // (counted as `failed`) rather than destroying anything.
        if (!candidate.exists() && !assigned.contains(&candidate)) || n >= 9999 {
            return candidate;
        }
        n += 1;
    }
}

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
    fn unique_destination_disambiguates_collisions() {
        let tmp = std::env::temp_dir().join("fileid-uniq-dest-test");
        let _ = std::fs::create_dir_all(&tmp);
        let dest = tmp.join("audio.mp3");
        // Nothing assigned, file absent → original name.
        let assigned0: HashSet<PathBuf> = HashSet::new();
        assert_eq!(unique_destination(&dest, &assigned0), dest);
        // A second move targeting the same name in-batch → " (2)".
        let mut assigned1: HashSet<PathBuf> = HashSet::new();
        assigned1.insert(dest.clone());
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
}
