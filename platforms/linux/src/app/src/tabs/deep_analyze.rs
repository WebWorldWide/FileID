// Deep Analyze tab — on-device VLM captions + smart renames, the 1:1 port of
// macOS `DeepAnalyzeViews.swift` (`DeepAnalyzeView`).
//
// The tab is mostly engine-driven: it routes Deep Analyze commands to the
// engine and renders progress + results.
//
//   * a model picker over the installed VLM kinds (Qwen2.5-VL 7B / Gemma 3 4B /
//     Mistral-Small 3.2), install status read from the shared engine model
//     `registry`, active kind highlighted in gold,
//   * a "Library status" card (total images / not-yet-analyzed / ETA), counts
//     read directly from the same SQLite WAL DB (single-writer engine, many
//     readers — exactly like macOS `ReadStore`),
//   * a "Run Deep Analyze" action that sends `deepAnalyzeAll` (entire library)
//     or `deepAnalyzeFolder` (a picked sub-tree), plus per-file `deepAnalyzeFile`
//     re-analyze and `deepAnalyzeCancel`,
//   * live Starting / Working / Completion / Most-recent-caption cards driven by
//     the engine's `deepAnalyze*` events, and a model-download progress bar
//     driven by `modelDownloadProgress`,
//   * a "Smart names ready" list (files.vlm_proposed_name / vlm_description) with
//     per-file Rename (`renameFiles`) + an Apply-all.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use gtk::glib::clone;

use fileid_engine::ipc::{
    CommandPayload, DeepAnalyzeAllPayload, DeepAnalyzeFilePayload, DeepAnalyzeFolderPayload, Empty,
    RenameEntry, RenameFilesPayload,
};
use fileid_engine::models::registry::{self, LookupResult};

use crate::engine_client::{EngineClient, EngineEvent};
use super::util::glass_card;

const PROPOSED_LIMIT: i64 = 200;

// ─── VLM registry (Linux mirror of macOS `AIModelKind`) ──────────────────────
// `model_kind` keys are the exact strings the engine accepts (see the shared
// engine `models::registry::lookup_full`). RAM / seconds-per-image are display
// estimates only — macOS reads them from `AIModels.swift`; Linux has no RAM
// probe wired yet (no new crate), so they're informational and the picker shows
// every kind rather than OOM-gating per machine.
struct VlmKind {
    key: &'static str,
    display: &'static str,
    ram_gb: f64,
    secs_per_image: f64,
    license: &'static str,
}

const VLMS: [VlmKind; 3] = [
    VlmKind { key: "qwen2_5_vl_7b", display: "Qwen2.5-VL 7B", ram_gb: 7.0, secs_per_image: 6.0, license: "Apache-2.0" },
    VlmKind { key: "gemma_3_4b", display: "Gemma 3 4B", ram_gb: 5.0, secs_per_image: 4.0, license: "Gemma Terms" },
    VlmKind { key: "mistral_small_3_2", display: "Mistral-Small 3.2", ram_gb: 16.0, secs_per_image: 14.0, license: "Apache-2.0" },
];

const DEFAULT_KIND: &str = "qwen2_5_vl_7b";

fn vlm_by_key(key: &str) -> &'static VlmKind {
    VLMS.iter().find(|v| v.key == key).unwrap_or(&VLMS[0])
}

// ─── Tab entrypoint ──────────────────────────────────────────────────────────

pub fn build_deep_analyze_tab(engine: Rc<RefCell<EngineClient>>) -> gtk::Widget {
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .margin_top(20)
        .margin_bottom(20)
        .margin_start(24)
        .margin_end(24)
        .css_classes(["fileid-tab"])
        .build();

    content.append(&build_header());
    let explainer = build_explainer();
    content.append(&explainer);

    // Status card.
    let lbl_active = mono_value(vlm_by_key(DEFAULT_KIND).display);
    let lbl_total = mono_value("0");
    let lbl_pending = mono_value("0");
    let lbl_eta = mono_value("0s");
    content.append(&build_status_card(&lbl_active, &lbl_total, &lbl_pending, &lbl_eta));

    // Model picker (rows populated after `ui` exists).
    let picker_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .build();
    let download_label = gtk::Label::builder()
        .xalign(0.0)
        .css_classes(["dim-label"])
        .build();
    let download_bar = gtk::ProgressBar::builder().show_text(false).css_classes(["gold-accent"]).build();
    let download_card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .build();
    download_card.append(&download_label);
    download_card.append(&download_bar);
    download_card.set_visible(false);
    content.append(&build_picker_card(&picker_box, &download_card));

    // Actions card.
    let naming_banner = build_naming_banner();
    let skip_check = gtk::CheckButton::with_label("Skip images already analyzed by the active model");
    skip_check.set_active(true);
    let run_btn = gtk::Button::builder()
        .label("Analyze entire library")
        .sensitive(false)
        .css_classes(["gold-button"])
        .build();
    let folder_btn = gtk::Button::builder()
        .label("Analyze a folder…")
        .css_classes(["pill"])
        .build();
    let cancel_btn = gtk::Button::builder()
        .label("Cancel")
        .css_classes(["destructive-action"])
        .visible(false)
        .build();
    content.append(&build_actions_card(&naming_banner, &skip_check, &run_btn, &folder_btn, &cancel_btn));

    // Smart-names list.
    let smart_count = gtk::Label::builder()
        .xalign(1.0)
        .css_classes(["dim-label"])
        .build();
    let smart_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let smart_apply_all = gtk::Button::builder()
        .label("Apply all renames")
        .css_classes(["gold-button"])
        .build();
    let smart_card = build_smart_card(&smart_count, &smart_list, &smart_apply_all);
    smart_card.set_visible(false);
    content.append(&smart_card);

    // Live, event-driven cards.
    let starting_subtitle = wrap_caption(
        "Loading the on-device model. The first image usually appears in 5–15 seconds.",
    );
    let starting_card = build_starting_card(&starting_subtitle);
    starting_card.set_visible(false);
    content.append(&starting_card);

    let progress_bar = gtk::ProgressBar::builder().show_text(false).css_classes(["gold-accent"]).build();
    let progress_count = mono_value("0 / 0");
    let progress_eta = gtk::Label::builder().xalign(1.0).css_classes(["gold-accent"]).build();
    let progress_file = gtk::Label::builder()
        .xalign(0.0)
        .css_classes(["dim-label"])
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .single_line_mode(true)
        .build();
    let progress_caption = gtk::Label::builder().xalign(0.0).wrap(true).build();
    let progress_card = build_progress_card(
        &progress_bar, &progress_count, &progress_eta, &progress_file, &progress_caption,
    );
    progress_card.set_visible(false);
    content.append(&progress_card);

    let completion_icon = gtk::Label::new(None);
    let completion_label = gtk::Label::builder().xalign(0.0).css_classes(["dim-label"]).build();
    let completion_card = build_completion_card(&completion_icon, &completion_label);
    completion_card.set_visible(false);
    content.append(&completion_card);

    let last_desc = gtk::Label::builder().xalign(0.0).wrap(true).build();
    let last_name = gtk::Label::builder().xalign(0.0).css_classes(["gold-accent"]).build();
    let last_card = build_last_card(&last_desc, &last_name);
    last_card.set_visible(false);
    content.append(&last_card);

    let scroller = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&content)
        .css_classes(["fileid-tab"])
        .build();

    // ── Shared state ─────────────────────────────────────────────────────────
    let ui = Rc::new(DeepUi {
        engine,
        in_flight: Cell::new(false),
        active_kind: Cell::new(DEFAULT_KIND),
        lbl_active,
        lbl_total,
        lbl_pending,
        lbl_eta,
        run_btn,
        cancel_btn,
        skip_check,
        naming_banner,
        picker_box,
        pick_rows: RefCell::new(Vec::new()),
        download_card,
        download_bar,
        download_label,
        smart_card,
        smart_count,
        smart_list,
        proposed: RefCell::new(Vec::new()),
        starting_card,
        starting_subtitle,
        progress_card,
        progress_bar,
        progress_count,
        progress_eta,
        progress_file,
        progress_caption,
        completion_card,
        completion_label,
        completion_icon,
        last_card,
        last_desc,
        last_name,
    });

    populate_picker(&ui);
    wire_actions(&ui, &folder_btn, &smart_apply_all);

    // ── Live engine events → cards ───────────────────────────────────────────
    glib::MainContext::default().spawn_local(clone!(@strong ui => async move {
        let rx = ui.engine.borrow_mut().subscribe();
        while let Ok(ev) = rx.recv().await {
            apply_event(&ui, ev);
        }
    }));

    // Initial fill.
    refresh(&ui);

    scroller.upcast()
}

// ─── Shared UI state ─────────────────────────────────────────────────────────

struct PickRow {
    key: &'static str,
    btn: gtk::Button,
    indicator: gtk::Label,
    title: gtk::Label,
}

struct DeepUi {
    engine: Rc<RefCell<EngineClient>>,
    in_flight: Cell<bool>,
    active_kind: Cell<&'static str>,

    lbl_active: gtk::Label,
    lbl_total: gtk::Label,
    lbl_pending: gtk::Label,
    lbl_eta: gtk::Label,

    run_btn: gtk::Button,
    cancel_btn: gtk::Button,
    skip_check: gtk::CheckButton,
    naming_banner: gtk::Box,

    picker_box: gtk::Box,
    pick_rows: RefCell<Vec<PickRow>>,
    download_card: gtk::Box,
    download_bar: gtk::ProgressBar,
    download_label: gtk::Label,

    smart_card: gtk::Box,
    smart_count: gtk::Label,
    smart_list: gtk::ListBox,
    proposed: RefCell<Vec<ProposedRow>>,

    starting_card: gtk::Box,
    starting_subtitle: gtk::Label,
    progress_card: gtk::Box,
    progress_bar: gtk::ProgressBar,
    progress_count: gtk::Label,
    progress_eta: gtk::Label,
    progress_file: gtk::Label,
    progress_caption: gtk::Label,
    completion_card: gtk::Box,
    completion_label: gtk::Label,
    completion_icon: gtk::Label,
    last_card: gtk::Box,
    last_desc: gtk::Label,
    last_name: gtk::Label,
}

// ─── Command routing ─────────────────────────────────────────────────────────

fn send_cmd(engine: &Rc<RefCell<EngineClient>>, payload: CommandPayload) {
    if let Err(err) = engine.borrow_mut().send(payload) {
        tracing::warn!(target: "deep_analyze", "send failed: {err}");
    }
}

fn wire_actions(ui: &Rc<DeepUi>, folder_btn: &gtk::Button, apply_all: &gtk::Button) {
    // Analyze entire library → deepAnalyzeAll.
    ui.run_btn.connect_clicked(clone!(@strong ui => move |_| {
        if ui.in_flight.get() { return; }
        let payload = CommandPayload::DeepAnalyzeAll(DeepAnalyzeAllPayload {
            model_kind: ui.active_kind.get().to_string(),
            skip_existing: ui.skip_check.is_active(),
            tags_only: false,
            propose_renames: true,
        });
        begin_run(&ui);
        send_cmd(&ui.engine, payload);
    }));

    // Analyze a picked folder → deepAnalyzeFolder.
    folder_btn.connect_clicked(clone!(@strong ui => move |btn| {
        if ui.in_flight.get() { return; }
        let dialog = gtk::FileDialog::builder()
            .title("Pick a folder to Deep Analyze")
            .modal(true)
            .build();
        let parent = btn.root().and_downcast::<gtk::Window>();
        dialog.select_folder(parent.as_ref(), gio::Cancellable::NONE, clone!(@strong ui => move |res| {
            let Ok(file) = res else { return };
            let Some(path) = file.path() else { return };
            let payload = CommandPayload::DeepAnalyzeFolder(DeepAnalyzeFolderPayload {
                path_prefix: path.to_string_lossy().into_owned(),
                model_kind: ui.active_kind.get().to_string(),
            });
            begin_run(&ui);
            send_cmd(&ui.engine, payload);
        }));
    }));

    // Cancel → deepAnalyzeCancel.
    ui.cancel_btn.connect_clicked(clone!(@strong ui => move |_| {
        send_cmd(&ui.engine, CommandPayload::DeepAnalyzeCancel(Empty {}));
    }));

    // Apply all smart-name renames → renameFiles.
    apply_all.connect_clicked(clone!(@strong ui => move |_| {
        let renames: Vec<RenameEntry> = ui
            .proposed
            .borrow()
            .iter()
            .map(|r| RenameEntry { file_id: r.id, new_name: r.new_name() })
            .collect();
        if renames.is_empty() { return; }
        send_cmd(&ui.engine, CommandPayload::RenameFiles(RenameFilesPayload { renames }));
        schedule_refresh(&ui, 900);
    }));
}

/// Optimistic refresh after a fire-and-forget command (the engine's
/// `renameResult` events aren't surfaced on the current `EngineEvent` surface,
/// so we re-read the DB shortly after instead of waiting for confirmation).
fn schedule_refresh(ui: &Rc<DeepUi>, ms: u64) {
    let ui = ui.clone();
    glib::timeout_add_local_once(Duration::from_millis(ms), move || refresh(&ui));
}

/// Flip into the "running" presentation the instant a run is requested — before
/// the first engine event lands (mirrors macOS `isStartingDeepAnalyze`).
fn begin_run(ui: &Rc<DeepUi>) {
    ui.in_flight.set(true);
    ui.run_btn.set_sensitive(false);
    ui.cancel_btn.set_visible(true);
    ui.completion_card.set_visible(false);
    ui.progress_card.set_visible(false);
    ui.starting_subtitle
        .set_text("Loading the on-device model. The first image usually appears in 5–15 seconds.");
    reveal(&ui.starting_card);
}

/// Tear down the live cards (crash / cancel / clean exit). Mirrors macOS
/// F-C4-016 — a crash mid-run removes the live cards at once.
fn end_run(ui: &Rc<DeepUi>) {
    ui.in_flight.set(false);
    ui.starting_card.set_visible(false);
    ui.progress_card.set_visible(false);
    ui.cancel_btn.set_visible(false);
    ui.run_btn.set_sensitive(true);
}

// ─── Event handling ──────────────────────────────────────────────────────────

fn apply_event(ui: &Rc<DeepUi>, ev: EngineEvent) {
    match ev {
        EngineEvent::DeepAnalyzeStarting(s) => {
            ui.in_flight.set(true);
            ui.run_btn.set_sensitive(false);
            ui.cancel_btn.set_visible(true);
            ui.progress_card.set_visible(false);
            ui.completion_card.set_visible(false);
            ui.download_card.set_visible(false);
            ui.starting_subtitle.set_text(&s.message);
            reveal(&ui.starting_card);
        }
        EngineEvent::DeepAnalyzeProgress(p) => {
            ui.in_flight.set(true);
            ui.starting_card.set_visible(false);
            ui.cancel_btn.set_visible(true);
            ui.run_btn.set_sensitive(false);
            let total = p.total.max(1) as f64;
            ui.progress_bar.set_fraction(p.processed as f64 / total);
            ui.progress_count.set_text(&format!("{} / {}", p.processed, p.total));
            match p.eta_seconds {
                Some(eta) => ui.progress_eta.set_text(&format!("ETA {}", format_duration(eta))),
                None => ui.progress_eta.set_text(""),
            }
            match p.current_path.as_deref() {
                Some(path) => ui.progress_file.set_text(basename(path)),
                None => ui.progress_file.set_text(""),
            }
            match p.current_caption.as_deref().filter(|s| !s.is_empty()) {
                Some(cap) => {
                    ui.progress_caption.set_text(cap);
                    ui.progress_caption.set_visible(true);
                }
                None => ui.progress_caption.set_visible(false),
            }
            reveal(&ui.progress_card);
        }
        EngineEvent::DeepAnalyzeFileDone(d) => {
            ui.last_desc.set_text(&d.description);
            match d.proposed_name.as_deref().filter(|s| !s.is_empty()) {
                Some(name) => {
                    ui.last_name.set_text(&format!("Smart name: {name}"));
                    ui.last_name.set_visible(true);
                }
                None => ui.last_name.set_visible(false),
            }
            reveal(&ui.last_card);
        }
        EngineEvent::DeepAnalyzeComplete(c) => {
            end_run(ui);
            ui.completion_icon.set_text(if c.cancelled { "✗" } else { "✓" });
            ui.completion_icon
                .set_css_classes(if c.cancelled { &["lavender-accent"] } else { &["gold-accent"] });
            ui.completion_label.set_text(&format!(
                "{} processed · {} failed · {} wall time",
                c.processed,
                c.failed,
                format_duration(c.total_seconds),
            ));
            reveal(&ui.completion_card);
            refresh(ui);
        }
        EngineEvent::ModelDownloadProgress(m) => {
            ui.download_label.set_text(&m.message);
            ui.download_bar.set_fraction(m.fraction.clamp(0.0, 1.0));
            ui.download_card.set_visible(true);
        }
        EngineEvent::ScanComplete(_) => refresh(ui),
        EngineEvent::Error(_) | EngineEvent::Exited => end_run(ui),
        _ => {}
    }
}

// ─── DB-backed refresh (mirror of macOS ReadStore counts) ────────────────────

fn refresh(ui: &Rc<DeepUi>) {
    let active = ui.active_kind.get().to_string();
    ui.lbl_active.set_text(vlm_by_key(&active).display);

    let rx = query_status(active.clone());
    glib::MainContext::default().spawn_local(clone!(@strong ui => async move {
        if let Ok(c) = rx.recv().await {
            apply_status(&ui, c);
        }
    }));

    let rx2 = query_proposed();
    glib::MainContext::default().spawn_local(clone!(@strong ui => async move {
        if let Ok(rows) = rx2.recv().await {
            apply_proposed(&ui, rows);
        }
    }));
}

fn apply_status(ui: &Rc<DeepUi>, c: StatusCounts) {
    ui.lbl_total.set_text(&c.total_images.to_string());
    ui.lbl_pending.set_text(&c.pending.to_string());
    let secs = c.pending as f64 * vlm_by_key(ui.active_kind.get()).secs_per_image;
    ui.lbl_eta.set_text(&format_duration(secs));
    ui.naming_banner
        .set_visible(c.named_people == 0 && !ui.in_flight.get());
    if !ui.in_flight.get() {
        ui.run_btn.set_sensitive(true);
    }
}

fn apply_proposed(ui: &Rc<DeepUi>, rows: Vec<ProposedRow>) {
    while let Some(child) = ui.smart_list.first_child() {
        ui.smart_list.remove(&child);
    }
    ui.smart_count
        .set_text(&format!("{} file{}", rows.len(), if rows.len() == 1 { "" } else { "s" }));
    for row in &rows {
        ui.smart_list.append(&build_proposed_row(ui, row));
    }
    ui.smart_card.set_visible(!rows.is_empty());
    *ui.proposed.borrow_mut() = rows;
}

fn build_proposed_row(ui: &Rc<DeepUi>, row: &ProposedRow) -> gtk::Box {
    let hbox = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(10)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(8)
        .margin_end(8)
        .build();

    let names = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .hexpand(true)
        .build();
    let old = gtk::Label::builder()
        .label(&row.name)
        .xalign(0.0)
        .css_classes(["dim-label"])
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .single_line_mode(true)
        .build();
    let new = gtk::Label::builder()
        .label(&format!("→ {}", row.new_name()))
        .xalign(0.0)
        .css_classes(["gold-accent"])
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .single_line_mode(true)
        .build();
    names.append(&old);
    names.append(&new);
    if !row.description.is_empty() {
        let cap = gtk::Label::builder()
            .label(&row.description)
            .xalign(0.0)
            .css_classes(["tile-caption"])
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .single_line_mode(true)
            .build();
        names.append(&cap);
    }
    hbox.append(&names);

    let reanalyze = gtk::Button::builder()
        .label("Re-analyze")
        .css_classes(["pill"])
        .valign(gtk::Align::Center)
        .build();
    let id = row.id;
    reanalyze.connect_clicked(clone!(@strong ui => move |_| {
        send_cmd(&ui.engine, CommandPayload::DeepAnalyzeFile(DeepAnalyzeFilePayload {
            file_id: id,
            model_kind: ui.active_kind.get().to_string(),
        }));
        begin_run(&ui);
    }));
    hbox.append(&reanalyze);

    let rename = gtk::Button::builder()
        .label("Rename")
        .css_classes(["gold-button"])
        .valign(gtk::Align::Center)
        .build();
    let new_name = row.new_name();
    rename.connect_clicked(clone!(@strong ui => move |_| {
        send_cmd(&ui.engine, CommandPayload::RenameFiles(RenameFilesPayload {
            renames: vec![RenameEntry { file_id: id, new_name: new_name.clone() }],
        }));
        schedule_refresh(&ui, 700);
    }));
    hbox.append(&rename);

    hbox
}

// ─── Model picker ────────────────────────────────────────────────────────────

fn populate_picker(ui: &Rc<DeepUi>) {
    for vlm in VLMS.iter() {
        let (installed, gb) = model_install_info(vlm.key);

        let is_default = vlm.key == DEFAULT_KIND;

        let indicator = gtk::Label::builder()
            .label(if is_default { "●" } else { "○" })
            .valign(gtk::Align::Start)
            .build();
        if is_default {
            indicator.add_css_class("gold-accent");
        }

        let title = gtk::Label::builder().label(vlm.display).xalign(0.0).build();
        if is_default {
            title.add_css_class("gold-accent");
        }

        let badge = gtk::Label::builder()
            .label(if installed { "Downloaded".to_string() } else { format!("Will download {gb:.1} GB") })
            .css_classes(if installed { ["kind-badge"] } else { ["dim-label"] })
            .valign(gtk::Align::Center)
            .build();

        let title_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .build();
        title_row.append(&title);
        title_row.append(&badge);

        let stats = gtk::Label::builder()
            .label(&format!(
                "≈ {:.1} GB RAM · {:.1} s/image · {}",
                vlm.ram_gb, vlm.secs_per_image, vlm.license
            ))
            .xalign(0.0)
            .css_classes(["tile-caption"])
            .build();

        let text_col = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .hexpand(true)
            .build();
        text_col.append(&title_row);
        text_col.append(&stats);

        let inner = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(10)
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(10)
            .margin_end(10)
            .build();
        inner.append(&indicator);
        inner.append(&text_col);

        let btn = gtk::Button::builder()
            .css_classes(["glass-card"])
            .child(&inner)
            .build();
        if is_default {
            btn.add_css_class("file-tile-selected");
        }

        let key = vlm.key;
        btn.connect_clicked(clone!(@strong ui => move |_| {
            ui.active_kind.set(key);
            for row in ui.pick_rows.borrow().iter() {
                let active = row.key == key;
                if active {
                    row.btn.add_css_class("file-tile-selected");
                    row.indicator.set_text("●");
                    row.indicator.add_css_class("gold-accent");
                    row.title.add_css_class("gold-accent");
                } else {
                    row.btn.remove_css_class("file-tile-selected");
                    row.indicator.set_text("○");
                    row.indicator.remove_css_class("gold-accent");
                    row.title.remove_css_class("gold-accent");
                }
            }
            ui.skip_check
                .set_label(Some(&format!("Skip images already analyzed by {}", vlm_by_key(key).display)));
            refresh(&ui);
        }));

        ui.picker_box.append(&btn);
        ui.pick_rows.borrow_mut().push(PickRow { key, btn, indicator, title });
    }
}

/// (installed, approx-GB) for a VLM kind, read from the shared engine registry.
fn model_install_info(key: &str) -> (bool, f64) {
    match registry::lookup_full(key) {
        LookupResult::Found(model) => {
            let bytes: u64 = model.files.iter().map(|f| f.approx_bytes).sum();
            let installed = registry::sentinel_path(&model)
                .map(|p| p.exists())
                .unwrap_or(false);
            (installed, bytes as f64 / 1_073_741_824.0)
        }
        LookupResult::Unknown => (false, 0.0),
    }
}

// ─── DB queries (fresh read-only connection, WAL-safe) ───────────────────────

#[derive(Default)]
struct StatusCounts {
    total_images: i64,
    pending: i64,
    named_people: i64,
}

#[derive(Clone)]
struct ProposedRow {
    id: i64,
    name: String,
    extension: String,
    proposed: String,
    description: String,
}

impl ProposedRow {
    fn new_name(&self) -> String {
        if self.extension.is_empty() {
            self.proposed.clone()
        } else {
            format!("{}.{}", self.proposed, self.extension)
        }
    }
}

fn query_status(active: String) -> async_channel::Receiver<StatusCounts> {
    spawn_db(move |conn| {
        let total_images: i64 = conn
            .query_row("SELECT COUNT(*) FROM files WHERE kind = 'image'", [], |r| r.get(0))
            .unwrap_or(0);
        let pending: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE kind = 'image' AND \
                 (vlm_description IS NULL OR vlm_description = '' OR vlm_model IS NULL OR vlm_model <> ?1)",
                rusqlite::params![active],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let named_people: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM persons WHERE (name IS NOT NULL AND TRIM(name) <> '') \
                 OR (first_name IS NOT NULL AND TRIM(first_name) <> '') \
                 OR (last_name IS NOT NULL AND TRIM(last_name) <> '')",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        StatusCounts { total_images, pending, named_people }
    })
}

fn query_proposed() -> async_channel::Receiver<Vec<ProposedRow>> {
    spawn_db(|conn| {
        let mut stmt = match conn.prepare(
            "SELECT id, path_text, extension, vlm_proposed_name, vlm_description FROM files \
             WHERE vlm_proposed_name IS NOT NULL AND vlm_proposed_name <> '' \
             ORDER BY vlm_analyzed_at DESC LIMIT ?1",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let mapped = stmt.query_map(rusqlite::params![PROPOSED_LIMIT], |r| {
            let path: String = r.get(1)?;
            let name = std::path::Path::new(&path)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.clone());
            Ok(ProposedRow {
                id: r.get(0)?,
                name,
                extension: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                proposed: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                description: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
            })
        });
        match mapped {
            Ok(iter) => iter.collect::<rusqlite::Result<Vec<_>>>().unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    })
}

/// Run a closure against a fresh read-only DB connection off the main loop.
/// Returns `T::default()` if no scan DB exists yet (mirrors `query_files`).
fn spawn_db<T, F>(f: F) -> async_channel::Receiver<T>
where
    T: Default + Send + 'static,
    F: FnOnce(&rusqlite::Connection) -> T + Send + 'static,
{
    let (tx, rx) = async_channel::bounded::<T>(1);
    std::thread::spawn(move || {
        let value = run_db(f);
        let _ = tx.send_blocking(value);
    });
    rx
}

fn run_db<T, F>(f: F) -> T
where
    T: Default,
    F: FnOnce(&rusqlite::Connection) -> T,
{
    let Ok(db_path) = fileid_engine::paths::db_path() else {
        return T::default();
    };
    if !db_path.exists() {
        return T::default();
    }
    match fileid_engine::db::open_read(&db_path) {
        Ok(conn) => f(&conn),
        Err(_) => T::default(),
    }
}

// ─── Static card builders ────────────────────────────────────────────────────

fn build_header() -> gtk::Box {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(14)
        .build();
    let icon = gtk::Image::from_icon_name("starred-symbolic");
    icon.set_pixel_size(30);
    icon.add_css_class("gold-accent");
    icon.set_valign(gtk::Align::Start);
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .build();
    text.append(
        &gtk::Label::builder()
            .label("Deep Analyze")
            .xalign(0.0)
            .css_classes(["title-1"])
            .build(),
    );
    text.append(&wrap_caption(
        "Reads each of your images and writes a sentence about it plus a smart filename.",
    ));
    row.append(&icon);
    row.append(&text);
    row
}

fn build_explainer() -> gtk::Box {
    let card = glass_card();
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .build();
    let icon = gtk::Image::from_icon_name("starred-symbolic");
    icon.add_css_class("gold-accent");
    icon.set_valign(gtk::Align::Start);
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .hexpand(true)
        .build();
    text.append(&heading("Tagging vs. Deep Analyze"));
    text.append(&wrap_caption(
        "Tagging runs automatically as you scan — it adds quick keyword tags (like sunset, beach, \
         dog) so search works. Deep Analyze is an optional, heavier step: it reads each photo, \
         writes a full sentence describing it, and suggests a smart filename — using the people \
         you've named.",
    ));
    let dismiss = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .css_classes(["flat", "circular"])
        .valign(gtk::Align::Start)
        .tooltip_text("Hide this explanation")
        .build();
    dismiss.connect_clicked(clone!(@weak card => move |_| card.set_visible(false)));
    row.append(&icon);
    row.append(&text);
    row.append(&dismiss);
    card.append(&row);
    card
}

fn build_status_card(active: &gtk::Label, total: &gtk::Label, pending: &gtk::Label, eta: &gtk::Label) -> gtk::Box {
    let card = glass_card();
    card.append(&heading("Library status"));
    card.append(&wrap_caption(
        "Run a scan first (top bar). Then come back here — Deep Analyze adds human-readable \
         captions and suggests smart filenames for every image.",
    ));
    let grid = gtk::Grid::builder().row_spacing(4).column_spacing(16).build();
    grid.attach(&dim_key("Active model"), 0, 0, 1, 1);
    grid.attach(active, 1, 0, 1, 1);
    grid.attach(&dim_key("Total images"), 0, 1, 1, 1);
    grid.attach(total, 1, 1, 1, 1);
    grid.attach(&dim_key("Not yet analyzed"), 0, 2, 1, 1);
    grid.attach(pending, 1, 2, 1, 1);
    grid.attach(&dim_key("Estimated batch time"), 0, 3, 1, 1);
    grid.attach(eta, 1, 3, 1, 1);
    card.append(&grid);
    card
}

fn build_picker_card(picker_box: &gtk::Box, download_card: &gtk::Box) -> gtk::Box {
    let card = glass_card();
    card.append(&heading("AI Models — accuracy tier (Deep Analyze)"));
    card.append(&wrap_caption(
        "Reads images and writes captions plus smart filenames. Pick a model; it downloads on \
         first run.",
    ));
    card.append(picker_box);
    card.append(download_card);
    card
}

fn build_actions_card(
    naming_banner: &gtk::Box,
    skip_check: &gtk::CheckButton,
    run_btn: &gtk::Button,
    folder_btn: &gtk::Button,
    cancel_btn: &gtk::Button,
) -> gtk::Box {
    let card = glass_card();
    card.append(&heading("Run Deep Analyze"));
    card.append(naming_banner);
    card.append(skip_check);
    let actions = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(10)
        .build();
    actions.append(run_btn);
    actions.append(folder_btn);
    actions.append(cancel_btn);
    card.append(&actions);
    card.append(&wrap_caption(
        "Runs serially on the GPU. Safe to leave running — captions need named people to read \
         best (name faces in the People tab first).",
    ));
    card
}

fn build_naming_banner() -> gtk::Box {
    let banner = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(10)
        .css_classes(["glass-card"])
        .build();
    banner.set_visible(false);
    let icon = gtk::Image::from_icon_name("dialog-warning-symbolic");
    icon.add_css_class("lavender-accent");
    icon.set_valign(gtk::Align::Start);
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .hexpand(true)
        .build();
    text.append(&heading("Name your people first (recommended)"));
    text.append(&wrap_caption(
        "Deep Analyze writes captions like \"Mia playing piano\" — that needs at least one named \
         person. Without names, captions fall back to generic descriptions like \"a person \
         playing piano.\" You can still run now and re-run later after naming.",
    ));
    banner.append(&icon);
    banner.append(&text);
    banner
}

fn build_smart_card(count: &gtk::Label, list: &gtk::ListBox, apply_all: &gtk::Button) -> gtk::Box {
    let card = glass_card();
    let head = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    head.append(&heading("Smart names ready"));
    let spacer = gtk::Box::builder().hexpand(true).build();
    head.append(&spacer);
    head.append(count);
    card.append(&head);
    card.append(&wrap_caption(
        "Deep Analyze suggested new filenames for these images. Apply them per-file or all at \
         once — the originals are only renamed when you apply.",
    ));
    card.append(list);
    card.append(apply_all);
    card
}

fn build_starting_card(subtitle: &gtk::Label) -> gtk::Box {
    let card = glass_card();
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .build();
    let spinner = gtk::Spinner::builder().spinning(true).build();
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .hexpand(true)
        .build();
    text.append(&heading("Starting Deep Analyze…"));
    text.append(subtitle);
    row.append(&spinner);
    row.append(&text);
    card.append(&row);
    card
}

fn build_progress_card(
    bar: &gtk::ProgressBar,
    count: &gtk::Label,
    eta: &gtk::Label,
    file: &gtk::Label,
    caption: &gtk::Label,
) -> gtk::Box {
    let card = glass_card();
    let head = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    head.append(&heading("Working…"));
    let spacer = gtk::Box::builder().hexpand(true).build();
    head.append(&spacer);
    head.append(eta);
    card.append(&head);
    card.append(bar);
    card.append(count);
    card.append(file);
    card.append(caption);
    card
}

fn build_completion_card(icon: &gtk::Label, label: &gtk::Label) -> gtk::Box {
    let card = glass_card();
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    row.append(icon);
    row.append(&heading("Last run complete"));
    card.append(&row);
    card.append(label);
    card
}

fn build_last_card(desc: &gtk::Label, name: &gtk::Label) -> gtk::Box {
    let card = glass_card();
    card.append(&heading("Most recent caption"));
    card.append(desc);
    card.append(name);
    card
}

// ─── Small widget helpers ────────────────────────────────────────────────────

fn heading(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .css_classes(["title-4"])
        .build()
}

fn wrap_caption(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dim-label"])
        .build()
}

fn dim_key(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .css_classes(["dim-label"])
        .build()
}

fn mono_value(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .css_classes(["monospace"])
        .build()
}

/// Show a card with the shared brand spring fading it in (mirrors the macOS
/// `.spring(response: 0.35–0.4)` reveals).
fn reveal(card: &gtk::Box) {
    if card.is_visible() {
        return;
    }
    card.set_visible(true);
    let weak = card.downgrade();
    let _ = crate::spring::animate(card, 0.0, 1.0, move |v| {
        if let Some(c) = weak.upgrade() {
            c.set_opacity(v);
        }
    });
}

fn basename(path: &str) -> &str {
    std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
}

fn format_duration(seconds: f64) -> String {
    let s = seconds.max(0.0).round() as i64;
    let h = s / 3600;
    let m = (s % 3600) / 60;
    let sec = s % 60;
    if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {sec}s")
    } else {
        format!("{sec}s")
    }
}
