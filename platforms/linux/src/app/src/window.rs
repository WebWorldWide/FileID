// Main window — the app shell. Mirror of macOS `MainWindow` / Windows
// `MainWindow`: an `adw::ApplicationWindow` whose content is a `gtk::Overlay`
// stack — the animated `LavaLampBackground` at the bottom, a muted material
// scrim over it, and the UI on top (LavaLamp → frosting → content).
//
// Navigation is a left sidebar (260px), matching the macOS/Windows reference.
// It carries the folder picker, the six nav rows, the Search & Moments / File
// Tools / Chat workbenches, the scan controls and the engine-status line.

use adw::prelude::*;
use gtk::glib;
use gtk::glib::clone;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use crate::engine_client::{EngineClient, EngineState};
use fileid_engine::ipc::{CommandPayload, Empty, ScanPhase};

const TABS: [(&str, &str, &str); 6] = [
    ("library", "Library", "view-grid-symbolic"),
    ("people", "People", "system-users-symbolic"),
    ("cleanup", "Cleanup", "user-trash-symbolic"),
    ("deep-analyze", "Deep Analyze", "starred-symbolic"),
    ("restructure", "Restructure", "view-list-symbolic"),
    ("settings", "Settings", "emblem-system-symbolic"),
];

#[derive(Clone)]
struct ActiveWindow {
    window: adw::ApplicationWindow,
    folder_label: gtk::Label,
    selected_folder: Rc<RefCell<Option<String>>>,
    refresh_controls: Rc<dyn Fn()>,
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
    ACTIVE_WINDOW.with_borrow(|active| {
        let Some(active) = active else { return false };
        if let Some(path) = folder {
            apply_folder(&path, &active.folder_label, &active.selected_folder);
            (active.refresh_controls)();
        }
        active.window.present();
        true
    })
}

fn apply_folder(
    path: &std::path::Path,
    folder_label: &gtk::Label,
    selected_folder: &Rc<RefCell<Option<String>>>,
) {
    let display = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    folder_label.set_text(&display);
    folder_label.set_tooltip_text(Some(&path.to_string_lossy()));
    selected_folder.replace(Some(path.to_string_lossy().into_owned()));
    crate::app_settings::remember_folder(path);
}

fn build_window(app: &adw::Application, initial_folder: Option<PathBuf>) {
    let (width, height) = std::env::var("FILEID_WIN_SIZE")
        .ok()
        .and_then(|size| {
            let mut parts = size.split('x');
            Some((parts.next()?.trim().parse().ok()?, parts.next()?.trim().parse().ok()?))
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

    let stack = adw::ViewStack::new();
    let nav_buttons: Rc<RefCell<Vec<gtk::Button>>> = Rc::new(RefCell::new(Vec::new()));
    let activate_tab: Rc<dyn Fn(&str)> = {
        let stack = stack.clone();
        let nav_buttons = nav_buttons.clone();
        Rc::new(move |name| {
            stack.set_visible_child_name(name);
            crate::app_settings::remember_active_tab(name);
            if let Some(index) = TABS.iter().position(|(tab, _, _)| *tab == name) {
                for (i, button) in nav_buttons.borrow().iter().enumerate() {
                    if i == index {
                        button.add_css_class("active");
                    } else {
                        button.remove_css_class("active");
                    }
                }
            }
        })
    };

    let pages: [gtk::Widget; 6] = [
        crate::tabs::library::build(engine.clone()),
        crate::tabs::people::build(engine.clone()),
        crate::tabs::cleanup::build_cleanup_tab(engine.clone()),
        crate::tabs::deep_analyze::build_deep_analyze_tab(engine.clone()),
        crate::tabs::restructure::build_restructure_tab(engine.clone()),
        crate::tabs::settings::build(engine.clone()),
    ];
    for (page, (name, title, icon)) in pages.iter().zip(TABS) {
        stack.add_titled_with_icon(page, Some(name), title, icon);
    }
    stack.set_hexpand(true);
    stack.set_vexpand(true);

    // ── Sidebar ──────────────────────────────────────────────────────────────
    let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 4);
    sidebar.add_css_class("fileid-sidebar");
    sidebar.append(
        &gtk::Label::builder()
            .label("FileID")
            .css_classes(["title-2", "gold-accent"])
            .halign(gtk::Align::Start)
            .margin_start(8)
            .margin_top(4)
            .margin_bottom(8)
            .build(),
    );

    sidebar.append(&section_heading("FOLDER"));
    let pick_btn = gtk::Button::builder()
        .label("Pick folder…")
        .css_classes(["gold-button"])
        .margin_start(8)
        .margin_end(8)
        .build();
    sidebar.append(&pick_btn);
    let folder_label = gtk::Label::builder()
        .label("No folder selected")
        .css_classes(["dim-label"])
        .halign(gtk::Align::Start)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .margin_start(10)
        .margin_top(4)
        .build();
    sidebar.append(&folder_label);

    sidebar.append(&section_heading("NAVIGATE"));
    for (i, (name, title, icon)) in TABS.into_iter().enumerate() {
        let row = gtk::Button::builder().css_classes(["nav-row"]).build();
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        content.append(&gtk::Image::from_icon_name(icon));
        content.append(&gtk::Label::builder().label(title).halign(gtk::Align::Start).hexpand(true).build());
        row.set_child(Some(&content));
        if i == 0 {
            row.add_css_class("active");
        }
        let activate_tab = activate_tab.clone();
        row.connect_clicked(move |_| activate_tab(name));
        nav_buttons.borrow_mut().push(row.clone());
        sidebar.append(&row);
    }
    if let Some(tab) = crate::app_settings::active_tab() {
        if TABS.iter().any(|(name, _, _)| *name == tab) {
            activate_tab(&tab);
        }
    }

    sidebar.append(&section_heading("WORKBENCHES"));
    for (title, icon, build) in [
        ("Search & Moments", "system-search-symbolic", crate::tabs::workbenches::catalog as fn(_) -> _),
        ("File Tools", "applications-utilities-symbolic", crate::tabs::workbenches::tools as fn(_) -> _),
        ("Chat", "chat-symbolic", crate::tabs::workbenches::chat as fn(_) -> _),
    ] {
        let row = gtk::Button::builder().css_classes(["nav-row"]).build();
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        content.append(&gtk::Image::from_icon_name(icon));
        content.append(&gtk::Label::builder().label(title).halign(gtk::Align::Start).hexpand(true).build());
        row.set_child(Some(&content));
        let workbench = adw::Window::builder()
            .title(title)
            .default_width(900)
            .default_height(680)
            .transient_for(&window)
            .hide_on_close(true)
            .content(&build(engine.clone()))
            .build();
        row.connect_clicked(move |_| workbench.present());
        sidebar.append(&row);
    }

    let spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    spacer.set_vexpand(true);
    sidebar.append(&spacer);

    sidebar.append(&section_heading("SCAN"));
    let scan_buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    scan_buttons.set_margin_start(8);
    scan_buttons.set_margin_end(8);
    let start_btn = gtk::Button::builder()
        .label("Start scan")
        .css_classes(["gold-button"])
        .hexpand(true)
        .sensitive(false)
        .build();
    let pause_btn = gtk::Button::builder().label("Pause").sensitive(false).build();
    scan_buttons.append(&start_btn);
    scan_buttons.append(&pause_btn);
    sidebar.append(&scan_buttons);
    let status_label = gtk::Label::builder()
        .label("Engine: starting…")
        .css_classes(["dim-label"])
        .halign(gtk::Align::Start)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .margin_start(10)
        .margin_top(6)
        .build();
    sidebar.append(&status_label);
    // Visible only while a scan runs — mirrors the Windows sidebar progress.
    let scan_progress = gtk::ProgressBar::builder()
        .visible(false)
        .margin_start(10)
        .margin_end(10)
        .margin_top(6)
        .build();
    sidebar.append(&scan_progress);

    // ── Header + collapsible split ───────────────────────────────────────────
    let header = adw::HeaderBar::builder().css_classes(["fileid-headerbar"]).build();
    // The sidebar carries the wordmark; an empty title keeps the bar draggable.
    header.set_title_widget(Some(&gtk::Box::new(gtk::Orientation::Horizontal, 0)));
    let split = adw::OverlaySplitView::builder()
        .min_sidebar_width(260.0)
        .max_sidebar_width(260.0)
        .sidebar_width_fraction(0.24)
        .show_sidebar(crate::app_settings::sidebar_visible())
        .build();
    split.set_sidebar(Some(&sidebar));
    split.set_content(Some(&stack));
    let sidebar_toggle = gtk::Button::builder()
        .icon_name("sidebar-show-symbolic")
        .css_classes(["flat"])
        .tooltip_text("Toggle sidebar")
        .build();
    sidebar_toggle.connect_clicked(clone!(
        #[weak]
        split,
        move |_| {
            let show = !split.shows_sidebar();
            split.set_show_sidebar(show);
            crate::app_settings::remember_sidebar_visible(show);
        }
    ));
    header.pack_start(&sidebar_toggle);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
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

    // ── Layering: LavaLamp → scrim → UI ──────────────────────────────────────
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
    window.set_content(Some(&overlay));

    // ── Scan state → controls ────────────────────────────────────────────────
    let selected_folder: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let scanning = Rc::new(Cell::new(false));
    let paused = Rc::new(Cell::new(false));
    let scan_terminal = Rc::new(RefCell::new(None::<ScanPhase>));
    let refresh_controls: Rc<dyn Fn()> = Rc::new(clone!(
        #[weak]
        start_btn,
        #[weak]
        pause_btn,
        #[strong]
        engine,
        #[strong]
        selected_folder,
        #[strong]
        scanning,
        #[strong]
        paused,
        move || {
            let ready = engine.borrow().is_ready();
            let active = scanning.get();
            if active {
                start_btn.set_label("Stop scan");
                start_btn.remove_css_class("gold-button");
                start_btn.add_css_class("destructive-action");
            } else {
                start_btn.set_label("Start scan");
                start_btn.remove_css_class("destructive-action");
                start_btn.add_css_class("gold-button");
                paused.set(false);
            }
            start_btn.set_sensitive(ready && (active || selected_folder.borrow().is_some()));
            pause_btn.set_sensitive(ready && active);
            pause_btn.set_label(if paused.get() { "Resume" } else { "Pause" });
        }
    ));

    if let Some(path) = initial_folder.or_else(crate::app_settings::last_folder) {
        apply_folder(&path, &folder_label, &selected_folder);
    }
    ACTIVE_WINDOW.with_borrow_mut(|active| {
        *active = Some(ActiveWindow {
            window: window.clone(),
            folder_label: folder_label.clone(),
            selected_folder: selected_folder.clone(),
            refresh_controls: refresh_controls.clone(),
        });
    });
    let engine_for_close = engine.clone();
    window.connect_close_request(move |_| {
        ACTIVE_WINDOW.with_borrow_mut(|active| *active = None);
        engine_for_close.borrow_mut().shutdown();
        glib::Propagation::Proceed
    });

    pick_btn.connect_clicked(clone!(
        #[weak]
        window,
        #[weak]
        folder_label,
        #[strong]
        selected_folder,
        #[strong]
        refresh_controls,
        move |_| {
            let dialog = gtk::FileDialog::builder()
                .title("Pick a folder to organize")
                .modal(true)
                .build();
            dialog.select_folder(
                Some(&window),
                gtk::gio::Cancellable::NONE,
                clone!(
                    #[weak]
                    folder_label,
                    #[strong]
                    selected_folder,
                    #[strong]
                    refresh_controls,
                    move |result| {
                        if let Some(path) = result.ok().and_then(|file| file.path()) {
                            apply_folder(&path, &folder_label, &selected_folder);
                            refresh_controls();
                        }
                    }
                ),
            );
        }
    ));

    // One button, two states: idle → `startScan`; scanning → `cancelScan`.
    start_btn.connect_clicked(clone!(
        #[strong]
        engine,
        #[strong]
        selected_folder,
        #[strong]
        scanning,
        #[strong]
        refresh_controls,
        #[weak]
        status_label,
        move |_| {
            if scanning.get() {
                match engine.borrow_mut().send(CommandPayload::CancelScan(Empty {})) {
                    Ok(()) => status_label.set_label("Stopping scan…"),
                    Err(err) => status_label.set_label(&format!("Engine: {err}")),
                }
                return;
            }
            let Some(folder) = selected_folder.borrow().clone() else { return };
            let started = engine.borrow_mut().start_scan(&folder);
            match started {
                Ok(()) => {
                    scanning.set(true);
                    status_label.set_label("Engine: scanning…");
                }
                Err(err) => status_label.set_label(&format!("scan failed: {err}")),
            }
            refresh_controls();
        }
    ));
    pause_btn.connect_clicked(clone!(
        #[strong]
        engine,
        #[strong]
        paused,
        #[strong]
        refresh_controls,
        #[weak]
        status_label,
        move |_| {
            let payload = if paused.get() {
                CommandPayload::ResumeScan(Empty {})
            } else {
                CommandPayload::PauseScan(Empty {})
            };
            let sent = engine.borrow_mut().send(payload);
            match sent {
                Ok(()) => {
                    paused.set(!paused.get());
                    status_label.set_label(if paused.get() { "Pause requested" } else { "Resume requested" });
                }
                Err(err) => status_label.set_label(&format!("Engine: {err}")),
            }
            refresh_controls();
        }
    ));

    // ── Engine status → sidebar status line + progress bar ───────────────────
    let status_rx = engine.borrow_mut().subscribe();
    glib::MainContext::default().spawn_local(clone!(
        #[weak]
        status_label,
        #[weak]
        scan_progress,
        #[strong]
        scanning,
        #[strong]
        scan_terminal,
        #[strong]
        refresh_controls,
        #[strong]
        activate_tab,
        async move {
            while let Ok(state) = status_rx.recv().await {
                let text = match state {
                    EngineState::Spawning => Some("Engine: starting…".to_owned()),
                    EngineState::Ready => Some("Engine: ready".to_owned()),
                    EngineState::Scanning => {
                        scanning.set(true);
                        scan_terminal.replace(None);
                        None
                    }
                    EngineState::Progress(p) => {
                        scanning.set(true);
                        scan_progress.set_visible(true);
                        if p.total > 0 {
                            scan_progress.set_fraction(p.processed as f64 / p.total as f64);
                            Some(format!(
                                "Scanning… {} / {} · {} failed · {:.1} files/s",
                                p.processed, p.total, p.failed, p.files_per_second
                            ))
                        } else {
                            // `total` stays 0 until the discovery walk completes.
                            scan_progress.pulse();
                            Some(format!("Scanning… {} processed", p.processed))
                        }
                    }
                    EngineState::BatchLanded(n) => {
                        scanning.set(true);
                        Some(format!("Scanning… {n} files"))
                    }
                    EngineState::ScanPhase(phase) => {
                        let terminal = matches!(
                            phase,
                            ScanPhase::Idle | ScanPhase::Completed | ScanPhase::Cancelled | ScanPhase::Failed
                        );
                        if terminal {
                            scanning.set(false);
                            scan_progress.set_visible(false);
                            scan_terminal.replace(Some(phase));
                        }
                        match phase {
                            ScanPhase::Failed => Some("Scan failed".to_owned()),
                            ScanPhase::Cancelled => Some("Scan cancelled".to_owned()),
                            _ => None,
                        }
                    }
                    EngineState::ScanComplete(n) => {
                        scanning.set(false);
                        scan_progress.set_visible(false);
                        Some(scan_count_label(n, scan_terminal.borrow().as_ref()))
                    }
                    EngineState::Error { kind, message, .. } if kind == "models_not_installed" => {
                        scanning.set(false);
                        scan_progress.set_visible(false);
                        activate_tab("settings");
                        let missing = message
                            .split_once("Missing:")
                            .map(|(_, kinds)| kinds.trim().to_owned())
                            .unwrap_or_else(|| "required scan models".to_owned());
                        Some(format!(
                            "Scan blocked: {missing} Install the missing models in Settings, then retry."
                        ))
                    }
                    EngineState::Error { kind, message, model_kind: Some(model_kind) }
                        if kind != "prewarm_cancelled" =>
                    {
                        Some(format!("Model {model_kind}: {message}"))
                    }
                    EngineState::Error { model_kind: None, message, .. } => Some(format!("Engine: {message}")),
                    EngineState::Failed(message) => {
                        scanning.set(false);
                        scan_progress.set_visible(false);
                        Some(format!("Engine: {message}"))
                    }
                    EngineState::Exited => {
                        scanning.set(false);
                        scan_progress.set_visible(false);
                        Some("Engine: restarting…".to_owned())
                    }
                    _ => continue,
                };
                if let Some(text) = text {
                    status_label.set_label(&text);
                    status_label.set_tooltip_text(Some(&text));
                }
                refresh_controls();
            }
        }
    ));

    EngineClient::start(&engine);
    window.present();

    // First-launch onboarding re-shows while any core model is missing.
    if crate::welcome::should_show() && std::env::var("FILEID_SELF_SHOT").is_err() {
        crate::welcome::present(&window, engine.clone());
    }

    // Dev-only self-capture for compositors without a screenshot API. Gated by
    // `FILEID_SELF_SHOT=<path>`; `FILEID_SELF_SHOT_TAB=<name>` selects a tab.
    if let Ok(path) = std::env::var("FILEID_SELF_SHOT") {
        if let Ok(tab) = std::env::var("FILEID_SELF_SHOT_TAB") {
            activate_tab(&tab);
        }
        let window = window.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(2200), move || {
            self_capture(&window, &path);
        });
    }
}

fn scan_count_label(processed: u64, terminal: Option<&ScanPhase>) -> String {
    let outcome = match terminal {
        Some(ScanPhase::Cancelled) => "cancelled",
        Some(ScanPhase::Failed) => "failed",
        _ => "complete",
    };
    format!("Scan {outcome} — {processed} files processed")
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
    gtk::prelude::PaintableExt::snapshot(&paintable, &snapshot, f64::from(width), f64::from(height));
    let Some(node) = snapshot.to_node() else {
        eprintln!("[self_capture] empty render node");
        return;
    };
    match renderer.render_texture(&node, None).save_to_png(path) {
        Ok(()) => eprintln!("[self_capture] wrote {path} ({width}x{height})"),
        Err(err) => eprintln!("[self_capture] save failed: {err}"),
    }
}

fn section_heading(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .css_classes(["sidebar-heading"])
        .halign(gtk::Align::Start)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_scan_counts_do_not_relabel_cancelled_or_failed_scans_as_success() {
        assert_eq!(scan_count_label(4, Some(&ScanPhase::Cancelled)), "Scan cancelled — 4 files processed");
        assert_eq!(scan_count_label(4, Some(&ScanPhase::Failed)), "Scan failed — 4 files processed");
        assert_eq!(scan_count_label(4, Some(&ScanPhase::Completed)), "Scan complete — 4 files processed");
    }
}
