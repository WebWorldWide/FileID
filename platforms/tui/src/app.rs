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

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyModifiers};

use crate::data::{FileRow, LoadMsg, Snapshot};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Library,
    People,
    Cleanup,
    Restructure,
    Settings,
}

impl Tab {
    pub const ALL: [Tab; 5] =
        [Tab::Library, Tab::People, Tab::Cleanup, Tab::Restructure, Tab::Settings];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Library => "Library",
            Tab::People => "People",
            Tab::Cleanup => "Cleanup",
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

pub struct App {
    pub tab: Tab,
    pub data: Snapshot,
    /// Per-tab cursor position (indexed by `Tab::index`).
    pub selected: [usize; 5],
    pub search: String,
    pub search_active: bool,
    pub status: String,
    pub loading: bool,
    pub show_help: bool,
    pub should_quit: bool,
    /// Set when the user asks for a reload; `main` consumes it and re-spawns
    /// the loader, then clears it.
    pub reload_requested: bool,
    pub db_label: String,

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
}

impl App {
    pub fn new(db_label: String) -> Self {
        Self {
            tab: Tab::Library,
            data: Snapshot::default(),
            selected: [0; 5],
            search: String::new(),
            search_active: false,
            status: "Starting…".to_string(),
            loading: true,
            show_help: false,
            should_quit: false,
            reload_requested: false,
            db_label,
            input_active: false,
            input: String::new(),
            input_error: None,
            scan_root: None,
            scanning: false,
            scan_requested: None,
            browser: None,
        }
    }

    /// Files visible under the current search filter (Library tab).
    pub fn visible_files(&self) -> Vec<&FileRow> {
        if self.search.is_empty() {
            self.data.files.iter().collect()
        } else {
            let q = self.search.to_lowercase();
            self.data
                .files
                .iter()
                .filter(|f| f.path.to_lowercase().contains(&q) || f.kind.to_lowercase().contains(&q))
                .collect()
        }
    }

    /// Length of the list backing the active tab.
    pub fn list_len(&self) -> usize {
        match self.tab {
            Tab::Library => self.visible_files().len(),
            Tab::People => self.data.people.len(),
            Tab::Cleanup => self.data.dupes.len(),
            Tab::Restructure => self.data.plan.len(),
            Tab::Settings => 0,
        }
    }

    /// Cursor index for the active tab, clamped to the current list length.
    pub fn cursor(&self) -> usize {
        let len = self.list_len();
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
            let c = (self.cursor() + 1).min(len - 1);
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
            LoadMsg::Status(s) => self.status = s,
            LoadMsg::Done(snap) => {
                self.data = *snap;
                self.loading = false;
                self.scanning = false;
                // Re-clamp every cursor against the freshly loaded lengths.
                self.clamp_all();
            }
            LoadMsg::Error(e) => {
                self.status = e;
                self.loading = false;
                self.scanning = false;
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
        match code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => self.should_quit = true,
            KeyCode::Tab => self.switch_tab(self.tab.next()),
            KeyCode::BackTab => self.switch_tab(self.tab.prev()),
            KeyCode::Char(d @ '1'..='5') => {
                let i = (d as u8 - b'1') as usize;
                self.switch_tab(Tab::from_index(i));
            }
            KeyCode::Down | KeyCode::Char('j') => self.select_next(),
            KeyCode::Up | KeyCode::Char('k') => self.select_prev(),
            KeyCode::Home | KeyCode::Char('g') => self.select_first(),
            KeyCode::End | KeyCode::Char('G') => self.select_last(),
            KeyCode::Char('s') => self.open_browser(),
            KeyCode::Char('r') => {
                self.loading = true;
                self.status = "Reloading…".to_string();
                self.reload_requested = true;
            }
            KeyCode::Char('?') => self.show_help = !self.show_help,
            KeyCode::Char('/') if self.tab == Tab::Library => {
                self.search_active = true;
                self.show_help = false;
            }
            _ => {}
        }
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
                if let Some(b) = self.browser.take() {
                    self.arm_scan(b.cwd);
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
            }
            KeyCode::Enter => self.search_active = false,
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
}

impl Browser {
    /// Open a browser rooted at `start`. Never panics — an unreadable root just
    /// shows an empty list with a notice.
    pub fn open(start: PathBuf) -> Browser {
        let mut b = Browser { cwd: start, rows: Vec::new(), selected: 0, notice: None };
        b.refresh();
        b
    }

    /// Re-read `cwd`'s immediate subdirectories into `rows` (dirs only, sorted
    /// case-insensitively), prefixed by `..` when `cwd` has a parent. An
    /// unreadable directory leaves the list empty and sets `notice`; never
    /// panics. `file_type()` is preferred so we don't follow symlinks into
    /// loops; unreadable individual entries are simply skipped.
    fn refresh(&mut self) {
        let mut dirs: Vec<PathBuf> = Vec::new();
        match std::fs::read_dir(&self.cwd) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let is_dir = entry
                        .file_type()
                        .map(|t| t.is_dir())
                        .unwrap_or_else(|_| entry.path().is_dir());
                    if is_dir {
                        dirs.push(entry.path());
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

        let mut rows = Vec::with_capacity(dirs.len() + 1);
        if self.cwd.parent().is_some() {
            rows.push(BrowseRow::Parent);
        }
        rows.extend(dirs.into_iter().map(BrowseRow::Dir));
        self.rows = rows;
        if self.selected >= self.rows.len() {
            self.selected = self.rows.len().saturating_sub(1);
        }
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
                    self.cwd = path;
                    self.selected = 0;
                    self.refresh();
                } else {
                    self.notice = Some(format!("Can't open {} (permission denied).", dir_label(&path)));
                }
            }
            None => {}
        }
    }

    /// Go up to the parent directory (no-op at the filesystem root).
    fn go_up(&mut self) {
        if let Some(parent) = self.cwd.parent().map(Path::to_path_buf) {
            self.cwd = parent;
            self.selected = 0;
            self.refresh();
        }
    }
}

/// Case-insensitive sort key for a directory (its final segment).
fn dir_key(p: &Path) -> String {
    p.file_name().unwrap_or(p.as_os_str()).to_string_lossy().to_lowercase()
}

/// The directory's display label (final segment, or the whole path if none).
pub fn dir_label(p: &Path) -> String {
    p.file_name().map_or_else(|| p.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned())
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
    } else if let Some(rest) = input.strip_prefix("~/").or_else(|| input.strip_prefix("~\\")) {
        if let Some(h) = home {
            return h.join(rest);
        }
    }
    PathBuf::from(input)
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
        app.data.files = (0..n).map(|i| file(i, &format!("/x/file{i}.jpg"), "image")).collect();
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

    #[test]
    fn slash_only_enters_search_on_library() {
        let mut app = app_with_files(0);
        app.switch_tab(Tab::People);
        app.on_key(KeyCode::Char('/'), KeyModifiers::NONE);
        assert!(!app.search_active);
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

    #[test]
    fn apply_done_loads_and_clamps() {
        let mut app = app_with_files(0);
        app.switch_tab(Tab::People);
        app.selected[Tab::People.index()] = 99;
        // empty people -> cursor must clamp to 0
        let snap = Snapshot { db_exists: true, people: vec![], ..Snapshot::default() };
        app.apply_load(LoadMsg::Done(Box::new(snap)));
        assert!(!app.loading);
        assert_eq!(app.cursor(), 0);
    }

    #[test]
    fn expand_tilde_resolves_home() {
        let home = Path::new("/home/u");
        assert_eq!(expand_tilde_with("~", Some(home)), PathBuf::from("/home/u"));
        assert_eq!(expand_tilde_with("~/Pictures", Some(home)), PathBuf::from("/home/u/Pictures"));
        // No home → left verbatim (no panic).
        assert_eq!(expand_tilde_with("~/x", None), PathBuf::from("~/x"));
        // A literal `~foo` (not `~/`) is not tilde-expansion; left as-is.
        assert_eq!(expand_tilde_with("~foo", Some(home)), PathBuf::from("~foo"));
        // Absolute paths pass through untouched.
        assert_eq!(expand_tilde_with("/var/data", Some(home)), PathBuf::from("/var/data"));
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
        app.apply_load(LoadMsg::Done(Box::new(Snapshot { db_exists: true, ..Snapshot::default() })));
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
}
