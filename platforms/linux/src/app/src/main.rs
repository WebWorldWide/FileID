#![allow(deprecated)]


#![allow(clippy::type_complexity)]

mod app_settings;
mod changes;
mod engine_client;
mod engine_errors;
mod event_queue;
mod fonts;
mod job_replay;
mod lavalamp;
mod model_license;
mod motion;
mod shell;
mod spring;
mod v3;
mod tabs;
mod theme;
mod thumb_cache;
mod welcome;
mod window;

use adw::prelude::*;
use gtk::glib;

const APP_ID: &str = "io.github.fileid.FileID";

fn main() -> glib::ExitCode {
    window::note_launch();

    // Local-only structured logging; NEVER transmits.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();
    fonts::register();

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
