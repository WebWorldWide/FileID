#![allow(deprecated)]

// FileID Linux — gtk4 + libadwaita entrypoint.
//
// Mirror of macOS FileIDApp.swift / Windows App.xaml.cs. Boots an
// adw::Application, installs the shared design system (palette, glass
// surfaces, force-dark), and presents the main window over the LavaLamp
// background. The window spawns the engine subprocess.

#![allow(clippy::type_complexity)]

mod app_settings;
mod engine_client;
mod lavalamp;
mod model_license;
mod spring;
mod tabs;
mod theme;
mod welcome;
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

    app.connect_startup(|_| {
        theme::install();
        // Wayland matches the app_id to the installed .desktop icon; X11/KDE need this.
        gtk::Window::set_default_icon_name(APP_ID);
    });

    app.connect_activate(window::on_activate);
    app.connect_open(|app, files, _| window::on_open(app, files));

    app.run()
}
