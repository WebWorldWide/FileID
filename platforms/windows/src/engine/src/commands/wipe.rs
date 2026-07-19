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
    scan_cancel_requested: Arc<std::sync::atomic::AtomicBool>,
    face_cluster_active: Arc<std::sync::atomic::AtomicBool>,
    deep_analyze_cancel: Arc<std::sync::atomic::AtomicBool>,
    deep_analyze_active: Arc<std::sync::atomic::AtomicBool>,
    restructure_cancel: Arc<std::sync::atomic::AtomicBool>,
    restructure_active: Arc<std::sync::atomic::AtomicBool>,
    mutation_gate: Arc<tokio::sync::Mutex<()>>,
) {
    scan_cancel_requested.store(true, std::sync::atomic::Ordering::Release);
    if let Some(coord) = scan_state.lock().clone() {
        coord.request_cancel();
    }
    deep_analyze_cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    restructure_cancel.store(true, std::sync::atomic::Ordering::Relaxed);

    let _exclusive = match tokio::time::timeout(
        std::time::Duration::from_secs(10),
        mutation_gate.lock_owned(),
    )
    .await
    {
        Ok(permit) => permit,
        Err(_) => {
            let message = format!(
                "Timed out waiting for library operations to stop (scan={}, faces={}, deepAnalyze={}, restructure={}). Nothing was wiped.",
                scan_state.lock().is_some(),
                face_cluster_active.load(std::sync::atomic::Ordering::Acquire),
                deep_analyze_active.load(std::sync::atomic::Ordering::Acquire),
                restructure_active.load(std::sync::atomic::Ordering::Acquire),
            );
            tracing::warn!("{message}");
            sink.send(IpcEvent::now(EventPayload::LibraryWiped(Wrap::new(
                LibraryWiped { ok: false, message: Some(message) },
            ))))
            .await;
            return;
        }
    };

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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn exclusive_gate_times_out_while_a_mutation_is_active() {
        let gate = Arc::new(tokio::sync::Mutex::new(()));
        let read = gate.clone().lock_owned().await;
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(10),
            gate.clone().lock_owned(),
        )
        .await;
        assert!(result.is_err());
        drop(read);
        assert!(tokio::time::timeout(
            std::time::Duration::from_secs(1),
            gate.lock_owned(),
        )
        .await
        .is_ok());
    }
}
