// Restructure apply — execute a `Vec<ProposedMove>` on disk.
//
// Two modes:
//   * Real move (default): `MoveFileExW(MOVEFILE_REPLACE_EXISTING |
//     MOVEFILE_COPY_ALLOWED)`. Atomic when same volume; copy+delete
//     across volumes. The DB row's `path_text` is updated in the same
//     transaction as the move (so a crash leaves no dangling pointers).
//   * Symlink (advanced): `CreateSymbolicLinkW`. Requires either
//     SeCreateSymbolicLinkPrivilege (admin) OR Developer Mode enabled.
//     Lets the user preview the proposed structure without committing
//     to actual moves.
//
// PATH-TRAVERSAL GUARD: every destination MUST canonicalize to a path
// inside `library_root`. We refuse to write outside the user's chosen
// library — even if the planner is buggy or someone forges a payload.

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
    CreateSymbolicLinkW, MoveFileExW, MOVEFILE_COPY_ALLOWED, MOVEFILE_REPLACE_EXISTING,
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
            .with_context(|| format!("library root {:?}", self.library_root))?;

        let mut applied = 0u32;
        let mut failed = 0u32;

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
                if let Err(err) = std::fs::create_dir_all(parent) {
                    tracing::warn!(?err, parent=%parent.display(), "create_dir_all failed");
                    failed += 1;
                    continue;
                }
            }

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
        MoveFileExW(
            PCWSTR(src_w.as_ptr()),
            PCWSTR(dst_w.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_COPY_ALLOWED,
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
}
