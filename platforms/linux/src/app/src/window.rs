#![allow(deprecated)]

// Library and People share the engine connection with scan controls.

use adw::prelude::*;
use gtk::glib::clone;
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::engine_client::{EngineClient, EngineState};

pub fn on_activate(app: &adw::Application) {
    if let Some(window) = app.active_window() {
        window.present();
        return;
    }
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("FileID")
        .default_width(1200)
        .default_height(800)
        .build();

    // Single shared EngineClient. Wrapped in Rc<RefCell<>> for closure
    // capture; GTK is single-threaded on the main context.
    let engine = Rc::new(RefCell::new(EngineClient::new()));

    let header = adw::HeaderBar::builder().css_classes(["fileid-headerbar"]).build();

    // Folder display label on the left of the title — same idea as
    // macOS SidebarFolderHeader / Windows SidebarFolderHeader.
    let folder_label = gtk::Label::builder()
        .label("No folder selected")
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(32)
        .css_classes(["dim-label"])
        .build();
    header.set_title_widget(Some(&folder_label));

    let pick_btn = gtk::Button::builder()
        .label("Pick folder")
        .css_classes(["suggested-action"])
        .build();
    header.pack_start(&pick_btn);
    let pages = gtk::Stack::new();
    pages.set_hexpand(true);
    pages.set_vexpand(true);
    let switcher = gtk::StackSwitcher::new();
    switcher.set_stack(Some(&pages));
    header.pack_start(&switcher);

    let start_btn = gtk::Button::builder()
        .label("Start scan")
        .sensitive(false)
        .build();
    header.pack_end(&start_btn);

    let status_label = gtk::Label::builder()
        .label("Engine: spawning…")
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(40)
        .css_classes(["caption"])
        .build();
    header.pack_end(&status_label);

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["fileid-glass"])
        .margin_top(16)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .build();
    pages.add_titled(&crate::tabs::library::build(engine.clone()), Some("library"), "Library");
    pages.add_titled(&crate::tabs::people::build(engine.clone()), Some("people"), "People");
    pages.add_titled(&crate::tabs::cleanup::build_cleanup_tab(engine.clone()), Some("cleanup"), "Cleanup");
    pages.add_titled(
        &crate::tabs::deep_analyze::build_deep_analyze_tab(engine.clone()),
        Some("deep-analyze"),
        "Deep Analyze",
    );
    pages.add_titled(
        &crate::tabs::restructure::build_restructure_tab(engine.clone()),
        Some("restructure"),
        "Restructure",
    );
    pages.add_titled(&crate::tabs::settings::build(engine.clone()), Some("settings"), "Settings");
    let valid_tabs = ["library", "people", "cleanup", "deep-analyze", "restructure", "settings"];
    let active_tab = crate::app_settings::active_tab()
        .filter(|tab| valid_tabs.contains(&tab.as_str()))
        .unwrap_or_else(|| "library".to_owned());
    pages.set_visible_child_name(&active_tab);
    pages.connect_notify_local(Some("visible-child-name"), |stack, _| {
        if let Some(name) = stack.visible_child_name() {
            crate::app_settings::remember_active_tab(name.as_str());
        }
    });
    content.append(&pages);

    let root = adw::ToolbarView::new();
    root.add_top_bar(&header);
    root.set_content(Some(&content));
    window.set_content(Some(&root));

    let restored_folder = crate::app_settings::last_folder();
    if let Some(folder) = restored_folder.as_ref() {
        let display = folder
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| folder.to_string_lossy().into_owned());
        folder_label.set_label(&display);
    }
    let selected_folder: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(
        restored_folder.map(|folder| folder.to_string_lossy().into_owned()),
    ));
    let can_scan = Rc::new(Cell::new(false));

    // Pick folder → GTK native FileDialog (folder mode).
    pick_btn.connect_clicked(clone!(
        #[weak] window, #[weak] folder_label, #[weak] start_btn,
        #[strong] selected_folder, #[strong] can_scan,
        move |_| {
            let dialog = gtk::FileDialog::builder()
                .title("Pick a folder to organize")
                .modal(true)
                .build();
            dialog.select_folder(Some(&window), gtk::gio::Cancellable::NONE, clone!(
                #[weak] folder_label, #[weak] start_btn,
                #[strong] selected_folder, #[strong] can_scan,
                move |result| {
                    if let Ok(file) = result {
                        if let Some(path) = file.path() {
                            let display = path.file_name()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_else(|| path.to_string_lossy().into_owned());
                            folder_label.set_label(&display);
                            crate::app_settings::remember_folder(&path);
                            *selected_folder.borrow_mut() = Some(path.to_string_lossy().into_owned());
                    start_btn.set_sensitive(can_scan.get());
                        }
                    }
                }
            ));
        }
    ));

    // Start scan → IPC startScan to the engine.
    start_btn.connect_clicked(clone!(
        #[strong] engine, #[strong] selected_folder, #[strong] can_scan,
        #[weak] start_btn, #[weak] status_label,
        move |_| {
            let Some(folder) = selected_folder.borrow().clone() else { return; };
            let mut e = engine.borrow_mut();
            match e.start_scan(&folder) {
                Ok(()) => {
                    can_scan.set(false);
                    start_btn.set_sensitive(false);
                    status_label.set_label("Engine: scanning…");
                }
                Err(err) => {
                    can_scan.set(false);
                    start_btn.set_sensitive(false);
                    status_label.set_label(&format!("scan failed: {err}"));
                }
            }
        }
    ));

    // Spawn the engine + poll its state events back into the status label.
    // EngineClient pushes events through an async_channel; we pump from
    // the GTK main context so UI updates stay single-threaded.
    let rx = engine.borrow_mut().spawn();
    glib::MainContext::default().spawn_local(clone!(
        #[weak] status_label, #[weak] start_btn, #[weak] pages,
        #[strong] selected_folder, #[strong] can_scan,
        async move {
        while let Ok(state) = rx.recv().await {
            if matches!(&state, EngineState::ModelDownloadProgress(_)
                | EngineState::Error { model_kind: Some(_), .. }
                | EngineState::FaceClusteringComplete(_)
                | EngineState::FaceClusteringFailed(_)
                | EngineState::FaceClusteringBusy(_)
                | EngineState::BulkActionResult(_)
                | EngineState::MergeSuggestions(_)
                | EngineState::DeepAnalyzeStarting(_)
                | EngineState::DeepAnalyzeProgress(_)
                | EngineState::DeepAnalyzeFileDone(_)
                | EngineState::DeepAnalyzeComplete(_)
                | EngineState::RestructurePlan(_)
                | EngineState::RestructureApplyResult(_)) {
                continue;
            }
            let available = matches!(&state, EngineState::Ready | EngineState::ScanComplete(_) | EngineState::Error { .. });
            let label = match state {
                EngineState::Spawning => "Engine: spawning…".to_string(),
                EngineState::Ready => "Engine: ready".to_string(),
                EngineState::Scanning => "Engine: scanning…".to_string(),
                EngineState::BatchLanded(n) => format!("Engine: scanning… {n} files processed"),
                EngineState::ScanComplete(n) => format!("Scan complete — {n} files processed"),
                EngineState::Error { kind, message, .. } if kind == "models_not_installed" => {
                    pages.set_visible_child_name("settings");
                    let missing = message.split_once("Missing:").map(|(_, kinds)| kinds.trim())
                        .unwrap_or("required scan models");
                    format!("Last scan blocked: {missing} Install missing bundles in Settings if needed, then return to Library and retry.")
                }
                EngineState::Error { message, .. } => format!("Engine: {message}"),
                EngineState::ModelDownloadProgress(_) => continue,
                EngineState::FaceClusteringComplete(_)
                | EngineState::FaceClusteringFailed(_)
                | EngineState::FaceClusteringBusy(_)
                | EngineState::BulkActionResult(_)
                | EngineState::MergeSuggestions(_)
                | EngineState::DeepAnalyzeStarting(_)
                | EngineState::DeepAnalyzeProgress(_)
                | EngineState::DeepAnalyzeFileDone(_)
                | EngineState::DeepAnalyzeComplete(_)
                | EngineState::RestructurePlan(_)
                | EngineState::RestructureApplyResult(_) => continue,
                EngineState::Failed(message) => format!("Engine: {message}"),
                EngineState::Exited => "Engine: exited".to_string(),
            };
            can_scan.set(available);
            start_btn.set_sensitive(can_scan.get() && selected_folder.borrow().is_some());
            status_label.set_label(&label);
            status_label.set_tooltip_text(Some(&label));
        }
            }
    ));

    window.present();
    if crate::welcome::should_show() {
        crate::welcome::present(&window, engine.clone());
    }
}
