//! `fileid scan --models`: the full ML pipeline, driven by spawning the engine binary over newline-delimited JSON IPC (the engine owns its async runtime lifecycle). Pre-flights check the models and engine binary before spawning. Post-scan hints add `--db` when reads would open a different library (macOS desktop app).

use std::io::{IsTerminal, Write};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, Result};
use fileid_engine::client::{self, ScanOutcome};
use fileid_engine::ipc::EventPayload;
use fileid_engine::models::required::{MissingModel, missing_required};

use crate::context::{Ctx, display_path, print_json};

pub fn run(ctx: &Ctx, root: &Path, rescan: bool) -> Result<()> {
    let root_abs = std::fs::canonicalize(root)
        .with_context(|| format!("resolving scan root {}", root.display()))?;
    if !root_abs.is_dir() {
        anyhow::bail!("scan root is not a directory: {}", root_abs.display());
    }

    // Pin the engine's own models dir so the gate and the scan use what `fileid models download` installs to (macOS default is the read-only CoreML dir).
    crate::ensure_engine_models_dir();

    let models_dir = fileid_engine::paths::models_dir().ok();
    let engine_db = if ctx.db_explicit {
        Some(ctx.db.clone())
    } else {
        fileid_engine::paths::db_path().ok()
    };

    // Runs before the --db caveat: on macOS full-ML scanning is the desktop app's job, so that advice must not trail a moot --db note.
    let missing = missing_required();
    if !missing.is_empty() {
        report_missing_models(ctx, &missing, models_dir.as_deref());
        anyhow::bail!("full-pipeline scan did not run: AI models not installed");
    }

    // Forward an explicit DB to the child. Otherwise the engine writes its XDG library, and on macOS reads may prefer the Swift app's library; surface that mismatch.
    if engine_writes_unseen_library(engine_db.as_deref(), ctx.db.as_path()) {
        let where_ = engine_db.as_ref().map_or_else(
            || "<engine default>".to_string(),
            |p| p.display().to_string(),
        );
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

    let Some(engine_bin) = client::locate_engine() else {
        report_no_engine(ctx);
        anyhow::bail!("full-pipeline scan did not run: engine binary not found");
    };

    drive_scan(ctx, &engine_bin, &root_abs, rescan, engine_db.as_deref())
}

/// True when the engine's target differs from the library reads open (macOS desktop app vs XDG default). An unresolved engine path counts as a divergence.
fn engine_writes_unseen_library(engine_db: Option<&Path>, reader_db: &Path) -> bool {
    engine_db != Some(reader_db)
}

/// The ` --db <engine library>` suffix for the post-scan hints. Empty when reads already resolve to the engine's library, keeping that line byte-identical.
fn explore_db_arg(engine_db: Option<&Path>, reader_db: &Path) -> String {
    match engine_db {
        Some(p) if engine_writes_unseen_library(Some(p), reader_db) => {
            format!(" --db {}", p.display())
        }
        _ => String::new(),
    }
}

fn report_missing_models(ctx: &Ctx, missing: &[MissingModel], models_dir: Option<&Path>) {
    // macOS uses separate Rust-engine weights from the desktop app's CoreML set.
    let on_macos = cfg!(target_os = "macos");

    let install_command = format!(
        "fileid models download {}",
        missing
            .iter()
            .map(|missing| missing.kind)
            .collect::<Vec<_>>()
            .join(" ")
    );
    if ctx.json {
        let hint = if on_macos {
            format!(
                "install the engine's own models with `{install_command}` (downloads from huggingface.co), then re-run; or scan with full ML in the FileID desktop app, which owns separate CoreML models"
            )
        } else {
            format!(
                "install the required models with `{install_command}` (downloads from huggingface.co); `fileid models list` shows optional models and licenses"
            )
        };
        print_json(&serde_json::json!({
            "command": "scan",
            "mode": "models",
            "error": "models_not_installed",
            "message": "the full ML pipeline requires installed AI models",
            "missing": missing.iter().map(|m| serde_json::json!({"kind": m.kind, "name": m.display_name}))
                .collect::<Vec<_>>(),
            "modelsDir": models_dir.map(|p| p.display().to_string()),
            "hint": hint,
        }));
        return;
    }

    println!(
        "{}",
        ctx.bold("Full-pipeline scan unavailable — AI models not installed.")
    );
    println!("  Missing:");
    for missing in missing {
        println!(
            "    {} {}",
            missing.display_name,
            ctx.dim(&format!("({})", missing.kind))
        );
    }
    if let Some(dir) = models_dir {
        println!(
            "  Expected under: {}",
            display_path(dir.to_string_lossy().as_ref())
        );
    }
    println!("  {}", ctx.bold("To install:"));
    println!("    {}", ctx.bold(&install_command));
    println!(
        "    {}",
        ctx.dim(&format!(
            "Preview first: {install_command} --dry-run   ·   fileid models list"
        ))
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
    let exe = client::engine_exe_name();
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
    println!(
        "{}",
        ctx.bold("Full-pipeline scan unavailable — engine binary not found.")
    );
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

/// Spawns the engine, sends `startScan`, streams progress, and reports the outcome. Closing stdin lets the engine's parent-EOF watchdog shut it down.
fn drive_scan(
    ctx: &Ctx,
    engine_bin: &Path,
    root: &Path,
    rescan: bool,
    engine_db: Option<&Path>,
) -> Result<()> {
    ctx.progress(&format!(
        "  {}",
        ctx.dim(&format!("starting engine: {}", engine_bin.display()))
    ));

    let envs: &[(&str, &std::ffi::OsStr)] = if ctx.db_explicit {
        &[("FILEID_DB", ctx.db.as_os_str())]
    } else {
        &[]
    };
    let mut child = client::spawn(engine_bin, envs, Stdio::inherit())
        .with_context(|| format!("spawning engine binary {}", engine_bin.display()))?;

    let mut stdin = child.stdin.take().context("engine stdin pipe")?;
    let stdout = child.stdout.take().context("engine stdout pipe")?;

    client::send_command(
        &mut stdin,
        &client::start_scan_command("fileid-cli-scan", root, rescan),
    )
    .context("sending startScan to engine")?;

    let live = !ctx.quiet && !ctx.json && std::io::stderr().is_terminal();
    let outcome = client::drive_scan(stdout, &mut stdin, "fileid-cli-scan", |payload| {
        report_progress(ctx, live, payload)
    });
    if live {
        clear_progress_line();
    }

    // EOF on stdin → the engine's watchdog exits; then reap it.
    drop(stdin);
    let _ = client::wait_briefly(&mut child, Duration::from_secs(10));

    match outcome {
        ScanOutcome::Complete { scan, clustering } => {
            let (total, processed, failed, seconds) = (
                scan.total_files,
                scan.processed_files,
                scan.failed_files,
                scan.total_seconds,
            );
            let (people, faces, unmatched_faces, clustering_seconds) = (
                clustering.person_count,
                clustering.face_count,
                clustering.unmatched_faces,
                clustering.duration_seconds,
            );
            if ctx.json {
                print_json(&serde_json::json!({
                    "command": "scan",
                    "mode": "models",
                    "root": root.to_string_lossy(),
                    "totalFiles": total,
                    "processedFiles": processed,
                    "failedFiles": failed,
                    "durationSeconds": seconds,
                    "faceClustering": {
                        "people": people,
                        "faces": faces,
                        "unmatchedFaces": unmatched_faces,
                        "durationSeconds": clustering_seconds,
                    },
                }));
            } else {
                let rate = if seconds > 0.0 {
                    format!("  ·  {:.0} files/s", processed as f64 / seconds)
                } else {
                    String::new()
                };
                println!("{}", ctx.bold("AI scan complete."));
                println!(
                    "  Root:       {}",
                    display_path(root.to_string_lossy().as_ref())
                );
                println!(
                    "  Processed:  {processed} / {total}{}",
                    if failed > 0 {
                        ctx.dim(&format!("  ({failed} failed)"))
                    } else {
                        String::new()
                    }
                );
                if let Some((tags, faces, people)) = engine_db.and_then(count_results) {
                    println!(
                        "  Results:    {tags} tags · {faces} files with faces · {people} people"
                    );
                }
                println!(
                    "  Clustering: {people} people · {faces} faces · {unmatched_faces} unmatched ({clustering_seconds:.2}s)"
                );
                println!("  Duration:   {seconds:.2}s{rate}");
                // Qualify the explore hints with `--db` when reads default elsewhere, so they open the library this scan just populated.
                let db_hint = explore_db_arg(engine_db, ctx.db.as_path());
                println!(
                    "  Explore:    {}  ·  {}  ·  {}",
                    ctx.bold(&format!("fileid search \"...\"{db_hint}")),
                    ctx.bold(&format!("fileid people{db_hint}")),
                    ctx.bold(&format!("fileid dedupe --similar{db_hint}")),
                );
            }
            if failed > 0 {
                return Err(crate::scan::PartialScan { failed }.into());
            }
            Ok(())
        }
        ScanOutcome::Error { kind, message } => {
            // The engine did not scan; always exit non-zero so `scan --models && next` cannot proceed.
            if ctx.json {
                print_json(&serde_json::json!({
                    "command": "scan",
                    "mode": "models",
                    "error": kind,
                    "message": message,
                }));
            }
            if kind.starts_with("face_clustering") {
                anyhow::bail!("scan completed, but face grouping failed: {message}")
            }
            anyhow::bail!("engine scan failed [{kind}]: {message}")
        }
        ScanOutcome::Aborted => {
            anyhow::bail!("engine exited before the scan completed (no scanComplete event)")
        }
    }
}

fn clear_progress_line() {
    let _ = write!(std::io::stderr(), "\r\x1b[K");
    let _ = std::io::stderr().flush();
}

/// Live carriage-return progress at a TTY, coarse lines otherwise; phase
/// milestones and the face-grouping step clear the bar first.
fn report_progress(ctx: &Ctx, live: bool, payload: &EventPayload) {
    match payload {
        EventPayload::Progress(w) => {
            let p = &w.inner;
            if p.total == 0 {
                return;
            }
            let pct = (p.processed as f64 / p.total as f64 * 100.0).min(100.0);
            let phase = ctx.dim(&format!("{:?}", p.phase));
            if live {
                let failed = if p.failed > 0 {
                    format!(" · {} failed", p.failed)
                } else {
                    String::new()
                };
                let mut err = std::io::stderr();
                let _ = write!(
                    err,
                    "\r  {phase} {}/{} ({pct:.0}%) · {:.0} files/s{failed}\x1b[K",
                    p.processed, p.total, p.files_per_second,
                );
                let _ = err.flush();
            } else {
                let failed = if p.failed > 0 {
                    format!(", {} failed", p.failed)
                } else {
                    String::new()
                };
                ctx.progress(&format!(
                    "  {phase} {}/{} files ({:.0} files/s){failed}",
                    p.processed, p.total, p.files_per_second,
                ));
            }
        }
        EventPayload::PhaseChanged(w) => {
            if live {
                clear_progress_line();
            }
            ctx.progress(&format!("  {}", ctx.dim(&format!("phase: {:?}", w.inner))));
        }
        EventPayload::ScanComplete(_) => {
            if live {
                clear_progress_line();
            }
            ctx.progress(&format!("  {}", ctx.dim("grouping detected faces…")));
        }
        _ => {}
    }
}

/// Best-effort `(tags, files-with-faces, people)` for the completion summary. Read-only and never fatal; the engine has already exited.
fn count_results(db: &Path) -> Option<(i64, i64, i64)> {
    let conn = fileid_engine::db::open_read(db).ok()?;
    let tags: i64 = conn
        .query_row("SELECT COUNT(*) FROM tags", [], |r| r.get(0))
        .ok()?;
    let faces: i64 = conn
        .query_row("SELECT COUNT(*) FROM files WHERE has_faces = 1", [], |r| {
            r.get(0)
        })
        .unwrap_or(0);
    let people: i64 = conn
        .query_row("SELECT COUNT(*) FROM persons", [], |r| r.get(0))
        .unwrap_or(0);
    Some((tags, faces, people))
}

#[cfg(test)]
mod tests {
    use super::*;

    // macOS default: the engine writes its XDG library while reads default to the desktop app's, so the note must fire and the explore hints must redirect reads.
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

    // Win/Linux default (and macOS without a desktop-app library): reads resolve to the engine's library, so the note and suffix must stay absent.
    #[test]
    fn no_divergence_when_reader_is_engine_library() {
        let p = Path::new("/state/FileID/fileid.sqlite");
        assert!(!engine_writes_unseen_library(Some(p), p));
        assert_eq!(explore_db_arg(Some(p), p), "");
    }

    // An explicit `--db` is inherited via FILEID_DB, so engine and reader paths agree.
    #[test]
    fn explicit_db_override_eliminates_divergence() {
        let pinned = Path::new("/tmp/lib/fileid.sqlite");
        assert!(!engine_writes_unseen_library(Some(pinned), pinned));
        assert_eq!(explore_db_arg(Some(pinned), pinned), "");
    }

    // Engine path unresolvable: reads cannot open it either, so warn but emit no `--db` hint.
    #[test]
    fn unresolvable_engine_db_warns_without_hint() {
        let reader = Path::new("/state/FileID/fileid.sqlite");
        assert!(engine_writes_unseen_library(None, reader));
        assert_eq!(explore_db_arg(None, reader), "");
    }
}
