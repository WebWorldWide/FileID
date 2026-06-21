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
mod scan;
mod ui;

use std::io::{self, Stdout};
use std::process::ExitCode;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use anyhow::{Context as _, Result};
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use app::App;
use context::{Ctx, Invocation};
use data::LoadMsg;

fn main() -> ExitCode {
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

    let (tx, rx): (Sender<LoadMsg>, Receiver<LoadMsg>) = mpsc::channel();
    data::spawn_load(ctx.db.clone(), tx.clone());

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
    loop {
        terminal.draw(|f| ui::render(f, app))?;

        // Drain any pending loader events (non-blocking).
        while let Ok(msg) = rx.try_recv() {
            app.apply_load(msg);
        }

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    app.on_key(key.code, key.modifiers);
                }
            }
        }

        if app.reload_requested {
            app.reload_requested = false;
            data::spawn_load(ctx.db.clone(), tx.clone());
        }
        // A confirmed folder-pick arms a scan; drive it on a worker thread so
        // the UI stays live (q keeps quitting; the TerminalGuard still restores
        // the terminal). The thread streams status + reloads on completion.
        if let Some(root) = app.scan_requested.take() {
            scan::spawn_scan(ctx.db.clone(), root, ctx.engine_data_home.clone(), tx.clone());
        }
        if app.should_quit {
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
    execute!(stdout, EnterAlternateScreen)?;
    let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    Ok(TerminalGuard { terminal })
}
