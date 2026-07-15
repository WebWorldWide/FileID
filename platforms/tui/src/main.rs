//! `fileid-tui` — FileID terminal UI (ratatui + crossterm) over the shared
//! Rust engine.
//!
//! Like `platforms/cli`, this links `fileid-engine` as a library and reuses its
//! read surface (`db::open_read`), path resolution (`paths`), and restructure
//! rule cascade (`pipeline::restructure::classify`) IN-PROCESS, so the
//! DB/IPC contract can never drift. Reads are live; the `s` key drives a real
//! full-pipeline scan by spawning the `FileIDEngine` binary and speaking the
//! engine's own `ipc` types over stdio (see `scan.rs`). Face clustering remains
//! a documented follow-on (see README).
//!
//! Cross-OS despite living under `platforms/`: builds + runs identically on
//! macOS, Linux, and Windows.

mod app;
mod context;
mod data;
mod models;
mod scan;
mod ui;

use std::io::{self, Stdout};
use std::process::ExitCode;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use anyhow::{Context as _, Result};
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use app::App;
use context::{Ctx, Invocation};
use data::LoadMsg;

/// Point the engine at its OWN writable models dir for this process and any
/// engine subprocess `scan.rs` spawns (which inherits the env), unless the user
/// already pinned `FILEID_MODELS_DIR`. Mirrors the CLI's `ensure_engine_models_dir`:
/// on macOS this is the engine's XDG `~/.local/share/FileID/Models` — where
/// `fileid models download` / the Settings `D` action install — NOT the desktop
/// app's read-only CoreML `~/Library/Application Support/FileID/Models`, whose
/// CoreML weights the Rust engine can't load. On Windows/Linux it's the default
/// the engine already resolves to, made explicit + inheritable. Both
/// `scan::missing_models` (via `paths::models_dir`) and the spawned engine then
/// agree on one dir. Call before any worker thread spawns.
fn ensure_engine_models_dir() {
    if std::env::var_os("FILEID_MODELS_DIR").is_some_and(|v| !v.is_empty()) {
        return;
    }
    if let Ok(dir) = fileid_engine::paths::engine_models_dir() {
        std::env::set_var("FILEID_MODELS_DIR", dir);
    }
}

fn main() -> ExitCode {
    // FIX 1: pin the engine's OWN models dir BEFORE any worker thread spawns
    // (`set_var` must not race a concurrent env read on another thread, so this
    // has to run while the process is still single-threaded — before
    // `data::spawn_load` / `scan::spawn_scan`). Without it, on macOS the model
    // gate (`scan::missing_models`) and the spawned engine resolve models via the
    // desktop app's read-only CoreML dir, whose Swift weights the Rust engine
    // can't load — so a scan could crash mid-pipeline on incompatible files.
    // Pointing both at the engine's writable dir (where `fileid models download`
    // / the Settings `D` action install) turns that into a graceful "models not
    // installed" and lets a downloaded model set light up full-ML scanning.
    ensure_engine_models_dir();

    match context::parse_args(std::env::args().skip(1)) {
        Invocation::Print(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Invocation::Error(msg) => {
            eprintln!("error: {msg}");
            eprintln!("try `fileid-tui --help`");
            ExitCode::from(2)
        }
        Invocation::Run { db } => match run(db) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e:#}");
                ExitCode::FAILURE
            }
        },
    }
}

fn run(db_flag: Option<std::path::PathBuf>) -> Result<()> {
    let ctx = Ctx::resolve(db_flag)?;
    let mut app = App::new(ctx.db_label());
    app.scratch = ctx.scratch();
    // Seed the standing "models missing" banner before the first frame so a
    // fresh install sees the prompt to press `D` immediately, not only after the
    // async load lands.
    app.missing_models = scan::missing_models_display();

    let (tx, rx): (Sender<LoadMsg>, Receiver<LoadMsg>) = mpsc::channel();
    data::spawn_load(ctx.db.clone(), String::new(), tx.clone());

    // RAII: the guard owns the terminal and its `Drop` is the single source of
    // truth for teardown — it restores cooked mode + the main screen on a normal
    // return AND if the event loop panics, so the user's shell is never left in
    // raw/alternate-screen state.
    let mut guard = setup_terminal().context("entering terminal raw/alt-screen mode")?;
    event_loop(&mut guard.terminal, &mut app, &ctx, &tx, &rx)
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    ctx: &Ctx,
    tx: &Sender<LoadMsg>,
    rx: &Receiver<LoadMsg>,
) -> Result<()> {
    // Held across iterations so we can `kill()` an in-flight model download on
    // quit — the `fileid` CLI has no parent-PID/EOF watchdog (unlike the scan
    // engine), so without this it would keep downloading after the TUI exits.
    let mut download: Option<models::DownloadHandle> = None;
    // Redraw only when something changed — a key, a loader/scan/download message,
    // or a resize. A pure 100 ms poll-timeout tick with none of those leaves the
    // screen untouched, so an idle TUI stops rebuilding every widget (and
    // re-filtering visible_files) ~10x/second for nothing. There is no time-based
    // animation here (download/scan progress arrives as loader messages, each of
    // which sets dirty), so gating on real events can't freeze the UI. ratatui
    // already diffs the back buffer, so this saves the rebuild CPU, not writes.
    let mut dirty = true;
    loop {
        if dirty {
            terminal.draw(|f| ui::render(f, app))?;
            dirty = false;
        }

        // Drain any pending loader events (non-blocking). Re-check the model
        // install state whenever a load/scan/download settles (loading goes
        // true→false), so the standing "models missing" banner clears the moment
        // a download finishes and re-appears if one failed.
        let was_loading = app.loading;
        let mut got_msg = false;
        while let Ok(msg) = rx.try_recv() {
            app.apply_load(msg);
            got_msg = true;
        }
        if was_loading && !app.loading {
            app.missing_models = scan::missing_models_display();
        }
        if got_msg {
            dirty = true;
        }

        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                // Accept Press AND Repeat; only ignore Release. Some terminals/configs
                // report key kinds other than Press, which silently dropped keys.
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    app.on_key(key.code, key.modifiers);
                    dirty = true;
                }
                // A resize changes the layout — force a redraw next iteration.
                Event::Resize(_, _) => dirty = true,
                _ => {}
            }
        }

        if app.reload_requested {
            app.reload_requested = false;
            data::spawn_load(ctx.db.clone(), app.search.clone(), tx.clone());
        }
        // A confirmed folder-pick arms a scan; drive it on a worker thread so
        // the UI stays live (q keeps quitting; the TerminalGuard still restores
        // the terminal). The thread streams status + reloads on completion.
        if let Some(root) = app.scan_requested.take() {
            scan::spawn_scan(
                ctx.db.clone(),
                root,
                ctx.engine_data_home.clone(),
                app.search.clone(),
                tx.clone(),
            );
        }
        // A Settings `D` arms an AI-model download; drive it on a worker thread
        // so the UI stays live (q keeps quitting; TerminalGuard still restores).
        // The thread streams progress to the status line and reloads on success.
        if app.download_requested {
            app.download_requested = false;
            download = Some(models::spawn_download(
                ctx.db.clone(),
                app.search.clone(),
                tx.clone(),
            ));
        }
        if app.should_quit {
            // Kill (don't take) an in-flight download child so it can't keep
            // running orphaned; the worker thread still reclaims + reaps it.
            if let Some(handle) = &download {
                if let Ok(mut slot) = handle.lock() {
                    if let Some(child) = slot.as_mut() {
                        let _ = child.kill();
                    }
                }
            }
            return Ok(());
        }
    }
}

/// Owns the terminal while it is in raw mode + the alternate screen. The `Drop`
/// impl is the single source of truth for teardown (RAII): it runs on a normal
/// return AND while unwinding from a panic in the event loop, so cooked mode,
/// the main screen, and a visible cursor are always restored.
struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Best-effort and idempotent: we may be unwinding, so swallow errors.
        // (Mouse capture is never enabled here, so there is no DisableMouseCapture.)
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

fn setup_terminal() -> Result<TerminalGuard> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    if let Err(error) = execute!(stdout, EnterAlternateScreen) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        return Err(error.into());
    }
    let terminal = match Terminal::new(CrosstermBackend::new(stdout)) {
        Ok(terminal) => terminal,
        Err(error) => {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            return Err(error.into());
        }
    };
    // Restore the terminal on panic BEFORE the default hook prints, so a panic
    // mid-event-loop lands on the cooked main screen instead of being wiped by
    // the alt-screen teardown in TerminalGuard::drop. The guard's Drop still
    // restores on unwind; this only makes the panic message visible.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        prev_hook(info);
    }));
    Ok(TerminalGuard { terminal })
}
