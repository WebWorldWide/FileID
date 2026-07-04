// Settings tab — the 1:1 GTK port of macOS `SettingsView.swift` (the
// "AI models · engine info · logs · privacy" page).
//
// Sections, each a `.glass-card`:
//   * AI models — one card per installable model slot. State is read off
//     disk via the engine crate's own `models::registry` (the canonical
//     source of every model's files + sizes, so the contract can't drift).
//     "Download" sends `prewarmModel` (the engine fetches + warms the model,
//     mirroring Windows); "Cancel" sends `cancelPrewarm`; "Remove" deletes the
//     on-disk artifacts. Progress is polled off disk (see the NOTE on the
//     missing `modelDownloadProgress` event surface).
//   * Engine — connection status (live, from the event stream) + Restart.
//   * Storage — total files, images, DB path/size, Models dir (DB read).
//   * Recent scans — the `scan_sessions` table (DB read).
//   * Logs — a tail of the engine's rolling `engine.jsonl` log.
//   * Privacy — the no-telemetry guarantee.
//
// Mirrors library.rs: exported `build(engine: Rc<RefCell<EngineClient>>)`,
// engine-event subscription on the main context, direct read-only DB access
// via `fileid_engine::db::open_read` + `paths`, gold-palette glass cards, and
// a springy reveal on the shared brand spring.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use gtk::glib::clone;

use crate::engine_client::{EngineClient, EngineEvent};
use super::util::glass_card;
use fileid_engine::ipc::{CancelPrewarmPayload, CommandPayload, Empty, PrewarmModelPayload};

/// Installable model slots, in macOS card order. Each `model_kind` is resolved
/// against `fileid_engine::models::registry` for its display name, files, and
/// sizes — the same registry the engine downloads from.
const MODEL_SLOTS: &[(&str, &str)] = &[
    (
        "mobileclip_s2",
        "Type natural-language searches like \"sunset at the beach\" and FileID ranks every photo by visual relevance. OpenCLIP ViT-B/32 image encoder (MIT) — runs entirely on-device.",
    ),
    (
        "clip_text",
        "The CLIP text encoder + BPE vocabulary that pairs with the image encoder so your typed query and your photos land in the same space.",
    ),
    (
        "ram_plus",
        "RAM++ recognizes 4585 everyday tags on-device — far richer than the built-in classifier. Apache-2.0, one click, no Python. Without it, tagging uses the lighter built-in classifier.",
    ),
    (
        "bge_text",
        "BGE-small reads a document's content so Restructure groups files by what they say, not their filename (a physics paper joins your physics folder). MIT. Without it, documents group by filename.",
    ),
    (
        "arcface",
        "YuNet detection (MIT) + SFace recognition (Apache-2.0, 128-d) cluster the same person across your library, powering the People tab. Without it, no faces are grouped.",
    ),
    (
        "qwen2_5_vl_7b",
        "Deep Analyze VLM (default): on-device captions + smart filenames. Qwen2.5-VL 7B, Apache-2.0. ~6 GB.",
    ),
    (
        "gemma_3_4b",
        "Deep Analyze VLM: lighter + faster captions + smart filenames. Gemma 3 4B. ~3.3 GB.",
    ),
    (
        "mistral_small_3_2",
        "Deep Analyze VLM: max-quality captions + smart filenames. Mistral-Small 3.2 24B, Apache-2.0. ~15 GB.",
    ),
];

const LOG_TAIL_LINES: usize = 200;

pub fn build(engine: Rc<RefCell<EngineClient>>) -> gtk::Widget {
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .margin_top(16)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .css_classes(["fileid-tab"])
        .build();

    root.append(
        &gtk::Label::builder()
            .label("Settings")
            .xalign(0.0)
            .css_classes(["title-1"])
            .build(),
    );

    // ── AI models ────────────────────────────────────────────────────────────
    root.append(&section_label("AI models"));
    root.append(
        &gtk::Label::builder()
            .label("Everything runs on this device. Models download once, directly from huggingface.co — the only network FileID ever touches.")
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label"])
            .build(),
    );
    for (kind, blurb) in MODEL_SLOTS {
        root.append(&build_model_card(&engine, kind, blurb));
    }

    // ── Engine · Storage · Scans · Logs · Privacy ────────────────────────────
    root.append(&section_label("Engine & storage"));
    root.append(&build_engine_card(&engine));
    root.append(&build_storage_card(&engine));
    root.append(&build_recent_scans_card(&engine));
    root.append(&build_logs_card());
    root.append(&build_privacy_card());

    // Constrain to a centered column (macOS/Windows settings are NOT full-width).
    let clamp = adw::Clamp::builder()
        .maximum_size(820)
        .tightening_threshold(680)
        .child(&root)
        .build();

    let scroller = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&clamp)
        .css_classes(["fileid-tab"])
        .build();

    // Springy reveal on the shared brand spring (macOS parity).
    root.set_opacity(0.0);
    let root_weak = root.downgrade();
    let _ = crate::spring::animate(&root, 0.0, 1.0, move |v| {
        if let Some(r) = root_weak.upgrade() {
            r.set_opacity(v);
        }
    });

    scroller.upcast()
}

// ── AI model card ──────────────────────────────────────────────────────────

/// One installable model slot. Resolves its files from the engine registry,
/// renders an install-state-aware footer, and drives download / cancel /
/// remove. State recomputes from disk (the registry's `dest` paths), so a
/// model installed out-of-band (CLI, another platform) is detected too.
fn build_model_card(engine: &Rc<RefCell<EngineClient>>, kind: &'static str, blurb: &str) -> gtk::Widget {
    let (display_name, files) = resolve_slot(kind);
    let total_bytes: u64 = files.iter().map(|(_, b)| *b).sum();

    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .margin_top(14)
        .margin_bottom(14)
        .margin_start(16)
        .margin_end(16)
        .css_classes(["glass-card"])
        .build();

    card.append(
        &gtk::Label::builder()
            .label(&display_name)
            .xalign(0.0)
            .css_classes(["heading"])
            .build(),
    );
    card.append(
        &gtk::Label::builder()
            .label(blurb)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label"])
            .build(),
    );

    // Per-file paths (static; the footer carries live status).
    for (path, _) in &files {
        card.append(
            &gtk::Label::builder()
                .label(path.display().to_string())
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .single_line_mode(true)
                .css_classes(["dim-label", "monospace"])
                .build(),
        );
    }

    card.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let footer = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .build();
    card.append(&footer);

    let downloading = Rc::new(Cell::new(false));

    // Self-referential render closure: button handlers re-trigger it through
    // the shared cell so the footer rebuilds on each state change. Same pattern
    // as library.rs's `reload: Rc<dyn Fn()>`, extended to re-entrancy.
    let render_cell: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let render: Rc<dyn Fn()> = {
        let self_cell = render_cell.clone();
        let engine = engine.clone();
        let footer = footer.clone();
        let files = files.clone();
        let downloading = downloading.clone();
        Rc::new(move || {
            // Clear the previous footer.
            while let Some(child) = footer.first_child() {
                footer.remove(&child);
            }

            if files.is_empty() {
                footer.append(
                    &gtk::Label::builder()
                        .label("Model registry unavailable on this build.")
                        .xalign(0.0)
                        .css_classes(["dim-label"])
                        .build(),
                );
                return;
            }

            let on_disk: u64 = files
                .iter()
                .filter_map(|(p, _)| std::fs::metadata(p).ok().map(|m| m.len()))
                .sum();
            let installed = files.iter().all(|(p, _)| p.exists());

            if installed {
                downloading.set(false);
                let row = gtk::Box::builder()
                    .orientation(gtk::Orientation::Horizontal)
                    .spacing(8)
                    .build();
                row.append(
                    &gtk::Label::builder()
                        .label(format!("✓ Installed · {} on disk", fmt_bytes(on_disk)))
                        .xalign(0.0)
                        .hexpand(true)
                        .css_classes(["gold-accent"])
                        .build(),
                );
                let open_btn = gtk::Button::with_label("Show files");
                if let Some(dir) = files.first().and_then(|(p, _)| p.parent().map(|d| d.to_path_buf())) {
                    open_btn.connect_clicked(move |_| open_path(&dir));
                }
                row.append(&open_btn);
                let remove_btn = gtk::Button::builder()
                    .label("Remove")
                    .css_classes(["destructive-action"])
                    .build();
                {
                    let again = self_cell.clone();
                    let files = files.clone();
                    remove_btn.connect_clicked(move |_| {
                        for (p, _) in files.iter() {
                            let _ = std::fs::remove_file(p);
                        }
                        rerender(&again);
                    });
                }
                row.append(&remove_btn);
                footer.append(&row);
            } else if downloading.get() {
                let frac = if total_bytes > 0 {
                    (on_disk as f64 / total_bytes as f64).clamp(0.0, 0.999)
                } else {
                    0.0
                };
                let bar = gtk::ProgressBar::new();
                bar.set_fraction(frac);
                bar.set_show_text(true);
                bar.set_text(Some(&format!(
                    "Downloading… {} / {}",
                    fmt_bytes(on_disk),
                    fmt_bytes(total_bytes)
                )));
                footer.append(&bar);

                let row = gtk::Box::builder()
                    .orientation(gtk::Orientation::Horizontal)
                    .spacing(8)
                    .build();
                row.append(
                    &gtk::Label::builder()
                        .label("Fetching from huggingface.co — keep this window open.")
                        .xalign(0.0)
                        .hexpand(true)
                        .wrap(true)
                        .css_classes(["dim-label"])
                        .build(),
                );
                let cancel_btn = gtk::Button::with_label("Cancel");
                {
                    let again = self_cell.clone();
                    let engine = engine.clone();
                    let downloading = downloading.clone();
                    cancel_btn.connect_clicked(move |_| {
                        let _ = engine.borrow_mut().send(CommandPayload::CancelPrewarm(
                            CancelPrewarmPayload { model_kind: Some(kind.to_string()) },
                        ));
                        downloading.set(false);
                        rerender(&again);
                    });
                }
                row.append(&cancel_btn);
                footer.append(&row);
            } else {
                let row = gtk::Box::builder()
                    .orientation(gtk::Orientation::Horizontal)
                    .spacing(8)
                    .build();
                let label = if total_bytes > 0 {
                    format!("Download (~{})", fmt_bytes(total_bytes))
                } else {
                    "Download".to_string()
                };
                let dl_btn = gtk::Button::builder()
                    .label(&label)
                    .css_classes(["gold-button"])
                    .build();
                {
                    let again = self_cell.clone();
                    let engine = engine.clone();
                    let downloading = downloading.clone();
                    let files = files.clone();
                    dl_btn.connect_clicked(move |_| {
                        let _ = engine.borrow_mut().send(CommandPayload::PrewarmModel(
                            PrewarmModelPayload { model_kind: kind.to_string() },
                        ));
                        downloading.set(true);
                        rerender(&again);
                        // Poll disk for progress + completion until done/cancelled.
                        // NOTE: would be event-driven if EngineClient surfaced
                        // `modelDownloadProgress` (see return summary).
                        let again = again.clone();
                        let downloading = downloading.clone();
                        let files = files.clone();
                        glib::timeout_add_local(Duration::from_millis(1200), move || {
                            if files.iter().all(|(p, _)| p.exists()) {
                                downloading.set(false);
                            }
                            rerender(&again);
                            if downloading.get() {
                                glib::ControlFlow::Continue
                            } else {
                                glib::ControlFlow::Break
                            }
                        });
                    });
                }
                row.append(&dl_btn);
                footer.append(&row);
            }
        })
    };
    *render_cell.borrow_mut() = Some(render.clone());
    render();

    card.upcast()
}

/// Re-run the stored render closure without holding a borrow across the call.
fn rerender(cell: &Rc<RefCell<Option<Rc<dyn Fn()>>>>) {
    let f = cell.borrow().clone();
    if let Some(f) = f {
        f();
    }
}

/// Resolve a `model_kind` into its display name + (dest path, approx bytes)
/// list via the engine's canonical model registry.
fn resolve_slot(kind: &str) -> (String, Vec<(PathBuf, u64)>) {
    use fileid_engine::models::registry::{lookup_full, LookupResult};
    match lookup_full(kind) {
        LookupResult::Found(model) => (
            model.display_name.to_string(),
            model.files.iter().map(|f| (f.dest.clone(), f.approx_bytes)).collect(),
        ),
        LookupResult::Unknown => (kind.to_string(), Vec::new()),
    }
}

// ── Engine card ──────────────────────────────────────────────────────────────

fn build_engine_card(engine: &Rc<RefCell<EngineClient>>) -> gtk::Widget {
    let card = glass_card();

    card.append(
        &gtk::Label::builder()
            .label("Engine")
            .xalign(0.0)
            .css_classes(["heading"])
            .build(),
    );

    let status = gtk::Label::builder()
        .label("Status: starting…")
        .xalign(0.0)
        .css_classes(["dim-label", "monospace"])
        .build();
    card.append(&status);

    // Live status from the engine event stream (mirrors window.rs).
    let rx = engine.borrow_mut().subscribe();
    glib::MainContext::default().spawn_local(clone!(@weak status => async move {
        while let Ok(ev) = rx.recv().await {
            let text = match ev {
                EngineEvent::Spawning => "Status: starting…".to_string(),
                EngineEvent::Ready => "Status: ready".to_string(),
                EngineEvent::Progress(p) => format!("Status: scanning… {} / {}", p.processed, p.total),
                EngineEvent::BatchLanded(n) => format!("Status: scanning… {n} files"),
                EngineEvent::ScanComplete(n) => format!("Status: ready — last scan {n} files"),
                EngineEvent::Error(m) => format!("Status: {m}"),
                EngineEvent::Exited => "Status: restarting…".to_string(),
                // Deep Analyze / Restructure / model-download events are driven
                // by their own subscribers — don't clobber the engine status.
                _ => continue,
            };
            status.set_label(&text);
        }
    }));

    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();

    let restart = gtk::Button::with_label("Restart engine");
    restart.connect_clicked(clone!(@strong engine, @weak status => move |_| {
        // Shutdown → clean exit → EngineClient's fan-out pump auto-respawns.
        let _ = engine.borrow_mut().send(CommandPayload::Shutdown(Empty {}));
        status.set_label("Status: restarting…");
    }));
    row.append(&restart);

    let refresh = gtk::Button::with_label("Refresh status");
    refresh.connect_clicked(clone!(@strong engine => move |_| {
        // RequestStatus re-emits `ready`, refreshing the label above.
        let _ = engine.borrow_mut().send(CommandPayload::RequestStatus(Empty {}));
    }));
    row.append(&refresh);

    card.append(&row);
    card.append(
        &gtk::Label::builder()
            .label("Restarting spawns a fresh engine process and cancels any in-flight scan.")
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label"])
            .build(),
    );

    card.upcast()
}

// ── Storage card ─────────────────────────────────────────────────────────────

fn build_storage_card(engine: &Rc<RefCell<EngineClient>>) -> gtk::Widget {
    let card = glass_card();
    card.append(
        &gtk::Label::builder()
            .label("Storage")
            .xalign(0.0)
            .css_classes(["heading"])
            .build(),
    );

    let total = info_row("Total files", "—");
    let images = info_row("Images", "—");
    let dbsize = info_row("Database size", "—");
    let dbpath = info_row("Database", "—");
    let models = info_row("Models folder", "—");
    for r in [&total, &images, &dbsize, &dbpath, &models] {
        card.append(r);
    }

    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();

    // Populate (off the main loop) — reusable so "Refresh" repeats it.
    let populate: Rc<dyn Fn()> = {
        let total = total.clone();
        let images = images.clone();
        let dbsize = dbsize.clone();
        let dbpath = dbpath.clone();
        let models = models.clone();
        Rc::new(move || {
            let rx = spawn_blocking(read_storage);
            let (total, images, dbsize, dbpath, models) = (
                total.clone(),
                images.clone(),
                dbsize.clone(),
                dbpath.clone(),
                models.clone(),
            );
            glib::MainContext::default().spawn_local(async move {
                if let Ok(s) = rx.recv().await {
                    set_info_value(&total, &s.total_files.to_string());
                    set_info_value(&images, &s.total_images.to_string());
                    set_info_value(&dbsize, &fmt_bytes(s.db_bytes));
                    set_info_value(&dbpath, &s.db_path);
                    set_info_value(&models, &s.models_dir);
                }
            });
        })
    };
    populate();

    let refresh = gtk::Button::with_label("Refresh");
    refresh.connect_clicked(clone!(@strong populate => move |_| populate()));
    buttons.append(&refresh);

    let open_db = gtk::Button::with_label("Show database");
    open_db.connect_clicked(|_| {
        if let Ok(p) = fileid_engine::paths::db_path() {
            if let Some(dir) = p.parent() {
                open_path(dir);
            }
        }
    });
    buttons.append(&open_db);

    let open_models = gtk::Button::with_label("Open Models folder");
    open_models.connect_clicked(|_| {
        if let Ok(p) = fileid_engine::paths::models_dir() {
            open_path(&p);
        }
    });
    buttons.append(&open_models);

    // Refresh storage after a scan finishes (counts changed).
    let rx = engine.borrow_mut().subscribe();
    glib::MainContext::default().spawn_local(clone!(@strong populate => async move {
        while let Ok(ev) = rx.recv().await {
            if matches!(ev, EngineEvent::ScanComplete(_)) {
                populate();
            }
        }
    }));

    card.append(&buttons);
    card.upcast()
}

// ── Recent scans card ────────────────────────────────────────────────────────

fn build_recent_scans_card(engine: &Rc<RefCell<EngineClient>>) -> gtk::Widget {
    let card = glass_card();
    card.append(
        &gtk::Label::builder()
            .label("Recent scans")
            .xalign(0.0)
            .css_classes(["heading"])
            .build(),
    );

    let list = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .build();
    card.append(&list);

    let populate: Rc<dyn Fn()> = {
        let list = list.clone();
        Rc::new(move || {
            let rx = spawn_blocking(|| read_recent_scans(12));
            glib::MainContext::default().spawn_local(clone!(@weak list => async move {
                let rows = rx.recv().await.unwrap_or_default();
                while let Some(child) = list.first_child() {
                    list.remove(&child);
                }
                if rows.is_empty() {
                    list.append(
                        &gtk::Label::builder()
                            .label("No scans recorded yet.")
                            .xalign(0.0)
                            .css_classes(["dim-label"])
                            .build(),
                    );
                    return;
                }
                for s in rows {
                    list.append(&scan_row(&s));
                }
            }));
        })
    };
    populate();

    let rx = engine.borrow_mut().subscribe();
    glib::MainContext::default().spawn_local(clone!(@strong populate => async move {
        while let Ok(ev) = rx.recv().await {
            if matches!(ev, EngineEvent::ScanComplete(_)) {
                populate();
            }
        }
    }));

    card.upcast()
}

fn scan_row(s: &ScanRow) -> gtk::Widget {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    let mark = match s.status.as_str() {
        "completed" => "✓",
        "running" => "…",
        _ => "✗",
    };
    let marker = gtk::Label::builder()
        .label(mark)
        .css_classes(if s.status == "completed" { ["gold-accent"] } else { ["dim-label"] })
        .build();
    row.append(&marker);

    let detail = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(1)
        .hexpand(true)
        .build();
    detail.append(
        &gtk::Label::builder()
            .label(&s.root_path)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .single_line_mode(true)
            .css_classes(["monospace"])
            .build(),
    );
    let count = s.last_file_index.map(|n| format!(" · {n} files")).unwrap_or_default();
    detail.append(
        &gtk::Label::builder()
            .label(format!("{} · {}{}", fmt_unix(s.started_at), s.status, count))
            .xalign(0.0)
            .css_classes(["dim-label", "monospace"])
            .build(),
    );
    row.append(&detail);
    row.upcast()
}

// ── Logs card ────────────────────────────────────────────────────────────────

fn build_logs_card() -> gtk::Widget {
    let card = glass_card();
    card.append(
        &gtk::Label::builder()
            .label("Logs")
            .xalign(0.0)
            .css_classes(["heading"])
            .build(),
    );
    card.append(
        &gtk::Label::builder()
            .label("The tail of the engine's local-only log, for troubleshooting. Never transmitted.")
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label"])
            .build(),
    );

    let view = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .css_classes(["dim-label"])
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .min_content_height(200)
        .max_content_height(360)
        .vexpand(false)
        .child(&view)
        .build();
    card.append(&scroller);

    let buffer = view.buffer();
    let populate: Rc<dyn Fn()> = {
        let buffer = buffer.clone();
        Rc::new(move || {
            let rx = spawn_blocking(|| read_log_tail(LOG_TAIL_LINES));
            glib::MainContext::default().spawn_local(clone!(@weak buffer => async move {
                if let Ok(text) = rx.recv().await {
                    buffer.set_text(&text);
                }
            }));
        })
    };
    populate();

    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    let refresh = gtk::Button::with_label("Refresh");
    refresh.connect_clicked(clone!(@strong populate => move |_| populate()));
    buttons.append(&refresh);
    let open = gtk::Button::with_label("Open logs folder");
    open.connect_clicked(|_| {
        if let Ok(p) = fileid_engine::paths::logs_dir() {
            open_path(&p);
        }
    });
    buttons.append(&open);
    card.append(&buttons);

    card.upcast()
}

// ── Privacy card ─────────────────────────────────────────────────────────────

fn build_privacy_card() -> gtk::Widget {
    let card = glass_card();
    card.append(
        &gtk::Label::builder()
            .label("Privacy")
            .xalign(0.0)
            .css_classes(["heading"])
            .build(),
    );
    card.append(
        &gtk::Label::builder()
            .label("No telemetry, ever. FileID has no analytics, no crash reporting, no update pings, and no download instrumentation. The only network egress is the model downloads you start above — fetched directly from huggingface.co. Your files, tags, faces, and captions never leave this device.")
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label"])
            .build(),
    );
    card.upcast()
}

// ── Shared UI helpers ────────────────────────────────────────────────────────

fn section_label(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text.to_uppercase())
        .xalign(0.0)
        .margin_top(12)
        .css_classes(["settings-heading"])
        .build()
}

/// A `key  value` row. The value label is the last child (see `set_info_value`).
fn info_row(key: &str, value: &str) -> gtk::Box {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    row.append(
        &gtk::Label::builder()
            .label(key)
            .xalign(0.0)
            .width_request(130)
            .css_classes(["dim-label"])
            .build(),
    );
    row.append(
        &gtk::Label::builder()
            .label(value)
            .xalign(0.0)
            .hexpand(true)
            .selectable(true)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .single_line_mode(true)
            .css_classes(["monospace"])
            .build(),
    );
    row
}

fn set_info_value(row: &gtk::Box, value: &str) {
    if let Some(label) = row.last_child().and_then(|w| w.downcast::<gtk::Label>().ok()) {
        label.set_label(value);
    }
}

/// Launch the platform file manager on `path` (XDG `xdg-open`), off the main
/// loop so a slow handler can't stall the UI. Local-only; never transmits.
fn open_path(path: &std::path::Path) {
    let path = path.to_path_buf();
    std::thread::spawn(move || {
        if let Ok(mut child) = std::process::Command::new("xdg-open").arg(&path).spawn() {
            let _ = child.wait();
        }
    });
}

// ── Off-main-loop reads (mirror engine_client's worker pattern) ──────────────

/// Run `f` on a worker thread; await the single result via `spawn_local`.
fn spawn_blocking<T, F>(f: F) -> async_channel::Receiver<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (tx, rx) = async_channel::bounded::<T>(1);
    std::thread::spawn(move || {
        let _ = tx.send_blocking(f());
    });
    rx
}

struct StorageStats {
    total_files: i64,
    total_images: i64,
    db_bytes: u64,
    db_path: String,
    models_dir: String,
}

fn read_storage() -> StorageStats {
    let db_path = fileid_engine::paths::db_path().ok();
    let db_path_str = db_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "unavailable".into());
    let models_dir = fileid_engine::paths::models_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "unavailable".into());
    let db_bytes = db_path
        .as_ref()
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .unwrap_or(0);

    let (total_files, total_images) = (|| {
        let p = db_path?;
        if !p.exists() {
            return None;
        }
        let conn = fileid_engine::db::open_read(&p).ok()?;
        let tf: i64 = conn
            .query_row("SELECT COUNT(*) FROM files", rusqlite::params![], |r| r.get(0))
            .ok()?;
        let ti: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE kind = 'image'",
                rusqlite::params![],
                |r| r.get(0),
            )
            .unwrap_or(0);
        Some((tf, ti))
    })()
    .unwrap_or((0, 0));

    StorageStats { total_files, total_images, db_bytes, db_path: db_path_str, models_dir }
}

#[derive(Default)]
struct ScanRow {
    root_path: String,
    started_at: f64,
    status: String,
    last_file_index: Option<i64>,
}

fn read_recent_scans(limit: i64) -> Vec<ScanRow> {
    (|| {
        let p = fileid_engine::paths::db_path().ok()?;
        if !p.exists() {
            return None;
        }
        let conn = fileid_engine::db::open_read(&p).ok()?;
        let mut stmt = conn
            .prepare(
                "SELECT root_path, started_at, status, last_file_index \
                 FROM scan_sessions ORDER BY started_at DESC LIMIT ?1",
            )
            .ok()?;
        let rows = stmt
            .query_map(rusqlite::params![limit], |r| {
                Ok(ScanRow {
                    root_path: r.get(0)?,
                    started_at: r.get(1)?,
                    status: r.get(2)?,
                    last_file_index: r.get(3)?,
                })
            })
            .ok()?
            .collect::<rusqlite::Result<Vec<ScanRow>>>()
            .ok()?;
        Some(rows)
    })()
    .unwrap_or_default()
}

/// Tail the most-recently-modified `engine*.jsonl` in the logs dir.
fn read_log_tail(max_lines: usize) -> String {
    let Ok(dir) = fileid_engine::paths::logs_dir() else {
        return String::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return "No engine log yet. Run a scan to generate logs.".to_string();
    };

    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !name.starts_with("engine") {
            continue;
        }
        if let Ok(modified) = entry.metadata().and_then(|m| m.modified()) {
            if newest.as_ref().map(|(t, _)| modified > *t).unwrap_or(true) {
                newest = Some((modified, path));
            }
        }
    }

    let Some((_, path)) = newest else {
        return "No engine log yet. Run a scan to generate logs.".to_string();
    };
    let Ok(content) = std::fs::read_to_string(&path) else {
        return String::new();
    };
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

// ── Formatting ───────────────────────────────────────────────────────────────

fn fmt_bytes(b: u64) -> String {
    const MB: f64 = 1_048_576.0;
    const GB: f64 = 1_073_741_824.0;
    let b = b as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.0} MB", b / MB)
    } else {
        format!("{:.0} KB", (b / 1024.0).max(0.0))
    }
}

fn fmt_unix(secs: f64) -> String {
    glib::DateTime::from_unix_local(secs as i64)
        .ok()
        .and_then(|dt| dt.format("%Y-%m-%d %H:%M").ok())
        .map(|g| g.to_string())
        .unwrap_or_else(|| "—".into())
}
