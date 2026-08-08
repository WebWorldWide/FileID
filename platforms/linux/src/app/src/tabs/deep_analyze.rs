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

use super::util::glass_card;
use crate::engine_client::{EngineClient, EngineEvent};

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
    VlmKind {
        key: "qwen2_5_vl_7b",
        display: "Qwen2.5-VL 7B",
        ram_gb: 7.0,
        secs_per_image: 6.0,
        license: "Apache-2.0",
    },
    VlmKind {
        key: "gemma_3_4b",
        display: "Gemma 3 4B",
        ram_gb: 5.0,
        secs_per_image: 4.0,
        license: "Gemma Terms",
    },
    VlmKind {
        key: "mistral_small_3_2",
        display: "Mistral-Small 3.2",
        ram_gb: 16.0,
        secs_per_image: 14.0,
        license: "Apache-2.0",
    },
];

fn vlm_by_key(key: &str) -> &'static VlmKind {
    VLMS.iter().find(|v| v.key == key).unwrap_or(&VLMS[0])
}

fn recommended_vlm_kind(
    total_ram_gb: f64,
    available_ram_gb: f64,
    free_disk_bytes: Option<u64>,
) -> &'static str {
    let order: &[&str] = if total_ram_gb >= 47.5 {
        &["mistral_small_3_2", "qwen2_5_vl_7b", "gemma_3_4b"]
    } else if total_ram_gb >= 15.0 {
        &["qwen2_5_vl_7b", "gemma_3_4b"]
    } else {
        &["gemma_3_4b"]
    };
    order
        .iter()
        .copied()
        .find(|kind| vlm_fits(kind, total_ram_gb, available_ram_gb, free_disk_bytes))
        .or_else(|| {
            order
                .iter()
                .copied()
                .find(|kind| vlm_fits(kind, total_ram_gb, available_ram_gb, None))
        })
        .unwrap_or("gemma_3_4b")
}

fn vlm_fits(
    kind: &str,
    total_ram_gb: f64,
    available_ram_gb: f64,
    free_disk_bytes: Option<u64>,
) -> bool {
    let (minimum_total, working_set) = match kind {
        "mistral_small_3_2" => (23.5, 16.0),
        "qwen2_5_vl_7b" => (11.5, 7.0),
        "gemma_3_4b" => (7.5, 4.5),
        _ => return false,
    };
    if total_ram_gb < minimum_total {
        return false;
    }
    let reserve = if total_ram_gb <= 10.0 {
        2.0
    } else if total_ram_gb <= 20.0 {
        4.0
    } else {
        6.0
    };
    let mut usable = (total_ram_gb - reserve).max(0.0);
    if available_ram_gb > 0.0 {
        usable = usable.min((available_ram_gb - 1.5).max(0.0));
    }
    if usable < working_set {
        return false;
    }
    let Some(free) = free_disk_bytes else {
        return true;
    };
    let bytes = model_install_info(kind).1 * 1_073_741_824.0;
    free >= fileid_engine::downloader::required_install_free_bytes(bytes as u64)
}

/// Display name of the machine-sized VLM recommendation (used by the Welcome
/// sheet's "optional later" pointer).
pub fn recommended_vlm_display() -> &'static str {
    vlm_by_key(host_recommended_vlm_kind()).display
}

fn host_recommended_vlm_kind() -> &'static str {
    let total = fileid_engine::platform::physical_memory_gb();
    let available = fileid_engine::platform::available_memory_mb() as f64 / 1024.0;
    let free = fileid_engine::paths::models_dir()
        .ok()
        .and_then(|path| fileid_engine::platform::available_disk_bytes(&path));
    recommended_vlm_kind(total, available, free)
}

pub fn recommended_vlm_kind_for_host() -> &'static str {
    host_recommended_vlm_kind()
}

// ─── Tab entrypoint ──────────────────────────────────────────────────────────

pub fn build_deep_analyze_tab(engine: Rc<RefCell<EngineClient>>) -> gtk::Widget {
    let default_kind = host_recommended_vlm_kind();
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
    let lbl_active = mono_value(vlm_by_key(default_kind).display);
    let lbl_total = mono_value("0");
    let lbl_pending = mono_value("0");
    let lbl_eta = mono_value("0s");
    content.append(&build_status_card(
        &lbl_active,
        &lbl_total,
        &lbl_pending,
        &lbl_eta,
    ));

    // Model picker (rows populated after `ui` exists).
    let picker_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .build();
    let download_label = gtk::Label::builder()
        .xalign(0.0)
        .css_classes(["dim-label"])
        .build();
    let download_bar = gtk::ProgressBar::builder()
        .show_text(false)
        .css_classes(["gold-accent"])
        .build();
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
    let skip_check =
        gtk::CheckButton::with_label("Skip files already analyzed by the active model");
    skip_check.set_active(true);
    let run_btn = gtk::Button::builder()
        .label("Analyze entire library")
        .sensitive(false)
        .css_classes(["gold-button"])
        .build();
    let folder_btn = gtk::Button::builder()
        .label("Analyze a folder…")
        .css_classes(["pill"])
        .sensitive(vlm_runtime_available())
        .build();
    let cancel_btn = gtk::Button::builder()
        .label("Cancel")
        .css_classes(["destructive-action"])
        .visible(false)
        .build();
    content.append(&build_actions_card(
        &naming_banner,
        &skip_check,
        &run_btn,
        &folder_btn,
        &cancel_btn,
    ));

    let apply_status = gtk::Label::builder()
        .xalign(0.0)
        .css_classes(["dim-label"])
        .wrap(true)
        .build();
    let apply_tags = gtk::Button::builder()
        .label("Apply tags")
        .css_classes(["pill"])
        .build();
    let apply_people = gtk::Button::builder()
        .label("Apply people as tags")
        .css_classes(["pill"])
        .build();
    content.append(&build_apply_card(&apply_status, &apply_tags, &apply_people));

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

    let progress_bar = gtk::ProgressBar::builder()
        .show_text(false)
        .css_classes(["gold-accent"])
        .build();
    let progress_count = mono_value("0 / 0");
    let progress_eta = gtk::Label::builder()
        .xalign(1.0)
        .css_classes(["gold-accent"])
        .build();
    let progress_file = gtk::Label::builder()
        .xalign(0.0)
        .css_classes(["dim-label"])
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .single_line_mode(true)
        .build();
    let progress_caption = gtk::Label::builder().xalign(0.0).wrap(true).build();
    let progress_card = build_progress_card(
        &progress_bar,
        &progress_count,
        &progress_eta,
        &progress_file,
        &progress_caption,
    );
    progress_card.set_visible(false);
    content.append(&progress_card);

    let completion_icon = gtk::Label::new(None);
    let completion_label = gtk::Label::builder()
        .xalign(0.0)
        .css_classes(["dim-label"])
        .build();
    let completion_card = build_completion_card(&completion_icon, &completion_label);
    completion_card.set_visible(false);
    content.append(&completion_card);

    let last_desc = gtk::Label::builder().xalign(0.0).wrap(true).build();
    let last_name = gtk::Label::builder()
        .xalign(0.0)
        .css_classes(["gold-accent"])
        .build();
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
        apply_in_flight: Cell::new(false),
        apply_generation: Cell::new(0),
        active_kind: Cell::new(default_kind),
        lbl_active,
        lbl_total,
        lbl_pending,
        lbl_eta,
        run_btn,
        cancel_btn,
        skip_check,
        naming_banner,
        apply_status,
        apply_tags,
        apply_people,
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
    glib::MainContext::default().spawn_local(clone!(
        #[strong]
        ui,
        async move {
            let rx = ui.engine.borrow_mut().subscribe();
            while let Ok(ev) = rx.recv().await {
                apply_event(&ui, ev);
            }
        }
    ));

    // Initial fill + a fresh read on every tab switch (startup reads can race
    // the engine's DB open; renames/analyzes finish while other tabs are up).
    refresh(&ui);
    {
        let ui = ui.clone();
        scroller.connect_map(move |_| refresh(&ui));
    }

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
    apply_in_flight: Cell<bool>,
    apply_generation: Cell<u64>,
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

    apply_status: gtk::Label,
    apply_tags: gtk::Button,
    apply_people: gtk::Button,
}

// ─── Command routing ─────────────────────────────────────────────────────────

fn send_cmd(ui: &Rc<DeepUi>, payload: CommandPayload) -> bool {
    match ui.engine.borrow_mut().send(payload) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(target: "deep_analyze", "send failed: {error}");
            end_run(ui);
            ui.completion_icon.set_text("!");
            ui.completion_label
                .set_text(&format!("Command could not be sent: {error}"));
            reveal(&ui.completion_card);
            false
        }
    }
}

fn wire_actions(ui: &Rc<DeepUi>, folder_btn: &gtk::Button, apply_all: &gtk::Button) {
    // Analyze entire library → deepAnalyzeAll.
    ui.run_btn.connect_clicked(clone!(
        #[strong]
        ui,
        move |button| {
            if ui.in_flight.get() {
                return;
            }
            // Gate a restricted-model (e.g. Gemma) download behind license
            // acceptance, like macOS gates every Deep Analyze entry point. On
            // acceptance the gate re-emits this click so the run proceeds.
            let kind = ui.active_kind.get();
            if !crate::model_license::ensure_or_prompt(button, kind) {
                return;
            }
            let payload = CommandPayload::DeepAnalyzeAll(DeepAnalyzeAllPayload {
                model_kind: kind.to_string(),
                skip_existing: ui.skip_check.is_active(),
                file_ids: None,
                tags_only: false,
                propose_renames: true,
                excluded_folders: crate::app_settings::deep_analyze_excluded_folders(),
            });
            if send_cmd(&ui, payload) {
                begin_run(&ui);
            }
        }
    ));

    // Analyze a picked folder → deepAnalyzeFolder.
    folder_btn.connect_clicked(clone!(
        #[strong]
        ui,
        move |btn| {
            if ui.in_flight.get() {
                return;
            }
            // Gate the restricted-model download before opening the picker; the
            // gate re-emits this click after acceptance (parity with macOS).
            if !crate::model_license::ensure_or_prompt(btn, ui.active_kind.get()) {
                return;
            }
            let dialog = gtk::FileDialog::builder()
                .title("Pick a folder to Deep Analyze")
                .modal(true)
                .build();
            let parent = btn.root().and_downcast::<gtk::Window>();
            dialog.select_folder(
                parent.as_ref(),
                gio::Cancellable::NONE,
                clone!(
                    #[strong]
                    ui,
                    move |res| {
                        let Ok(file) = res else { return };
                        let Some(path) = file.path() else { return };
                        let payload = CommandPayload::DeepAnalyzeFolder(DeepAnalyzeFolderPayload {
                            path_prefix: path.to_string_lossy().into_owned(),
                            model_kind: ui.active_kind.get().to_string(),
                        });
                        if send_cmd(&ui, payload) {
                            begin_run(&ui);
                        }
                    }
                ),
            );
        }
    ));

    // Cancel → deepAnalyzeCancel.
    ui.cancel_btn.connect_clicked(clone!(
        #[strong]
        ui,
        move |_| {
            send_cmd(&ui, CommandPayload::DeepAnalyzeCancel(Empty {}));
        }
    ));

    // Apply all smart-name renames → renameFiles.
    apply_all.connect_clicked(clone!(
        #[strong]
        ui,
        move |_| {
            let renames: Vec<RenameEntry> = ui
                .proposed
                .borrow()
                .iter()
                .map(|r| RenameEntry {
                    file_id: r.id,
                    new_name: r.new_name(),
                })
                .collect();
            if renames.is_empty() {
                return;
            }
            if send_cmd(
                &ui,
                CommandPayload::RenameFiles(RenameFilesPayload { renames }),
            ) {
                schedule_refresh(&ui, 900);
            }
            apply_file_tags_modes(&ui, &[false, true]);
        }
    ));

    ui.apply_tags.connect_clicked(clone!(
        #[strong]
        ui,
        move |_| apply_file_tags(&ui, false),
    ));
    ui.apply_people.connect_clicked(clone!(
        #[strong]
        ui,
        move |_| apply_file_tags(&ui, true),
    ));
}

fn apply_file_tags(ui: &Rc<DeepUi>, people: bool) {
    apply_file_tags_modes(ui, &[people]);
}

fn apply_file_tags_modes(ui: &Rc<DeepUi>, modes: &[bool]) {
    if ui.in_flight.get() || ui.apply_in_flight.replace(true) {
        return;
    }
    let generation = ui.apply_generation.get().wrapping_add(1);
    ui.apply_generation.set(generation);
    ui.apply_tags.set_sensitive(false);
    ui.apply_people.set_sensitive(false);
    ui.apply_status.set_text(if modes.len() == 1 && modes[0] {
        "Reading named people…"
    } else if modes.len() == 1 {
        "Reading analyzed tags…"
    } else {
        "Reading analyzed tags and named people…"
    });
    let modes = modes.to_vec();
    let ui = ui.clone();
    let event_rx = ui.engine.borrow_mut().subscribe();
    glib::MainContext::default().spawn_local(async move {
        let mut expected_results = 0usize;
        let mut requested_files = 0usize;
        for people in modes {
            let groups = query_tag_groups(people).recv().await.unwrap_or_default();
            for (tag, ids) in groups {
                if ids.is_empty() {
                    continue;
                }
                let sent = ui.engine.borrow_mut().send(CommandPayload::ApplyTags(
                    fileid_engine::ipc::ApplyTagsPayload {
                        file_ids: ids.clone(),
                        tags: vec![tag],
                        mode: fileid_engine::ipc::TagMode::Add,
                    },
                ));
                if sent.is_ok() {
                    requested_files += ids.len();
                    expected_results += 1;
                } else {
                    tracing::warn!(target: "deep_analyze", "applyTags command could not be sent");
                }
            }
        }

        if expected_results == 0 {
            finish_apply_tags(&ui, generation, "Nothing to apply yet. Name people in People or run Deep Analyze first.");
            return;
        }

        // A missing result must not strand the controls after an engine crash.
        // The generation check keeps a late result from an expired job from
        // overwriting the status of a newer apply operation.
        let timeout_ui = ui.clone();
        glib::timeout_add_local_once(Duration::from_secs(15), move || {
            if timeout_ui.apply_in_flight.get()
                && timeout_ui.apply_generation.get() == generation
            {
                finish_apply_tags(
                    &timeout_ui,
                    generation,
                    "Timed out waiting for the engine to finish applying tags. Check the engine status and try again.",
                );
            }
        });

        let mut received_results = 0usize;
        let mut succeeded = 0u32;
        let mut failed = 0u32;
        while received_results < expected_results {
            match event_rx.recv().await {
                Ok(EngineEvent::BulkActionResult(result)) if result.action == "applyTags" => {
                    received_results += 1;
                    succeeded = succeeded.saturating_add(result.succeeded);
                    failed = failed.saturating_add(result.failed);
                }
                Ok(EngineEvent::Exited) | Err(_) => break,
                Ok(_) => {}
            }
        }
        if received_results == expected_results {
            let status = if failed == 0 {
                format!("Applied tags to {succeeded} file updates ({requested_files} queued).")
            } else {
                format!("Applied {succeeded} file updates; {failed} failed. Check the engine log for details.")
            };
            finish_apply_tags(&ui, generation, &status);
        } else if ui.apply_in_flight.get() && ui.apply_generation.get() == generation {
            finish_apply_tags(
                &ui,
                generation,
                "The engine stopped before tag application completed. Check the engine status and try again.",
            );
        }
    });
}

fn finish_apply_tags(ui: &Rc<DeepUi>, generation: u64, status: &str) {
    if ui.apply_generation.get() != generation || !ui.apply_in_flight.get() {
        return;
    }
    ui.apply_in_flight.set(false);
    ui.apply_tags.set_sensitive(true);
    ui.apply_people.set_sensitive(true);
    ui.apply_status.set_text(status);
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
    ui.run_btn.set_sensitive(vlm_runtime_available());
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
            ui.progress_count
                .set_text(&format!("{} / {}", p.processed, p.total));
            match p.eta_seconds {
                Some(eta) => ui
                    .progress_eta
                    .set_text(&format!("ETA {}", format_duration(eta))),
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
            ui.completion_icon
                .set_text(if c.cancelled { "✗" } else { "✓" });
            ui.completion_icon.set_css_classes(if c.cancelled {
                &["lavender-accent"]
            } else {
                &["gold-accent"]
            });
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
        EngineEvent::Error(_) | EngineEvent::ModelDownloadFailed { .. } | EngineEvent::Exited => {
            end_run(ui)
        }
        _ => {}
    }
}

// ─── DB-backed refresh (mirror of macOS ReadStore counts) ────────────────────

fn refresh(ui: &Rc<DeepUi>) {
    let active = ui.active_kind.get().to_string();
    ui.lbl_active.set_text(vlm_by_key(&active).display);

    let rx = query_status(active.clone());
    glib::MainContext::default().spawn_local(clone!(
        #[strong]
        ui,
        async move {
            if let Ok(c) = rx.recv().await {
                apply_status(&ui, c);
            }
        }
    ));

    let rx2 = query_proposed();
    glib::MainContext::default().spawn_local(clone!(
        #[strong]
        ui,
        async move {
            if let Ok(rows) = rx2.recv().await {
                apply_proposed(&ui, rows);
            }
        }
    ));
}

fn apply_status(ui: &Rc<DeepUi>, c: StatusCounts) {
    ui.lbl_total.set_text(&c.total_files.to_string());
    ui.lbl_pending.set_text(&c.pending.to_string());
    let secs = c.pending as f64 * vlm_by_key(ui.active_kind.get()).secs_per_image;
    ui.lbl_eta.set_text(&format_duration(secs));
    ui.naming_banner
        .set_visible(c.named_people == 0 && !ui.in_flight.get());
    if !ui.in_flight.get() {
        ui.run_btn.set_sensitive(vlm_runtime_available());
    }
}

fn apply_proposed(ui: &Rc<DeepUi>, rows: Vec<ProposedRow>) {
    while let Some(child) = ui.smart_list.first_child() {
        ui.smart_list.remove(&child);
    }
    ui.smart_count.set_text(&format!(
        "{} file{}",
        rows.len(),
        if rows.len() == 1 { "" } else { "s" }
    ));
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
        .label(format!("→ {}", row.new_name()))
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
    reanalyze.connect_clicked(clone!(
        #[strong]
        ui,
        move |button| {
            // Gate the restricted-model download (parity with macOS, which gates
            // every Deep Analyze entry point); the gate re-emits on acceptance.
            let kind = ui.active_kind.get();
            if !crate::model_license::ensure_or_prompt(button, kind) {
                return;
            }
            if send_cmd(
                &ui,
                CommandPayload::DeepAnalyzeFile(DeepAnalyzeFilePayload {
                    file_id: id,
                    model_kind: kind.to_string(),
                }),
            ) {
                begin_run(&ui);
            }
        }
    ));
    hbox.append(&reanalyze);

    let rename = gtk::Button::builder()
        .label("Rename")
        .css_classes(["gold-button"])
        .valign(gtk::Align::Center)
        .build();
    let new_name = row.new_name();
    rename.connect_clicked(clone!(
        #[strong]
        ui,
        move |_| {
            if send_cmd(
                &ui,
                CommandPayload::RenameFiles(RenameFilesPayload {
                    renames: vec![RenameEntry {
                        file_id: id,
                        new_name: new_name.clone(),
                    }],
                }),
            ) {
                schedule_refresh(&ui, 700);
            }
        }
    ));
    hbox.append(&rename);

    hbox
}

// ─── Model picker ────────────────────────────────────────────────────────────

pub fn vlm_runtime_available() -> bool {
    std::env::var_os("FLATPAK_ID").is_none()
}

fn populate_picker(ui: &Rc<DeepUi>) {
    for vlm in VLMS.iter() {
        let (installed, gb) = model_install_info(vlm.key);

        let is_default = vlm.key == ui.active_kind.get();

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
            .label(if installed {
                "Downloaded".to_string()
            } else {
                format!("Will download {gb:.1} GB")
            })
            .css_classes(if installed {
                ["kind-badge"]
            } else {
                ["dim-label"]
            })
            .valign(gtk::Align::Center)
            .build();

        let title_row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .build();
        title_row.append(&title);
        title_row.append(&badge);

        let stats = gtk::Label::builder()
            .label(format!(
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
        btn.set_sensitive(vlm_runtime_available());

        let key = vlm.key;
        btn.connect_clicked(clone!(
            #[strong]
            ui,
            move |_| {
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
                ui.skip_check.set_label(Some(&format!(
                    "Skip files already analyzed by {}",
                    vlm_by_key(key).display
                )));
                refresh(&ui);
            }
        ));

        ui.picker_box.append(&btn);
        ui.pick_rows.borrow_mut().push(PickRow {
            key,
            btn,
            indicator,
            title,
        });
    }
}

/// (installed, approx-GB) for a VLM kind, read from the shared engine registry.
fn model_install_info(key: &str) -> (bool, f64) {
    match registry::lookup_full(key) {
        LookupResult::Found(model) => {
            let bytes: u64 = model.files.iter().map(|f| f.approx_bytes).sum();
            let installed = registry::installation_complete(&model);
            (installed, bytes as f64 / 1_073_741_824.0)
        }
        LookupResult::Unknown => (false, 0.0),
    }
}

// ─── DB queries (fresh read-only connection, WAL-safe) ───────────────────────

#[derive(Default)]
struct StatusCounts {
    total_files: i64,
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
    spawn_db(move |conn| status_counts(conn, &active))
}

fn status_counts(conn: &rusqlite::Connection, active: &str) -> StatusCounts {
    let total_files: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM files WHERE kind IN ('image','video','pdf','doc','audio','model') \
             AND failed = 0 AND (kind != 'model' OR lower(path_text) LIKE '%.obj')",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let pending: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM files WHERE kind IN ('image','video','pdf','doc','audio','model') \
             AND failed = 0 AND (kind != 'model' OR lower(path_text) LIKE '%.obj') \
             AND (vlm_full_model IS NULL OR vlm_full_model <> ?1)",
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
    StatusCounts {
        total_files,
        pending,
        named_people,
    }
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
            Ok(iter) => iter
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    })
}

fn query_tag_groups(people: bool) -> async_channel::Receiver<Vec<(String, Vec<i64>)>> {
    spawn_db(move |conn| {
        let sql = if people {
            "SELECT fp.file_id, p.title, p.first_name, p.middle_name, p.last_name, p.suffix, p.name \
             FROM face_prints fp INNER JOIN persons p ON p.id = fp.person_id \
             INNER JOIN files f ON f.id = fp.file_id \
             WHERE f.failed = 0 AND IFNULL(p.is_unknown, 0) = 0 \
             AND (p.name IS NOT NULL OR p.title IS NOT NULL OR p.first_name IS NOT NULL \
                  OR p.middle_name IS NOT NULL OR p.last_name IS NOT NULL OR p.suffix IS NOT NULL)"
        } else {
            "SELECT file_id, tag, NULL, NULL, NULL, NULL, NULL FROM tags \
             INNER JOIN files ON files.id = tags.file_id \
             WHERE files.failed = 0 AND tag IS NOT NULL AND tag <> ''"
        };
        let Ok(mut stmt) = conn.prepare(sql) else {
            return Vec::new();
        };
        let Ok(rows) = stmt.query_map([], |row| {
            let file_id = row.get::<_, i64>(0)?;
            if people {
                let mut parts = Vec::new();
                for index in 1..=5 {
                    if let Ok(Some(value)) = row.get::<_, Option<String>>(index) {
                        let value = value.trim();
                        if !value.is_empty() {
                            parts.push(value.to_string());
                        }
                    }
                }
                let legacy = row.get::<_, Option<String>>(6)?.unwrap_or_default();
                Ok((file_id, person_tag_name(&parts, &legacy)))
            } else {
                Ok((file_id, row.get::<_, String>(1)?))
            }
        }) else {
            return Vec::new();
        };
        group_tag_rows(rows.flatten())
    })
}

fn group_tag_rows(rows: impl IntoIterator<Item = (i64, String)>) -> Vec<(String, Vec<i64>)> {
    let mut grouped: std::collections::BTreeMap<String, std::collections::BTreeSet<i64>> =
        std::collections::BTreeMap::new();
    for (file_id, raw_tag) in rows {
        let tag = raw_tag.trim();
        if tag.is_empty() {
            continue;
        }
        grouped.entry(tag.to_string()).or_default().insert(file_id);
    }
    grouped
        .into_iter()
        .map(|(tag, ids)| (tag, ids.into_iter().collect()))
        .collect()
}

fn person_tag_name(parts: &[String], legacy: &str) -> String {
    if parts.is_empty() {
        legacy.trim().to_string()
    } else {
        parts.join(" ")
    }
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
        "Reads photos, videos, documents, PDFs, and audio metadata to write useful descriptions and smart filenames.",
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
    dismiss.connect_clicked(clone!(
        #[weak]
        card,
        move |_| card.set_visible(false)
    ));
    row.append(&icon);
    row.append(&text);
    row.append(&dismiss);
    card.append(&row);
    card
}

fn build_status_card(
    active: &gtk::Label,
    total: &gtk::Label,
    pending: &gtk::Label,
    eta: &gtk::Label,
) -> gtk::Box {
    let card = glass_card();
    card.append(&heading("Library status"));
    card.append(&wrap_caption(
        "Run a scan first (top bar). Then come back here — Deep Analyze adds human-readable \
         captions and suggests smart filenames for every supported file.",
    ));
    let grid = gtk::Grid::builder()
        .row_spacing(4)
        .column_spacing(16)
        .build();
    grid.attach(&dim_key("Active model"), 0, 0, 1, 1);
    grid.attach(active, 1, 0, 1, 1);
    grid.attach(&dim_key("Total files"), 0, 1, 1, 1);
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
    let runtime_note = if std::env::var_os("FLATPAK_ID").is_some() {
        "Deep Analyze is unavailable in this Flatpak until a reviewed llama-mtmd-cli is bundled; do not download VLM weights yet."
    } else {
        "Weights download on first run. A compatible external llama-mtmd-cli must be visible on PATH."
    };
    card.append(&wrap_caption(&format!(
        "Reads supported media and documents, then writes captions, tags, and smart filenames. {runtime_note}"
    )));
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

fn build_apply_card(status: &gtk::Label, tags: &gtk::Button, people: &gtk::Button) -> gtk::Box {
    let card = glass_card();
    card.append(&heading("Apply to your files"));
    card.append(&wrap_caption(
        "Write analyzed tags and named people onto the files as native Linux file tags. Existing tags are preserved.",
    ));
    let actions = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(10)
        .build();
    actions.append(tags);
    actions.append(people);
    card.append(&actions);
    card.append(status);
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

#[cfg(test)]
mod recommendation_tests {
    use super::*;

    const PLENTY: u64 = 200 * 1024 * 1024 * 1024;

    #[test]
    fn low_ram_uses_gemma() {
        assert_eq!(recommended_vlm_kind(8.0, 6.0, Some(PLENTY)), "gemma_3_4b");
    }

    #[test]
    fn balanced_cpu_machine_uses_qwen() {
        assert_eq!(
            recommended_vlm_kind(15.5, 12.0, Some(PLENTY)),
            "qwen2_5_vl_7b"
        );
    }

    #[test]
    fn cpu_only_linux_reserves_mistral_for_very_large_memory_hosts() {
        assert_eq!(
            recommended_vlm_kind(30.9, 24.0, Some(PLENTY)),
            "qwen2_5_vl_7b"
        );
        assert_eq!(
            recommended_vlm_kind(48.0, 40.0, Some(PLENTY)),
            "mistral_small_3_2"
        );
    }

    #[test]
    fn status_pending_uses_full_completion_model() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE files (
                id INTEGER PRIMARY KEY,
                kind TEXT NOT NULL,
                path_text TEXT NOT NULL DEFAULT '',
                failed INTEGER NOT NULL DEFAULT 0,
                vlm_description TEXT,
                vlm_model TEXT,
                vlm_full_model TEXT
            );
            CREATE TABLE persons (
                name TEXT,
                first_name TEXT,
                last_name TEXT
            );
            INSERT INTO files VALUES
                (1, 'image', 'a.jpg', 0, NULL, 'model-a', 'model-a'),
                (2, 'image', 'b.jpg', 0, 'legacy', 'model-a', NULL),
                (3, 'image', 'c.jpg', 0, 'other', 'model-b', 'model-b'),
                (4, 'video', 'd.mp4', 0, 'done', 'model-a', 'model-a');
            INSERT INTO persons VALUES (NULL, 'Ada', NULL);",
        )
        .unwrap();

        let counts = status_counts(&conn, "model-a");
        assert_eq!(counts.total_files, 4);
        assert_eq!(counts.pending, 2);
        assert_eq!(counts.named_people, 1);
    }

    #[test]
    fn person_tag_name_prefers_structured_fields_and_trims_legacy() {
        assert_eq!(
            person_tag_name(&["Dr".into(), "Ada".into(), "Lovelace".into()], " old "),
            "Dr Ada Lovelace"
        );
        assert_eq!(person_tag_name(&[], "  old name  "), "old name");
        assert_eq!(person_tag_name(&[], "   "), "");
    }

    #[test]
    fn group_tag_rows_is_sorted_and_deduplicates_file_ids() {
        assert_eq!(
            group_tag_rows(vec![
                (9, " Ada Lovelace ".into()),
                (3, "Ada Lovelace".into()),
                (9, "Ada Lovelace".into()),
                (3, "".into()),
                (4, "Beach".into()),
            ]),
            vec![
                ("Ada Lovelace".into(), vec![3, 9]),
                ("Beach".into(), vec![4]),
            ]
        );
    }
}
