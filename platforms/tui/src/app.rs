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
            KeyCode::Char('s') => self.open_input(),
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

    /// Open the single-line folder-path prompt (the `s` key). Ignored while a
    /// scan is already running so two scans can't race the engine.
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
        self.input_error = None;
        let display = path.to_string_lossy().into_owned();
        self.status = format!("Starting scan of {display}…");
        self.scan_root = Some(display);
        self.scanning = true;
        self.loading = true;
        self.scan_requested = Some(path);
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

    #[test]
    fn s_opens_path_prompt_and_typing_builds_input() {
        let mut app = app_with_files(0);
        app.on_key(KeyCode::Char('s'), KeyModifiers::NONE);
        assert!(app.input_active);
        for ch in "/tmp".chars() {
            app.on_key(KeyCode::Char(ch), KeyModifiers::NONE);
        }
        assert_eq!(app.input, "/tmp");
        // Esc cancels without arming a scan.
        app.on_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.input_active);
        assert!(app.scan_requested.is_none());
        assert!(!app.scanning);
    }

    #[test]
    fn confirm_bad_path_errors_inline_and_keeps_prompt_open() {
        let mut app = app_with_files(0);
        app.on_key(KeyCode::Char('s'), KeyModifiers::NONE);
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
    fn confirm_real_dir_arms_scan() {
        let mut app = app_with_files(0);
        // The test process always has an existing working directory.
        let dir = std::env::current_dir().expect("cwd");
        app.on_key(KeyCode::Char('s'), KeyModifiers::NONE);
        for ch in dir.to_string_lossy().chars() {
            app.on_key(KeyCode::Char(ch), KeyModifiers::NONE);
        }
        // Tab also confirms (not just Enter).
        app.on_key(KeyCode::Tab, KeyModifiers::NONE);
        assert!(!app.input_active);
        assert!(app.scanning);
        assert!(app.loading);
        assert!(app.scan_requested.is_some(), "a real dir must arm a scan");
        assert!(app.scan_root.is_some());

        // A terminal load message (e.g. the post-scan reload) clears `scanning`.
        app.apply_load(LoadMsg::Done(Box::new(Snapshot { db_exists: true, ..Snapshot::default() })));
        assert!(!app.scanning);
    }

    #[test]
    fn s_ignored_while_scanning() {
        let mut app = app_with_files(0);
        app.scanning = true;
        app.on_key(KeyCode::Char('s'), KeyModifiers::NONE);
        assert!(!app.input_active, "must not open a second scan prompt mid-scan");
    }
}
