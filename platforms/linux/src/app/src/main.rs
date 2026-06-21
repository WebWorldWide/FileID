// FileID Linux — gtk4 + libadwaita entrypoint.
//
// Mirror of macOS FileIDApp.swift / Windows App.xaml.cs. Boots an
// adw::Application, installs the shared brand design system (gold palette +
// glass surfaces + force-dark), and presents the main window (LavaLamp shell +
// six tabs; Library implemented, the rest placeholders). The engine subprocess
// is spawned by `EngineClient` from the window.

mod engine_client;
mod lavalamp;
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
        .build();

    // Install the FileID design system (palette CSS + glass classes) and force
    // dark mode, matching the macOS + Windows siblings.
    app.connect_startup(|_| theme::install());
    app.connect_activate(window::on_activate);

    app.run()
}
