//! Engine-spawn scan driver.
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

use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{ChildStderr, Stdio};
use std::sync::mpsc::SyncSender as Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result};
use fileid_engine::client::{self, ScanOutcome};
use fileid_engine::ipc::{EventPayload, ScanPhase};
use fileid_engine::models::required::{missing_required, MissingModel};
use fileid_engine::util::display::file_size;

use crate::data::{self, short, LoadMsg};

/// Spawn the scan on a worker thread. Non-blocking; the UI keeps drawing and
/// `q` keeps quitting. On success the thread reloads the DB and sends `Done`;
/// any failure becomes a single `LoadMsg::Error` (clears `loading`/`scanning`).
pub fn spawn_scan(
    db: PathBuf,
    root: PathBuf,
    engine_data_home: Option<PathBuf>,
    query: String,
    tx: Sender<LoadMsg>,
    generation: u64,
) {
    std::thread::spawn(
        move || match run_scan(&db, &root, engine_data_home.as_deref(), &tx) {
            Ok(summary) => {
                let _ = tx.send(LoadMsg::Status(format!(
                    "{} — reloading library…",
                    summary.message
                )));
                if summary.failed > 0 {
                    let _ = data::send_versioned(
                        &tx,
                        generation,
                        LoadMsg::ScanPartial(summary.message.clone()),
                    );
                }
                match data::load(&db, &query, &tx, generation) {
                    Ok(snap) => {
                        let _ = tx.send(LoadMsg::Status(format!(
                            "{} · {} files indexed",
                            summary.message, snap.total_files
                        )));
                        let _ =
                            data::send_versioned(&tx, generation, LoadMsg::Done(Box::new(snap)));
                        data::run_deferred_dupes(&db, &tx, generation);
                    }
                    Err(e) => {
                        let _ = data::send_versioned(
                            &tx,
                            generation,
                            LoadMsg::Error(format!("scan ok, reload failed: {e}")),
                        );
                    }
                }
            }
            Err(e) => {
                let _ = data::send_versioned(&tx, generation, LoadMsg::Error(e.to_string()));
            }
        },
    );
}

/// Pre-flight, spawn the engine, send `startScan`, and stream events until the
/// scan completes. Returns a one-line success summary or a descriptive error.
struct ScanSummary {
    message: String,
    failed: u64,
}

fn run_scan(
    db: &Path,
    root: &Path,
    engine_data_home: Option<&Path>,
    tx: &Sender<LoadMsg>,
) -> Result<ScanSummary> {
    let root_abs = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());

    // ── Pre-flight 1: models installed? Give a clear, actionable message
    //    rather than a cryptic engine error mid-scan. Resolves against the SAME
    //    dir the engine loads from (see `missing_models`), so this gate, the
    //    standing banner, and the engine all agree. ──
    let missing = missing_required();
    if !missing.is_empty() {
        anyhow::bail!("{}", message_for_missing(&missing));
    }

    // ── Pre-flight 1.5 (macOS): ONNX Runtime installed? The engine's
    //    `load-dynamic` build needs `libonnxruntime.dylib`, but `ort`'s
    //    `download-binaries` ships only a static lib for arm64 — a separate,
    //    one-time install from the model download. No-op off macOS. ──
    #[cfg(target_os = "macos")]
    if !fileid_engine::ort_runtime::is_available() {
        anyhow::bail!("{}", runtime_missing_message());
    }

    // ── Pre-flight 2: engine binary located? ──
    let Some(engine_bin) = client::locate_engine() else {
        anyhow::bail!(
            "{} not found — set FILEID_ENGINE_BIN, put it on PATH/next to fileid-tui, or build \
             it (cargo build --release in platforms/windows/src/engine).",
            client::engine_exe_name()
        );
    };

    let _ = tx.send(LoadMsg::Status(format!(
        "Starting engine for {}…",
        short(&root_abs.to_string_lossy())
    )));

    // stderr → a captured pipe, NOT inherited: the TUI owns the alternate screen,
    // so the engine must never write to the real terminal. stdout carries the
    // newline-JSON event stream; stderr (engine logs/panics) is drained on a
    // separate thread into a bounded ring so that if the engine dies WITHOUT an
    // IPC `error` event (a crash/panic/dlopen failure), its tail still surfaces in
    // the abort message instead of a blank "exited before the scan completed".
    // In scratch mode, hand the engine the same data home the TUI resolved.
    // its `paths::root()` honors, so the scan writes `<home>/FileID/fileid.sqlite`
    // — exactly the scratch library the TUI opened and reloads. `prepare_scratch_dir`
    // is best-effort layout setup (see its doc).
    if let Some(home) = engine_data_home {
        prepare_scratch_dir(home);
    }
    let (storage_env, storage_path) = engine_storage_env(db, engine_data_home);
    let mut child = client::spawn(
        &engine_bin,
        &[(storage_env, storage_path.as_os_str())],
        Stdio::piped(),
    )
    .with_context(|| format!("spawning engine binary {}", engine_bin.display()))?;

    let mut stdin = child.stdin.take().context("engine stdin pipe")?;
    let stdout = child.stdout.take().context("engine stdout pipe")?;
    let stderr = child.stderr.take().context("engine stderr pipe")?;
    let stderr_tail = Arc::new(Mutex::new(Vec::<u8>::new()));
    let stderr_reader = spawn_stderr_capture(stderr, Arc::clone(&stderr_tail));

    client::send_command(
        &mut stdin,
        &client::start_scan_command("fileid-tui-scan", &root_abs, false),
    )
    .context("sending startScan to engine")?;

    let outcome = client::drive_scan(stdout, &mut stdin, "fileid-tui-scan", |payload| {
        if let Some(status) = scan_status(payload) {
            let _ = tx.send(LoadMsg::Status(status));
        }
    });

    // EOF on stdin → the engine's parent-EOF watchdog exits; then reap it so we
    // don't leave a zombie. Bounded so a misbehaving engine can't wedge us.
    drop(stdin);
    let _ = client::wait_briefly(&mut child, Duration::from_secs(10));
    // stderr hits EOF once the engine exits (or is killed above), so the reader
    // thread finishes; join it before reading the captured tail.
    let _ = stderr_reader.join();

    match outcome {
        ScanOutcome::Complete { scan, clustering } => {
            let (total, processed, failed, seconds) = (
                scan.total_files,
                scan.processed_files,
                scan.failed_files,
                scan.total_seconds,
            );
            let (people, faces) = (clustering.person_count, clustering.face_count);
            Ok(ScanSummary {
                message: if failed > 0 {
                    format!(
                    "Scan partial: {processed}/{total} files, {failed} failed in {seconds:.1}s · {people} people from {faces} faces"
                )
                } else {
                    format!(
                    "Scan complete: {processed}/{total} files in {seconds:.1}s · {people} people from {faces} faces"
                )
                },
                failed,
            })
        }
        // The engine's own `models_not_installed` (the pre-flight gate didn't
        // catch it — e.g. a pin bump) maps to the SAME actionable TUI message.
        ScanOutcome::Error { kind, message } => {
            if kind == "models_not_installed" {
                anyhow::bail!("{}", message_for_missing(&missing_required()))
            }
            // The engine's macOS `runtime_not_installed` (the pre-flight didn't
            // catch it — e.g. the dylib vanished) maps to the one-time runtime
            // install guidance, distinct from the model download (D).
            if kind == "runtime_not_installed" {
                anyhow::bail!("{}", runtime_missing_message())
            }
            if kind.starts_with("face_clustering") {
                anyhow::bail!("scan completed, but face grouping failed: {message}")
            }
            anyhow::bail!("engine scan failed [{kind}]: {message}")
        }
        // No IPC `error` event arrived — surface whatever the engine logged on
        // stderr (a crash/panic/missing-library failure) so it isn't a dead end.
        ScanOutcome::Aborted => {
            let tail = stderr_tail_summary(&stderr_tail);
            if tail.is_empty() {
                anyhow::bail!("engine exited before the scan completed (no scanComplete event)")
            }
            anyhow::bail!("engine exited before the scan completed — engine log: {tail}")
        }
    }
}

/// One status line per engine event worth showing. Terminal phases carry no
/// standalone text: `Failed`/`Cancelled` would read as the last word when the
/// real reason is the error outcome, and `Completed` is superseded by the
/// summary.
fn scan_status(payload: &EventPayload) -> Option<String> {
    match payload {
        EventPayload::Progress(w) => {
            let p = &w.inner;
            Some(if p.total > 0 {
                let failed = if p.failed > 0 {
                    format!(" · {} failed", p.failed)
                } else {
                    String::new()
                };
                format!(
                    "Scanning [{:?}] {}/{} ({:.0} files/s){failed}",
                    p.phase, p.processed, p.total, p.files_per_second
                )
            } else {
                format!("Scanning [{:?}]…", p.phase)
            })
        }
        EventPayload::PhaseChanged(w) => match w.inner {
            ScanPhase::Discovering | ScanPhase::Tagging | ScanPhase::PostScan => {
                Some(format!("Scan phase: {:?}", w.inner))
            }
            ScanPhase::Idle | ScanPhase::Completed | ScanPhase::Cancelled | ScanPhase::Failed => {
                None
            }
        },
        EventPayload::DiscoveryComplete(d) => Some(format!("Discovered {} files…", d.total_files)),
        EventPayload::ScanComplete(_) => Some("Grouping detected faces…".to_string()),
        _ => None,
    }
}

/// Display names of the required models that aren't installed (empty ⇒ all
/// present) — the same gate the engine's `startScan` enforces.
pub fn missing_models_display() -> Vec<String> {
    missing_required()
        .into_iter()
        .map(|missing| missing.display_name)
        .collect()
}

/// One-line "ONNX Runtime not installed" message (macOS). Distinct from the
/// model download (D) — the runtime is the inference library that loads the
/// models, and on macOS it's a separate one-time install. Shared by the scan
/// pre-flight and the engine's `runtime_not_installed` mapping.
fn runtime_missing_message() -> String {
    format!(
        "ONNX Runtime not installed — run `{}` in a shell (or `brew install onnxruntime`), \
         then press s again. One-time setup, separate from the AI models (D).",
        fileid_engine::ort_runtime::INSTALL_COMMAND
    )
}

fn message_for_missing(missing: &[MissingModel]) -> String {
    if missing.is_empty() {
        return "AI models not installed — press D on the Settings tab to download them, then \
                press s again."
            .to_string();
    }
    let kinds = missing
        .iter()
        .map(|missing| missing.kind)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "AI models not installed — press D to download (~{} for the {} needed: {kinds}), then \
         press s again.",
        file_size(missing.iter().map(|missing| missing.approx_bytes).sum()),
        missing.len(),
    )
}

/// Capture cap for the engine's stderr ring (last bytes only) — room for a panic
/// message plus a few log lines, bounded so a chatty/looping engine can't grow it
/// without limit. Drained on its OWN thread ([`spawn_stderr_capture`]) so a full
/// stderr pipe can never block (deadlock) the stdout IPC reader.
const STDERR_CAP: usize = 4 * 1024;

/// Drain `stderr` into `tail`, keeping only the last [`STDERR_CAP`] bytes.
fn spawn_stderr_capture(
    stderr: ChildStderr,
    tail: Arc<Mutex<Vec<u8>>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Ok(mut buf) = tail.lock() {
                        buf.extend_from_slice(&chunk[..n]);
                        if buf.len() > STDERR_CAP {
                            let excess = buf.len() - STDERR_CAP;
                            buf.drain(..excess);
                        }
                    }
                }
            }
        }
    })
}

/// Compact single-line summary of the captured stderr tail for an `Aborted`
/// error (the status line is one line): the last couple of non-empty log lines,
/// joined, length-bounded.
fn stderr_tail_summary(tail: &Arc<Mutex<Vec<u8>>>) -> String {
    let Ok(bytes) = tail.lock() else {
        return String::new();
    };
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let joined = lines
        .iter()
        .rev()
        .take(2)
        .rev()
        .copied()
        .collect::<Vec<_>>()
        .join(" · ");
    const MAX: usize = 300;
    if joined.chars().count() > MAX {
        let kept: String = joined
            .chars()
            .rev()
            .take(MAX - 1)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        format!("…{kept}")
    } else {
        joined
    }
}

fn engine_storage_env<'a>(db: &'a Path, data_home: Option<&'a Path>) -> (&'static str, &'a Path) {
    match data_home {
        Some(home) if cfg!(windows) => ("LOCALAPPDATA", home),
        Some(home) => ("XDG_DATA_HOME", home),
        None => ("FILEID_DB", db),
    }
}

/// Best-effort scratch-state setup before a scratch-mode scan. Ensures
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

    /// The `models_not_installed` message is TUI-appropriate — press D, the
    /// download size for the gated models, their kinds, and NONE of the
    /// desktop-app "Welcome screen" wording.
    #[test]
    fn models_not_installed_message_is_tui_appropriate() {
        let model = |kind, approx_bytes| MissingModel {
            kind,
            display_name: kind.to_string(),
            approx_bytes,
        };
        let missing = vec![
            model("mobileclip_s2", 350_000_000),
            model("arcface", 38_000_000),
        ];
        let msg = message_for_missing(&missing);
        assert!(
            msg.contains("press D"),
            "must tell the user to press D: {msg}"
        );
        assert!(
            msg.contains("the 2 needed: mobileclip_s2, arcface"),
            "must name the gated kinds: {msg}"
        );
        assert!(
            msg.contains("~388.0 MB"),
            "must state the download size: {msg}"
        );
        assert!(
            msg.contains("press s again"),
            "must tell the user to retry: {msg}"
        );
        assert!(
            !msg.to_lowercase().contains("welcome"),
            "no desktop-app wording: {msg}"
        );

        // Empty list → the generic but still actionable fallback.
        assert!(message_for_missing(&[]).contains("press D"));
    }

    /// The captured-stderr summary collapses to one bounded line for the status
    /// row, keeping the last lines and never panicking on empty/huge input.
    #[test]
    fn stderr_tail_summary_is_one_bounded_line() {
        let tail = Arc::new(Mutex::new(Vec::new()));
        assert_eq!(stderr_tail_summary(&tail), "");

        *tail.lock().unwrap() = b"line one\n\nlast line\n".to_vec();
        let s = stderr_tail_summary(&tail);
        assert!(s.contains("last line"), "keeps the tail: {s}");
        assert!(!s.contains('\n'), "single line: {s:?}");

        *tail.lock().unwrap() = vec![b'x'; 10_000];
        assert!(stderr_tail_summary(&tail).chars().count() <= 300);
    }

    #[test]
    fn explicit_db_is_forwarded_to_the_engine_child() {
        let db = Path::new("/isolated/test.sqlite");
        assert_eq!(engine_storage_env(db, None), ("FILEID_DB", db));
    }

    #[test]
    fn scratch_scan_uses_the_platform_data_home() {
        let db = Path::new("/scratch/FileID/fileid.sqlite");
        let home = Path::new("/scratch");
        let (name, value) = engine_storage_env(db, Some(home));
        assert_eq!(
            name,
            if cfg!(windows) {
                "LOCALAPPDATA"
            } else {
                "XDG_DATA_HOME"
            }
        );
        assert_eq!(value, home);
    }
}
