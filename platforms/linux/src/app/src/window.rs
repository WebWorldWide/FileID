// Main window — the Prototype v3 shell (shared/docs/DESIGN_V3.md), mirror of
// macOS `MainWindow`: an `adw::ApplicationWindow` whose content is a
// `gtk::Overlay` stack — the animated `LavaLampBackground` at the bottom, a
// muted scrim over it, and the UI on top (LavaLamp → frosting → content).
//
// The UI is the 48 px title bar (folder switcher, search or ask, activity,
// settings) over a hairline-split 232 px sidebar (Browse / Analyze / Act area
// switcher, page rows, scan card) and the page, which carries a crumb + serif
// title header. Search & Moments, Chat and File Tools are pages like macOS.

use adw::prelude::*;
use gtk::glib;
use std::cell::{Cell, OnceCell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

use crate::engine_client::{EngineClient, EngineState};
use crate::shell::page_header::{self, PageHeader};
use crate::shell::sidebar::Sidebar;
use crate::shell::status::Status;
use crate::shell::title_bar::TitleBar;
use crate::shell::{self, Action};
use fileid_engine::ipc::{CommandPayload, Empty, ScanPhase};

const LIBRARY_SEARCH_HINT: &str = "Search filenames, tags, text in images…";

#[derive(Clone)]
struct ActiveWindow {
    window: adw::ApplicationWindow,
    shell: Weak<Shell>,
}

thread_local! {
    static ACTIVE_WINDOW: RefCell<Option<ActiveWindow>> = const { RefCell::new(None) };
}

pub fn on_activate(app: &adw::Application) {
    if !present_existing(None) {
        build_window(app, None);
    }
}

pub fn on_open(app: &adw::Application, files: &[gtk::gio::File]) {
    let folder = files
        .iter()
        .filter_map(gtk::gio::File::path)
        .find(|path| path.is_dir());
    if !present_existing(folder.clone()) {
        build_window(app, folder);
    }
}

fn present_existing(folder: Option<PathBuf>) -> bool {
    let active = ACTIVE_WINDOW.with_borrow(Clone::clone);
    let Some(active) = active else { return false };
    if let (Some(path), Some(shell)) = (folder, active.shell.upgrade()) {
        shell.set_folder(&path);
    }
    active.window.present();
    true
}

struct Shell {
    window: adw::ApplicationWindow,
    engine: Rc<RefCell<EngineClient>>,
    stack: adw::ViewStack,
    split: adw::OverlaySplitView,
    title_bar: Rc<TitleBar>,
    sidebar: Rc<Sidebar>,
    headers: Vec<(&'static str, PageHeader)>,
    contents: Vec<(&'static str, gtk::Widget)>,
    status: RefCell<Status>,
    folder_path: RefCell<Option<PathBuf>>,
    note: RefCell<Option<String>>,
    current: Cell<&'static str>,
    counting: Cell<bool>,
    inspector: gtk::Revealer,
}

fn build_window(app: &adw::Application, initial_folder: Option<PathBuf>) {
    let (width, height) = std::env::var("FILEID_WIN_SIZE")
        .ok()
        .and_then(|size| {
            let mut parts = size.split('x');
            Some((
                parts.next()?.trim().parse().ok()?,
                parts.next()?.trim().parse().ok()?,
            ))
        })
        .unwrap_or((1320, 860));
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("FileID")
        .default_width(width)
        .default_height(height)
        .build();
    // The sidebar collapses below the breakpoint and the tabs reflow narrow.
    window.set_size_request(360, 320);

    let engine = Rc::new(RefCell::new(EngineClient::new()));
    let slot: Rc<OnceCell<Weak<Shell>>> = Rc::new(OnceCell::new());
    let act: Rc<dyn Fn(Action)> = {
        let slot = slot.clone();
        Rc::new(move |action| {
            if let Some(shell) = slot.get().and_then(Weak::upgrade) {
                shell.handle(action);
            }
        })
    };

    let stack = adw::ViewStack::new();
    stack.set_hexpand(true);
    stack.set_vexpand(true);
    let contents: Vec<(&'static str, gtk::Widget)> = vec![
        ("library", crate::tabs::library::build(engine.clone())),
        ("people", crate::tabs::people::build(engine.clone())),
        ("moments", crate::tabs::workbenches::catalog(engine.clone())),
        (
            "deep-analyze",
            crate::tabs::deep_analyze::build_deep_analyze_tab(engine.clone()),
        ),
        ("chat", crate::tabs::workbenches::chat(engine.clone())),
        (
            "cleanup",
            crate::tabs::cleanup::build_cleanup_tab(engine.clone()),
        ),
        (
            "restructure",
            crate::tabs::restructure::build_restructure_tab(engine.clone()),
        ),
        ("tools", crate::tabs::workbenches::tools(engine.clone())),
        ("settings", crate::tabs::settings::build(engine.clone())),
    ];
    let mut headers = Vec::new();
    for (id, content) in &contents {
        let page = shell::page(id).expect("every built page is registered");
        let (column, header) = page_header::wrap(page, content);
        stack.add_titled_with_icon(&column, Some(id), page.title, page.icon);
        headers.push((*id, header));
    }

    let title_bar = TitleBar::new(act.clone());
    let sidebar = Sidebar::new(act.clone());

    let split = adw::OverlaySplitView::builder()
        .min_sidebar_width(232.0)
        .max_sidebar_width(232.0)
        .sidebar_width_fraction(0.2)
        .show_sidebar(crate::app_settings::sidebar_visible())
        .build();
    split.set_sidebar(Some(&sidebar.root));
    let inspector = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideLeft)
        .transition_duration(if crate::spring::animations_enabled() { 240 } else { 0 })
        .build();
    let close_slot = slot.clone();
    inspector.set_child(Some(&crate::tabs::workbenches::chat_inspector(
        engine.clone(),
        Rc::new(move || {
            if let Some(shell) = close_slot.get().and_then(Weak::upgrade) {
                shell.handle(Action::ToggleChat);
            }
        }),
    )));
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    content.append(&stack);
    content.append(&inspector);
    split.set_content(Some(&content));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&title_bar.header);
    toolbar.set_content(Some(&split));
    toolbar.set_hexpand(true);
    toolbar.set_vexpand(true);
    let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        720.0,
        adw::LengthUnit::Px,
    ));
    breakpoint.add_setter(&split, "collapsed", Some(&true.to_value()));
    window.add_breakpoint(breakpoint);

    // ── Layering: LavaLamp → scrim → UI → drop hint ─────────────────────────
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&crate::lavalamp::build()));
    let scrim = gtk::Box::builder()
        .css_classes(["fileid-scrim"])
        .hexpand(true)
        .vexpand(true)
        .build();
    scrim.set_can_target(false);
    overlay.add_overlay(&scrim);
    overlay.add_overlay(&toolbar);
    let drop_hint = drop_hint();
    overlay.add_overlay(&drop_hint);
    window.set_content(Some(&overlay));

    let shell = Rc::new(Shell {
        window: window.clone(),
        engine: engine.clone(),
        stack,
        split,
        title_bar,
        sidebar,
        headers,
        contents,
        status: RefCell::new(Status::default()),
        folder_path: RefCell::new(None),
        note: RefCell::new(None),
        current: Cell::new("library"),
        counting: Cell::new(false),
        inspector,
    });
    let _ = slot.set(Rc::downgrade(&shell));

    install_shortcuts(&window, &act);
    install_drop_target(&overlay, &drop_hint, &shell);

    if let Some(path) = initial_folder.or_else(crate::app_settings::last_folder) {
        shell.set_folder(&path);
    }
    let initial = crate::app_settings::active_tab()
        .and_then(|id| shell::page(&id))
        .map_or("library", |page| page.id);
    shell.navigate(initial);
    shell.refresh();
    shell.count_files();

    ACTIVE_WINDOW.with_borrow_mut(|active| {
        *active = Some(ActiveWindow {
            window: window.clone(),
            shell: Rc::downgrade(&shell),
        });
    });
    let shell_for_close = shell.clone();
    window.connect_close_request(move |_| {
        ACTIVE_WINDOW.with_borrow_mut(|active| *active = None);
        shell_for_close.engine.borrow_mut().shutdown();
        glib::Propagation::Proceed
    });

    let status_rx = engine.borrow_mut().subscribe();
    let weak = Rc::downgrade(&shell);
    glib::MainContext::default().spawn_local(async move {
        while let Ok(state) = status_rx.recv().await {
            let Some(shell) = weak.upgrade() else { break };
            shell.on_engine(state);
        }
    });

    EngineClient::start(&engine);
    window.present();
    // GTK focuses the first focusable widget on map; the search field should
    // only light up when asked for (click or Ctrl+K), as on macOS.
    let unfocus = window.clone();
    glib::idle_add_local_once(move || GtkWindowExt::set_focus(&unfocus, gtk::Widget::NONE));

    // First-launch onboarding re-shows while any core model is missing.
    if crate::welcome::should_show() && std::env::var("FILEID_SELF_SHOT").is_err() {
        crate::welcome::present(&window, engine.clone());
    }

    // Dev-only self-capture for compositors without a screenshot API. Gated by
    // `FILEID_SELF_SHOT=<path>`; `FILEID_SELF_SHOT_TAB=<name>` selects a tab.
    if let Ok(path) = std::env::var("FILEID_SELF_SHOT") {
        if let Some(page) = std::env::var("FILEID_SELF_SHOT_TAB")
            .ok()
            .and_then(|tab| shell::page(&tab))
        {
            shell.navigate(page.id);
        }
        let window = window.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(2200), move || {
            self_capture(&window, &path);
        });
    }
}

impl Shell {
    fn handle(self: &Rc<Self>, action: Action) {
        match action {
            Action::Navigate(id) => self.navigate(id),
            Action::SelectArea(area) => {
                let stored = crate::app_settings::area_page(area.name());
                let page = area.remembered(self.current.get(), stored.as_deref());
                self.navigate(page.id);
            }
            Action::ToggleSidebar => {
                let show = !self.split.shows_sidebar();
                self.split.set_show_sidebar(show);
                crate::app_settings::remember_sidebar_visible(show);
            }
            Action::PickFolder => self.pick_folder(),
            Action::StartScan => self.start_scan(),
            Action::PauseScan | Action::ResumeScan => {
                let resume = action == Action::ResumeScan;
                let payload = if resume {
                    CommandPayload::ResumeScan(Empty {})
                } else {
                    CommandPayload::PauseScan(Empty {})
                };
                let sent = self.engine.borrow_mut().send(payload);
                match sent {
                    Ok(()) => self.status.borrow_mut().paused = !resume,
                    Err(err) => self.set_note(Some(format!("Engine: {err}"))),
                }
            }
            Action::CancelScan => self.send(CommandPayload::CancelScan(Empty {})),
            Action::CancelDeep => self.send(CommandPayload::DeepAnalyzeCancel(Empty {})),
            Action::ClearFolder => self.clear_folder(),
            Action::WipeAndRescan => self.confirm_wipe(),
            Action::AskChat(text) => {
                self.navigate("chat");
                if let Some((_, page)) = self.contents.iter().find(|(id, _)| *id == "chat") {
                    let _ = page.activate_action("chat.ask", Some(&text.to_variant()));
                }
            }
            Action::LibraryQuery(text) => {
                self.navigate("library");
                self.hand_off("library", LIBRARY_SEARCH_HINT, &text);
            }
            Action::FocusSearch => self.title_bar.focus_search(),
            Action::ToggleChat => {
                let open = !self.inspector.reveals_child() && self.current.get() != "chat";
                self.inspector.set_reveal_child(open);
                self.title_bar.set_chat_state(open, self.current.get() == "chat");
            }
            Action::DismissNote => self.set_note(None),
        }
        self.refresh();
    }

    fn navigate(&self, id: &'static str) {
        let Some(page) = shell::page(id) else { return };
        let changed = self.current.replace(page.id) != page.id;
        self.stack.set_visible_child_name(page.id);
        crate::app_settings::remember_active_tab(page.id);
        if let Some(area) = page.area {
            crate::app_settings::remember_area_page(area.name(), page.id);
        }
        self.sidebar.show_page(page.id);
        self.title_bar.set_settings_active(page.id == "settings");
        if page.id == "chat" {
            self.inspector.set_reveal_child(false);
        }
        self.title_bar.set_chat_state(self.inspector.reveals_child(), page.id == "chat");
        if self.split.is_collapsed() {
            self.split.set_show_sidebar(false);
        }
        if changed {
            if let Some(child) = self.stack.visible_child() {
                let target = child.clone();
                let _ = crate::spring::animate(&child, 0.0, 1.0, move |v| {
                    target.set_opacity(v.clamp(0.0, 1.0))
                });
            }
        }
    }

    fn hand_off(&self, page: &str, placeholder: &str, text: &str) {
        let Some((_, root)) = self.contents.iter().find(|(id, _)| *id == page) else {
            return;
        };
        if let Some(editable) = find_editable(root, placeholder) {
            editable.set_text(text);
            editable.grab_focus();
            editable.set_position(-1);
        }
    }

    fn send(&self, payload: CommandPayload) {
        let sent = self.engine.borrow_mut().send(payload);
        if let Err(err) = sent {
            self.set_note(Some(format!("Engine: {err}")));
        }
    }

    fn set_note(&self, note: Option<String>) {
        self.sidebar.set_note(note.as_deref());
        self.note.replace(note);
    }

    fn set_folder(&self, path: &Path) {
        let display = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        self.window.set_title(Some(&format!("FileID — {display}")));
        let changed = self.folder_path.borrow().as_deref() != Some(path);
        {
            let mut status = self.status.borrow_mut();
            if changed && !status.scan_running() {
                status.progress = None;
                status.phase = None;
            }
            status.folder = Some(display);
        }
        self.folder_path.replace(Some(path.to_path_buf()));
        crate::app_settings::remember_folder(path);
        self.refresh();
    }

    fn clear_folder(&self) {
        if self.status.borrow().scan_running() {
            self.send(CommandPayload::CancelScan(Empty {}));
        }
        self.folder_path.replace(None);
        {
            let mut status = self.status.borrow_mut();
            status.folder = None;
            status.progress = None;
            status.phase = None;
            status.start_requested = false;
        }
        self.window.set_title(Some("FileID"));
        crate::app_settings::forget_folder();
    }

    fn pick_folder(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder()
            .title("Pick a folder to organize")
            .modal(true)
            .build();
        let weak = Rc::downgrade(self);
        dialog.select_folder(
            Some(&self.window),
            gtk::gio::Cancellable::NONE,
            move |result| {
                let (Some(shell), Some(path)) =
                    (weak.upgrade(), result.ok().and_then(|file| file.path()))
                else {
                    return;
                };
                shell.set_folder(&path);
            },
        );
    }

    fn start_scan(&self) {
        let folder = self.folder_path.borrow().clone();
        let Some(folder) = folder else {
            return;
        };
        if !self.status.borrow().can_start() {
            return;
        }
        let started = self
            .engine
            .borrow_mut()
            .start_scan(&folder.to_string_lossy());
        match started {
            Ok(()) => {
                self.status.borrow_mut().start_requested = true;
                self.set_note(None);
            }
            Err(err) => self.set_note(Some(format!("Scan failed: {err}"))),
        }
    }

    fn confirm_wipe(self: &Rc<Self>) {
        let dialog = adw::AlertDialog::new(
            Some("Wipe library and rescan from scratch?"),
            Some("Deletes every tag, caption, face cluster and embedding for this library, then runs the full scan again. Your files are not touched."),
        );
        dialog.add_responses(&[("cancel", "Cancel"), ("wipe", "Wipe and rescan")]);
        dialog.set_response_appearance("wipe", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            if response != "wipe" {
                return;
            }
            let Some(shell) = weak.upgrade() else { return };
            let wiped = shell
                .engine
                .borrow_mut()
                .send(CommandPayload::WipeLibrary(Empty {}));
            if let Err(err) = wiped {
                shell.set_note(Some(format!("Engine: {err}")));
                return;
            }
            {
                let mut status = shell.status.borrow_mut();
                status.total_files = 0;
                status.progress = None;
                status.phase = None;
                status.people = None;
                status.deep_done = None;
                status.restructure_plan = false;
            }
            shell.start_scan();
            shell.refresh();
        });
        dialog.present(Some(&self.window));
    }

    fn count_files(self: &Rc<Self>) {
        if self.counting.replace(true) {
            return;
        }
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let count = fileid_engine::paths::db_path()
                .ok()
                .filter(|path| path.exists())
                .and_then(|path| fileid_engine::db::open_read(&path).ok())
                .and_then(|conn| {
                    conn.query_row("SELECT COUNT(*) FROM files", [], |row| row.get::<_, i64>(0))
                        .ok()
                })
                .unwrap_or(0);
            let _ = tx.send_blocking(count.max(0) as u64);
        });
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let count = rx.recv().await.unwrap_or(0);
            let Some(shell) = weak.upgrade() else { return };
            shell.counting.set(false);
            shell.status.borrow_mut().total_files = count;
            shell.refresh();
        });
    }

    fn refresh(&self) {
        let status = self.status.borrow().clone();
        let path = self
            .folder_path
            .borrow()
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned());
        self.title_bar.update(&status, path.as_deref());
        self.sidebar.update(&status);
        for (id, header) in &self.headers {
            header.set_meta(&status.page_meta(id));
        }
    }

    fn on_engine(self: &Rc<Self>, state: EngineState) {
        let mut recount = false;
        let mut note: Option<Option<String>> = None;
        {
            let mut status = self.status.borrow_mut();
            match state {
                EngineState::Ready => {
                    status.engine_ready = true;
                    recount = true;
                    note = Some(None);
                }
                EngineState::Spawning => status.engine_ready = self.engine.borrow().is_ready(),
                EngineState::Scanning => {
                    status.start_requested = false;
                    status.paused = false;
                    status.progress = None;
                    status.phase = Some(ScanPhase::Discovering);
                }
                EngineState::Progress(progress) => {
                    status.start_requested = false;
                    status.phase = Some(progress.phase);
                    status.progress = Some(progress);
                }
                EngineState::ScanPhase(phase) => {
                    status.phase = Some(phase);
                    if matches!(
                        phase,
                        ScanPhase::Idle
                            | ScanPhase::Completed
                            | ScanPhase::Cancelled
                            | ScanPhase::Failed
                    ) {
                        status.paused = false;
                        status.start_requested = false;
                        recount = true;
                    }
                    if phase == ScanPhase::Failed {
                        note = Some(Some("Scan failed. Check the engine log.".to_owned()));
                    }
                }
                EngineState::BatchLanded(processed) => {
                    if let Some(progress) = status.progress.as_mut() {
                        progress.processed = progress.processed.max(processed);
                    }
                }
                EngineState::ScanComplete(processed) => {
                    if let Some(progress) = status.progress.as_mut() {
                        progress.processed = progress.processed.max(processed);
                    }
                    if !matches!(status.phase, Some(ScanPhase::Cancelled | ScanPhase::Failed)) {
                        status.phase = Some(ScanPhase::Completed);
                    }
                    status.paused = false;
                    status.start_requested = false;
                    recount = true;
                }
                EngineState::DeepAnalyzeStarting(_) => {
                    status.deep_running = true;
                    status.deep_done = None;
                }
                EngineState::DeepAnalyzeProgress(progress) => {
                    status.deep_running = true;
                    status.deep = Some(progress);
                }
                EngineState::DeepAnalyzeComplete(done) => {
                    status.deep_running = false;
                    status.deep_done = (!done.cancelled).then_some(done.processed);
                }
                EngineState::FaceClusteringComplete(result) => {
                    status.people = Some(result.person_count)
                }
                EngineState::RestructurePlan(_) => status.restructure_plan = true,
                EngineState::RestructureApplyResult(_) => status.restructure_plan = false,
                EngineState::Error { kind, message, .. } if kind == "models_not_installed" => {
                    status.start_requested = false;
                    if status.scan_running() {
                        status.phase = None;
                    }
                    let missing = message
                        .split_once("Missing:")
                        .map(|(_, kinds)| kinds.trim().to_owned())
                        .unwrap_or_else(|| "required scan models".to_owned());
                    note = Some(Some(format!(
                        "Scan blocked: {missing} Install the missing models in Settings, then retry."
                    )));
                }
                EngineState::Error {
                    kind,
                    message,
                    model_kind,
                } => {
                    if kind.starts_with("deep_analyze") {
                        status.deep_running = false;
                    }
                    if kind == "scan_already_running" || kind == "scan_failed" {
                        status.start_requested = false;
                    }
                    note = match model_kind {
                        Some(_) if kind == "prewarm_cancelled" => None,
                        Some(model) => Some(Some(format!("Model {model}: {message}"))),
                        None => Some(Some(format!("Engine: {message}"))),
                    };
                }
                EngineState::Failed(message) => {
                    status.engine_ready = false;
                    clear_running(&mut status);
                    note = Some(Some(format!("Engine: {message}")));
                }
                EngineState::Exited => {
                    status.engine_ready = false;
                    clear_running(&mut status);
                    note = Some(Some("Engine stopped; restarting…".to_owned()));
                }
                _ => return,
            }
        }
        let blocked = matches!(&note, Some(Some(text)) if text.starts_with("Scan blocked"));
        if let Some(note) = note {
            self.set_note(note);
        }
        if blocked {
            self.navigate("settings");
        }
        if recount {
            self.count_files();
        }
        self.refresh();
    }
}

fn clear_running(status: &mut Status) {
    if status.scan_running() {
        status.phase = None;
    }
    status.paused = false;
    status.start_requested = false;
    status.deep_running = false;
}

fn find_editable(root: &gtk::Widget, placeholder: &str) -> Option<gtk::Editable> {
    if let Some(entry) = root.downcast_ref::<gtk::Entry>() {
        if entry.placeholder_text().as_deref() == Some(placeholder) {
            return Some(entry.clone().upcast());
        }
    }
    if let Some(entry) = root.downcast_ref::<gtk::SearchEntry>() {
        if entry.placeholder_text().as_deref() == Some(placeholder) {
            return Some(entry.clone().upcast());
        }
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        if let Some(found) = find_editable(&widget, placeholder) {
            return Some(found);
        }
        child = widget.next_sibling();
    }
    None
}

fn install_shortcuts(window: &adw::ApplicationWindow, act: &Rc<dyn Fn(Action)>) {
    let controller = gtk::ShortcutController::new();
    controller.set_scope(gtk::ShortcutScope::Global);
    let mut bindings: Vec<(String, Action)> = (1..=9)
        .filter_map(|n| {
            shell::page_for_shortcut(n)
                .map(|page| (format!("<Control>{n}"), Action::Navigate(page.id)))
        })
        .collect();
    bindings.extend([
        ("<Control>comma".to_owned(), Action::Navigate("settings")),
        ("<Control>k".to_owned(), Action::FocusSearch),
        ("<Control><Shift>k".to_owned(), Action::ToggleChat),
        ("<Control>o".to_owned(), Action::PickFolder),
        ("<Control>r".to_owned(), Action::StartScan),
        ("F9".to_owned(), Action::ToggleSidebar),
    ]);
    for (trigger, action) in bindings {
        let act = act.clone();
        let callback = gtk::CallbackAction::new(move |_, _| {
            act(action.clone());
            glib::Propagation::Stop
        });
        controller.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string(&trigger),
            Some(callback),
        ));
    }
    window.add_controller(controller);

    // Pages ask the shell to switch pages through `win.navigate` ("moments",
    // "tools", ...) without knowing about the window.
    let navigate = gtk::gio::SimpleAction::new(NAVIGATE_ACTION, Some(glib::VariantTy::STRING));
    let act = act.clone();
    navigate.connect_activate(move |_, parameter| {
        let page = parameter
            .and_then(|value| value.str())
            .and_then(shell::page);
        if let Some(page) = page {
            act(Action::Navigate(page.id));
        }
    });
    window.add_action(&navigate);
}

const NAVIGATE_ACTION: &str = "navigate";

fn drop_hint() -> gtk::Box {
    let hint = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .css_classes(["drop-zone"])
        .halign(gtk::Align::Fill)
        .valign(gtk::Align::Fill)
        .visible(false)
        .can_target(false)
        .build();
    let inner = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .vexpand(true)
        .build();
    let icon = gtk::Image::from_icon_name("folder-download-symbolic");
    icon.set_pixel_size(40);
    icon.add_css_class("tone-gold");
    inner.append(&icon);
    inner.append(
        &gtk::Label::builder()
            .label("Drop folder to scan")
            .css_classes(["title-2", "tone-gold"])
            .build(),
    );
    hint.append(&inner);
    hint
}

fn install_drop_target(overlay: &gtk::Overlay, hint: &gtk::Box, shell: &Rc<Shell>) {
    let target = gtk::DropTarget::new(gtk::gio::File::static_type(), gtk::gdk::DragAction::COPY);
    let hint_enter = hint.clone();
    target.connect_enter(move |_, _, _| {
        hint_enter.set_visible(true);
        gtk::gdk::DragAction::COPY
    });
    let hint_leave = hint.clone();
    target.connect_leave(move |_| hint_leave.set_visible(false));
    let hint_drop = hint.clone();
    let weak = Rc::downgrade(shell);
    target.connect_drop(move |_, value, _, _| {
        hint_drop.set_visible(false);
        let Some(path) = value
            .get::<gtk::gio::File>()
            .ok()
            .and_then(|file| file.path())
        else {
            return false;
        };
        if !path.is_dir() {
            return false;
        }
        if let Some(shell) = weak.upgrade() {
            shell.set_folder(&path);
        }
        true
    });
    overlay.add_controller(target);
}

/// Render the window's current frame to a PNG via GTK's own renderer.
fn self_capture(window: &adw::ApplicationWindow, path: &str) {
    let widget = window.upcast_ref::<gtk::Widget>();
    let (width, height) = (widget.width(), widget.height());
    if width <= 0 || height <= 0 {
        eprintln!("[self_capture] window not sized yet ({width}x{height})");
        return;
    }
    let Some(renderer) = widget.native().and_then(|native| native.renderer()) else {
        eprintln!("[self_capture] no renderer");
        return;
    };
    let paintable = gtk::WidgetPaintable::new(Some(widget));
    let snapshot = gtk::Snapshot::new();
    gtk::prelude::PaintableExt::snapshot(
        &paintable,
        &snapshot,
        f64::from(width),
        f64::from(height),
    );
    let Some(node) = snapshot.to_node() else {
        eprintln!("[self_capture] empty render node");
        return;
    };
    match renderer.render_texture(&node, None).save_to_png(path) {
        Ok(()) => eprintln!("[self_capture] wrote {path} ({width}x{height})"),
        Err(err) => eprintln!("[self_capture] save failed: {err}"),
    }
}
