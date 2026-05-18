// FileID Linux — gtk4 + libadwaita entrypoint.
//
// Mirror of macOS FileIDApp.swift / Windows App.xaml.cs. Boots an
// adw::Application, registers the shared brand CSS, spawns the
// engine subprocess, and presents the main window.

mod engine_client;
mod window;

use adw::prelude::*;
use gtk::glib;

const APP_ID: &str = "io.github.fileid.FileID";

fn main() -> glib::ExitCode {
    // Local-only structured logging. Same envelope shape as the engine
    // so the two log streams interleave cleanly. NEVER transmits.
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

    app.connect_startup(|_| {
        load_brand_css();
        // Force dark mode regardless of system, matching macOS + Windows.
        // Power-users on a light desktop can override via settings later.
        if let Some(style_manager) = adw::StyleManager::default().into() {
            let sm: adw::StyleManager = style_manager;
            sm.set_color_scheme(adw::ColorScheme::ForceDark);
        }
    });

    app.connect_activate(window::on_activate);

    app.run()
}

/// Inject the FileID brand palette into the GTK CSS provider so the
/// app feels consistent with the macOS + Windows siblings.
/// Gold #FFCC00, lavender #B19BCE, cyan #A0E2EA, pink #F2A6C0.
fn load_brand_css() {
    let css = r#"
        @define-color fileid_gold     #FFCC00;
        @define-color fileid_lavender #B19BCE;
        @define-color fileid_cyan     #A0E2EA;
        @define-color fileid_pink     #F2A6C0;
        @define-color fileid_panel    alpha(white, 0.04);
        @define-color fileid_stroke   alpha(white, 0.10);

        .fileid-glass {
            background-color: @fileid_panel;
            border: 1px solid @fileid_stroke;
            border-radius: 12px;
        }
        .fileid-accent-gold { color: @fileid_gold; }
        .fileid-headerbar {
            background-color: transparent;
        }
    "#;
    let provider = gtk::CssProvider::new();
    provider.load_from_data(css);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}
