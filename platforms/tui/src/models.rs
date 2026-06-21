//! AI-model download driver (the Settings `D` key, FEATURE 3).
//!
//! Runs the FileID CLI's `models download --all` on a worker thread and streams
//! its stdout + stderr into the TUI status line, so the user can fetch the AI
//! weights that full-ML scanning needs without leaving the terminal. The default
//! models come from `huggingface.co` — the only network egress the project ever
//! makes (see the no-telemetry principle).
//!
//! Non-blocking, mirroring [`crate::scan`]: the UI keeps drawing, `q` keeps
//! quitting, and the `TerminalGuard` still restores the terminal on exit. On
//! success it reloads the library and sends [`LoadMsg::Done`]; any failure — the
//! `fileid` CLI not found, a non-zero exit — becomes a single, clear
//! [`LoadMsg::Error`] on the status line, never a panic.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;

use anyhow::{Context as _, Result};

use crate::data::{self, LoadMsg};

/// Spawn `fileid models download --all` on a worker thread, streaming progress
/// to the status line. Non-blocking. On success reloads `db` and sends `Done`
/// (which clears `downloading`); any failure becomes a single `LoadMsg::Error`.
pub fn spawn_download(db: PathBuf, tx: Sender<LoadMsg>) {
    std::thread::spawn(move || match run_download(&tx) {
        Ok(()) => {
            let _ = tx.send(LoadMsg::Status("AI models installed — refreshing…".to_string()));
            // Reloading after a model install never adds files (it fetches
            // weights, not library rows), but it refreshes state and clears the
            // `downloading`/`loading` flags. A not-yet-created scratch DB loads
            // as an empty snapshot rather than erroring.
            match data::load(&db, &tx) {
                Ok(snap) => {
                    let _ = tx.send(LoadMsg::Status(
                        "AI models ready. Press s to scan a folder with full AI.".to_string(),
                    ));
                    let _ = tx.send(LoadMsg::Done(Box::new(snap)));
                }
                Err(e) => {
                    let _ = tx.send(LoadMsg::Error(format!("models installed, but reload failed: {e}")));
                }
            }
        }
        Err(e) => {
            let _ = tx.send(LoadMsg::Error(e.to_string()));
        }
    });
}

/// Locate the `fileid` CLI, spawn `models download --all`, and forward each line
/// of its output to the status line until it exits. Returns `Ok` on a zero exit,
/// else a descriptive error (CLI missing / non-zero exit) — never panics.
fn run_download(tx: &Sender<LoadMsg>) -> Result<()> {
    let Some(bin) = locate_fileid_cli() else {
        anyhow::bail!(
            "`{}` command not found — install the FileID CLI or put it on PATH (or set \
             FILEID_CLI_BIN), then press D again.",
            fileid_exe_name()
        );
    };

    let _ = tx.send(LoadMsg::Status(
        "Downloading AI models — fileid models download --all…".to_string(),
    ));

    let mut child = Command::new(&bin)
        .args(["models", "download", "--all"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning `{} models download --all`", bin.display()))?;

    // Drain BOTH pipes so a chatty downloader can't deadlock on a full pipe:
    // stderr on a side thread, stdout on this one. Progress can land on either.
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stderr_handle = stderr.map(|err| {
        let tx = tx.clone();
        std::thread::spawn(move || stream_lines(&tx, err))
    });
    if let Some(out) = stdout {
        stream_lines(tx, out);
    }
    if let Some(h) = stderr_handle {
        let _ = h.join();
    }

    let status = child.wait().context("waiting for the model download to finish")?;
    if status.success() {
        Ok(())
    } else {
        let code = status
            .code()
            .map(|c| format!("exit {c}"))
            .unwrap_or_else(|| "terminated by signal".to_string());
        anyhow::bail!(
            "model download failed ({code}) — check your network and free disk space, then retry."
        );
    }
}

/// Forward each non-empty, trimmed line of a child stream to the status line so
/// the user sees live download progress.
fn stream_lines<R: std::io::Read>(tx: &Sender<LoadMsg>, stream: R) {
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            let _ = tx.send(LoadMsg::Status(trimmed.to_string()));
        }
    }
}

fn fileid_exe_name() -> &'static str {
    if cfg!(windows) {
        "fileid.exe"
    } else {
        "fileid"
    }
}

/// Locate the `fileid` CLI: explicit `FILEID_CLI_BIN` → beside this exe → the
/// dev-layout CLI target dir → PATH. Mirrors [`crate::scan`]'s engine resolver.
fn locate_fileid_cli() -> Option<PathBuf> {
    let exe = fileid_exe_name();

    if let Some(p) = std::env::var_os("FILEID_CLI_BIN") {
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
            // .../platforms/cli/target/<profile>/fileid.
            if let Some(platforms) = dir.ancestors().find(|a| a.ends_with("platforms")) {
                for profile in ["release", "debug"] {
                    let cand = platforms.join("cli/target").join(profile).join(exe);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fileid_exe_name_matches_platform() {
        if cfg!(windows) {
            assert_eq!(fileid_exe_name(), "fileid.exe");
        } else {
            assert_eq!(fileid_exe_name(), "fileid");
        }
    }

    #[test]
    fn locate_fileid_cli_is_panic_free() {
        // Whatever the environment, resolution returns a well-typed Option and
        // never panics (the `fileid`-absent path must not crash the TUI).
        let _ = locate_fileid_cli();
    }
}
