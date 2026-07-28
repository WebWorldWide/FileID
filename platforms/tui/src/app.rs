//! Backend-agnostic application state + key handling.
//!
//! This module owns NO terminal types — it's a state machine over
//! [`crossterm::event::KeyCode`], which makes it unit-testable headlessly
//! (navigation clamps, tab cycling, search filtering, the load-event reducer).
//! `ui.rs` renders a `&App`; `main.rs` feeds it key codes + load messages.
//!
//! The one filesystem touch is the scan-path prompt: confirming the typed path
//! checks `exists()` / `is_dir()` so a bad path gets an *inline* error instead
//! of a deferred async one. The `~`-expansion is factored into a pure helper so
//! it stays unit-testable.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyModifiers};

use fileid_engine::pipeline::discovery::FileKind;

use crate::data::{FileRow, LoadMsg, Snapshot};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Library,
    People,
    Cleanup,
    DeepAnalyze,
    Restructure,
    Settings,
}

impl Tab {
    pub const ALL: [Tab; 6] = [
        Tab::Library,
        Tab::People,
        Tab::Cleanup,
        Tab::DeepAnalyze,
        Tab::Restructure,
        Tab::Settings,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Library => "Library",
            Tab::People => "People",
            Tab::Cleanup => "Cleanup",
            Tab::DeepAnalyze => "Deep Analyze",
            Tab::Restructure => "Restructure",
            Tab::Settings => "Settings",
        }
    }

    pub fn index(self) -> usize {
        Tab::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }

    pub fn from_index(i: usize) -> Tab {
        Tab::ALL[i % Tab::ALL.len()]
    }

    pub fn next(self) -> Tab {
        Tab::from_index(self.index() + 1)
    }

    pub fn prev(self) -> Tab {
        Tab::from_index(self.index() + Tab::ALL.len() - 1)
    }
}

/// Live AI-model install progress backing the gauge (`ui.rs`). Set when a
/// download is armed (at 0%), advanced by each [`LoadMsg::DownloadProgress`], and
/// cleared when the worker finishes (its terminal `Done`/`Error`). `done` flips
/// once the final `PROGRESS\t100\tdone` lands, switching the gauge to its brief
/// success state before it clears.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DownloadState {
    pub percent: u16,
    pub label: String,
    pub done: bool,
}

pub struct App {
    pub tab: Tab,
    pub data: Snapshot,
    /// Per-tab cursor position (indexed by `Tab::index`).
    pub selected: [usize; 6],
    pub search: String,
    pub search_active: bool,
    pub status: String,
    pending_scan_warning: Option<String>,
    pub loading: bool,
    pub load_generation: u64,
    pub show_help: bool,
    pub should_quit: bool,
    /// Set when the user asks for a reload; `main` consumes it and re-spawns
    /// the loader, then clears it.
    pub reload_requested: bool,
    pub db_label: String,
    /// True when running against the default SCRATCH library (no `--db`): the
    /// TUI opens EMPTY and only accumulates what the user scans here, never the
    /// desktop app's library. Drives the friendly empty-screen + Settings copy.
    pub scratch: bool,

    // ── Folder-pick + scan (FIX 2) ──────────────────────────────────────────
    /// The single-line path-input prompt is open (typing a folder to scan).
    pub input_active: bool,
    /// The text typed so far in the path prompt.
    pub input: String,
    /// Inline validation error shown in the prompt (bad/!dir path). The prompt
    /// stays open so the user can fix it — never a panic.
    pub input_error: Option<String>,
    /// The folder the user chose to scan (shown in the Library header / status).
    pub scan_root: Option<String>,
    /// A scan is in flight (drives the header + blocks a second concurrent scan).
    pub scanning: bool,
    /// Set when a scan is confirmed; `main` consumes it, spawns the engine-driven
    /// scan thread, and clears it (mirrors `reload_requested`).
    pub scan_requested: Option<PathBuf>,
    /// The folder-browser overlay (the PRIMARY `s` UX, FIX 2). `Some` while the
    /// user is navigating directories to pick one to scan; `None` otherwise.
    pub browser: Option<Browser>,

    // ── AI-model download (Settings `D`) ────────────────────────────────────
    /// A `fileid models download --all` is in flight (drives the status line +
    /// blocks a second concurrent download).
    pub downloading: bool,
    /// Set when the user presses `D`; `main` consumes it, spawns the download
    /// worker thread, and clears it (mirrors `scan_requested`).
    pub download_requested: bool,
    /// Live model-install progress driving the gauge (`None` ⇒ no install in
    /// flight). Armed at 0% by `request_download`, advanced by each
    /// `DownloadProgress`, cleared on the worker's terminal `Done`/`Error`.
    pub download: Option<DownloadState>,

    // ── Persistent model-state banner + error feedback ──────────────────────
    /// Display names of the required AI models that aren't installed yet (empty
    /// ⇒ all present). Drives the standing "models missing" banner shown above
    /// EVERY tab. `main` sets it at startup and re-checks whenever a load/scan/
    /// download settles, so the banner can't go stale.
    pub missing_models: Vec<String>,
    /// The current status line reports a FAILURE (a failed scan/download/load),
    /// so the status row shows a distinct ⚠ instead of the success ✓ — feedback
    /// that persists rather than flashing for a frame.
    pub status_error: bool,
}

impl App {
    pub fn new(db_label: String) -> Self {
        Self {
            tab: Tab::Library,
            data: Snapshot::default(),
            selected: [0; 6],
            search: String::new(),
            search_active: false,
            status: "Starting…".to_string(),
            pending_scan_warning: None,
            loading: true,
            load_generation: 0,
            show_help: false,
            should_quit: false,
            reload_requested: false,
            db_label,
            scratch: false,
            input_active: false,
            input: String::new(),
            input_error: None,
            scan_root: None,
            scanning: false,
            scan_requested: None,
            browser: None,
            downloading: false,
            download_requested: false,
            download: None,
            missing_models: Vec::new(),
            status_error: false,
        }
    }

    pub fn advance_load_generation(&mut self) -> u64 {
        self.pending_scan_warning = None;
        self.load_generation = self.load_generation.wrapping_add(1).max(1);
        crate::data::begin_generation(self.load_generation);
        self.load_generation
    }

    /// Files visible under the current search filter (Library tab).
    pub fn visible_files(&self) -> Vec<&FileRow> {
        if !self.search_active || self.search == self.data.query {
            self.data.files.iter().collect()
        } else {
            let q = self.search.to_lowercase();
            self.data
                .files
                .iter()
                .filter(|f| {
                    f.path.to_lowercase().contains(&q) || f.kind.to_lowercase().contains(&q)
                })
                .collect()
        }
    }

    /// Length of the list backing the active tab.
    pub fn list_len(&self) -> usize {
        match self.tab {
            Tab::Library => self.visible_files().len(),
            Tab::People => self.data.people.len(),
            Tab::Cleanup => self.data.dupes.len(),
            Tab::DeepAnalyze => 0,
            Tab::Restructure => self.data.plan.len(),
            Tab::Settings => 0,
        }
    }

    /// Cursor index for the active tab, clamped to the current list length.
    pub fn cursor(&self) -> usize {
        self.cursor_clamped(self.list_len())
    }

    /// Clamp the active tab's stored cursor against an ALREADY-KNOWN length, so a
    /// caller holding the list (e.g. a rendered `visible_files`) skips recomputing
    /// it through `list_len`.
    pub fn cursor_clamped(&self, len: usize) -> usize {
        if len == 0 {
            0
        } else {
            self.selected[self.tab.index()].min(len - 1)
        }
    }

    fn set_cursor(&mut self, v: usize) {
        let idx = self.tab.index();
        self.selected[idx] = v;
    }

    pub fn select_next(&mut self) {
        let len = self.list_len();
        if len > 0 {
            // cursor_clamped(len) reuses the length we just computed instead of
            // self.cursor() recomputing list_len() — one fewer visible_files()
            // filter+alloc per keypress on the Library tab. (Matches select_last.)
            let c = (self.cursor_clamped(len) + 1).min(len - 1);
            self.set_cursor(c);
        }
    }

    pub fn select_prev(&mut self) {
        let c = self.cursor().saturating_sub(1);
        self.set_cursor(c);
    }

    pub fn select_first(&mut self) {
        self.set_cursor(0);
    }

    pub fn select_last(&mut self) {
        let len = self.list_len();
        self.set_cursor(len.saturating_sub(1));
    }

    fn switch_tab(&mut self, tab: Tab) {
        self.tab = tab;
    }

    /// Fold a loader message into state.
    pub fn apply_load(&mut self, msg: LoadMsg) {
        match msg {
            LoadMsg::Versioned {
                generation,
                message,
            } => {
                if generation == self.load_generation {
                    self.apply_load(*message);
                }
            }
            LoadMsg::Status(s) => {
                self.status = s;
                self.status_error = false;
            }
            LoadMsg::ScanPartial(warning) => {
                self.status = warning.clone();
                self.pending_scan_warning = Some(warning);
                self.status_error = true;
            }
            LoadMsg::DownloadProgress { percent, label } => {
                // Clamp defensively (the parser already does, but the gauge must
                // never see >100). A live install means no error on the status row.
                let percent = percent.min(100);
                self.download = Some(DownloadState {
                    percent,
                    label,
                    done: percent >= 100,
                });
                self.status_error = false;
            }
            LoadMsg::Done(snap) => {
                self.data = *snap;
                self.loading = false;
                self.scanning = false;
                self.downloading = false;
                self.download = None;
                self.status_error = false;
                // Re-clamp every cursor against the freshly loaded lengths.
                self.clamp_all();
            }
            LoadMsg::Dupes(report) => {
                // Deferred duplicate verification landing after Done — fold it
                // into the already-painted snapshot. (audit 2026-07-14)
                self.data.dupes = report.dupes;
                self.data.dupes_truncated = report.dupes_truncated;
                self.data.dupe_candidate_count = report.dupe_candidate_count;
                self.data.dupes_pending = false;
                self.clamp_all();
                if let Some(warning) = self.pending_scan_warning.take() {
                    self.status = warning;
                    self.status_error = true;
                }
            }
            LoadMsg::Error(e) => {
                self.status = e;
                self.loading = false;
                self.scanning = false;
                self.downloading = false;
                self.download = None;
                self.pending_scan_warning = None;
                self.status_error = true;
            }
        }
    }

    fn clamp_all(&mut self) {
        let tab = self.tab;
        for t in Tab::ALL {
            self.tab = t;
            let c = self.cursor();
            self.set_cursor(c);
        }
        self.tab = tab;
    }

    /// Handle one key press. Returns nothing; mutates state (incl. `should_quit`
    /// and `reload_requested`, which `main` acts on).
    pub fn on_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        // Control chords are resolved BEFORE the modal handlers below, which
        // dispatch on `code` alone. Without this, a chord reached their
        // `KeyCode::Char(c)` arm and was typed in as a bare letter: Ctrl+C in
        // the search box appended "c" instead of quitting, and Ctrl+U appended
        // "u" instead of doing nothing.
        //
        // AltGr reports as CONTROL|ALT on Windows, and it types real characters
        // on European layouts (@, €, ł). So this only claims CONTROL *without*
        // ALT — otherwise AltGr text would be silently swallowed.
        if mods.contains(KeyModifiers::CONTROL) && !mods.contains(KeyModifiers::ALT) {
            if let KeyCode::Char(c) = code {
                if c.eq_ignore_ascii_case(&'c') {
                    self.should_quit = true;
                }
                return;
            }
        }
        if self.input_active {
            self.on_key_input(code);
            return;
        }
        if self.browser.is_some() {
            self.on_key_browser(code);
            return;
        }
        if self.search_active {
            self.on_key_search(code);
            return;
        }
        // The `?` help overlay is modal for quit keys: Esc/q dismiss it instead of
        // quitting the app, so a user reading the keys can back out safely.
        if self.show_help {
            if let KeyCode::Esc | KeyCode::Char('q') = code {
                self.show_help = false;
                return;
            }
        }
        match code {
            // A committed search (Enter) leaves `search_active` false but keeps
            // the term, and the empty-results panel then advertises
            // `Esc  clear the search` (ui.rs). Honour that before the quit arm
            // below — otherwise the on-screen instruction closed the app.
            KeyCode::Esc if !self.search.is_empty() => {
                self.search.clear();
                self.set_cursor(0);
                if !self.data.query.is_empty() {
                    self.request_reload();
                }
            }
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Tab => self.switch_tab(self.tab.next()),
            KeyCode::BackTab => self.switch_tab(self.tab.prev()),
            KeyCode::Char(d @ '1'..='6') => {
                let i = (d as u8 - b'1') as usize;
                self.switch_tab(Tab::from_index(i));
            }
            KeyCode::Down | KeyCode::Char('j') => self.select_next(),
            KeyCode::Up | KeyCode::Char('k') => self.select_prev(),
            KeyCode::Home | KeyCode::Char('g') => self.select_first(),
            KeyCode::End | KeyCode::Char('G') => self.select_last(),
            KeyCode::Char('s') => self.open_browser(),
            KeyCode::Char('r') => self.request_reload(),
            KeyCode::Char('?') => self.show_help = !self.show_help,
            KeyCode::Char('/') if self.tab == Tab::Library && !self.loading => {
                self.search_active = true;
                self.show_help = false;
            }
            // Download the AI models the full-ML scan needs — GLOBAL (any tab),
            // so it's reachable from the first-run welcome screen and every tab's
            // standing "models missing" banner, not just Settings.
            KeyCode::Char('D') => self.request_download(),
            _ => {}
        }
    }

    /// Arm a library reload (the `r` key): `main` consumes `reload_requested`
    /// next tick and re-spawns the DB loader. Guarded against a live download AND
    /// a live scan — both share the loader channel, and `data::load`'s terminal
    /// `Done` (which `apply_load` folds in) clears `downloading`/`scanning`
    /// wholesale. A reload racing a download would clear the install gauge
    /// mid-install (inviting a second `D` that double-spawns the same download); a
    /// reload racing a scan lands its quick DB-read `Done` first, clearing
    /// `scanning` while the engine subprocess is still writing the DB — ending the
    /// scan in the UI and re-enabling `s`/`D`, which could arm a SECOND engine on
    /// the same SQLite file (violating the single-writer invariant). Ignored while
    /// either runs.
    fn request_reload(&mut self) {
        if self.loading {
            self.status = "A library load is already running…".to_string();
            return;
        }
        if self.downloading {
            self.status =
                "A model download is running — wait for it to finish before reloading…".to_string();
            return;
        }
        if self.scanning {
            self.status =
                "A scan is in progress — wait for it to finish before reloading…".to_string();
            return;
        }
        self.loading = true;
        self.status = "Reloading…".to_string();
        self.reload_requested = true;
    }

    /// Arm a `fileid models download` (the global `D` key): `main`
    /// consumes `download_requested` next tick and spawns the worker thread.
    /// Guarded so a second download — or a download racing a live scan — can't
    /// start. Never blocks: the spawned thread streams progress to the status
    /// line and `q` keeps quitting.
    fn request_download(&mut self) {
        if self.downloading {
            self.status = "A model download is already running…".to_string();
            return;
        }
        if self.scanning {
            self.status = "Finish the current scan before downloading models…".to_string();
            return;
        }
        self.downloading = true;
        self.loading = true;
        self.status = "Starting AI model download…".to_string();
        self.download_requested = true;
        // Arm the gauge at 0% immediately so the bar appears the same frame the
        // user presses D — before the first `PROGRESS` line lands.
        self.download = Some(DownloadState {
            percent: 0,
            label: "Preparing…".to_string(),
            done: false,
        });
        self.show_help = false;
    }

    /// Open the folder BROWSER (the primary `s` UX, FIX 2). Starts at `$HOME`
    /// (falling back to the cwd, then `/`), so the user navigates with arrows +
    /// Enter instead of typing a raw path. Ignored mid-scan so two scans can't
    /// race the engine.
    fn open_browser(&mut self) {
        if self.scanning {
            self.status = "A scan is already in progress…".to_string();
            return;
        }
        if self.downloading {
            self.status = "Finish the model download before scanning…".to_string();
            return;
        }
        let start = home_dir()
            .filter(|h| h.is_dir())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"));
        self.browser = Some(Browser::open(start));
        self.show_help = false;
    }

    /// Drive the folder-browser overlay. ↑↓/jk move; Enter (or →/l) opens the
    /// highlighted subfolder; Backspace (or ←/h) goes up; `s`/`S` scans the
    /// CURRENT folder; `t` falls back to typing a path; Esc cancels. Filesystem
    /// errors surface as an in-overlay notice — never a panic.
    fn on_key_browser(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => self.browser = None,
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(b) = self.browser.as_mut() {
                    b.move_down();
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(b) = self.browser.as_mut() {
                    b.move_up();
                }
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                if let Some(b) = self.browser.as_mut() {
                    b.enter_selected();
                }
            }
            KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') => {
                if let Some(b) = self.browser.as_mut() {
                    b.go_up();
                }
            }
            // Scan THIS folder — the headline action. Closing the browser first
            // mirrors the typed-path flow (`confirm_input`).
            KeyCode::Char('s') | KeyCode::Char('S') => {
                if self
                    .browser
                    .as_ref()
                    .is_some_and(|browser| browser.virtual_drives)
                {
                    if let Some(browser) = self.browser.as_mut() {
                        browser.notice =
                            Some("Open a drive before choosing a folder to scan.".to_string());
                    }
                } else if let Some(browser) = self.browser.take() {
                    self.arm_scan(browser.cwd);
                }
            }
            // Jump straight to where external/removable drives mount (FEATURE 1).
            KeyCode::Char('d') | KeyCode::Char('D') => {
                if let Some(b) = self.browser.as_mut() {
                    b.go_to_drives();
                }
            }
            // Toggle showing dot-prefixed (hidden) entries — hidden by default
            // (FEATURE 2).
            KeyCode::Char('.') => {
                if let Some(b) = self.browser.as_mut() {
                    b.toggle_hidden();
                }
            }
            // Typed-path fallback (kept trivial; the browser is primary).
            KeyCode::Char('t') => {
                self.browser = None;
                self.open_input();
            }
            _ => {}
        }
    }

    /// Open the single-line folder-path prompt — the typed-path fallback reached
    /// with `t` from the browser. Ignored while a scan is already running so two
    /// scans can't race the engine.
    fn open_input(&mut self) {
        if self.scanning {
            self.status = "A scan is already in progress…".to_string();
            return;
        }
        if self.downloading {
            self.status = "Finish the model download before scanning…".to_string();
            return;
        }
        self.input_active = true;
        self.input.clear();
        self.input_error = None;
        self.show_help = false;
    }

    /// Arm an engine scan of `path`: `main` consumes `scan_requested` next tick
    /// and spawns the engine-driven scan thread. Shared by the folder browser
    /// (`s`) and the typed-path fallback (`confirm_input`) so both routes behave
    /// identically.
    fn arm_scan(&mut self, path: PathBuf) {
        let display = path.to_string_lossy().into_owned();
        self.status = format!("Starting scan of {display}…");
        self.scan_root = Some(display);
        self.scanning = true;
        self.loading = true;
        self.input_error = None;
        self.scan_requested = Some(path);
    }

    fn on_key_input(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                self.input_active = false;
                self.input.clear();
                self.input_error = None;
            }
            // Tab and Enter both confirm.
            KeyCode::Enter | KeyCode::Tab => self.confirm_input(),
            KeyCode::Backspace => {
                self.input.pop();
                self.input_error = None;
            }
            KeyCode::Char(c) => {
                self.input.push(c);
                self.input_error = None;
            }
            _ => {}
        }
    }

    /// Validate the typed path (with `~` expansion) and, if it's a real folder,
    /// arm a scan that `main` will drive through the engine. A bad path sets an
    /// inline error and keeps the prompt open — it never panics or wedges.
    fn confirm_input(&mut self) {
        let raw = self.input.trim().to_string();
        if raw.is_empty() {
            self.input_error = Some("Enter a folder path.".to_string());
            return;
        }
        let path = expand_tilde_with(&raw, home_dir().as_deref());
        if !path.exists() {
            self.input_error = Some(format!("No such path: {}", path.display()));
            return;
        }
        if !path.is_dir() {
            self.input_error = Some(format!("Not a folder: {}", path.display()));
            return;
        }
        self.input_active = false;
        self.arm_scan(path);
    }

    fn on_key_search(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                self.search_active = false;
                self.search.clear();
                self.set_cursor(0);
                if !self.data.query.is_empty() {
                    self.request_reload();
                }
            }
            KeyCode::Enter => {
                self.search = self.search.trim().to_string();
                self.search_active = false;
                self.set_cursor(0);
                if self.search != self.data.query {
                    self.request_reload();
                }
            }
            KeyCode::Backspace => {
                self.search.pop();
                self.set_cursor(0);
            }
            KeyCode::Char(c) => {
                self.search.push(c);
                self.set_cursor(0);
            }
            _ => {}
        }
    }
}

/// Max directory entries any single `read_dir` count walks before it stops and
/// reports a `capped` (≥) result — bounds the work on enormous folders so a
/// count can never hang the UI (shown as e.g. `500+`).
const COUNT_CAP: usize = 500;
/// Max files listed in the browser's dimmed "files here" preview; the rest
/// collapse into a `+N more` line.
const FILE_LIST_CAP: usize = 64;
/// Hard ceiling on entries a single `refresh` examines. The per-kind budgets are
/// independent (so a file-heavy folder keeps its subdirs navigable), which means
/// the "both budgets full" break never fires in a flat folder of only files (the
/// dir budget never fills) — this ceiling bounds that walk so hundreds of
/// thousands of files (or hidden dot-entries) can't freeze the UI. Comfortably
/// above `COUNT_CAP` so merely file-heavy folders still surface their subdirs.
const WALK_CAP: usize = COUNT_CAP * 10;

/// A cheap, shallow tally of a directory's immediate contents (FIX 2): how many
/// images, total files, and subfolders it holds. `capped` means the walk hit
/// [`COUNT_CAP`] and the real totals are at least these — rendered with a `+`.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct DirCounts {
    pub images: usize,
    pub files: usize,
    pub dirs: usize,
    pub capped: bool,
}

/// One file in the current folder's dimmed preview list (FIX 2): its display
/// name plus whether it's an image, so a scan's actual targets are visible.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FileEntry {
    pub name: String,
    pub is_image: bool,
}

/// A row in the folder browser's list: either the "go up" affordance or one of
/// the current folder's subdirectories.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum BrowseRow {
    /// `..` — go up to the parent directory (shown when one exists).
    Parent,
    /// A subdirectory the user can open or scan.
    Dir(PathBuf),
}

/// Folder-navigator state (the `s` key, FIX 2). Replaces raw path-typing as the
/// primary "what do I scan?" UX: start at `$HOME`, list this folder's
/// subdirectories, walk in with Enter, up with Backspace, and scan the *current*
/// folder with `s`.
///
/// Reads the filesystem but is panic-free: an unreadable / missing directory
/// yields an empty list plus a `notice`, never a crash.
pub struct Browser {
    /// The directory currently shown.
    pub cwd: PathBuf,
    /// Visible rows: an optional leading `..`, then `cwd`'s subdirectories
    /// (sorted, case-insensitive). Rebuilt whenever `cwd` changes.
    pub rows: Vec<BrowseRow>,
    /// Highlighted index into `rows`.
    pub selected: usize,
    /// A transient one-line notice (e.g. a permission-denied skip).
    pub notice: Option<String>,
    /// `cwd`'s OWN shallow counts (FIX 2) — shown in the browser title so the
    /// user sees what a "scan this folder" would pick up at a glance.
    pub here: DirCounts,
    /// `cwd`'s immediate files (FIX 2), capped at [`FILE_LIST_CAP`] for the
    /// dimmed preview; `files_total` is how many there really are.
    pub files: Vec<FileEntry>,
    /// Total files in `cwd` (≥ `files.len()`), for the `+N more` line.
    pub files_total: usize,
    /// Show dot-prefixed (hidden) entries. Hidden by DEFAULT (FEATURE 2);
    /// toggled with `.`. Applies to the subdir list, the file preview, AND every
    /// count so what's shown and what's tallied always agree.
    pub show_hidden: bool,
    /// A virtual "This computer" list of mount roots. Windows has no common
    /// parent above `C:\\`, `D:\\`, …, so this state is what makes every local
    /// and removable drive reachable from the keyboard-only picker.
    pub virtual_drives: bool,
    /// Lazy per-subdir count cache (FIX 2), filled DURING RENDER for the rows
    /// actually on screen so opening a folder never eagerly walks every child.
    /// `None` caches an unreadable subdir so it isn't retried each frame.
    /// `RefCell` because rendering takes `&Browser`; the cache is per-`cwd` and
    /// cleared on navigation, so it stays bounded.
    counts: RefCell<HashMap<PathBuf, Option<DirCounts>>>,
}

impl Browser {
    /// Open a browser rooted at `start`. Never panics — an unreadable root just
    /// shows an empty list with a notice.
    pub fn open(start: PathBuf) -> Browser {
        let mut b = Browser {
            cwd: start,
            rows: Vec::new(),
            selected: 0,
            notice: None,
            here: DirCounts::default(),
            files: Vec::new(),
            files_total: 0,
            show_hidden: false,
            virtual_drives: false,
            counts: RefCell::new(HashMap::new()),
        };
        b.refresh();
        b
    }

    /// Re-read `cwd` in ONE shallow pass (FIX 2): split it into subdirectories
    /// (→ `rows`, sorted case-insensitively, prefixed by `..`), a dimmed file
    /// preview (→ `files`, capped), and `cwd`'s own [`DirCounts`] (→ `here`,
    /// for the title). File and directory work have independent [`COUNT_CAP`]
    /// bounds, so a file-heavy folder keeps its subdirs navigable; a separate
    /// [`WALK_CAP`] ceiling bounds the total entries examined so a huge flat
    /// folder can never be walked to the end (which would freeze the UI).
    /// An unreadable directory yields empty lists + a `notice`;
    /// never panics. `file_type()` is preferred so symlink loops aren't
    /// followed; unreadable individual entries are simply skipped. Clears the
    /// per-subdir count cache since we've navigated to a new `cwd`.
    fn refresh(&mut self) {
        self.virtual_drives = false;
        self.counts.borrow_mut().clear();
        let mut dirs: Vec<PathBuf> = Vec::new();
        let mut files: Vec<FileEntry> = Vec::new();
        let mut here = DirCounts::default();
        match std::fs::read_dir(&self.cwd) {
            Ok(entries) => {
                let mut examined = 0usize;
                for entry in entries.flatten() {
                    // Hard ceiling FIRST (before any per-entry work): a flat
                    // folder of hundreds of thousands of files — or hidden
                    // dot-entries — where the dir budget never fills would
                    // otherwise be walked to the very end on every refresh,
                    // freezing the UI (`q` included). Counting every entry, even
                    // ones skipped below, is what bounds the dotfile case.
                    examined += 1;
                    if examined > WALK_CAP {
                        here.capped = true;
                        break;
                    }
                    let path = entry.path();
                    let name = dir_label(&path);
                    // Hidden-by-default: skip dot-entries unless toggled on, so the
                    // subdir list, the file preview, and the counts below all
                    // agree (FEATURE 2).
                    if !self.show_hidden && name.starts_with('.') {
                        continue;
                    }
                    // Both display budgets full: every further entry can only
                    // re-set `capped`. The budgets are independent, so this fires
                    // only when the folder has ≥COUNT_CAP of BOTH kinds — a
                    // file-heavy folder keeps looking for subdirs (up to WALK_CAP).
                    if here.dirs >= COUNT_CAP && here.files >= COUNT_CAP {
                        here.capped = true;
                        break;
                    }
                    let is_dir = entry
                        .file_type()
                        .map(|t| t.is_dir())
                        .unwrap_or_else(|_| path.is_dir());
                    if is_dir && here.dirs < COUNT_CAP {
                        here.dirs += 1;
                        dirs.push(path);
                    } else if is_dir {
                        here.capped = true;
                    } else if here.files < COUNT_CAP {
                        here.files += 1;
                        let is_image = is_image_path(&path);
                        if is_image {
                            here.images += 1;
                        }
                        if files.len() < FILE_LIST_CAP {
                            files.push(FileEntry { name, is_image });
                        }
                    } else {
                        here.capped = true;
                    }
                }
                self.notice = None;
            }
            Err(_) => {
                self.notice =
                    Some("Can't read this folder (permission denied or unavailable).".to_string());
            }
        }
        dirs.sort_by_key(|a| dir_key(a));
        files.sort_by_cached_key(|f| f.name.to_lowercase());

        let mut rows = Vec::with_capacity(dirs.len() + 1);
        if self.cwd.parent().is_some() {
            rows.push(BrowseRow::Parent);
        }
        rows.extend(dirs.into_iter().map(BrowseRow::Dir));
        self.rows = rows;
        self.here = here;
        self.files_total = here.files;
        self.files = files;
        if self.selected >= self.rows.len() {
            self.selected = self.rows.len().saturating_sub(1);
        }
    }

    /// Shallow counts for a subdirectory `path` (FIX 2), memoized. Called from
    /// the renderer for the rows currently ON SCREEN, so scrolling computes at
    /// most one `read_dir` per newly-visible row and never re-walks a folder.
    /// `None` = unreadable (rendered without counts); cached so it isn't retried
    /// every frame. Bounded by [`COUNT_CAP`]; never panics.
    pub fn count_for(&self, path: &Path) -> Option<DirCounts> {
        if let Some(&cached) = self.counts.borrow().get(path) {
            return cached;
        }
        let computed = count_dir_shallow(path, self.show_hidden);
        self.counts
            .borrow_mut()
            .insert(path.to_path_buf(), computed);
        computed
    }

    fn move_down(&mut self) {
        if !self.rows.is_empty() {
            self.selected = (self.selected + 1).min(self.rows.len() - 1);
        }
    }

    fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Act on the highlighted row: `..` goes up, a subdirectory is entered (only
    /// if readable — you can't scan what you can't read; otherwise a notice is
    /// shown and we stay put).
    fn enter_selected(&mut self) {
        match self.rows.get(self.selected).cloned() {
            Some(BrowseRow::Parent) => self.go_up(),
            Some(BrowseRow::Dir(path)) => {
                if std::fs::read_dir(&path).is_ok() {
                    self.virtual_drives = false;
                    self.cwd = path;
                    self.selected = 0;
                    self.refresh();
                } else {
                    self.notice = Some(format!(
                        "Can't open {} (permission denied).",
                        dir_label(&path)
                    ));
                }
            }
            None => {}
        }
    }

    /// Go up to the parent directory. Walks all the way to the filesystem root
    /// `/` (the root has no parent, so it's a no-op there) — so the user can
    /// always climb out to reach any drive.
    fn go_up(&mut self) {
        if self.virtual_drives {
            return;
        }
        if let Some(parent) = self.cwd.parent().map(Path::to_path_buf) {
            self.cwd = parent;
            self.selected = 0;
            self.refresh();
        } else {
            self.go_to_drives();
        }
    }

    /// Jump straight to where external/removable drives mount (the `d` key,
    /// FEATURE 1): macOS `/Volumes`; Linux `/media/$USER` · `/media` · `/mnt`;
    /// else the filesystem root `/`. Re-reads the listing so the user can step
    /// into a drive. Panic-free (an unreadable target just shows a notice).
    fn go_to_drives(&mut self) {
        self.cwd = PathBuf::from("Drives");
        self.rows = drive_roots().into_iter().map(BrowseRow::Dir).collect();
        self.selected = 0;
        self.notice = None;
        self.here = DirCounts::default();
        self.files.clear();
        self.files_total = 0;
        self.virtual_drives = true;
        self.counts.borrow_mut().clear();
    }

    /// Toggle showing dot-prefixed (hidden) entries (the `.` key, FEATURE 2).
    /// Re-reads `cwd` — which also clears the per-subdir count cache — so the
    /// subdir list, file preview, and counts all reflect the new state at once.
    fn toggle_hidden(&mut self) {
        if self.virtual_drives {
            return;
        }
        self.show_hidden = !self.show_hidden;
        self.refresh();
    }
}

/// Case-insensitive sort key for a directory (its final segment).
fn dir_key(p: &Path) -> String {
    p.file_name()
        .unwrap_or(p.as_os_str())
        .to_string_lossy()
        .to_lowercase()
}

/// The directory's display label (final segment, or the whole path if none).
pub fn dir_label(p: &Path) -> String {
    p.file_name().map_or_else(
        || p.to_string_lossy().into_owned(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// Is `path` an image, by extension? Uses the engine's OWN `FileKind` table so
/// the browser's "N images" tally matches exactly what a scan would classify.
fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .map(|ext| FileKind::from_extension(ext) == FileKind::Image)
        .unwrap_or(false)
}

/// One shallow, bounded `read_dir` tally of `path`'s immediate children (FIX 2):
/// images / total files / subfolders. Stops at [`COUNT_CAP`] entries (sets
/// `capped`) so a folder with a million files can't hang the count. Returns
/// `None` on a read error (permission denied / unavailable) so the caller can
/// render the row without counts. Never recurses; never panics.
fn count_dir_shallow(path: &Path, show_hidden: bool) -> Option<DirCounts> {
    let entries = std::fs::read_dir(path).ok()?;
    let mut c = DirCounts::default();
    for entry in entries.flatten() {
        if c.dirs + c.files >= COUNT_CAP {
            c.capped = true;
            break;
        }
        let p = entry.path();
        // Honor the same hidden-by-default policy as the visible listing so a
        // folder's preview count matches what a scan-here would show (FEATURE 2).
        if !show_hidden && dir_label(&p).starts_with('.') {
            continue;
        }
        let is_dir = entry
            .file_type()
            .map(|t| t.is_dir())
            .unwrap_or_else(|_| p.is_dir());
        if is_dir {
            c.dirs += 1;
        } else {
            c.files += 1;
            if is_image_path(&p) {
                c.images += 1;
            }
        }
    }
    Some(c)
}

/// `$HOME` (or `%USERPROFILE%` on Windows), if set + non-empty.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// Expand a leading `~` / `~/…` (or `~\…`) against `home`. Pure given `home`,
/// so it's unit-testable without touching the process environment.
fn expand_tilde_with(input: &str, home: Option<&Path>) -> PathBuf {
    if input == "~" {
        if let Some(h) = home {
            return h.to_path_buf();
        }
    } else if let Some(rest) = input
        .strip_prefix("~/")
        .or_else(|| input.strip_prefix("~\\"))
    {
        if let Some(h) = home {
            return h.join(rest);
        }
    }
    PathBuf::from(input)
}

/// Where external / removable drives mount, for the browser's `d` jump (FEATURE
/// 1): macOS `/Volumes`; Linux `/media/$USER` · `/media` · `/mnt` (first that
/// exists); else the filesystem root `/`. Never panics.
fn drive_roots() -> Vec<PathBuf> {
    existing_drive_roots(&drives_candidates(), |path| path.is_dir())
}

/// Ordered candidate mount locations for the current OS (see [`drives_root`]).
/// Pure (no filesystem touch), so the platform branch is obvious and testable.
fn drives_candidates() -> Vec<PathBuf> {
    if cfg!(target_os = "macos") {
        vec![PathBuf::from("/Volumes")]
    } else if cfg!(target_os = "windows") {
        ('A'..='Z')
            .map(|letter| PathBuf::from(format!("{letter}:\\")))
            .collect()
    } else {
        let mut v = Vec::with_capacity(3);
        if let Some(user) = std::env::var_os("USER").filter(|u| !u.is_empty()) {
            v.push(Path::new("/media").join(user));
        }
        v.push(PathBuf::from("/media"));
        v.push(PathBuf::from("/mnt"));
        v
    }
}

/// Every candidate satisfying `exists`, with a platform-correct fallback when
/// none are available. Injecting the probe keeps multi-drive behavior testable
/// without requiring removable hardware in CI.
fn existing_drive_roots(candidates: &[PathBuf], exists: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let roots: Vec<PathBuf> = candidates
        .iter()
        .filter(|path| exists(path))
        .cloned()
        .collect();
    if !roots.is_empty() {
        return roots;
    }
    if cfg!(windows) {
        let system = std::env::var("SystemDrive")
            .ok()
            .filter(|drive| !drive.is_empty())
            .unwrap_or_else(|| "C:".to_string());
        vec![PathBuf::from(format!("{system}\\"))]
    } else {
        vec![PathBuf::from("/")]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::FileRow;

    fn file(id: i64, path: &str, kind: &str) -> FileRow {
        FileRow {
            id,
            path: path.to_string(),
            kind: kind.to_string(),
            extension: String::new(),
            size: 0,
            modified: None,
            has_text: false,
            has_faces: false,
        }
    }

    fn app_with_files(n: i64) -> App {
        let mut app = App::new("db".into());
        app.data.files = (0..n)
            .map(|i| file(i, &format!("/x/file{i}.jpg"), "image"))
            .collect();
        app.loading = false;
        app
    }

    #[test]
    fn tab_cycle_wraps_both_ways() {
        assert_eq!(Tab::Settings.next(), Tab::Library);
        assert_eq!(Tab::Library.prev(), Tab::Settings);
        assert_eq!(Tab::Library.next(), Tab::People);
    }

    #[test]
    fn number_keys_jump_to_tab() {
        let mut app = app_with_files(0);
        app.on_key(KeyCode::Char('3'), KeyModifiers::NONE);
        assert_eq!(app.tab, Tab::Cleanup);
        app.on_key(KeyCode::Char('1'), KeyModifiers::NONE);
        assert_eq!(app.tab, Tab::Library);
    }

    #[test]
    fn selection_clamps_at_both_ends() {
        let mut app = app_with_files(3);
        // up at the top stays at 0
        app.select_prev();
        assert_eq!(app.cursor(), 0);
        // down past the end clamps at len-1
        for _ in 0..10 {
            app.select_next();
        }
        assert_eq!(app.cursor(), 2);
    }

    #[test]
    fn empty_list_cursor_is_zero() {
        let app = app_with_files(0);
        assert_eq!(app.cursor(), 0);
        assert_eq!(app.list_len(), 0);
    }

    /// Empty-scratch start: every tab's list (and the cursor math behind it) is
    /// empty, so navigation keys must clamp at 0 and never index out of range —
    /// `cursor()`/`select_*` use `is_empty`/`saturating_sub`, not `len - 1`.
    #[test]
    fn navigation_on_every_empty_tab_is_panic_free() {
        for tab in Tab::ALL {
            let mut app = app_with_files(0); // no files, no people/dupes/plan
            app.switch_tab(tab);
            // A pre-existing stale cursor (e.g. left over from a populated load)
            // must not let any of these index past the now-empty list.
            app.selected[tab.index()] = 999;
            app.select_next();
            app.select_prev();
            app.select_first();
            app.select_last();
            assert_eq!(app.cursor(), 0, "{tab:?}: empty-tab cursor must clamp to 0");
            assert_eq!(app.list_len(), 0, "{tab:?}: list must be empty");
        }
    }

    #[test]
    fn search_filters_and_resets_cursor() {
        let mut app = app_with_files(0);
        app.data.files = vec![
            file(1, "/x/cat.jpg", "image"),
            file(2, "/x/dog.png", "image"),
            file(3, "/x/cat-notes.txt", "doc"),
        ];
        app.select_last();
        assert_eq!(app.cursor(), 2);

        // enter search mode and type "cat"
        app.on_key(KeyCode::Char('/'), KeyModifiers::NONE);
        assert!(app.search_active);
        for ch in "cat".chars() {
            app.on_key(KeyCode::Char(ch), KeyModifiers::NONE);
        }
        let visible = app.visible_files();
        assert_eq!(visible.len(), 2);
        assert_eq!(app.cursor(), 0); // reset on each keystroke

        // escape clears the filter
        app.on_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.search_active);
        assert_eq!(app.visible_files().len(), 3);
    }

    /// Ctrl+C is the terminal's universal abort, so it must work from the modal
    /// overlays too — they dispatch on `KeyCode` alone, so it used to be
    /// swallowed there and typed in as a bare "c".
    #[test]
    fn ctrl_c_quits_from_every_context() {
        for enter_context in [
            (|app: &mut App| {
                app.on_key(KeyCode::Char('/'), KeyModifiers::NONE);
            }) as fn(&mut App),
            |app: &mut App| {
                app.on_key(KeyCode::Char('?'), KeyModifiers::NONE);
            },
            |app: &mut App| {
                app.switch_tab(Tab::People);
            },
            |_app: &mut App| {},
        ] {
            let mut app = app_with_files(3);
            enter_context(&mut app);
            app.on_key(KeyCode::Char('c'), KeyModifiers::CONTROL);
            assert!(app.should_quit, "Ctrl+C must quit from this context");
        }
    }

    /// The empty-results panel advertises `Esc  clear the search`; before the
    /// guarded arm that honours it, Esc fell through to the quit arm and closed
    /// the app instead of doing what the screen said.
    #[test]
    fn esc_clears_a_committed_search_before_it_quits() {
        let mut app = app_with_files(3);
        app.on_key(KeyCode::Char('/'), KeyModifiers::NONE);
        for ch in "zzzz".chars() {
            app.on_key(KeyCode::Char(ch), KeyModifiers::NONE);
        }
        app.on_key(KeyCode::Enter, KeyModifiers::NONE); // commit; term is retained
        assert!(!app.search_active);
        assert_eq!(app.search, "zzzz");

        app.on_key(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(
            app.search, "",
            "Esc must clear the term the panel points at"
        );
        assert!(
            !app.should_quit,
            "Esc must not quit while a search is showing"
        );

        // With nothing left to clear, Esc reverts to quitting.
        app.on_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.should_quit);
    }

    /// A control chord must never land in a text field as its bare letter.
    /// Ctrl+U previously appended "u" to the query.
    #[test]
    fn control_chords_are_not_typed_into_the_search_box() {
        let mut app = app_with_files(3);
        app.on_key(KeyCode::Char('/'), KeyModifiers::NONE);
        for ch in "cat".chars() {
            app.on_key(KeyCode::Char(ch), KeyModifiers::NONE);
        }
        for chord in ['u', 'w', 'a', 'd'] {
            app.on_key(KeyCode::Char(chord), KeyModifiers::CONTROL);
        }
        assert_eq!(app.search, "cat", "control chords must not edit the query");
        assert!(!app.should_quit);
    }

    /// AltGr reports as CONTROL|ALT on Windows and types real characters on
    /// European layouts, so it must NOT be treated as a control chord.
    #[test]
    fn altgr_still_types_into_the_search_box() {
        let mut app = app_with_files(3);
        app.on_key(KeyCode::Char('/'), KeyModifiers::NONE);
        app.on_key(
            KeyCode::Char('e'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        );
        assert_eq!(app.search, "e", "AltGr text must survive");
        assert!(!app.should_quit, "AltGr+C must not quit");
    }

    #[test]
    fn slash_only_enters_search_on_library() {
        let mut app = app_with_files(0);
        app.switch_tab(Tab::People);
        app.on_key(KeyCode::Char('/'), KeyModifiers::NONE);
        assert!(!app.search_active);
    }

    #[test]
    fn committed_search_requests_a_database_reload() {
        let mut app = app_with_files(3);
        app.on_key(KeyCode::Char('/'), KeyModifiers::NONE);
        for ch in "invoice".chars() {
            app.on_key(KeyCode::Char(ch), KeyModifiers::NONE);
        }
        app.on_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(!app.search_active);
        assert!(app.loading);
        assert!(app.reload_requested);
        assert_eq!(app.search, "invoice");
    }

    #[test]
    fn q_and_ctrl_c_quit() {
        let mut app = app_with_files(0);
        app.on_key(KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(app.should_quit);

        let mut app2 = app_with_files(0);
        app2.on_key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(app2.should_quit);
    }

    #[test]
    fn reload_sets_request_flag() {
        let mut app = app_with_files(0);
        app.on_key(KeyCode::Char('r'), KeyModifiers::NONE);
        assert!(app.reload_requested);
        assert!(app.loading);
    }

    /// A reload, scan-browser, or typed-path request must be IGNORED while a model
    /// download is in flight. Otherwise `data::load` would send a terminal `Done`
    /// that `apply_load` folds in — clearing the `downloading` gauge mid-install
    /// and inviting a second `D` that double-spawns the same download (writing the
    /// same model files concurrently).
    #[test]
    fn reload_and_scan_are_ignored_while_downloading() {
        let mut app = app_with_files(0);
        app.downloading = true;

        app.on_key(KeyCode::Char('r'), KeyModifiers::NONE);
        assert!(!app.reload_requested, "reload must be ignored mid-download");

        app.on_key(KeyCode::Char('s'), KeyModifiers::NONE);
        assert!(
            app.browser.is_none(),
            "scan browser must not open mid-download"
        );

        app.on_key(KeyCode::Char('t'), KeyModifiers::NONE);
        assert!(
            !app.input_active,
            "typed-path prompt must not open mid-download"
        );

        // The download is left intact for the worker to finish.
        assert!(app.downloading);
    }

    /// While the `?` help overlay is open, Esc and q dismiss it instead of quitting
    /// the app — a user reading the keys can back out without losing their session.
    #[test]
    fn esc_and_q_close_help_without_quitting() {
        let mut app = app_with_files(0);
        app.on_key(KeyCode::Char('?'), KeyModifiers::NONE);
        assert!(app.show_help);
        app.on_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.show_help, "Esc closes the help overlay");
        assert!(!app.should_quit, "Esc must NOT quit while help is open");

        app.on_key(KeyCode::Char('?'), KeyModifiers::NONE);
        assert!(app.show_help);
        app.on_key(KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(!app.show_help, "q closes the help overlay");
        assert!(!app.should_quit, "q must NOT quit while help is open");

        // With help closed, q quits as normal.
        app.on_key(KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(app.should_quit);
    }

    #[test]
    fn apply_done_loads_and_clamps() {
        let mut app = app_with_files(0);
        app.switch_tab(Tab::People);
        app.selected[Tab::People.index()] = 99;
        // empty people -> cursor must clamp to 0
        let snap = Snapshot {
            db_exists: true,
            people: vec![],
            ..Snapshot::default()
        };
        app.apply_load(LoadMsg::Done(Box::new(snap)));
        assert!(!app.loading);
        assert_eq!(app.cursor(), 0);
    }

    #[test]
    fn partial_scan_warning_survives_reload_and_duplicate_verification() {
        let mut app = app_with_files(0);
        let warning = "Scan partial: 9/10 files, 1 failed".to_string();
        app.apply_load(LoadMsg::ScanPartial(warning.clone()));
        app.apply_load(LoadMsg::Status("Reading files…".into()));
        app.apply_load(LoadMsg::Done(Box::default()));
        app.apply_load(LoadMsg::Status(
            "Duplicates verified · 0 duplicate groups".into(),
        ));
        app.apply_load(LoadMsg::Dupes(Box::default()));

        assert_eq!(app.status, warning);
        assert!(app.status_error);
        assert!(app.pending_scan_warning.is_none());
    }

    #[test]
    fn stale_versioned_completion_cannot_replace_newer_snapshot() {
        let mut app = app_with_files(0);
        app.load_generation = 2;
        let newer = Snapshot {
            db_exists: true,
            query: "newer".into(),
            total_files: 2,
            ..Snapshot::default()
        };
        app.apply_load(LoadMsg::Versioned {
            generation: 2,
            message: Box::new(LoadMsg::Done(Box::new(newer))),
        });
        let stale = Snapshot {
            db_exists: true,
            query: "stale".into(),
            total_files: 1,
            ..Snapshot::default()
        };
        app.apply_load(LoadMsg::Versioned {
            generation: 1,
            message: Box::new(LoadMsg::Done(Box::new(stale))),
        });
        assert_eq!(app.data.query, "newer");
        assert_eq!(app.data.total_files, 2);
    }

    #[test]
    fn expand_tilde_resolves_home() {
        let home = Path::new("/home/u");
        assert_eq!(expand_tilde_with("~", Some(home)), PathBuf::from("/home/u"));
        assert_eq!(
            expand_tilde_with("~/Pictures", Some(home)),
            PathBuf::from("/home/u/Pictures")
        );
        // No home → left verbatim (no panic).
        assert_eq!(expand_tilde_with("~/x", None), PathBuf::from("~/x"));
        // A literal `~foo` (not `~/`) is not tilde-expansion; left as-is.
        assert_eq!(expand_tilde_with("~foo", Some(home)), PathBuf::from("~foo"));
        // Absolute paths pass through untouched.
        assert_eq!(
            expand_tilde_with("/var/data", Some(home)),
            PathBuf::from("/var/data")
        );
    }

    /// A throwaway temp directory containing two subdirs + a file (the file must
    /// NOT appear in the browser, which is dirs-only).
    fn temp_tree(tag: &str) -> PathBuf {
        let mut base = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        base.push(format!("fileid-tui-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(base.join("alpha")).unwrap();
        std::fs::create_dir_all(base.join("beta")).unwrap();
        std::fs::write(base.join("zeta.txt"), "not a dir").unwrap();
        base
    }

    #[test]
    fn s_opens_folder_browser_not_a_text_prompt() {
        let mut app = app_with_files(0);
        app.on_key(KeyCode::Char('s'), KeyModifiers::NONE);
        assert!(app.browser.is_some(), "s must open the folder browser");
        assert!(!app.input_active, "s must NOT open the raw text prompt");
        // Esc cancels without arming a scan.
        app.on_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.browser.is_none());
        assert!(app.scan_requested.is_none());
        assert!(!app.scanning);
    }

    #[test]
    fn browser_lists_only_subdirs_sorted_with_parent_row() {
        let base = temp_tree("browse-list");
        let b = Browser::open(base.clone());
        // First row is the `..` affordance (base has a parent).
        assert_eq!(b.rows.first(), Some(&BrowseRow::Parent));
        // Then the two subdirs, sorted, and crucially NOT the file.
        let dirs: Vec<String> = b
            .rows
            .iter()
            .filter_map(|r| match r {
                BrowseRow::Dir(p) => Some(dir_label(p)),
                BrowseRow::Parent => None,
            })
            .collect();
        assert_eq!(dirs, vec!["alpha".to_string(), "beta".to_string()]);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn browser_enter_descends_and_backspace_returns() {
        let base = temp_tree("browse-nav");
        let mut app = app_with_files(0);
        app.browser = Some(Browser::open(base.clone()));
        // Row 0 is `..`; row 1 is the first subdir (alpha). Highlight it + Enter.
        app.on_key(KeyCode::Down, KeyModifiers::NONE);
        app.on_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.browser.as_ref().unwrap().cwd, base.join("alpha"));
        // Backspace walks back up to base.
        app.on_key(KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(app.browser.as_ref().unwrap().cwd, base);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn browser_s_arms_scan_of_current_folder() {
        let base = temp_tree("browse-scan");
        let mut app = app_with_files(0);
        app.browser = Some(Browser::open(base.clone()));
        app.on_key(KeyCode::Char('s'), KeyModifiers::NONE);
        assert!(app.browser.is_none(), "scanning closes the browser");
        assert!(app.scanning);
        assert!(app.loading);
        assert_eq!(app.scan_requested.as_deref(), Some(base.as_path()));
        assert!(app.scan_root.is_some());
        // A terminal load message clears `scanning` (post-scan reload).
        app.apply_load(LoadMsg::Done(Box::new(Snapshot {
            db_exists: true,
            ..Snapshot::default()
        })));
        assert!(!app.scanning);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn browser_missing_or_unreadable_dir_is_panic_free() {
        // A non-existent path must not panic: it yields just the `..` row (the
        // path has a parent) and a notice.
        let b = Browser::open(PathBuf::from("/no/such/place-xyz-12345"));
        assert!(b.rows.iter().all(|r| matches!(r, BrowseRow::Parent)));
        assert!(b.notice.is_some(), "unreadable dir surfaces a notice");
    }

    #[test]
    fn browser_t_falls_back_to_typed_prompt_then_confirms() {
        let mut app = app_with_files(0);
        let dir = std::env::current_dir().expect("cwd");
        app.on_key(KeyCode::Char('s'), KeyModifiers::NONE); // open browser
        app.on_key(KeyCode::Char('t'), KeyModifiers::NONE); // typed fallback
        assert!(app.browser.is_none());
        assert!(app.input_active, "t opens the typed-path prompt");
        for ch in dir.to_string_lossy().chars() {
            app.on_key(KeyCode::Char(ch), KeyModifiers::NONE);
        }
        // Tab confirms (not just Enter); a real dir arms a scan.
        app.on_key(KeyCode::Tab, KeyModifiers::NONE);
        assert!(!app.input_active);
        assert!(app.scanning);
        assert!(app.scan_requested.is_some());
        assert!(app.scan_root.is_some());
    }

    #[test]
    fn typed_fallback_bad_path_errors_inline_and_keeps_prompt_open() {
        let mut app = app_with_files(0);
        app.on_key(KeyCode::Char('s'), KeyModifiers::NONE);
        app.on_key(KeyCode::Char('t'), KeyModifiers::NONE);
        for ch in "/no/such/dir/here-xyz".chars() {
            app.on_key(KeyCode::Char(ch), KeyModifiers::NONE);
        }
        app.on_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.input_active, "prompt stays open on a bad path");
        assert!(app.input_error.is_some());
        assert!(app.scan_requested.is_none());
        assert!(!app.scanning);
    }

    #[test]
    fn s_ignored_while_scanning() {
        let mut app = app_with_files(0);
        app.scanning = true;
        app.on_key(KeyCode::Char('s'), KeyModifiers::NONE);
        assert!(app.browser.is_none(), "must not open the browser mid-scan");
        assert!(!app.input_active);
    }

    /// A throwaway tree with KNOWN contents so the FIX-2 counts are exact:
    /// `base/{photo.jpg, notes.txt, docs/, sub/{a.jpg,b.png,c.txt,nested/}}`.
    fn count_tree(tag: &str) -> PathBuf {
        let mut base = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        base.push(format!("fileid-tui-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(base.join("docs")).unwrap();
        std::fs::create_dir_all(base.join("sub").join("nested")).unwrap();
        std::fs::write(base.join("photo.jpg"), "img").unwrap();
        std::fs::write(base.join("notes.txt"), "txt").unwrap();
        std::fs::write(base.join("sub").join("a.jpg"), "img").unwrap();
        std::fs::write(base.join("sub").join("b.png"), "img").unwrap();
        std::fs::write(base.join("sub").join("c.txt"), "txt").unwrap();
        base
    }

    #[test]
    fn browser_counts_cwd_and_subdirs_and_lists_files() {
        let base = count_tree("counts");
        let b = Browser::open(base.clone());

        // cwd's own counts (browser title): 1 image, 2 files, 2 dirs.
        assert_eq!(
            b.here,
            DirCounts {
                images: 1,
                files: 2,
                dirs: 2,
                capped: false
            }
        );
        assert_eq!(b.files_total, 2);

        // The dimmed file preview lists the actual files a scan would pick up,
        // sorted, with the image flagged.
        let names: Vec<&str> = b.files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["notes.txt", "photo.jpg"]);
        let photo = b.files.iter().find(|f| f.name == "photo.jpg").unwrap();
        assert!(photo.is_image, "photo.jpg must be flagged an image");
        assert!(
            !b.files
                .iter()
                .find(|f| f.name == "notes.txt")
                .unwrap()
                .is_image
        );

        // Lazy per-subdir count: sub/ has 2 images, 3 files, 1 subfolder.
        let sub = base.join("sub");
        assert_eq!(
            b.count_for(&sub),
            Some(DirCounts {
                images: 2,
                files: 3,
                dirs: 1,
                capped: false
            })
        );
        // docs/ is empty.
        assert_eq!(b.count_for(&base.join("docs")), Some(DirCounts::default()));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn browser_count_is_cached_and_bounded_on_huge_dirs() {
        let mut base = std::env::temp_dir();
        base.push(format!("fileid-tui-huge-{}", std::process::id()));
        let huge = base.join("huge");
        std::fs::create_dir_all(&huge).unwrap();
        for i in 0..(COUNT_CAP + 5) {
            std::fs::write(huge.join(format!("f{i}.bin")), "x").unwrap();
        }
        let b = Browser::open(base.clone());
        let counts = b.count_for(&huge).expect("readable dir counts");
        assert!(counts.capped, "a >COUNT_CAP folder must report capped");
        assert_eq!(
            counts.dirs + counts.files,
            COUNT_CAP,
            "walk stops at the cap"
        );
        // Second call is served from cache (same result, no re-walk).
        assert_eq!(b.count_for(&huge), Some(counts));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn browser_keeps_directories_after_file_count_cap() {
        let mut base = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        base.push(format!("fileid-tui-nav-cap-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        for i in 0..(COUNT_CAP + 5) {
            std::fs::write(base.join(format!("f{i:04}.bin")), "x").unwrap();
        }
        let wanted = base.join("z-navigable");
        std::fs::create_dir_all(&wanted).unwrap();

        let browser = Browser::open(base.clone());
        assert!(browser.here.capped);
        assert!(browser
            .rows
            .iter()
            .any(|row| matches!(row, BrowseRow::Dir(path) if path == &wanted)));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn browser_count_of_unreadable_dir_is_none_not_a_panic() {
        let b = Browser::open(std::env::temp_dir());
        // A path that does not exist counts to None (rendered without counts),
        // and the miss is cached.
        assert_eq!(b.count_for(Path::new("/no/such/dir-xyz-98765")), None);
        assert_eq!(b.count_for(Path::new("/no/such/dir-xyz-98765")), None);
    }

    /// A tree with one visible + one hidden subdir AND one visible + one hidden
    /// file, so the default-hidden filter (FEATURE 2) has something to drop.
    fn dotfile_tree(tag: &str) -> PathBuf {
        let mut base = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        base.push(format!("fileid-tui-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(base.join("Shown")).unwrap();
        std::fs::create_dir_all(base.join(".hidden_dir")).unwrap();
        std::fs::write(base.join("shown.txt"), "x").unwrap();
        std::fs::write(base.join(".secret"), "x").unwrap();
        base
    }

    #[test]
    fn browser_hides_dotfiles_by_default_with_consistent_counts() {
        let base = dotfile_tree("dot-default");
        let b = Browser::open(base.clone());
        assert!(!b.show_hidden, "hidden entries are off by default");
        // No dot-prefixed subdir in the rows.
        let has_hidden_dir = b
            .rows
            .iter()
            .any(|r| matches!(r, BrowseRow::Dir(p) if dir_label(p).starts_with('.')));
        assert!(!has_hidden_dir, "hidden subdir must be filtered by default");
        // No dot-prefixed file in the preview.
        assert!(
            b.files.iter().all(|f| !f.name.starts_with('.')),
            "hidden file must be filtered"
        );
        // Counts AGREE with what's shown: the hidden pair is dropped → 1 file, 1 dir.
        assert_eq!(
            b.here,
            DirCounts {
                images: 0,
                files: 1,
                dirs: 1,
                capped: false
            }
        );
        assert_eq!(b.files_total, 1);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn browser_dot_key_toggles_hidden_and_counts_follow() {
        let base = dotfile_tree("dot-toggle");
        let mut app = app_with_files(0);
        app.browser = Some(Browser::open(base.clone()));
        // Press `.` to reveal hidden entries.
        app.on_key(KeyCode::Char('.'), KeyModifiers::NONE);
        let b = app.browser.as_ref().unwrap();
        assert!(b.show_hidden, ". must toggle hidden on");
        let has_hidden_dir = b
            .rows
            .iter()
            .any(|r| matches!(r, BrowseRow::Dir(p) if dir_label(p) == ".hidden_dir"));
        assert!(has_hidden_dir, "hidden subdir appears once toggled on");
        assert!(
            b.files.iter().any(|f| f.name == ".secret"),
            "hidden file appears once toggled on"
        );
        // Counts now include the hidden pair: 2 files, 2 dirs.
        assert_eq!(
            b.here,
            DirCounts {
                images: 0,
                files: 2,
                dirs: 2,
                capped: false
            }
        );
        assert_eq!(b.files_total, 2);
        // Press `.` again to hide them — counts revert.
        app.on_key(KeyCode::Char('.'), KeyModifiers::NONE);
        let b = app.browser.as_ref().unwrap();
        assert!(!b.show_hidden);
        assert_eq!(
            b.here,
            DirCounts {
                images: 0,
                files: 1,
                dirs: 1,
                capped: false
            }
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn browser_per_subdir_count_respects_hidden_filter() {
        // A subdir holding a visible file and a hidden file: its shallow count
        // must match the default hidden-off state (counts consistent w/ shown).
        let base = dotfile_tree("dot-subcount");
        let sub = base.join("Shown");
        std::fs::write(sub.join("visible.txt"), "x").unwrap();
        std::fs::write(sub.join(".dot"), "x").unwrap();
        let b = Browser::open(base.clone());
        assert_eq!(
            b.count_for(&sub),
            Some(DirCounts {
                images: 0,
                files: 1,
                dirs: 0,
                capped: false
            }),
            "the hidden .dot file must not be counted by default",
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn drive_roots_resolve_without_panicking() {
        let roots = drive_roots();
        assert!(!roots.is_empty(), "the drive picker always has a fallback");
        assert!(
            roots.iter().all(|root| root.is_absolute()),
            "every drive-picker row must be an absolute path: {roots:?}"
        );
    }

    #[test]
    fn existing_drive_roots_keeps_every_available_mount() {
        let a = PathBuf::from("/aaa-xyz");
        let b = PathBuf::from("/bbb-xyz");
        assert_eq!(
            existing_drive_roots(&[a.clone(), b.clone()], |_| true),
            vec![a.clone(), b.clone()],
            "the picker must not collapse multiple drives to the first one"
        );
        let fallback = existing_drive_roots(std::slice::from_ref(&a), |_| false);
        assert_eq!(fallback.len(), 1);
        assert!(fallback[0].is_absolute());
    }

    #[test]
    fn browser_d_key_jumps_to_drives_panic_free() {
        let mut app = app_with_files(0);
        app.browser = Some(Browser::open(std::env::temp_dir()));
        app.on_key(KeyCode::Char('d'), KeyModifiers::NONE);
        let b = app.browser.as_ref().unwrap();
        assert!(b.virtual_drives, "d opens the virtual drive selector");
        assert_eq!(
            b.rows,
            drive_roots()
                .into_iter()
                .map(BrowseRow::Dir)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn virtual_drive_selector_cannot_be_scanned_as_a_fake_folder() {
        let mut app = app_with_files(0);
        app.browser = Some(Browser::open(std::env::temp_dir()));
        app.on_key(KeyCode::Char('d'), KeyModifiers::NONE);
        app.on_key(KeyCode::Char('s'), KeyModifiers::NONE);
        assert!(app.browser.is_some(), "the selector stays open");
        assert!(
            app.scan_requested.is_none(),
            "no fake Drives path is scanned"
        );
        assert!(!app.scanning);
        assert!(app
            .browser
            .as_ref()
            .and_then(|browser| browser.notice.as_deref())
            .is_some_and(|notice| notice.contains("Open a drive")));
    }

    #[test]
    fn settings_capital_d_requests_model_download() {
        let mut app = app_with_files(0);
        app.tab = Tab::Settings;
        app.on_key(KeyCode::Char('D'), KeyModifiers::SHIFT);
        assert!(
            app.download_requested,
            "D on Settings arms a model download"
        );
        assert!(app.downloading);
        assert!(app.loading);
        // The gauge is armed at 0% the same frame, before any PROGRESS line.
        assert_eq!(app.download.as_ref().map(|d| d.percent), Some(0));
        // A terminal load message (e.g. a `fileid`-not-found error) clears
        // `downloading` AND the gauge — no panic, q still quits.
        app.apply_load(LoadMsg::Error("`fileid` not found".into()));
        assert!(!app.downloading);
        assert!(!app.loading);
        assert!(
            app.download.is_none(),
            "an errored download clears the gauge"
        );
        assert!(
            app.status_error,
            "the failure marks the status row as an error"
        );
    }

    /// The porcelain gauge state: `DownloadProgress` advances the bar + label,
    /// the final 100/done flips `done`, and the worker's terminal `Done` clears
    /// the gauge — the exact lifecycle `ui.rs` renders.
    #[test]
    fn download_progress_drives_and_clears_the_gauge() {
        let mut app = app_with_files(0);
        app.on_key(KeyCode::Char('D'), KeyModifiers::SHIFT); // arm at 0%
        assert_eq!(app.download.as_ref().unwrap().percent, 0);

        let label = "arcface · 182/271 MB · 3.4 MB/s · model 2/9";
        app.apply_load(LoadMsg::DownloadProgress {
            percent: 62,
            label: label.to_string(),
        });
        let d = app.download.as_ref().expect("gauge present mid-install");
        assert_eq!(d.percent, 62);
        assert_eq!(d.label, label);
        assert!(!d.done, "62% is not done");

        // The final PROGRESS line flips the gauge into its brief success state.
        app.apply_load(LoadMsg::DownloadProgress {
            percent: 100,
            label: "done".to_string(),
        });
        assert!(
            app.download.as_ref().unwrap().done,
            "100% marks the gauge done"
        );

        // The worker's terminal Done clears the gauge entirely.
        app.apply_load(LoadMsg::Done(Box::new(Snapshot {
            db_exists: true,
            ..Snapshot::default()
        })));
        assert!(app.download.is_none(), "the gauge clears on completion");
        assert!(!app.downloading);
    }

    /// An out-of-range percent is clamped so the gauge widget (which panics above
    /// 100) is never handed a bad value, even if the reducer is fed one directly.
    #[test]
    fn download_progress_clamps_percent_to_100() {
        let mut app = app_with_files(0);
        app.apply_load(LoadMsg::DownloadProgress {
            percent: 150,
            label: "x".to_string(),
        });
        assert_eq!(app.download.as_ref().unwrap().percent, 100);
        assert!(app.download.as_ref().unwrap().done);
    }

    /// `D` is GLOBAL: it arms a model download from a non-Settings tab too, so a
    /// fresh install can trigger it from the welcome screen / any tab's banner.
    #[test]
    fn capital_d_requests_download_from_any_tab() {
        for tab in [
            Tab::Library,
            Tab::People,
            Tab::Cleanup,
            Tab::DeepAnalyze,
            Tab::Restructure,
        ] {
            let mut app = app_with_files(0);
            app.switch_tab(tab);
            app.on_key(KeyCode::Char('D'), KeyModifiers::SHIFT);
            assert!(
                app.download_requested,
                "{tab:?}: D must arm a download from any tab"
            );
            assert!(app.downloading, "{tab:?}: D must mark a download in flight");
        }
    }

    /// The Tab key cycles through ALL six tabs — Settings included — and wraps
    /// from the last tab back to the first (and BackTab the reverse), so Settings
    /// is always reachable by tabbing.
    #[test]
    fn tab_key_cycles_through_settings_and_wraps() {
        let mut app = app_with_files(0); // starts on Library
        let order = [
            Tab::People,
            Tab::Cleanup,
            Tab::DeepAnalyze,
            Tab::Restructure,
            Tab::Settings,
            Tab::Library,
        ];
        for expected in order {
            app.on_key(KeyCode::Tab, KeyModifiers::NONE);
            assert_eq!(app.tab, expected, "Tab must advance to {expected:?}");
        }
        // From Library, BackTab wraps backwards straight to Settings.
        app.on_key(KeyCode::BackTab, KeyModifiers::NONE);
        assert_eq!(
            app.tab,
            Tab::Settings,
            "BackTab from the first tab must wrap to Settings"
        );
        // The number key jumps straight to Settings (index 5 → '6').
        app.on_key(KeyCode::Char('1'), KeyModifiers::NONE);
        assert_eq!(app.tab, Tab::Library);
        app.on_key(KeyCode::Char('6'), KeyModifiers::NONE);
        assert_eq!(app.tab, Tab::Settings, "6 must jump to Settings");
    }
}
