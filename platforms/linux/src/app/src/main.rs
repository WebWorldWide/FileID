// FileID Linux — gtk4 + libadwaita entrypoint.
//
// Mirror of macOS FileIDApp.swift / Windows App.xaml.cs. Boots an
// adw::Application, installs the shared brand design system (gold palette +
// glass surfaces + force-dark), and presents the main window (LavaLamp shell +
// all six tabs, 1:1 ports of the macOS views over the shared engine). The engine
// subprocess is spawned by `EngineClient` from the window.

// GTK signal-handler + model closures are inherently tuple-heavy; the engine
// crate allows this lint for the same reason.
#![allow(clippy::type_complexity)]

mod engine_client;
mod lavalamp;
mod model_license;
mod spring;
mod tabs;
mod theme;
mod window;

use adw::prelude::*;
use gtk::glib;

const APP_ID: &str = "io.github.fileid.FileID";

fn main() -> glib::ExitCode {
    // Local-only structured logging. Same envelope shape as the engine so the
    // two log streams interleave cleanly. NEVER transmits.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gtk::gio::ApplicationFlags::HANDLES_OPEN)
        .build();

    // Install the FileID design system (palette CSS + glass classes) and force
    // dark mode, matching the macOS + Windows siblings.
    app.connect_startup(|_| {
        theme::install();
        // Set the default window icon by name so the taskbar/dock shows the
        // FileID icon. On Wayland the compositor also matches the window's
        // `app_id` (== APP_ID) to the installed `io.github.fileid.FileID.desktop`
        // → `Icon=`; this line covers X11 / KDE and CSD title-bar icons too.
        // Requires the icon installed in the hicolor theme (see
        // `platforms/linux/data/io.github.fileid.FileID.svg` + build/install).
        gtk::Window::set_default_icon_name(APP_ID);
    });
    app.connect_activate(window::on_activate);
    app.connect_open(|app, files, _| window::on_open(app, files));

    app.run()
}
