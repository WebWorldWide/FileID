// Welcome sheet — the Linux port of the Windows/macOS first-launch onboarding
// modal. Presents the five core on-device models with live install state, a
// one-click "Install everything" that fans out `prewarmModel` for whatever is
// missing, and the machine-sized Deep Analyze VLM recommendation.
//
// Shown when any core model is missing, or on the very first launch
// (`welcomeSheetSeen`, same key as Windows). Dismissable at any time — the
// same installs remain available from Settings.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;

use crate::engine_client::{EngineClient, EngineEvent};
use fileid_engine::ipc::{CommandPayload, PrewarmModelPayload};
use fileid_engine::models::registry::{self, LookupResult};

/// The always-on scan stack, in install order. VLMs are opt-in via Deep
/// Analyze and deliberately not part of the welcome install.
const CORE_MODELS: &[(&str, &str)] = &[
    ("mobileclip_s2", "Search photos by what's in them"),
    ("clip_text", "Understands your typed searches"),
    ("ram_plus", "Recognizes 4585 everyday tags"),
    ("bge_text", "Reads documents for Restructure"),
    ("arcface", "Finds and groups the people in photos"),
];

struct WelcomeRow {
    kind: &'static str,
    state: gtk::Label,
    bar: gtk::ProgressBar,
}

fn model_info(kind: &str) -> Option<(String, u64, bool)> {
    match registry::lookup_full(kind) {
        LookupResult::Found(model) => {
            let bytes: u64 = model.files.iter().map(|f| f.approx_bytes).sum();
            let installed = registry::installation_complete(&model);
            Some((model.display_name.to_string(), bytes, installed))
        }
        LookupResult::Unknown => None,
    }
}

fn missing_core_models() -> Vec<&'static str> {
    CORE_MODELS
        .iter()
        .filter(|(kind, _)| matches!(model_info(kind), Some((_, _, false))))
        .map(|(kind, _)| *kind)
        .collect()
}

pub fn should_show() -> bool {
    !missing_core_models().is_empty() || !crate::app_settings::welcome_sheet_seen()
}

fn fmt_gb(bytes: u64) -> String {
    let gb = bytes as f64 / 1_073_741_824.0;
    if gb >= 1.0 {
        format!("{gb:.1} GB")
    } else {
        format!("{:.0} MB", bytes as f64 / 1_048_576.0)
    }
}

pub fn present(parent: &adw::ApplicationWindow, engine: Rc<RefCell<EngineClient>>) {
    let dialog = adw::Dialog::new();
    dialog.set_title("Welcome to FileID");
    dialog.set_content_width(560);

    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(14)
        .margin_top(20)
        .margin_bottom(20)
        .margin_start(24)
        .margin_end(24)
        .build();

    let title = gtk::Label::builder()
        .label("Welcome to FileID")
        .xalign(0.0)
        .css_classes(["title-2", "gold-accent"])
        .build();
    body.append(&title);
    body.append(
        &gtk::Label::builder()
            .label(
                "Your files never leave this machine. FileID's AI runs entirely on-device — \
                 the only download is the models themselves, once, from huggingface.co.",
            )
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label"])
            .build(),
    );

    let list = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .css_classes(["glass-card"])
        .build();
    let mut rows: Vec<WelcomeRow> = Vec::new();
    let mut missing_bytes = 0u64;
    for (kind, blurb) in CORE_MODELS {
        let Some((display, bytes, installed)) = model_info(kind) else {
            continue;
        };
        if !installed {
            missing_bytes += bytes;
        }

        let name = gtk::Label::builder()
            .label(&display)
            .xalign(0.0)
            .css_classes(["heading"])
            .build();
        let caption = gtk::Label::builder()
            .label(*blurb)
            .xalign(0.0)
            .css_classes(["tile-caption"])
            .build();
        let text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(1)
            .hexpand(true)
            .build();
        text.append(&name);
        text.append(&caption);

        let state = gtk::Label::builder()
            .label(if installed {
                "✓ Installed".to_string()
            } else {
                fmt_gb(bytes)
            })
            .xalign(1.0)
            .valign(gtk::Align::Center)
            .css_classes(if installed {
                ["gold-accent"]
            } else {
                ["dim-label"]
            })
            .build();

        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(10)
            .build();
        row.append(&text);
        row.append(&state);

        let bar = gtk::ProgressBar::builder().visible(false).build();
        let cell = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(3)
            .build();
        cell.append(&row);
        cell.append(&bar);
        list.append(&cell);

        rows.push(WelcomeRow { kind, state, bar });
    }
    body.append(&list);

    // Machine-sized Deep Analyze recommendation (display-only pointer; VLMs
    // stay opt-in from the Deep Analyze tab, exactly like Windows).
    let recommended = crate::tabs::deep_analyze::recommended_vlm_display();
    body.append(
        &gtk::Label::builder()
            .label(format!(
                "Optional later: Deep Analyze writes full captions and smart filenames. \
                 For this machine we recommend {recommended} — install it any time from the \
                 Deep Analyze tab."
            ))
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label", "caption"])
            .build(),
    );

    let status = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .visible(false)
        .css_classes(["dim-label", "caption"])
        .build();
    body.append(&status);

    let install_btn = gtk::Button::builder()
        .label(format!("Install everything ({})", fmt_gb(missing_bytes)))
        .visible(missing_bytes > 0)
        .css_classes(["gold-button"])
        .build();
    let later_btn = gtk::Button::builder()
        .label(if missing_bytes > 0 {
            "Not now"
        } else {
            "Start exploring"
        })
        .build();
    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(10)
        .halign(gtk::Align::End)
        .build();
    buttons.append(&later_btn);
    buttons.append(&install_btn);
    body.append(&buttons);

    let toolbar = adw::ToolbarView::new();
    // The body carries its own gold title; a header title would duplicate it.
    let header = adw::HeaderBar::new();
    header.set_show_title(false);
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&body));
    dialog.set_child(Some(&toolbar));

    dialog.connect_closed(|_| {
        crate::app_settings::remember_welcome_sheet_seen();
    });
    {
        let dialog = dialog.clone();
        later_btn.connect_clicked(move |_| {
            dialog.close();
        });
    }

    let rows = Rc::new(rows);
    {
        let engine = engine.clone();
        let rows = rows.clone();
        let status = status.clone();
        install_btn.connect_clicked(move |btn| {
            let missing = missing_core_models();
            if missing.is_empty() {
                btn.set_sensitive(false);
                return;
            }
            let mut sent = 0usize;
            for kind in &missing {
                let ok = engine
                    .borrow_mut()
                    .send(CommandPayload::PrewarmModel(PrewarmModelPayload {
                        model_kind: (*kind).to_string(),
                    }))
                    .is_ok();
                if ok {
                    sent += 1;
                }
            }
            if sent == 0 {
                status.set_text("Engine is still starting — try again in a moment.");
                status.set_visible(true);
                return;
            }
            btn.set_sensitive(false);
            btn.set_label("Installing…");
            status.set_text(
                "Downloading from huggingface.co. You can close this window — installs \
                 continue and Settings shows the same progress.",
            );
            status.set_visible(true);
            for row in rows.iter() {
                if missing.contains(&row.kind) {
                    row.state.set_text("Queued…");
                    row.bar.set_visible(true);
                    row.bar.set_fraction(0.0);
                }
            }
        });
    }

    // Live progress: engine events drive the per-model rows; a bounded disk
    // re-check flips rows to Installed even if a terminal event is missed.
    let by_kind: Rc<HashMap<&'static str, usize>> = Rc::new(
        rows.iter()
            .enumerate()
            .map(|(index, row)| (row.kind, index))
            .collect(),
    );
    let rx = engine.borrow_mut().subscribe();
    {
        let rows = rows.clone();
        let by_kind = by_kind.clone();
        let dialog_weak = dialog.downgrade();
        glib::MainContext::default().spawn_local(async move {
            while let Ok(event) = rx.recv().await {
                if dialog_weak.upgrade().is_none() {
                    break;
                }
                match event {
                    EngineEvent::ModelDownloadProgress(progress) => {
                        if let Some(&index) = by_kind.get(progress.model_kind.as_str()) {
                            let row = &rows[index];
                            row.bar.set_visible(true);
                            row.bar.set_fraction(progress.fraction.clamp(0.0, 1.0));
                            row.state.set_text(&progress.message);
                            if progress.fraction >= 1.0 {
                                mark_row(row, true);
                            }
                        }
                    }
                    EngineEvent::ModelDownloadFailed {
                        model_kind,
                        message,
                    } => {
                        if let Some(&index) = by_kind.get(model_kind.as_str()) {
                            let row = &rows[index];
                            row.bar.set_visible(false);
                            row.state.set_text(&format!("Failed: {message}"));
                        }
                    }
                    _ => {}
                }
            }
        });
    }
    {
        let rows = rows.clone();
        let dialog_weak = dialog.downgrade();
        glib::timeout_add_local(Duration::from_millis(1500), move || {
            if dialog_weak.upgrade().is_none() {
                return glib::ControlFlow::Break;
            }
            for row in rows.iter() {
                if let Some((_, _, true)) = model_info(row.kind) {
                    mark_row(row, true);
                }
            }
            glib::ControlFlow::Continue
        });
    }

    dialog.present(Some(parent));
}

fn mark_row(row: &WelcomeRow, installed: bool) {
    if installed {
        row.bar.set_visible(false);
        row.state.set_text("✓ Installed");
        row.state.set_css_classes(&["gold-accent"]);
    }
}
