// Main window — the app shell. Mirror of macOS `ContentView` / Windows
// `MainWindow`. An `adw::ApplicationWindow` whose content is a `gtk::Overlay`
// stack: the animated `LavaLampBackground` at the bottom, a muted material
// scrim over it, and the UI (transparent `adw::ToolbarView`) on top — exactly
// the layering macOS uses (LavaLamp → ultraThinMaterial → content).
//
// Navigation is an `adw::ViewStack` driven by an `adw::ViewSwitcher` in the
// header: six tabs — Library / People / Cleanup / Deep Analyze / Restructure /
// Settings — each a 1:1 port of the macOS reference view, all sharing the one
// `EngineClient`. The header also carries the gold "Pick folder" CTA (a
// `gtk::FileDialog`) and "Start scan", which drive that shared client.

use adw::prelude::*;
use gtk::glib;
use gtk::glib::clone;
use std::cell::RefCell;
use std::rc::Rc;

use crate::engine_client::{EngineClient, EngineEvent};

pub fn on_activate(app: &adw::Application) {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("FileID")
        .default_width(1280)
        .default_height(840)
        .build();

    // Single shared engine client (single-threaded on the GTK main context).
    let engine = Rc::new(RefCell::new(EngineClient::new()));

    // ── Tabs ─────────────────────────────────────────────────────────────────
    // Six tabs, each a 1:1 port of the macOS reference view, all sharing the one
    // engine client. Page names / icons / titles match the former placeholders
    // so the ViewSwitcher ordering and any deep links are unchanged.
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
    stack.add_titled_with_icon(&settings, Some("settings"), "Settings", "emblem-system-symbolic");

    let switcher = adw::ViewSwitcher::builder()
        .stack(&stack)
        .policy(adw::ViewSwitcherPolicy::Wide)
        .build();

    // ── Header ───────────────────────────────────────────────────────────────
    let header = adw::HeaderBar::builder()
        .css_classes(["fileid-headerbar"])
        .build();
    header.set_title_widget(Some(&switcher));

    let pick_btn = gtk::Button::builder()
        .label("Pick folder")
        .css_classes(["gold-button"])
        .build();
    header.pack_start(&pick_btn);

    let folder_label = gtk::Label::builder()
        .label("No folder selected")
        .css_classes(["dim-label"])
        .build();
    header.pack_start(&folder_label);

    let status_label = gtk::Label::builder()
        .label("Engine: starting…")
        .css_classes(["dim-label"])
        .build();
    header.pack_end(&status_label);

    let start_btn = gtk::Button::builder()
        .label("Start scan")
        .sensitive(false)
        .build();
    header.pack_end(&start_btn);

    // ── Layering: LavaLamp → scrim → UI ──────────────────────────────────────
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&stack));
    toolbar.set_hexpand(true);
    toolbar.set_vexpand(true);

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

    pick_btn.connect_clicked(clone!(
        @weak window, @weak folder_label, @weak start_btn, @strong selected_folder => move |_| {
            let dialog = gtk::FileDialog::builder()
                .title("Pick a folder to organize")
                .modal(true)
                .build();
            dialog.select_folder(Some(&window), gtk::gio::Cancellable::NONE, clone!(
                @weak folder_label, @weak start_btn, @strong selected_folder => move |result| {
                    if let Ok(file) = result {
                        if let Some(path) = file.path() {
                            let display = path.file_name()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_else(|| path.to_string_lossy().into_owned());
                            folder_label.set_label(&display);
                            *selected_folder.borrow_mut() = Some(path.to_string_lossy().into_owned());
                            start_btn.set_sensitive(true);
                        }
                    }
                }
            ));
        }
    ));

    // ── Start scan → engine ──────────────────────────────────────────────────
    start_btn.connect_clicked(clone!(
        @strong engine, @strong selected_folder, @weak status_label => move |_| {
            let Some(folder) = selected_folder.borrow().clone() else { return };
            match engine.borrow_mut().start_scan(&folder, false) {
                Ok(()) => status_label.set_label("Engine: scanning…"),
                Err(err) => status_label.set_label(&format!("scan failed: {err}")),
            }
        }
    ));

    // ── Engine status → header label ─────────────────────────────────────────
    let status_rx = engine.borrow_mut().subscribe();
    glib::MainContext::default().spawn_local(clone!(@weak status_label => async move {
        while let Ok(ev) = status_rx.recv().await {
            let text = match ev {
                EngineEvent::Spawning => "Engine: starting…".to_string(),
                EngineEvent::Ready => "Engine: ready".to_string(),
                EngineEvent::Progress(p) => {
                    format!("Scanning… {} / {}", p.processed, p.total)
                }
                EngineEvent::BatchLanded(n) => format!("Scanning… {n} files"),
                EngineEvent::ScanComplete(n) => format!("Scan complete — {n} files"),
                EngineEvent::Error(m) => format!("Engine: {m}"),
                EngineEvent::Exited => "Engine: restarting…".to_string(),
                // Deep Analyze / Restructure / model-download events are handled
                // by their own tabs — don't clobber the header scan status.
                _ => continue,
            };
            status_label.set_label(&text);
        }
    }));

    // Boot the engine + thumbnail worker + event fan-out pump.
    EngineClient::start(&engine);

    window.present();
}
