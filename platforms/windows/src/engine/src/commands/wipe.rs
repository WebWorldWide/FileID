//! `wipeLibrary` IPC handler — truncates all learned library state in-process
//! on the engine's single writer connection, then clears the face-crop and
//! thumbnail caches. Doing this in the engine (the sole DB-handle owner)
//! avoids the cross-process file-lock race the app hit when it deleted
//! `fileid.sqlite` right after the engine exited.

use std::sync::Arc;

use parking_lot::Mutex;
use rusqlite::Connection;

use crate::ipc::{sink::Sink, EventPayload, IpcEvent, LibraryWiped, Wrap};
use crate::{db, paths};

pub(crate) async fn handle_wipe_library(
    sink: Sink,
    db: Arc<Mutex<Connection>>,
    scan_state: Arc<Mutex<Option<crate::coordinator::ScanCoordinator>>>,
    face_cluster_active: Arc<std::sync::atomic::AtomicBool>,
) {
    // Cancel any in-flight scan and wait (bounded) for it to release the single
    // writer before truncating. Otherwise the running DbWriter keeps committing
    // batches into the just-wiped DB between truncate and scan-end — both
    // serialize on the same mutex (no corruption), but the library ends up
    // half-populated, contradicting the "wiped" confirmation, and the
    // scan_sessions 'running' row survives the wipe. The engine is the sole DB
    // owner, so enforce this interlock here regardless of what the app sends.
    if let Some(coord) = scan_state.lock().clone() {
        coord.request_cancel();
    }
    let mut waited = 0u32;
    while scan_state.lock().is_some() && waited < 100 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        waited += 1;
    }

    // Also wait (bounded) for an in-flight face-clustering pass. Clustering now
    // drops the writer lock during cluster()+consolidate() and re-acquires it
    // only to persist (commands/face_clustering.rs three-phase split), so a wipe
    // that lands during that lock-free window would commit, and the clustering
    // persist would then re-INSERT phantom `persons` rows — built from its
    // pre-wipe in-memory anchors, pointing at now-deleted faces — into the
    // just-wiped DB, leaving ghost People cards after a "wipe" that reported
    // success. The single-flight FaceClusterActiveGuard clears this flag on the
    // pass's completion/error/panic, so the wait always terminates. Mirrors the
    // scan interlock above; enforced here per the engine's single-DB-owner rule.
    let mut cluster_waited = 0u32;
    while face_cluster_active.load(std::sync::atomic::Ordering::Acquire) && cluster_waited < 100 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        cluster_waited += 1;
    }

    let wiped = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let conn = db.lock();
        db::wipe_all(&conn)
    })
    .await;

    let (ok, message) = match wiped {
        Ok(Ok(())) => {
            // Best-effort: clear the on-disk face crops + thumbnail cache so a
            // fresh scan doesn't surface stale art. Non-fatal — the DB is the
            // source of truth and the next scan regenerates these.
            clear_dir_contents(paths::faces_dir().ok());
            clear_dir_contents(paths::thumbs_dir().ok());
            tracing::info!("library wiped in-process");
            (true, None)
        }
        Ok(Err(err)) => {
            tracing::warn!(?err, "wipe_all failed");
            (false, Some(format!("{err}")))
        }
        Err(err) => {
            tracing::warn!(?err, "wipe_all spawn_blocking failed");
            (false, Some(format!("wipe task failed: {err}")))
        }
    };

    sink.send(IpcEvent::now(EventPayload::LibraryWiped(Wrap::new(
        LibraryWiped { ok, message },
    ))))
    .await;
}

/// Delete the *contents* of a directory (keep the directory itself) so the
/// engine's startup `ensure_state_dirs` doesn't have to recreate it.
fn clear_dir_contents(dir: Option<std::path::PathBuf>) {
    let Some(dir) = dir else { return };
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let _ = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
    }
}
