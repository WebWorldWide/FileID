// Main window — the app shell. Mirror of macOS `MainWindow` / Windows
// `MainWindow`: an `adw::ApplicationWindow` whose content is a `gtk::Overlay`
// stack — the animated `LavaLampBackground` at the bottom, a muted material
// scrim over it, and the UI on top (LavaLamp → frosting → content).
//
// Navigation is a **left sidebar** (260px), matching the macOS/Windows
// reference — NOT a GNOME top `ViewSwitcher`. The sidebar carries the folder
// picker (gold CTA), the six nav rows (Library / People / Cleanup / Deep
// Analyze / Restructure / Settings) whose active row is gold-tinted, the
// "Start scan" CTA, and the engine-status line. Each nav row flips the
// `adw::ViewStack`; the six pages are 1:1 ports of the macOS views sharing the
// one `EngineClient`.

use adw::prelude::*;
use gtk::glib;
use gtk::glib::clone;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use crate::engine_client::{EngineClient, EngineEvent};

#[derive(Clone)]
struct ActiveWindow {
    window: adw::ApplicationWindow,
    folder_label: gtk::Label,
    start_button: gtk::Button,
    selected_folder: Rc<RefCell<Option<String>>>,
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
            apply_folder(
                &path,
                &active.folder_label,
                &active.start_button,
                &active.selected_folder,
            );
        }
        active.window.present();
        true
    })
}

fn apply_folder(
    path: &std::path::Path,
    folder_label: &gtk::Label,
    start_button: &gtk::Button,
    selected_folder: &Rc<RefCell<Option<String>>>,
) {
    let display = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    folder_label.set_text(&display);
    selected_folder.replace(Some(path.to_string_lossy().into_owned()));
    start_button.set_sensitive(true);
}

fn build_window(app: &adw::Application, initial_folder: Option<PathBuf>) {
    let (dw, dh) = std::env::var("FILEID_WIN_SIZE")
        .ok()
        .and_then(|s| {
            let mut p = s.split('x');
            Some((
                p.next()?.trim().parse().ok()?,
                p.next()?.trim().parse().ok()?,
            ))
        })
        .unwrap_or((1320, 860));
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("FileID")
        .default_width(dw)
        .default_height(dh)
        .build();
    // Small minimum so the window can shrink freely (the sidebar collapses via
    // the breakpoint below; the tabs are built to reflow narrow).
    window.set_size_request(360, 320);

    // Single shared engine client (single-threaded on the GTK main context).
    let engine = Rc::new(RefCell::new(EngineClient::new()));

    // ── Tabs (content pages) ─────────────────────────────────────────────────
    let stack = adw::ViewStack::new();
    let library = crate::tabs::library::build(engine.clone());
    stack.add_titled_with_icon(&library, Some("library"), "Library", "view-grid-symbolic");
    let people = crate::tabs::people::build(engine.clone());
    stack.add_titled_with_icon(&people, Some("people"), "People", "system-users-symbolic");
    let cleanup = crate::tabs::cleanup::build_cleanup_tab(engine.clone());
    stack.add_titled_with_icon(&cleanup, Some("cleanup"), "Cleanup", "user-trash-symbolic");
    let deep = crate::tabs::deep_analyze::build_deep_analyze_tab(engine.clone());
    stack.add_titled_with_icon(&deep, Some("deep"), "Deep Analyze", "starred-symbolic");
    let restructure = crate::tabs::restructure::build_restructure_tab(engine.clone());
    stack.add_titled_with_icon(
        &restructure,
        Some("restructure"),
        "Restructure",
        "view-list-symbolic",
    );
    let settings = crate::tabs::settings::build(engine.clone());
    stack.add_titled_with_icon(
        &settings,
        Some("settings"),
        "Settings",
        "emblem-system-symbolic",
    );
    stack.set_hexpand(true);
    stack.set_vexpand(true);

    // ── Sidebar ──────────────────────────────────────────────────────────────
    let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 4);
    sidebar.add_css_class("fileid-sidebar");

    // Wordmark
    let wordmark = gtk::Label::builder()
        .label("FileID")
        .css_classes(["title-2", "gold-accent"])
        .halign(gtk::Align::Start)
        .margin_start(8)
        .margin_top(4)
        .margin_bottom(8)
        .build();
    sidebar.append(&wordmark);

    // FOLDER section
    sidebar.append(&section_heading("FOLDER"));
    let pick_btn = gtk::Button::builder()
        .label("Pick folder…")
        .css_classes(["gold-button"])
        .build();
    pick_btn.set_margin_start(8);
    pick_btn.set_margin_end(8);
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

    // NAVIGATE section — the six nav rows
    sidebar.append(&section_heading("LIBRARY"));
    let nav_defs = [
        ("library", "Library", "view-grid-symbolic"),
        ("people", "People", "system-users-symbolic"),
        ("cleanup", "Cleanup", "user-trash-symbolic"),
        ("deep", "Deep Analyze", "starred-symbolic"),
        ("restructure", "Restructure", "view-list-symbolic"),
        ("settings", "Settings", "emblem-system-symbolic"),
    ];
    let nav_buttons: Rc<RefCell<Vec<gtk::Button>>> = Rc::new(RefCell::new(Vec::new()));
    for (i, &(name, label, icon)) in nav_defs.iter().enumerate() {
        let row = gtk::Button::builder().css_classes(["nav-row"]).build();
        let h = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        h.append(&gtk::Image::from_icon_name(icon));
        let lbl = gtk::Label::builder()
            .label(label)
            .halign(gtk::Align::Start)
            .hexpand(true)
            .build();
        h.append(&lbl);
        row.set_child(Some(&h));
        if i == 0 {
            row.add_css_class("active");
        }
        row.connect_clicked(clone!(
            #[weak]
            stack,
            #[strong]
            nav_buttons,
            move |_| {
                stack.set_visible_child_name(name);
                for (j, b) in nav_buttons.borrow().iter().enumerate() {
                    if j == i {
                        b.add_css_class("active");
                    } else {
                        b.remove_css_class("active");
                    }
                }
            }
        ));
        nav_buttons.borrow_mut().push(row.clone());
        sidebar.append(&row);
    }

    // Flexible spacer pushes scan controls to the bottom.
    let spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    spacer.set_vexpand(true);
    sidebar.append(&spacer);

    // SCAN section
    sidebar.append(&section_heading("SCAN"));
    let start_btn = gtk::Button::builder()
        .label("Start scan")
        .css_classes(["gold-button"])
        .sensitive(false)
        .build();
    start_btn.set_margin_start(8);
    start_btn.set_margin_end(8);
    sidebar.append(&start_btn);
    let status_label = gtk::Label::builder()
        .label("Engine: starting…")
        .css_classes(["dim-label"])
        .halign(gtk::Align::Start)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .margin_start(10)
        .margin_top(6)
        .build();
    sidebar.append(&status_label);

    // ── Header (thin, transparent — window controls + wordmark) ──────────────
    let header = adw::HeaderBar::builder()
        .css_classes(["fileid-headerbar"])
        .build();
    // Hidden title (macOS/Windows use a unified title bar) — the sidebar carries
    // the wordmark. An empty title widget keeps the bar draggable without the
    // duplicate centered "FileID".
    header.set_title_widget(Some(&gtk::Box::new(gtk::Orientation::Horizontal, 0)));

    // ── Collapsible split: sidebar | content ─────────────────────────────────
    // adw::OverlaySplitView gives a macOS-style collapsible sidebar for free
    // (animated show/hide + overlay when collapsed) and lets the window resize
    // down to a small width.
    let split = adw::OverlaySplitView::builder()
        .min_sidebar_width(230.0)
        .max_sidebar_width(300.0)
        .sidebar_width_fraction(0.24)
        .show_sidebar(true)
        .build();
    split.set_sidebar(Some(&sidebar));
    split.set_content(Some(&stack));

    // Sidebar toggle button in the header (macOS `sidebar.left`).
    let sidebar_toggle = gtk::Button::builder()
        .icon_name("sidebar-show-symbolic")
        .css_classes(["flat"])
        .tooltip_text("Toggle sidebar")
        .build();
    sidebar_toggle.connect_clicked(clone!(
        #[weak]
        split,
        move |_| {
            split.set_show_sidebar(!split.shows_sidebar());
        }
    ));
    header.pack_start(&sidebar_toggle);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&split));
    toolbar.set_hexpand(true);
    toolbar.set_vexpand(true);

    // Auto-collapse the sidebar when the window gets narrow, so it can resize
    // down small (and the sidebar overlays instead of squeezing the content).
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

    // ── Folder pick → enable scan ────────────────────────────────────────────
    let selected_folder: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    if let Some(path) = initial_folder {
        apply_folder(&path, &folder_label, &start_btn, &selected_folder);
    }
    ACTIVE_WINDOW.with_borrow_mut(|active| {
        *active = Some(ActiveWindow {
            window: window.clone(),
            folder_label: folder_label.clone(),
            start_button: start_btn.clone(),
            selected_folder: selected_folder.clone(),
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
        #[weak]
        start_btn,
        #[strong]
        selected_folder,
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
                    #[weak]
                    start_btn,
                    #[strong]
                    selected_folder,
                    move |result| {
                        if let Ok(file) = result {
                            if let Some(path) = file.path() {
                                let display = path
                                    .file_name()
                                    .map(|s| s.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| path.to_string_lossy().into_owned());
                                folder_label.set_text(&display);
                                *selected_folder.borrow_mut() =
                                    Some(path.to_string_lossy().into_owned());
                                start_btn.set_sensitive(true);
                            }
                        }
                    }
                ),
            );
        }
    ));

    // ── Start scan → engine ──────────────────────────────────────────────────
    start_btn.connect_clicked(clone!(
        #[strong]
        engine,
        #[strong]
        selected_folder,
        #[weak]
        status_label,
        move |_| {
            let Some(folder) = selected_folder.borrow().clone() else {
                return;
            };
            match engine.borrow_mut().start_scan(&folder, false) {
                Ok(()) => status_label.set_label("Engine: scanning…"),
                Err(err) => status_label.set_label(&format!("scan failed: {err}")),
            }
        }
    ));

    // ── Engine status → sidebar status line ──────────────────────────────────
    let status_rx = engine.borrow_mut().subscribe();
    glib::MainContext::default().spawn_local(clone!(
        #[weak]
        status_label,
        async move {
            while let Ok(ev) = status_rx.recv().await {
                let text = match ev {
                    EngineEvent::Spawning => "Engine: starting…".to_string(),
                    EngineEvent::Ready => "Engine: ready".to_string(),
                    EngineEvent::Progress(p) => format!("Scanning… {} / {}", p.processed, p.total),
                    EngineEvent::BatchLanded(n) => format!("Scanning… {n} files"),
                    EngineEvent::ScanComplete(n) => format!("Scan complete — {n} files"),
                    EngineEvent::Error(m) => format!("Engine: {m}"),
                    // Model-download failures were split out of Error; without
                    // this arm a failed download vanishes from the sidebar
                    // (only the Settings/Deep Analyze cards would notice).
                    EngineEvent::ModelDownloadFailed {
                        model_kind,
                        message,
                    } => {
                        format!("Model {model_kind}: {message}")
                    }
                    EngineEvent::Exited => "Engine: restarting…".to_string(),
                    _ => continue,
                };
                status_label.set_label(&text);
            }
        }
    ));

    // Boot the engine + thumbnail worker + event fan-out pump.
    EngineClient::start(&engine);

    window.present();

    // Dev-only self-capture: render the window to a PNG so the UI can be
    // inspected on compositors that expose no screenshot API (e.g. cosmic-comp
    // lacks wlr-screencopy). Gated by `FILEID_SELF_SHOT=<path>`; optional
    // `FILEID_SELF_SHOT_TAB=<name>` selects a tab first. No effect otherwise.
    if let Ok(path) = std::env::var("FILEID_SELF_SHOT") {
        if let Ok(tab) = std::env::var("FILEID_SELF_SHOT_TAB") {
            stack.set_visible_child_name(&tab);
            for b in nav_buttons.borrow().iter() {
                b.remove_css_class("active");
            }
        }
        let win = window.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(2200), move || {
            self_capture(&win, &path);
        });
    }
}

/// Render the window's current frame to a PNG via GTK's own renderer (no
/// compositor screenshot needed). Dev diagnostic only.
fn self_capture(window: &adw::ApplicationWindow, path: &str) {
    let widget = window.upcast_ref::<gtk::Widget>();
    let (w, h) = (widget.width(), widget.height());
    if w <= 0 || h <= 0 {
        eprintln!("[self_capture] window not sized yet ({w}x{h})");
        return;
    }
    let Some(native) = widget.native() else {
        eprintln!("[self_capture] no native");
        return;
    };
    let Some(renderer) = native.renderer() else {
        eprintln!("[self_capture] no renderer");
        return;
    };
    let paintable = gtk::WidgetPaintable::new(Some(widget));
    let snapshot = gtk::Snapshot::new();
    gtk::prelude::PaintableExt::snapshot(&paintable, &snapshot, f64::from(w), f64::from(h));
    let Some(node) = snapshot.to_node() else {
        eprintln!("[self_capture] empty render node");
        return;
    };
    let texture = renderer.render_texture(&node, None);
    match texture.save_to_png(path) {
        Ok(()) => eprintln!("[self_capture] wrote {path} ({w}x{h})"),
        Err(e) => eprintln!("[self_capture] save failed: {e}"),
    }
}

/// An uppercase, letter-spaced section header for the sidebar (mirrors the
/// macOS sidebar section labels).
fn section_heading(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .css_classes(["sidebar-heading"])
        .halign(gtk::Align::Start)
        .build()
}
