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

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result};
use fileid_engine::ipc::{
    CommandPayload, EventPayload, IpcCommand, IpcEvent, ScanPhase, StartScanPayload,
};
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
    query: String,
    tx: Sender<LoadMsg>,
) {
    std::thread::spawn(
        move || match run_scan(&db, &root, engine_data_home.as_deref(), &tx) {
            Ok(summary) => {
                let _ = tx.send(LoadMsg::Status(format!("{summary} — reloading library…")));
                match data::load(&db, &query, &tx) {
                    Ok(snap) => {
                        let _ = tx.send(LoadMsg::Status(format!(
                            "{summary} · {} files indexed",
                            snap.total_files
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
        },
    );
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
    //    rather than a cryptic engine error mid-scan. Resolves against the SAME
    //    dir the engine loads from (see `missing_models`), so this gate, the
    //    standing banner, and the engine all agree. ──
    if !missing_models().is_empty() {
        anyhow::bail!("{}", missing_models_message());
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
    let mut command = Command::new(&engine_bin);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

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
    let stderr = child.stderr.take().context("engine stderr pipe")?;
    let stderr_tail = Arc::new(Mutex::new(Vec::<u8>::new()));
    let stderr_reader = spawn_stderr_capture(stderr, Arc::clone(&stderr_tail));

    let cmd = IpcCommand {
        id: "fileid-tui-scan".to_string(),
        payload: CommandPayload::StartScan(StartScanPayload {
            root_path: root_abs.to_string_lossy().into_owned(),
            root_display: None,
            rescan: false,
            excluded_paths: None,
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
    // stderr hits EOF once the engine exits (or is killed above), so the reader
    // thread finishes; join it before reading the captured tail.
    let _ = stderr_reader.join();

    match outcome {
        ScanOutcome::Complete {
            total,
            processed,
            failed,
            seconds,
        } => Ok(format!(
            "Scan complete: {processed}/{total} files{} in {seconds:.1}s",
            if failed > 0 {
                format!(", {failed} failed")
            } else {
                String::new()
            }
        )),
        // The engine's own `models_not_installed` (the pre-flight gate didn't
        // catch it — e.g. a pin bump) maps to the SAME actionable TUI message.
        ScanOutcome::Error { kind, message } => {
            if kind == "models_not_installed" {
                anyhow::bail!("{}", missing_models_message())
            }
            // The engine's macOS `runtime_not_installed` (the pre-flight didn't
            // catch it — e.g. the dylib vanished) maps to the one-time runtime
            // install guidance, distinct from the model download (D).
            if kind == "runtime_not_installed" {
                anyhow::bail!("{}", runtime_missing_message())
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

enum ScanOutcome {
    Complete {
        total: u64,
        processed: u64,
        failed: u64,
        seconds: f64,
    },
    Error {
        kind: String,
        message: String,
    },
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
                };
                let _ = tx.send(LoadMsg::Status(msg));
            }
            EventPayload::PhaseChanged(w) => {
                // Only surface in-progress phases. The TERMINAL ones carry no
                // useful standalone text: `Failed`/`Cancelled` would print a
                // misleading bare "Scan phase: Failed" (looks like the last word
                // when the real reason is the `Error` outcome below), and
                // `Completed` is superseded by the `ScanComplete` summary.
                match w.inner {
                    ScanPhase::Discovering | ScanPhase::Tagging | ScanPhase::PostScan => {
                        let _ = tx.send(LoadMsg::Status(format!("Scan phase: {:?}", w.inner)));
                    }
                    ScanPhase::Idle
                    | ScanPhase::Completed
                    | ScanPhase::Cancelled
                    | ScanPhase::Failed => {}
                }
            }
            EventPayload::DiscoveryComplete(d) => {
                let _ = tx.send(LoadMsg::Status(format!(
                    "Discovered {} files…",
                    d.total_files
                )));
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
                return ScanOutcome::Error {
                    kind: e.kind,
                    message: e.message,
                };
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

/// Display names of the required models that aren't installed (empty ⇒ all
/// present). Thin public wrapper over [`missing_models`] so the UI's standing
/// "models missing" banner reads the SAME gate the scan pre-flight enforces —
/// the install check stays defined in exactly one place.
pub fn missing_models_display() -> Vec<String> {
    missing_models().into_iter().map(|(_, name)| name).collect()
}

/// Required models without an install sentinel, as `(kind, display_name)`.
/// Resolved against the engine's OWN model root (see [`engine_model_root`]) — the
/// exact dir the spawned engine loads from — so the pre-flight gate, the standing
/// banner, and the engine never disagree.
fn missing_models() -> Vec<(&'static str, String)> {
    missing_models_in(engine_model_root().as_deref())
}

/// The model directory the ENGINE actually loads from for THIS process:
/// `FILEID_MODELS_DIR` when the launcher pinned it (always, via
/// `main::ensure_engine_models_dir`), else the engine's OWN writable
/// [`engine_models_dir`](fileid_engine::paths::engine_models_dir)
/// (`~/.local/share/FileID/Models` on a Mac).
///
/// Deliberately NOT `paths::models_dir()`: on macOS, with `FILEID_MODELS_DIR`
/// unset, that PREFERS the desktop app's read-only CoreML dir
/// (`~/Library/Application Support/FileID/Models`). Its CoreML sentinels would
/// mask wiped ONNX weights in the engine dir, so the pre-flight would pass and a
/// doomed scan would spawn only to fail (the bug). Resolving here keeps the gate
/// honest on the dir that matters.
fn engine_model_root() -> Option<PathBuf> {
    match std::env::var_os("FILEID_MODELS_DIR") {
        Some(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => fileid_engine::paths::engine_models_dir().ok(),
    }
}

/// Pure core of [`missing_models`] against an explicit model root — so it is
/// unit-testable without mutating the process environment or reading the user's
/// real model dir. A required model counts as installed iff an
/// `<id>-*.installed` sentinel exists under `<root>/.sentinels` (the engine's own
/// revision-keyed install marker), resolved against the engine root we pass.
fn missing_models_in(root: Option<&Path>) -> Vec<(&'static str, String)> {
    REQUIRED_MODELS
        .iter()
        .filter_map(|kind| match registry::lookup_full(kind) {
            LookupResult::Found(model) if sentinel_present(root, model.id) => None,
            LookupResult::Found(model) => Some((*kind, model.display_name.to_string())),
            LookupResult::Unknown => Some((*kind, (*kind).to_string())),
        })
        .collect()
}

/// True iff an install sentinel for `model_id` exists under `<root>/.sentinels`.
/// Matches on the `<id>-` prefix so it honors the engine's revision-keyed name
/// (`<id>-<token>.installed`) without re-deriving the token (which would risk
/// drift). The trailing `-` makes the prefix exact: `arcface-` never matches an
/// unrelated id. Side-effect-free (a pure read), unlike `registry::sentinel_path`.
fn sentinel_present(root: Option<&Path>, model_id: &str) -> bool {
    let Some(root) = root else { return false };
    let Ok(entries) = std::fs::read_dir(root.join(".sentinels")) else {
        return false;
    };
    let prefix = format!("{model_id}-");
    entries.flatten().any(|e| {
        let name = e.file_name();
        let name = name.to_string_lossy();
        name.starts_with(&prefix) && name.ends_with(".installed")
    })
}

/// One-line, user-facing "you need models" message shared by the scan pre-flight
/// and the engine's `models_not_installed` mapping, so the banner, the gate, and
/// a mid-scan engine error all say the same TUI-appropriate thing (press D, the
/// real download size, the model kinds) — never the desktop app's welcome wording.
fn missing_models_message() -> String {
    message_for_missing(&missing_models())
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

fn message_for_missing(missing: &[(&'static str, String)]) -> String {
    if missing.is_empty() {
        return "AI models not installed — press D on the Settings tab to download them, then \
                press s again."
            .to_string();
    }
    let kinds = missing
        .iter()
        .map(|(k, _)| *k)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "AI models not installed — press D to download (~{} for the {} needed: {kinds}), then \
         press s again.",
        approx_download_size(missing),
        missing.len(),
    )
}

/// Rounded human size of the missing models' weights, summed from the registry's
/// `approx_bytes` (nearest 10 MiB, e.g. `370 MB`). Derived, so it stays accurate
/// if the gated model set ever changes.
fn approx_download_size(missing: &[(&'static str, String)]) -> String {
    let bytes: u64 = missing
        .iter()
        .filter_map(|(kind, _)| match registry::lookup_full(kind) {
            LookupResult::Found(m) => Some(m.files.iter().map(|f| f.approx_bytes).sum::<u64>()),
            LookupResult::Unknown => None,
        })
        .sum();
    let mib = bytes as f64 / (1024.0 * 1024.0);
    if mib >= 1024.0 {
        format!("{:.1} GB", mib / 1024.0)
    } else {
        format!("{} MB", ((mib / 10.0).round() * 10.0) as u64)
    }
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
                    let cand = platforms
                        .join("windows/src/engine/target")
                        .join(profile)
                        .join(exe);
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
    std::env::split_paths(&path)
        .map(|dir| dir.join(exe))
        .find(|cand| cand.is_file())
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

    fn unique_tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fileid-tui-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// FIX 1: the gate must report missing when the ENGINE model root lacks the
    /// sentinels — even if some other dir (the macOS app dir) has them. Resolving
    /// against an explicit empty root proves the check reads the right place, and
    /// flips to "installed" the moment the revision-keyed sentinels appear there.
    #[test]
    fn reports_missing_when_engine_dir_lacks_sentinels() {
        let dir = unique_tmp_dir("models-empty");

        let missing = missing_models_in(Some(&dir));
        let kinds: Vec<&str> = missing.iter().map(|(k, _)| *k).collect();
        assert!(
            kinds.contains(&"mobileclip_s2"),
            "mobileclip_s2 must be missing, got {kinds:?}"
        );
        assert!(
            kinds.contains(&"arcface"),
            "arcface must be missing, got {kinds:?}"
        );
        assert_eq!(missing.len(), REQUIRED_MODELS.len());

        // Drop the engine's revision-keyed sentinels → the gate clears.
        let sentinels = dir.join(".sentinels");
        std::fs::create_dir_all(&sentinels).unwrap();
        std::fs::write(
            sentinels.join("mobileclip_s2-deadbeef00000000.installed"),
            b"x",
        )
        .unwrap();
        std::fs::write(sentinels.join("arcface-cafebabe00000000.installed"), b"x").unwrap();
        assert!(
            missing_models_in(Some(&dir)).is_empty(),
            "both keyed sentinels present → nothing missing"
        );

        // A missing root (couldn't resolve) is treated as "nothing installed".
        assert_eq!(missing_models_in(None).len(), REQUIRED_MODELS.len());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// FIX 2: the `models_not_installed` message is TUI-appropriate — press D, the
    /// real ~370 MB size for the two gated models, their kinds, and NONE of the
    /// desktop-app "Welcome screen" wording.
    #[test]
    fn models_not_installed_message_is_tui_appropriate() {
        let missing = vec![
            ("mobileclip_s2", "CLIP ViT-B/32 image encoder".to_string()),
            ("arcface", "Face detection + recognition".to_string()),
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
            msg.contains("370 MB"),
            "must state the real ~370 MB size: {msg}"
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
}
