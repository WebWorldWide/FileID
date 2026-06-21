//! Engine-spawn scan driver (FIX 2).
//!
//! `startScan` runs the engine's FULL ML pipeline (image tags, CLIP embeddings,
//! face detect/embed, perceptual + content hashes, doc text). It owns its own
//! async runtime + ORT sessions, so — exactly like the CLI's `scan --models`
//! ([`platforms/cli/src/scan_models.rs`]) and the desktop clients — we can't run
//! it from a library call. We spawn the `FileIDEngine` binary and speak
//! newline-delimited JSON over stdio, reusing the engine's OWN
//! [`fileid_engine::ipc`] `IpcCommand` / `IpcEvent` types so the wire contract
//! can't drift.
//!
//! This runs on a worker thread and streams [`LoadMsg::Status`] over the same
//! `mpsc` channel the DB loader uses, so the status line shows live scan
//! progress. On completion it re-reads the DB (via [`crate::data::load`]) and
//! sends [`LoadMsg::Done`], refreshing every view — no extra reload keystroke
//! needed (though `r` still works).
//!
//! Crucial difference from the CLI: the engine's stderr is routed to
//! [`Stdio::null`], NOT inherited. The TUI holds the alternate screen in raw
//! mode; inheriting engine log lines would scribble over the UI.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::Sender;

use anyhow::{Context as _, Result};
use fileid_engine::ipc::{CommandPayload, EventPayload, IpcCommand, IpcEvent, StartScanPayload};
use fileid_engine::models::registry::{self, LookupResult};

use crate::data::{self, short, LoadMsg};

/// Models the engine's `startScan` pre-flight requires (mirror of the engine's
/// `commands/scan.rs` gate, identical to the CLI's). `clip_text` is query-time
/// only; RAM++ tags degrade to CLIP scene tags when absent, so neither is gated.
const REQUIRED_MODELS: [&str; 2] = ["mobileclip_s2", "arcface"];

/// Spawn the scan on a worker thread. Non-blocking; the UI keeps drawing and
/// `q` keeps quitting. On success the thread reloads the DB and sends `Done`;
/// any failure becomes a single `LoadMsg::Error` (clears `loading`/`scanning`).
pub fn spawn_scan(
    db: PathBuf,
    root: PathBuf,
    engine_data_home: Option<PathBuf>,
    tx: Sender<LoadMsg>,
) {
    std::thread::spawn(move || match run_scan(&db, &root, engine_data_home.as_deref(), &tx) {
        Ok(summary) => {
            let _ = tx.send(LoadMsg::Status(format!("{summary} — reloading library…")));
            match data::load(&db, &tx) {
                Ok(snap) => {
                    let _ = tx.send(LoadMsg::Status(format!(
                        "{summary} · {} files indexed",
                        snap.files.len()
                    )));
                    let _ = tx.send(LoadMsg::Done(Box::new(snap)));
                }
                Err(e) => {
                    let _ = tx.send(LoadMsg::Error(format!("scan ok, reload failed: {e}")));
                }
            }
        }
        Err(e) => {
            let _ = tx.send(LoadMsg::Error(e.to_string()));
        }
    });
}

/// Pre-flight, spawn the engine, send `startScan`, and stream events until the
/// scan completes. Returns a one-line success summary or a descriptive error.
fn run_scan(
    db: &Path,
    root: &Path,
    engine_data_home: Option<&Path>,
    tx: &Sender<LoadMsg>,
) -> Result<String> {
    let root_abs = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());

    // ── Pre-flight 1: models installed? Give a clear, actionable message
    //    rather than a cryptic engine error mid-scan. ──
    let missing = missing_models();
    if !missing.is_empty() {
        let names = missing
            .iter()
            .map(|(k, n)| format!("{n} ({k})"))
            .collect::<Vec<_>>()
            .join(", ");
        let hint = if cfg!(target_os = "macos") {
            "On macOS, scan a folder with full AI in the FileID desktop app (it has the models); \
             here the TUI does model-free indexing only — or run `fileid scan <folder>` then press r."
        } else {
            "Install them in the desktop app (Settings → Local AI), then press s again."
        };
        anyhow::bail!("scan needs AI models not installed: {names}. {hint}");
    }

    // ── Pre-flight 2: engine binary located? ──
    let Some(engine_bin) = locate_engine_binary() else {
        anyhow::bail!(
            "{} not found — set FILEID_ENGINE_BIN, put it on PATH/next to fileid-tui, or build \
             it (cargo build --release in platforms/windows/src/engine).",
            engine_exe_name()
        );
    };

    // In SCRATCH mode we point the engine at the SAME data home the TUI reads
    // (below), so the engine's library IS `db` — no mismatch to warn about. With
    // an explicit `--db`, the engine still writes its OWN canonical location; a
    // pinned `--db` that differs won't reflect on reload, so surface that (CLI
    // parity).
    if engine_data_home.is_none() {
        if let Ok(engine_db) = fileid_engine::paths::db_path() {
            if engine_db != db {
                let _ = tx.send(LoadMsg::Status(format!(
                    "note: engine writes its library at {}; your --db differs",
                    short(&engine_db.to_string_lossy())
                )));
            }
        }
    }

    let _ = tx.send(LoadMsg::Status(format!("Starting engine for {}…", short(&root_abs.to_string_lossy()))));

    // stderr → null: the TUI owns the alternate screen; inheriting engine logs
    // would corrupt it. stdout carries the newline-JSON event stream.
    let mut command = Command::new(&engine_bin);
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());

    // Scratch mode (FIX 1): hand the engine its data home via the SAME env var
    // its `paths::root()` honors, so the scan writes `<home>/FileID/fileid.sqlite`
    // — exactly the scratch library the TUI opened and reloads. `prepare_scratch_dir`
    // is best-effort layout setup (see its doc).
    if let Some(home) = engine_data_home {
        prepare_scratch_dir(home);
        if cfg!(windows) {
            command.env("LOCALAPPDATA", home);
        } else {
            command.env("XDG_DATA_HOME", home);
        }
    }

    let mut child = command
        .spawn()
        .with_context(|| format!("spawning engine binary {}", engine_bin.display()))?;

    let mut stdin = child.stdin.take().context("engine stdin pipe")?;
    let stdout = child.stdout.take().context("engine stdout pipe")?;

    let cmd = IpcCommand {
        id: "fileid-tui-scan".to_string(),
        payload: CommandPayload::StartScan(StartScanPayload {
            root_path: root_abs.to_string_lossy().into_owned(),
            root_display: None,
            rescan: false,
        }),
    };
    let line = serde_json::to_string(&cmd).context("serialize startScan command")?;
    stdin
        .write_all(line.as_bytes())
        .and_then(|()| stdin.write_all(b"\n"))
        .and_then(|()| stdin.flush())
        .context("sending startScan to engine")?;

    let outcome = stream_events(tx, stdout);

    // EOF on stdin → the engine's parent-EOF watchdog exits; then reap it so we
    // don't leave a zombie. Bounded so a misbehaving engine can't wedge us.
    drop(stdin);
    let _ = wait_briefly(&mut child);

    match outcome {
        ScanOutcome::Complete { total, processed, failed, seconds } => Ok(format!(
            "Scan complete: {processed}/{total} files{} in {seconds:.1}s",
            if failed > 0 { format!(", {failed} failed") } else { String::new() }
        )),
        ScanOutcome::Error { kind, message } => anyhow::bail!("engine scan failed [{kind}]: {message}"),
        ScanOutcome::Aborted => {
            anyhow::bail!("engine exited before the scan completed (no scanComplete event)")
        }
    }
}

enum ScanOutcome {
    Complete { total: u64, processed: u64, failed: u64, seconds: f64 },
    Error { kind: String, message: String },
    Aborted,
}

/// Read the engine's newline-JSON event stream, pushing a live status line per
/// progress event and returning the terminal outcome.
fn stream_events<R: std::io::Read>(tx: &Sender<LoadMsg>, stdout: R) -> ScanOutcome {
    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let event: IpcEvent = match serde_json::from_str(trimmed) {
            Ok(e) => e,
            Err(_) => continue, // tolerate non-event chatter on stdout
        };
        match event.payload {
            EventPayload::Progress(w) => {
                let p = w.inner;
                let msg = if p.total > 0 {
                    let failed = if p.failed > 0 { format!(" · {} failed", p.failed) } else { String::new() };
                    format!(
                        "Scanning [{:?}] {}/{} ({:.0} files/s){failed}",
                        p.phase, p.processed, p.total, p.files_per_second
                    )
                } else {
                    format!("Scanning [{:?}]…", p.phase)
                };
                let _ = tx.send(LoadMsg::Status(msg));
            }
            EventPayload::PhaseChanged(w) => {
                let _ = tx.send(LoadMsg::Status(format!("Scan phase: {:?}", w.inner)));
            }
            EventPayload::DiscoveryComplete(d) => {
                let _ = tx.send(LoadMsg::Status(format!("Discovered {} files…", d.total_files)));
            }
            EventPayload::ScanComplete(w) => {
                let c = w.inner;
                return ScanOutcome::Complete {
                    total: c.total_files,
                    processed: c.processed_files,
                    failed: c.failed_files,
                    seconds: c.total_seconds,
                };
            }
            EventPayload::Error(w) => {
                let e = w.inner;
                return ScanOutcome::Error { kind: e.kind, message: e.message };
            }
            _ => {}
        }
    }
    ScanOutcome::Aborted
}

/// Wait for the engine to exit, but don't hang forever if it ignores EOF.
fn wait_briefly(child: &mut Child) -> std::io::Result<()> {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match child.try_wait()? {
            Some(_) => return Ok(()),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                return child.wait().map(|_| ());
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// Required models without an install sentinel, as `(kind, display_name)`.
/// Resolution goes through the engine's own registry so the file layout + the
/// sentinel rule can't drift from what the engine actually checks.
fn missing_models() -> Vec<(&'static str, String)> {
    REQUIRED_MODELS
        .iter()
        .filter_map(|kind| match registry::lookup_full(kind) {
            LookupResult::Found(model) => match registry::sentinel_path(&model) {
                Some(p) if p.exists() => None,
                _ => Some((*kind, model.display_name.to_string())),
            },
            LookupResult::Unknown => Some((*kind, (*kind).to_string())),
        })
        .collect()
}

fn engine_exe_name() -> &'static str {
    if cfg!(windows) {
        "FileIDEngine.exe"
    } else {
        "FileIDEngine"
    }
}

/// Locate the `FileIDEngine` binary: explicit override → beside this exe → the
/// dev-layout engine target dir → PATH. Mirrors the CLI's resolver.
fn locate_engine_binary() -> Option<PathBuf> {
    let exe = engine_exe_name();

    if let Some(p) = std::env::var_os("FILEID_ENGINE_BIN") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }

    if let Ok(cur) = std::env::current_exe() {
        if let Some(dir) = cur.parent() {
            let beside = dir.join(exe);
            if beside.is_file() {
                return Some(beside);
            }
            // Dev layout: .../platforms/tui/target/<profile>/fileid-tui →
            // .../platforms/windows/src/engine/target/<profile>/FileIDEngine.
            if let Some(platforms) = dir.ancestors().find(|a| a.ends_with("platforms")) {
                for profile in ["release", "debug"] {
                    let cand = platforms.join("windows/src/engine/target").join(profile).join(exe);
                    if cand.is_file() {
                        return Some(cand);
                    }
                }
            }
        }
    }

    which_on_path(exe)
}

fn which_on_path(exe: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(exe)).find(|cand| cand.is_file())
}

/// Best-effort scratch-state setup before a scratch-mode scan (FIX 1). Ensures
/// the engine's scratch root (`<data_home>/FileID`) exists, and on Linux/Windows
/// — where the engine resolves its model dir from that same root — links the
/// REAL model directory in so a scratch scan can still find weights. Everything
/// is best-effort: each step swallows its error so this can never block or fail
/// a scan. macOS resolves models from `~/Library/Application Support/FileID/…`
/// independent of the data home, so no link is needed (and we make no symlink on
/// the user's Mac).
fn prepare_scratch_dir(data_home: &Path) {
    let root = data_home.join("FileID");
    let _ = std::fs::create_dir_all(&root);

    #[cfg(not(target_os = "macos"))]
    {
        let link = root.join("Models");
        if !link.exists() {
            if let Ok(real) = fileid_engine::paths::models_dir() {
                if real.is_dir() && real != link {
                    let _ = symlink_dir(&real, &link);
                }
            }
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn symlink_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

#[cfg(windows)]
fn symlink_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(src, dst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_exe_name_matches_platform() {
        let name = engine_exe_name();
        if cfg!(windows) {
            assert_eq!(name, "FileIDEngine.exe");
        } else {
            assert_eq!(name, "FileIDEngine");
        }
    }

    #[test]
    fn missing_or_unknown_models_listed_without_panicking() {
        // In a model-less dev env this returns the two required sentinels; the
        // point is the registry lookup + sentinel probe never panic.
        let missing = missing_models();
        assert!(missing.len() <= REQUIRED_MODELS.len());
        for (kind, _name) in &missing {
            assert!(REQUIRED_MODELS.contains(kind));
        }
    }
}
