//! AI-model download driver for the global `D` shortcut.
//!
//! Runs the FileID CLI's `models download --all --yes --porcelain-progress` on a
//! worker thread and parses its output into a live install gauge + status line,
//! so the user can fetch the AI weights that full-ML scanning needs without
//! leaving the terminal. The default models come from `huggingface.co` — the only
//! network egress the project ever makes (see the no-telemetry principle).
//!
//! ## Porcelain progress contract (shared with the CLI, byte-for-byte)
//! stdout lines of the form `PROGRESS\t{percent}\t{label}` (tab-separated;
//! `percent` = integer 0–100 overall; `label` = a short human string like
//! `arcface · 182/271 MB · 3.4 MB/s · model 2/9`) drive the gauge; the final
//! progress line is `PROGRESS\t100\tdone`. Every other non-empty line — milestones
//! (`✓ arcface installed`), the summary, errors, and all of stderr — is a status
//! message. See [`parse_porcelain_line`].
//!
//! Non-blocking, mirroring [`crate::scan`]: the UI keeps drawing, `q` keeps
//! quitting, and the `TerminalGuard` still restores the terminal on exit. On
//! success it reloads the library and sends [`LoadMsg::Done`]; any failure — the
//! `fileid` CLI not found, a non-zero exit — becomes a single, clear
//! [`LoadMsg::Error`] on the status line, never a panic.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender as Sender;
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result};

use crate::data::{self, LoadMsg};

/// A handle the UI thread holds to the running download child so it can `kill()`
/// it on quit. The worker thread parks the child here after spawning and reclaims
/// it (to reap) once the streams close; the UI thread only ever `kill()`s through
/// it, never `take()`s — so the two can't race over ownership.
#[derive(Clone)]
pub struct DownloadHandle {
    child: Arc<Mutex<Option<Child>>>,
    cancelled: Arc<AtomicBool>,
}

impl DownloadHandle {
    fn new() -> Self {
        Self {
            child: Arc::new(Mutex::new(None)),
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        if let Ok(mut slot) = self.child.lock() {
            if let Some(child) = slot.as_mut() {
                let _ = child.kill();
            }
        }
    }

    fn register(&self, mut child: Child) -> Result<()> {
        let mut slot = self
            .child
            .lock()
            .map_err(|_| anyhow::anyhow!("model download child lock poisoned"))?;
        if self.cancelled.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("model download was cancelled");
        }
        *slot = Some(child);
        Ok(())
    }
}

/// The CLI invocation, as a const so the SHARED CONTRACT (incl. the hidden
/// `--porcelain-progress` flag that emits the machine-readable `PROGRESS` lines
/// the gauge parses) is pinned in one place and guarded by a test. `--yes`
/// pre-confirms the non-interactive download; `--porcelain-progress` switches the
/// CLI to the structured output [`parse_porcelain_line`] understands.
///
/// Downloads ONLY the non-VLM weights the TUI actually uses (~1.6 GB) — faces
/// (`arcface` = YuNet + SFace), image search/tags (`mobileclip_s2`, `ram_plus`),
/// and text search (`clip_text`, `bge_text`) — NOT `--all`, which would also pull
/// the three Deep-Analyze VLMs (~24 GB) the TUI has no tab for.
const DOWNLOAD_ARGS: [&str; 9] = [
    "models",
    "download",
    "--yes",
    "--porcelain-progress",
    "arcface",
    "mobileclip_s2",
    "ram_plus",
    "clip_text",
    "bge_text",
];

/// Spawn `fileid models download` on a worker thread, parsing progress into the
/// install gauge + status line. Non-blocking. On success reloads `db` and sends
/// `Done` (which clears the gauge); any failure becomes a single `LoadMsg::Error`.
/// Returns a [`DownloadHandle`] the caller keeps so it can `kill()` the child on
/// quit (the CLI has no parent watchdog, so it would otherwise orphan).
pub fn spawn_download(
    db: PathBuf,
    query: String,
    tx: Sender<LoadMsg>,
    generation: u64,
) -> DownloadHandle {
    let handle = DownloadHandle::new();
    let worker_handle = handle.clone();
    std::thread::spawn(move || match run_download(&tx, &worker_handle) {
        Ok(()) => {
            let _ = tx.send(LoadMsg::Status(
                "AI models installed — refreshing…".to_string(),
            ));
            // Reloading after a model install never adds files (it fetches
            // weights, not library rows), but it refreshes state and clears the
            // `downloading`/`loading` flags. A not-yet-created scratch DB loads
            // as an empty snapshot rather than erroring.
            match data::load(&db, &query, &tx, generation) {
                Ok(snap) => {
                    let _ = tx.send(LoadMsg::Status(
                        "AI models ready. Press s to scan a folder with full AI.".to_string(),
                    ));
                    let _ = data::send_versioned(&tx, generation, LoadMsg::Done(Box::new(snap)));
                    data::run_deferred_dupes(&db, &tx, generation);
                }
                Err(e) => {
                    let _ = data::send_versioned(
                        &tx,
                        generation,
                        LoadMsg::Error(format!("models installed, but reload failed: {e}")),
                    );
                }
            }
        }
        Err(e) => {
            let _ = data::send_versioned(&tx, generation, LoadMsg::Error(e.to_string()));
        }
    });
    handle
}

/// Locate the `fileid` CLI, spawn the model download, and forward each line of
/// its output to the status line until it exits. Returns `Ok` on a zero exit,
/// else a descriptive error (CLI missing / non-zero exit) — never panics. The
/// spawned child is parked in `child_slot` so the UI can `kill()` it on quit.
fn run_download(tx: &Sender<LoadMsg>, handle: &DownloadHandle) -> Result<()> {
    if handle.cancelled.load(Ordering::Acquire) {
        anyhow::bail!("model download was cancelled");
    }
    let Some(bin) = locate_fileid_cli() else {
        anyhow::bail!(
            "`{}` command not found — install the FileID CLI or put it on PATH (or set \
             FILEID_CLI_BIN), then press D again.",
            fileid_exe_name()
        );
    };

    let _ = tx.send(LoadMsg::Status(
        "Downloading AI models — fileid models download…".to_string(),
    ));

    // `--yes` is REQUIRED here: the TUI drives this non-interactively with a null
    // stdin, so the CLI's large-download confirmation prompt would read EOF, treat
    // it as "no", and abort (exit 0, nothing downloaded) — which the TUI would
    // misreport as "models installed". Pre-confirming makes the download run.
    let mut child = Command::new(&bin)
        .args(DOWNLOAD_ARGS)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning `{} models download`", bin.display()))?;

    // Drain BOTH pipes so a chatty downloader can't deadlock on a full pipe:
    // stderr on a side thread, stdout on this one. Progress can land on either.
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // Park the child so the UI thread can `kill()` it on quit — unlike the scan
    // engine, the `fileid` CLI has no parent-PID/EOF watchdog, so without this it
    // would keep downloading (orphaned) after the TUI exits. The pipes are taken
    // first; once streaming hits EOF this thread reclaims the child and reaps it.
    handle.register(child)?;

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

    // Reclaim the child (the UI thread may have `kill()`-ed it, but never takes
    // it) and reap it so no zombie is left — bounded, never blocking the UI.
    let Some(mut child) = handle.child.lock().ok().and_then(|mut slot| slot.take()) else {
        anyhow::bail!("model download was cancelled");
    };
    let status = child
        .wait()
        .context("waiting for the model download to finish")?;
    if handle.cancelled.load(Ordering::Acquire) {
        anyhow::bail!("model download was cancelled");
    }
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

/// Parse each non-empty line of a child stream per the porcelain contract and
/// forward it: a `PROGRESS` line becomes a [`LoadMsg::DownloadProgress`] gauge
/// update, anything else a [`LoadMsg::Status`] line. Panic-free — a malformed
/// line degrades to a status message, never a crash.
fn stream_lines<R: std::io::Read>(tx: &Sender<LoadMsg>, stream: R) {
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let msg = match parse_porcelain_line(&line) {
            ParsedLine::Progress { percent, label } => LoadMsg::DownloadProgress { percent, label },
            ParsedLine::Status(s) if !s.is_empty() => LoadMsg::Status(s),
            ParsedLine::Status(_) => continue,
        };
        let _ = tx.send(msg);
    }
}

/// One parsed porcelain stdout line (the SHARED CONTRACT): a structured gauge
/// update, or a plain status/milestone line.
#[derive(Clone, PartialEq, Eq, Debug)]
enum ParsedLine {
    Progress { percent: u16, label: String },
    Status(String),
}

/// Parse ONE porcelain line. `PROGRESS\t{percent}\t{label}` (tab-separated,
/// integer percent 0–100; final line `PROGRESS\t100\tdone`) becomes a gauge
/// update with the percent clamped to 0–100; everything else — milestones, the
/// summary, errors, stderr — is a trimmed status message. A `PROGRESS`-prefixed
/// line whose percent doesn't parse degrades to a status message rather than
/// panicking, so a stray byte on the wire can never crash the TUI.
fn parse_porcelain_line(line: &str) -> ParsedLine {
    if let Some(rest) = line.strip_prefix("PROGRESS\t") {
        let mut parts = rest.splitn(2, '\t');
        let pct = parts.next().unwrap_or("").trim();
        let label = parts.next().unwrap_or("").trim();
        if let Ok(percent) = pct.parse::<u16>() {
            return ParsedLine::Progress {
                percent: percent.min(100),
                label: label.to_string(),
            };
        }
    }
    ParsedLine::Status(line.trim().to_string())
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
    std::env::split_paths(&path)
        .map(|dir| dir.join(exe))
        .find(|cand| cand.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_before_child_registration_kills_the_spawned_child() {
        let handle = DownloadHandle::new();
        handle.cancel();
        let child = Command::new(std::env::current_exe().unwrap())
            .arg("--help")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        assert!(handle.register(child).is_err());
        assert!(handle.child.lock().unwrap().is_none());
    }

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

    /// The CLI invocation matches the SHARED CONTRACT byte-for-byte, including
    /// the hidden `--porcelain-progress` flag the gauge depends on. If the CLI
    /// renames the flag, this test (and the gauge) must change in lockstep. The
    /// set is the TUI's non-VLM models — NOT `--all` (which would pull the
    /// Deep-Analyze VLMs the TUI has no tab for).
    #[test]
    fn download_invocation_matches_shared_contract() {
        assert_eq!(
            DOWNLOAD_ARGS,
            [
                "models",
                "download",
                "--yes",
                "--porcelain-progress",
                "arcface",
                "mobileclip_s2",
                "ram_plus",
                "clip_text",
                "bge_text",
            ]
        );
        // Never the VLMs (no Deep-Analyze tab in the TUI).
        for vlm in ["mistral_small_3_2", "qwen2_5_vl_7b", "gemma_3_4b", "--all"] {
            assert!(!DOWNLOAD_ARGS.contains(&vlm), "must not download {vlm}");
        }
    }

    /// A `PROGRESS\t{percent}\t{label}` line yields the integer percent + the
    /// exact label string (the gauge's driver).
    #[test]
    fn parses_progress_line_into_percent_and_label() {
        let label = "arcface · 182/271 MB · 3.4 MB/s · model 2/9";
        assert_eq!(
            parse_porcelain_line(&format!("PROGRESS\t62\t{label}")),
            ParsedLine::Progress {
                percent: 62,
                label: label.to_string()
            },
        );
    }

    /// The final progress line is `PROGRESS\t100\tdone`.
    #[test]
    fn final_progress_line_is_100_done() {
        assert_eq!(
            parse_porcelain_line("PROGRESS\t100\tdone"),
            ParsedLine::Progress {
                percent: 100,
                label: "done".to_string()
            },
        );
    }

    /// A non-`PROGRESS` line (milestone, summary, error, stderr) is a trimmed
    /// status message — never mistaken for a gauge update.
    #[test]
    fn non_progress_line_is_a_status_message() {
        assert_eq!(
            parse_porcelain_line("✓ arcface installed"),
            ParsedLine::Status("✓ arcface installed".to_string()),
        );
        // Leading/trailing whitespace (the CLI indents its milestones) is trimmed.
        assert_eq!(
            parse_porcelain_line("  All 9 models ready.  "),
            ParsedLine::Status("All 9 models ready.".to_string()),
        );
    }

    /// A `PROGRESS`-prefixed line with an unparseable percent degrades to a
    /// status message rather than panicking (defensive against wire corruption).
    #[test]
    fn malformed_progress_line_degrades_to_status_not_panic() {
        assert!(matches!(
            parse_porcelain_line("PROGRESS\tNaN\tlabel"),
            ParsedLine::Status(_)
        ));
        assert!(matches!(
            parse_porcelain_line("PROGRESS\t\t"),
            ParsedLine::Status(_)
        ));
    }

    /// An out-of-range percent is clamped to 0–100 so `Gauge::percent` (which
    /// panics above 100) can never be handed a bad value.
    #[test]
    fn progress_percent_is_clamped_to_100() {
        assert_eq!(
            parse_porcelain_line("PROGRESS\t150\tx"),
            ParsedLine::Progress {
                percent: 100,
                label: "x".to_string()
            },
        );
    }
}
