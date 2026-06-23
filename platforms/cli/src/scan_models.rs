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
//! which the engine binary resolves itself — so when the library the CLI's
//! reads resolve to differs (a pinned `--db`, or on macOS the desktop app's
//! library, preferred by default when present), that mismatch is surfaced here
//! rather than silently honored (unlike the read/model-free paths).

use std::io::{BufRead, BufReader, IsTerminal, Write};
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
        anyhow::bail!("full-pipeline scan did not run: AI models not installed");
    }

    // ── Pre-flight 1.5 (macOS): ONNX Runtime installed? ──────────────────
    // The engine's `load-dynamic` build `dlopen`s `libonnxruntime.dylib`, but
    // `ort`'s `download-binaries` ships only a STATIC lib for arm64 — so the
    // runtime is a separate, one-time install from the model download. Catch it
    // here with clear guidance instead of spawning an engine that will abort at
    // model-load. No-op off macOS (`is_available()` is always true there).
    #[cfg(target_os = "macos")]
    if !fileid_engine::ort_runtime::is_available() {
        report_runtime_missing(ctx);
        anyhow::bail!("full-pipeline scan did not run: ONNX Runtime not installed");
    }

    // `scan --models` ALWAYS writes the engine's OWN library (XDG /
    // %LOCALAPPDATA%), resolved by the engine binary itself. Whenever that
    // differs from the library the CLI's read/model-free commands open
    // (`ctx.db`), the AI results land where no `fileid` read command looks. The
    // divergence is NOT only the pinned-`--db` case: on macOS `ctx.db` defaults
    // to the desktop app's library when present (context.rs resolve step 4),
    // which is never where the Rust engine writes — so the note must fire for
    // ANY mismatch, not just an explicit pin. The old `db_explicit` gate let the
    // silent macOS default through, so info/search/people/dedupe/restructure all
    // reported no AI data immediately after a successful scan.
    if engine_writes_unseen_library(engine_db.as_deref(), ctx.db.as_path()) {
        let where_ = engine_db
            .as_ref()
            .map_or_else(|| "<engine default>".to_string(), |p| p.display().to_string());
        let reader = if ctx.db_explicit {
            "your pinned --db"
        } else {
            "the library your read commands use"
        };
        ctx.progress(&format!(
            "  {}",
            ctx.dim(&format!(
                "note: --models drives the engine, which writes its own library at {where_}; \
                 {reader} ({}) is what info/search/people/dedupe/restructure read. \
                 See the AI results there with `--db {where_}`, or relocate the engine \
                 library via XDG_DATA_HOME / %LOCALAPPDATA%.",
                ctx.db.display()
            ))
        ));
    }

    // ── Pre-flight 2: engine binary located? ─────────────────────────────
    let Some(engine_bin) = locate_engine_binary() else {
        report_no_engine(ctx);
        anyhow::bail!("full-pipeline scan did not run: engine binary not found");
    };

    drive_scan(ctx, &engine_bin, &root_abs, rescan)
}

/// Will `scan --models` write a library the CLI's read/model-free commands
/// won't open? The engine ALWAYS writes its OWN library (`engine_db`,
/// XDG/`%LOCALAPPDATA%`-located); reads use `reader_db` (`ctx.db`). They diverge
/// for a pinned `--db`/`$FILEID_DB`/`$CFFIXED_USER_HOME` AND — the silent macOS
/// default — when reads fall back to the desktop app's library (context.rs
/// resolve step 4) while the engine writes its XDG library. A `None` `engine_db`
/// (the engine can't resolve its own path) counts as a divergence we can't name.
fn engine_writes_unseen_library(engine_db: Option<&Path>, reader_db: &Path) -> bool {
    engine_db != Some(reader_db)
}

/// The ` --db <engine library>` suffix for the post-scan "explore" commands, so
/// they open the library the engine just wrote when the CLI's reads would
/// otherwise default elsewhere (the macOS desktop-app library). Empty — keeping
/// that line byte-identical to before — when reads already resolve to the
/// engine's library (the Win/Linux default, or macOS with no desktop app).
fn explore_db_arg(engine_db: Option<&Path>, reader_db: &Path) -> String {
    match engine_db {
        Some(p) if engine_writes_unseen_library(Some(p), reader_db) => {
            format!(" --db {}", p.display())
        }
        _ => String::new(),
    }
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

/// Clear, actionable "ONNX Runtime not installed" message (macOS). Distinct from
/// the model download — points at the one-time `fileid runtime install`. Reused
/// by the pre-flight gate and the engine's `runtime_not_installed` mapping.
fn report_runtime_missing(ctx: &Ctx) {
    let cmd = fileid_engine::ort_runtime::INSTALL_COMMAND;
    if ctx.json {
        print_json(&serde_json::json!({
            "command": "scan",
            "mode": "models",
            "error": "runtime_not_installed",
            "message": "the full ML pipeline needs ONNX Runtime, which isn't installed",
            "hint": format!(
                "install it once with `{cmd}` (or `brew install onnxruntime`), then re-run. \
                 This is separate from `fileid models download`. See shared/docs/RUNTIME.md"
            ),
        }));
        return;
    }
    println!("{}", ctx.bold("Full-pipeline scan unavailable — ONNX Runtime not installed."));
    println!(
        "  {}",
        ctx.dim("The AI models are installed, but the runtime that loads them isn't.")
    );
    println!("  {}", ctx.bold("Install it once (any one):"));
    println!("    {}", ctx.bold(cmd));
    println!("    brew install onnxruntime");
    println!("    shared/scripts/install_onnxruntime_macos.sh");
    println!(
        "  {}",
        ctx.dim("Then re-run this scan. (Separate from `fileid models download`.) See shared/docs/RUNTIME.md.")
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
                let rate = if seconds > 0.0 {
                    format!("  ·  {:.0} files/s", processed as f64 / seconds)
                } else {
                    String::new()
                };
                println!("{}", ctx.bold("AI scan complete."));
                println!("  Root:       {}", root.display());
                println!(
                    "  Processed:  {processed} / {total}{}",
                    if failed > 0 {
                        ctx.dim(&format!("  ({failed} failed)"))
                    } else {
                        String::new()
                    }
                );
                let engine_db = fileid_engine::paths::db_path().ok();
                if let Some((tags, faces, people)) =
                    engine_db.as_deref().and_then(count_results)
                {
                    println!(
                        "  Results:    {tags} tags · {faces} files with faces · {people} people"
                    );
                }
                println!("  Duration:   {seconds:.2}s{rate}");
                // The engine wrote its OWN library; when the CLI's reads default
                // elsewhere (the macOS desktop-app library), qualify the explore
                // hints with that `--db` so the suggested commands open the
                // library this scan just populated, not an empty one. Byte-
                // identical (no suffix) when reads already resolve to it.
                let db_hint = explore_db_arg(engine_db.as_deref(), ctx.db.as_path());
                println!(
                    "  Explore:    {}  ·  {}  ·  {}",
                    ctx.bold(&format!("fileid search \"...\"{db_hint}")),
                    ctx.bold(&format!("fileid people{db_hint}")),
                    ctx.bold(&format!("fileid dedupe --similar{db_hint}")),
                );
            }
            Ok(())
        }
        ScanOutcome::Error { kind, message } => {
            // The engine reported an error → it did NOT scan. Surface the right
            // output (JSON payload / clean install guidance / raw error) but
            // ALWAYS exit non-zero so `scan --models && next` can't proceed.
            if ctx.json {
                print_json(&serde_json::json!({
                    "command": "scan",
                    "mode": "models",
                    "error": kind,
                    "message": message,
                }));
            } else if kind == "runtime_not_installed" {
                // The pre-flight normally catches this; if the engine still
                // reports it (e.g. the dylib vanished after pre-flight), surface
                // the same clean install guidance rather than a raw engine error.
                report_runtime_missing(ctx);
            }
            anyhow::bail!("engine scan failed [{kind}]: {message}")
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
    // Live carriage-return progress only at a TTY; clear it before any milestone
    // (phase change) or terminal outcome so nothing is glued to the bar.
    let live = !ctx.quiet && !ctx.json && std::io::stderr().is_terminal();
    let clear = || {
        if live {
            let _ = write!(std::io::stderr(), "\r\x1b[K");
            let _ = std::io::stderr().flush();
        }
    };
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
                    let pct = (p.processed as f64 / p.total as f64 * 100.0).min(100.0);
                    if live {
                        let failed = if p.failed > 0 {
                            format!(" · {} failed", p.failed)
                        } else {
                            String::new()
                        };
                        let mut err = std::io::stderr();
                        let _ = write!(
                            err,
                            "\r  {} {}/{} ({pct:.0}%) · {:.0} files/s{failed}\x1b[K",
                            ctx.dim(&format!("{:?}", p.phase)),
                            p.processed,
                            p.total,
                            p.files_per_second,
                        );
                        let _ = err.flush();
                    } else {
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
            }
            EventPayload::PhaseChanged(w) => {
                clear();
                ctx.progress(&format!("  {}", ctx.dim(&format!("phase: {:?}", w.inner))));
            }
            EventPayload::ScanComplete(w) => {
                clear();
                let c = w.inner;
                return ScanOutcome::Complete {
                    total: c.total_files,
                    processed: c.processed_files,
                    failed: c.failed_files,
                    seconds: c.total_seconds,
                };
            }
            EventPayload::Error(w) => {
                clear();
                let e = w.inner;
                // A transient phase=Failed often precedes the real error; the
                // error event carries the actionable kind/message.
                return ScanOutcome::Error { kind: e.kind, message: e.message };
            }
            _ => {}
        }
    }
    clear();
    ScanOutcome::Aborted
}

/// Best-effort `(tags, files-with-faces, people)` for the completion summary.
/// Read-only, never fatal — the engine has already exited and freed the DB by
/// the time we call this (the scan is done).
fn count_results(db: &Path) -> Option<(i64, i64, i64)> {
    let conn = fileid_engine::db::open_read(db).ok()?;
    let tags: i64 = conn.query_row("SELECT COUNT(*) FROM tags", [], |r| r.get(0)).ok()?;
    let faces: i64 = conn
        .query_row("SELECT COUNT(*) FROM files WHERE has_faces = 1", [], |r| r.get(0))
        .unwrap_or(0);
    let people: i64 =
        conn.query_row("SELECT COUNT(*) FROM persons", [], |r| r.get(0)).unwrap_or(0);
    Some((tags, faces, people))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    // Regression for the macOS default invocation: a successful `scan --models`
    // writes the engine's XDG library, but the CLI's reads default to the
    // desktop app's library — a divergence the old `db_explicit` gate let pass
    // silently, so dedupe/search/people reported no AI data. The note must fire,
    // and the explore hints must redirect reads at the engine's library. Pure
    // path comparison, so this exercises the decision identically on every OS.
    #[test]
    fn divergence_fires_for_macos_app_db_default() {
        let engine = Path::new("/home/u/.local/share/FileID/fileid.sqlite");
        let app = Path::new("/home/u/Library/Application Support/FileID/fileid.sqlite");
        assert!(
            engine_writes_unseen_library(Some(engine), app),
            "engine XDG vs macOS app library must be flagged divergent"
        );
        assert_eq!(
            explore_db_arg(Some(engine), app),
            format!(" --db {}", engine.display()),
            "explore hints must point reads at the engine's library"
        );
    }

    // Win/Linux default invocation (and macOS without a desktop-app library):
    // reads resolve to the very library the engine writes. The note MUST stay
    // silent and the explore line MUST be byte-identical (no `--db` suffix) — no
    // false positive across platforms.
    #[test]
    fn no_divergence_when_reader_is_engine_library() {
        let p = Path::new("/state/FileID/fileid.sqlite");
        assert!(!engine_writes_unseen_library(Some(p), p));
        assert_eq!(explore_db_arg(Some(p), p), "");
    }

    // An explicit `--db` elsewhere also diverges from the engine's own library —
    // preserving the original (pre-fix) behavior for the pinned case.
    #[test]
    fn divergence_fires_for_explicit_db_elsewhere() {
        let engine = Path::new("/state/FileID/fileid.sqlite");
        let pinned = Path::new("/tmp/lib/fileid.sqlite");
        assert!(engine_writes_unseen_library(Some(engine), pinned));
        assert_eq!(
            explore_db_arg(Some(engine), pinned),
            format!(" --db {}", engine.display())
        );
    }

    // The engine can't resolve its own path (`db_path()` -> Err): we can't name
    // where it writes, so reads certainly don't open it — warn (note fires), but
    // emit no `--db` hint we'd be unable to fill in.
    #[test]
    fn unresolvable_engine_db_warns_without_hint() {
        let reader = Path::new("/state/FileID/fileid.sqlite");
        assert!(engine_writes_unseen_library(None, reader));
        assert_eq!(explore_db_arg(None, reader), "");
    }
}
