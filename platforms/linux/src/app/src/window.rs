// Main window — minimal scaffold. Mirror of macOS ContentView /
// Windows MainWindow. Phase 1 lands the 6 tabs (Library, People,
// Cleanup, Deep Analyze, Restructure, Settings) as adw::NavigationPage
// stacks. Today: HeaderBar + sidebar placeholder + main pane placeholder
// + "Pick folder" + "Start scan" hooked up to the engine.

use adw::prelude::*;
use gtk::glib::clone;
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;

use crate::engine_client::{EngineClient, EngineState};

pub fn on_activate(app: &adw::Application) {
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
        .css_classes(["dim-label"])
        .build();
    header.set_title_widget(Some(&folder_label));

    let pick_btn = gtk::Button::builder()
        .label("Pick folder")
        .css_classes(["suggested-action"])
        .build();
    header.pack_start(&pick_btn);

    let start_btn = gtk::Button::builder()
        .label("Start scan")
        .sensitive(false)
        .build();
    header.pack_end(&start_btn);

    let status_label = gtk::Label::builder()
        .label("Engine: spawning…")
        .css_classes(["caption"])
        .build();
    header.pack_end(&status_label);

    // Main content area. Placeholder for the tab navigation that lands
    // in Phase 1. Six adw::NavigationPage children, one per tab.
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["fileid-glass"])
        .margin_top(16)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .build();

    let placeholder = adw::StatusPage::builder()
        .icon_name("folder-symbolic")
        .title("FileID for Linux")
        .description("Phase 0 scaffold. Library / People / Cleanup / Deep Analyze / Restructure / Settings tabs land in Phase 1. Engine is shared with the Windows port.")
        .build();
    content.append(&placeholder);

    let root = adw::ToolbarView::new();
    root.add_top_bar(&header);
    root.set_content(Some(&content));
    window.set_content(Some(&root));

    let selected_folder: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    // Pick folder → GTK native FileDialog (folder mode).
    pick_btn.connect_clicked(clone!(
        @weak window, @weak folder_label, @weak start_btn, @strong selected_folder
        => move |_| {
            let dialog = gtk::FileDialog::builder()
                .title("Pick a folder to organize")
                .modal(true)
                .build();
            dialog.select_folder(Some(&window), gtk::gio::Cancellable::NONE, clone!(
                @weak folder_label, @weak start_btn, @strong selected_folder
                => move |result| {
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

    // Start scan → IPC startScan to the engine.
    start_btn.connect_clicked(clone!(
        @strong engine, @strong selected_folder, @weak status_label
        => move |_| {
            let Some(folder) = selected_folder.borrow().clone() else { return; };
            let mut e = engine.borrow_mut();
            match e.start_scan(&folder) {
                Ok(()) => status_label.set_label("Engine: scanning…"),
                Err(err) => status_label.set_label(&format!("scan failed: {err}")),
            }
        }
    ));

    // Spawn the engine + poll its state events back into the status label.
    // EngineClient pushes events through an async_channel; we pump from
    // the GTK main context so UI updates stay single-threaded.
    let rx = engine.borrow_mut().spawn();
    glib::MainContext::default().spawn_local(clone!(
        @weak status_label => async move {
            while let Ok(state) = rx.recv().await {
                let label = match state {
                    EngineState::Spawning => "Engine: spawning…".to_string(),
                    EngineState::Ready    => "Engine: ready".to_string(),
                    EngineState::Scanning => "Engine: scanning…".to_string(),
                    EngineState::Done(n)  => format!("Scan complete — {n} files"),
                    EngineState::Failed(m)=> format!("Engine: {m}"),
                };
                status_label.set_label(&label);
            }
        }
    ));

    window.present();
}
