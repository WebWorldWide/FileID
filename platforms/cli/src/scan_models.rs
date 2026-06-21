//! `fileid scan --models` — FULL-pipeline scan via the engine.
//!
//! The model-free `scan` (scan.rs) only indexes filenames + plain text. This
//! path drives the engine's real ML pipeline — image tags (RAM++), CLIP
//! embeddings, face detect/embed/cluster, perceptual + content hashes, binary-
//! document text — exactly as the desktop apps do.
//!
//! The engine's `startScan` hard-requires the AI models and owns its own async
//! runtime + ORT sessions, so we can't run it from a simple library call. We
//! reuse the engine the same way the desktop clients do: spawn the
//! `FileIDEngine` binary and speak newline-delimited JSON over stdio, reusing
//! the engine's own `ipc::IpcCommand` / `IpcEvent` types (no schema drift).
//!
//! Two pre-flights before we ever spawn:
//!   1. Models installed? We mirror the engine's exact `startScan` gate
//!      (`mobileclip_s2` + `arcface` sentinels) and, if anything is missing,
//!      print a clear, actionable message and stop. On macOS that message leads
//!      with the desktop app: the Rust engine can't reuse the macOS app's Swift
//!      models, so "install these models" would be a dead end — full-ML scanning
//!      there is the app's job, and the CLI/TUI do model-free FTS + browsing.
//!   2. Engine binary located? If not, we say how to point at it.
//!
//! The full pipeline writes the *engine's* library (XDG / `%LOCALAPPDATA%`),
//! which the engine binary resolves itself — so a pinned `--db` that differs is
//! reported as not-applicable here (unlike the read/model-free paths).

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use anyhow::{Context as _, Result};
use fileid_engine::ipc::{CommandPayload, EventPayload, IpcCommand, IpcEvent, StartScanPayload};
use fileid_engine::models::registry::{self, LookupResult};

use crate::context::{print_json, Ctx};

/// Models the engine's `startScan` pre-flight actually requires (mirror of
/// `commands/scan.rs`). `clip_text` is query-time-only and intentionally not
/// gated here; RAM++ tags degrade to CLIP scene tags when absent.
const REQUIRED_MODELS: [&str; 2] = ["mobileclip_s2", "arcface"];

pub fn run(ctx: &Ctx, root: &Path, rescan: bool) -> Result<()> {
    let root_abs = std::fs::canonicalize(root)
        .with_context(|| format!("resolving scan root {}", root.display()))?;
    if !root_abs.is_dir() {
        anyhow::bail!("scan root is not a directory: {}", root_abs.display());
    }

    // Pin the engine's OWN models dir (and inherit it into the spawned engine
    // below) so the model gate + the scan both look where `fileid models
    // download` installs — including on macOS, where the default would be the
    // desktop app's read-only CoreML dir. No-op when the user pinned it.
    crate::ensure_engine_models_dir();

    let models_dir = fileid_engine::paths::models_dir().ok();
    let engine_db = fileid_engine::paths::db_path().ok();

    // ── Pre-flight 1: models installed? ──────────────────────────────────
    // Runs before the --db caveat: on macOS this reports that full-ML scanning
    // is the desktop app's job (the Rust engine can't reuse the app's Swift
    // models), so that recommendation shouldn't trail a now-moot --db note.
    let missing = missing_models();
    if !missing.is_empty() {
        report_missing_models(ctx, &missing, models_dir.as_deref());
        return Ok(());
    }

    // Pinned --db doesn't apply to the full pipeline: the engine binary
    // resolves its own library location. Surface that rather than silently
    // writing somewhere the user didn't expect.
    if ctx.db_explicit && engine_db.as_deref() != Some(ctx.db.as_path()) {
        let where_ = engine_db
            .as_ref()
            .map_or_else(|| "<engine default>".to_string(), |p| p.display().to_string());
        ctx.progress(&format!(
            "  {}",
            ctx.dim(&format!(
                "note: --models drives the engine, which writes its own library at {where_}; \
                 your --db ({}) applies to read/model-free commands only. \
                 Set XDG_DATA_HOME / %LOCALAPPDATA% to relocate the engine library.",
                ctx.db.display()
            ))
        ));
    }

    // ── Pre-flight 2: engine binary located? ─────────────────────────────
    let Some(engine_bin) = locate_engine_binary() else {
        report_no_engine(ctx);
        return Ok(());
    };

    drive_scan(ctx, &engine_bin, &root_abs, rescan)
}

/// Required models without an install sentinel, as `(kind, display_name)`.
/// Resolution goes through the engine's own registry so the file layout and
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

fn report_missing_models(ctx: &Ctx, missing: &[(&'static str, String)], models_dir: Option<&Path>) {
    // On macOS the Rust engine can't reuse the desktop app's Swift models, so
    // "install these models" is a dead end — full-ML scanning there is the app's
    // job. Lead with that; elsewhere, installing the engine's models is correct.
    let on_macos = cfg!(target_os = "macos");

    if ctx.json {
        let (message, hint) = if on_macos {
            (
                "the full ML pipeline requires installed AI models",
                "install the engine's own models with `fileid models download --all` (downloads from huggingface.co), then re-run; or scan with full ML in the FileID desktop app, which owns the separate macOS CoreML models. `fileid models list` shows the set",
            )
        } else {
            (
                "the full ML pipeline requires installed AI models",
                "install them with `fileid models download --all` (downloads from huggingface.co); `fileid models list` shows the set + licenses. The desktop app's Welcome screen can also install them. See shared/docs/MODELS.md",
            )
        };
        print_json(&serde_json::json!({
            "command": "scan",
            "mode": "models",
            "error": "models_not_installed",
            "message": message,
            "missing": missing.iter().map(|(k, n)| serde_json::json!({"kind": k, "name": n}))
                .collect::<Vec<_>>(),
            "modelsDir": models_dir.map(|p| p.display().to_string()),
            "hint": hint,
        }));
        return;
    }

    println!("{}", ctx.bold("Full-pipeline scan unavailable — AI models not installed."));
    println!("  Missing:");
    for (kind, name) in missing {
        println!("    {} {}", name, ctx.dim(&format!("({kind})")));
    }
    if let Some(dir) = models_dir {
        println!("  Expected under: {}", dir.display());
    }
    println!("  {}", ctx.bold("To install:"));
    println!(
        "    {}   {}",
        ctx.bold("fileid models download --all"),
        ctx.dim("(or name specific models; downloads once from huggingface.co)")
    );
    println!(
        "    {}",
        ctx.dim("Preview first: fileid models download --all --dry-run   ·   fileid models list")
    );
    if on_macos {
        println!(
            "  {}",
            ctx.dim("These are the engine's own models. The FileID desktop app installs + uses a separate macOS CoreML set.")
        );
    }
    println!("    Registry + licenses: shared/docs/MODELS.md.");
    println!(
        "  {}",
        ctx.dim("The default `fileid scan` (model-free FTS) needs no models.")
    );
}

fn report_no_engine(ctx: &Ctx) {
    let exe = engine_exe_name();
    if ctx.json {
        print_json(&serde_json::json!({
            "command": "scan",
            "mode": "models",
            "error": "engine_not_found",
            "message": format!("could not locate the {exe} binary"),
            "hint": "build it (cargo build --release in platforms/windows/src/engine) and set FILEID_ENGINE_BIN, or put it on PATH / next to fileid",
        }));
        return;
    }
    println!("{}", ctx.bold("Full-pipeline scan unavailable — engine binary not found."));
    println!("  Models are installed, but the {exe} binary could not be located.");
    println!("  {}", ctx.bold("Provide it via any of:"));
    println!("    • FILEID_ENGINE_BIN=/path/to/{exe}");
    println!("    • place {exe} next to the `fileid` executable");
    println!("    • put {exe} on your PATH");
    println!(
        "  {}",
        ctx.dim("Build it with: cargo build --release  (in platforms/windows/src/engine)")
    );
}

/// Spawn the engine, send `startScan`, stream progress to stderr, and report
/// the outcome. Closes the engine's stdin on completion so its parent-EOF
/// watchdog shuts it down cleanly.
fn drive_scan(ctx: &Ctx, engine_bin: &Path, root: &Path, rescan: bool) -> Result<()> {
    ctx.progress(&format!("  {}", ctx.dim(&format!("starting engine: {}", engine_bin.display()))));

    let mut child = Command::new(engine_bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("spawning engine binary {}", engine_bin.display()))?;

    let mut stdin = child.stdin.take().context("engine stdin pipe")?;
    let stdout = child.stdout.take().context("engine stdout pipe")?;

    let cmd = IpcCommand {
        id: "fileid-cli-scan".to_string(),
        payload: CommandPayload::StartScan(StartScanPayload {
            root_path: root.to_string_lossy().into_owned(),
            root_display: None,
            rescan,
        }),
    };
    let line = serde_json::to_string(&cmd).context("serialize startScan command")?;
    stdin
        .write_all(line.as_bytes())
        .and_then(|()| stdin.write_all(b"\n"))
        .and_then(|()| stdin.flush())
        .context("sending startScan to engine")?;

    let outcome = stream_events(ctx, stdout);

    // EOF on stdin → the engine's watchdog exits; then reap it.
    drop(stdin);
    let _ = wait_briefly(&mut child);

    match outcome {
        ScanOutcome::Complete { total, processed, failed, seconds } => {
            if ctx.json {
                print_json(&serde_json::json!({
                    "command": "scan",
                    "mode": "models",
                    "root": root.to_string_lossy(),
                    "totalFiles": total,
                    "processedFiles": processed,
                    "failedFiles": failed,
                    "durationSeconds": seconds,
                }));
            } else {
                println!("{}", ctx.bold("Full-pipeline scan complete."));
                println!("  Root:       {}", root.display());
                println!("  Total:      {total}");
                println!("  Processed:  {processed}");
                println!("  Failed:     {failed}");
                println!("  Duration:   {seconds:.2}s");
            }
            Ok(())
        }
        ScanOutcome::Error { kind, message } => {
            if ctx.json {
                print_json(&serde_json::json!({
                    "command": "scan",
                    "mode": "models",
                    "error": kind,
                    "message": message,
                }));
                Ok(())
            } else {
                anyhow::bail!("engine scan failed [{kind}]: {message}")
            }
        }
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

fn stream_events<R: std::io::Read>(ctx: &Ctx, stdout: R) -> ScanOutcome {
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
                if p.total > 0 {
                    let failed = if p.failed > 0 {
                        format!(", {} failed", p.failed)
                    } else {
                        String::new()
                    };
                    ctx.progress(&format!(
                        "  {} {}/{} files ({:.0} files/s){failed}",
                        ctx.dim(&format!("{:?}", p.phase)),
                        p.processed,
                        p.total,
                        p.files_per_second,
                    ));
                }
            }
            EventPayload::PhaseChanged(w) => {
                ctx.progress(&format!("  {}", ctx.dim(&format!("phase: {:?}", w.inner))));
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
                // A transient phase=Failed often precedes the real error; the
                // error event carries the actionable kind/message.
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

fn engine_exe_name() -> &'static str {
    if cfg!(windows) {
        "FileIDEngine.exe"
    } else {
        "FileIDEngine"
    }
}

/// Locate the `FileIDEngine` binary: explicit override → beside `fileid` →
/// the dev-layout engine target dir → PATH.
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
            // Dev layout: .../platforms/cli/target/<profile>/fileid →
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
