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

pub(crate) async fn handle_wipe_library(sink: Sink, db: Arc<Mutex<Connection>>) {
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
