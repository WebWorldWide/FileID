//! `fileid-tui`: terminal UI over the shared engine. Reads run in-process; the `s` key spawns the engine binary for a full-pipeline scan (see `scan.rs`).

mod app;
mod context;
mod data;
mod models;
mod scan;
mod ui;

use std::io::{self, Stdout};
use std::process::ExitCode;
use std::sync::mpsc::{self, Receiver, SyncSender as Sender};
use std::time::Duration;

use anyhow::{Context as _, Result};
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use app::App;
use context::{Ctx, Invocation, terminal_text};
use data::LoadMsg;

/// Pins the engine's own writable models dir unless `FILEID_MODELS_DIR` is set, mirroring the CLI's `ensure_engine_models_dir`. Call before any worker thread spawns.
fn ensure_engine_models_dir() {
    if std::env::var_os("FILEID_MODELS_DIR").is_some_and(|v| !v.is_empty()) {
        return;
    }
    if let Ok(dir) = fileid_engine::paths::engine_models_dir() {
        // SAFETY: called at the top of main, before any thread is spawned.
        unsafe { std::env::set_var("FILEID_MODELS_DIR", dir) };
    }
}

fn main() -> ExitCode {
    // Must run before any worker thread (`set_var` races env reads). On macOS it keeps the model gate and the engine off the desktop app's CoreML dir, which the Rust engine cannot load.
    ensure_engine_models_dir();

    match context::parse_args(std::env::args().skip(1)) {
        Invocation::Print(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Invocation::Error(msg) => {
            eprintln!("error: {}", terminal_text(&msg));
            eprintln!("try `fileid-tui --help`");
            ExitCode::from(2)
        }
        Invocation::Run { db } => match run(db) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {}", terminal_text(&format!("{e:#}")));
                ExitCode::FAILURE
            }
        },
    }
}

fn run(db_flag: Option<std::path::PathBuf>) -> Result<()> {
    let ctx = Ctx::resolve(db_flag)?;
    let mut app = App::new(ctx.db_label());
    app.scratch = ctx.scratch();
    // Seed before the first frame so a fresh install shows the prompt without waiting for the async load.
    app.missing_models = scan::missing_models_display();

    let (tx, rx): (Sender<LoadMsg>, Receiver<LoadMsg>) = mpsc::sync_channel(256);
    let generation = app.advance_load_generation();
    data::spawn_load(ctx.db.clone(), String::new(), tx.clone(), generation);

    // RAII: Drop restores cooked mode and the main screen on return and on panic, so the shell is never left in raw or alternate-screen state.
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
    // Held across iterations so quitting can cancel an in-flight model download.
    let mut download: Option<models::DownloadHandle> = None;
    // Redraw only on a key, a loader/scan/download message or a resize: there is no time-based animation, and an idle tick must not rebuild every widget.
    let mut dirty = true;
    loop {
        if dirty {
            terminal.draw(|f| ui::render(f, app))?;
            dirty = false;
        }

        // Re-check model install state when a load/scan/download settles, so the "models missing" banner tracks reality.
        let was_loading = app.loading;
        let mut got_msg = false;
        for _ in 0..64 {
            let Ok(msg) = rx.try_recv() else { break };
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
                // Accept Press and Repeat; some terminals report other key kinds, which silently dropped keys.
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    app.on_key(key.code, key.modifiers);
                    dirty = true;
                }
                Event::Resize(_, _) => dirty = true,
                _ => {}
            }
        }

        if app.live_search_requested {
            app.live_search_requested = false;
            let query = app.search.trim().to_string();
            if !query.is_empty() {
                data::spawn_live_search(ctx.db.clone(), query, tx.clone());
            }
        }
        if app.reload_requested {
            app.reload_requested = false;
            let generation = app.advance_load_generation();
            data::spawn_load(ctx.db.clone(), app.search.clone(), tx.clone(), generation);
        }
        // Run the scan on a worker thread so the UI stays live.
        if let Some(root) = app.scan_requested.take() {
            let generation = app.advance_load_generation();
            scan::spawn_scan(
                ctx.db.clone(),
                root,
                ctx.engine_data_home.clone(),
                app.search.clone(),
                tx.clone(),
                generation,
            );
        }
        // Run the model download on a worker thread so the UI stays live.
        if app.download_requested {
            app.download_requested = false;
            let generation = app.advance_load_generation();
            download = Some(models::spawn_download(
                ctx.db.clone(),
                app.search.clone(),
                tx.clone(),
                generation,
            ));
        }
        if app.should_quit {
            if let Some(handle) = &download {
                handle.cancel();
            }
            return Ok(());
        }
    }
}

/// Owns raw mode and the alternate screen; `Drop` restores them on return and on panic unwind.
struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Best-effort and idempotent: we may be unwinding, so swallow errors.
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
    // Restore the terminal before the default hook prints, so the panic message lands on the main screen, not the alt screen.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        prev_hook(info);
    }));
    Ok(TerminalGuard { terminal })
}
