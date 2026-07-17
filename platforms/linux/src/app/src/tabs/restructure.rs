// Restructure tab — butler-grade folder reorg, the 1:1 port of macOS
// `RestructureView.swift` + `Restructure/SankeyFlowView.swift`.
//
//   * "Compute plan" sends `planRestructure`; the engine's authoritative
//     `restructurePlan` event maps onto a `Proposal` list + summary,
//   * a Cairo Sankey (`gtk::DrawingArea`, Bézier ribbons, Okabe-Ito CVD-safe
//     palette) traces source folders → destination buckets — hover a ribbon to
//     highlight it, click a node to drill in,
//   * recommendation cards grouped by destination bucket (count desc, name asc),
//     each row a checkbox + filename + "from X" + reason + confidence badge,
//   * a drill-down `adw::Dialog` lists every file in a scope,
//   * an apply bar sends `applyRestructure` with a `useSymlinks` toggle —
//     real moves are permanent-but-undoable; symlinks leave originals in place,
//   * after a real move, an undo bar replays the engine's on-disk undo journal.
//
// Ask-confidence moves start UNchecked (RESTRUCTURE.md §6) — the user must opt
// them in before applying.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::f64::consts::PI;
use std::path::Path;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use gtk::glib::clone;

use crate::engine_client::{EngineClient, EngineEvent};
use fileid_engine::ipc::{
    ApplyRestructurePayload, CommandPayload, PlanRestructurePayload, RestructureMove,
    RestructurePlan, UndoRestructurePayload,
};

// Okabe-Ito CVD-safe palette (RESTRUCTURE.md §7) as RGB triples in 0..1.
const OKABE_ITO: [(f64, f64, f64); 7] = [
    (0.000, 0.447, 0.698), // blue
    (0.835, 0.369, 0.000), // vermilion
    (0.000, 0.620, 0.451), // green
    (0.337, 0.706, 0.914), // sky blue
    (0.902, 0.624, 0.000), // orange
    (0.800, 0.475, 0.655), // purple
    (0.941, 0.894, 0.259), // yellow
];

const NEUTRAL: (f64, f64, f64) = (0.62, 0.62, 0.68);
const TOP_N: usize = 8;
const CARD_ROW_CAP: usize = 12;
const DRILL_ROW_CAP: usize = 300;
const SRC_OTHER: &str = "src:__other__";
const DST_OTHER: &str = "dst:__other__";

// ─── View-model ──────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProposalKind {
    Dissolved,         // file from a Junk folder
    MovedOutAsOutlier, // outlier from a Mixed folder
}

#[derive(Clone)]
struct Proposal {
    file_id: i64,
    bucket: String,        // destination bucket, e.g. "People/Marie Curie"
    source_folder: String, // current parent folder
    filename: String,
    source_name: String, // basename of the source folder
    kind: ProposalKind,
    confidence: String, // "auto" / "review" / "ask" / ""
    reason: Option<String>,
}

#[derive(Clone, Default)]
struct Summary {
    anchor_folders: u32,
    mixed_folders: u32,
    junk_folders: u32,
    moved_out_files: usize,
    dissolved_files: usize,
}

#[derive(Clone)]
struct SankeyNode {
    id: String,
    label: String,
    identity: String, // full path / bucket / rollup id
    count: usize,
    is_rollup: bool,
    tint: (f64, f64, f64),
    rollup_members: Vec<String>,
}

#[derive(Clone)]
struct SankeyFlow {
    src_id: String,
    dst_id: String,
    tint: (f64, f64, f64),
    count: usize,
}

#[derive(Clone, Default)]
struct SankeyModel {
    sources: Vec<SankeyNode>,
    destinations: Vec<SankeyNode>,
    flows: Vec<SankeyFlow>,
    total_flow: usize,
}

#[derive(Clone)]
enum DrillScope {
    Bucket(String),
    Source(String),
    SourceFolders(Vec<String>),
    DestBuckets(Vec<String>),
}

#[derive(Clone)]
struct ApplyRequest {
    root: String,
    moves: Vec<RestructureMove>,
    plan_id: Option<String>,
    total: usize,
}

#[derive(Default)]
struct State {
    root: Option<String>,
    plan: Option<RestructurePlan>,
    proposals: Vec<Proposal>,
    selected: HashSet<i64>,
    prior_deselected: HashSet<i64>,
    summary: Summary,
    sankey: SankeyModel,
    hovered_flow: Option<usize>,
    loading: bool,
    applying: bool,
    can_undo: bool,
    undo_in_flight: bool,
    last_apply_symlinks: bool,
}

struct Ui {
    root: gtk::Overlay,
    stack: gtk::Stack,
    empty_page: adw::StatusPage,
    dest_label: gtk::Label,
    pick_btn: gtk::Button,
    stat_hero: gtk::Box,
    sankey: gtk::DrawingArea,
    sankey_stat: gtk::Label,
    rec_list: gtk::Box,
    status_row: gtk::Box,
    status_icon: gtk::Image,
    status_label: gtk::Label,
    bottom_spacer: gtk::Box,
    apply_bar: gtk::Box,
    undo_bar: gtk::Box,
    selected_count_label: gtk::Label,
    apply_btn: gtk::Button,
    symlink_switch: gtk::Switch,
    apply_hint: gtk::Label,
}

// ─── Entry point ─────────────────────────────────────────────────────────────

pub fn build_restructure_tab(engine: Rc<RefCell<EngineClient>>) -> gtk::Widget {
    let state = Rc::new(RefCell::new(State::default()));

    // ── Header ───────────────────────────────────────────────────────────────
    let header_icon = gtk::Image::from_icon_name("view-list-symbolic");
    header_icon.add_css_class("gold-accent");
    header_icon.set_pixel_size(28);

    let title = gtk::Label::builder()
        .label("Restructure")
        .xalign(0.0)
        .css_classes(["title-1"])
        .build();
    let subtitle = gtk::Label::builder()
        .label("FileID keeps the well-named folders and proposes a tidier home for the rest. Nothing moves until you apply.")
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dim-label"])
        .build();
    let title_vb = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .hexpand(true)
        .build();
    title_vb.append(&title);
    title_vb.append(&subtitle);

    let pick_btn = gtk::Button::builder()
        .label("Pick destination root…")
        .valign(gtk::Align::Start)
        .build();

    let title_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .build();
    title_row.append(&header_icon);
    title_row.append(&title_vb);
    title_row.append(&pick_btn);

    let dest_label = gtk::Label::builder()
        .label("")
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::Start)
        .visible(false)
        .css_classes(["dim-label", "caption"])
        .build();

    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .build();
    header.append(&title_row);
    header.append(&dest_label);

    // ── Stack pages: empty / loading / content ────────────────────────────────
    let stack = gtk::Stack::new();

    let empty_page = adw::StatusPage::builder()
        .icon_name("view-list-symbolic")
        .title("Pick a destination root")
        .description("Choose where the tidy folder hierarchy should live. Nothing moves until you review the plan and choose Apply.")
        .height_request(440)
        .build();
    stack.add_named(&empty_page, Some("empty"));

    let loading_box = build_loading_page();
    stack.add_named(&loading_box, Some("loading"));

    // Content page.
    let stat_hero = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .homogeneous(true)
        .build();

    let (surface, surface_inner) = padded_card();
    surface_inner.set_spacing(12);

    // Sankey section.
    let sankey_icon = gtk::Image::from_icon_name("view-list-symbolic");
    sankey_icon.add_css_class("gold-accent");
    let sankey_title = gtk::Label::builder()
        .label("Folder map")
        .css_classes(["heading"])
        .build();
    let sankey_stat = gtk::Label::builder()
        .label("")
        .hexpand(true)
        .xalign(1.0)
        .css_classes(["dim-label", "caption"])
        .build();
    let sankey_header = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    sankey_header.append(&sankey_icon);
    sankey_header.append(&sankey_title);
    sankey_header.append(&sankey_stat);

    let sankey = gtk::DrawingArea::builder()
        .content_height(360)
        .hexpand(true)
        .build();

    let sep = gtk::Separator::new(gtk::Orientation::Horizontal);

    // Recommendations section.
    let rec_icon = gtk::Image::from_icon_name("starred-symbolic");
    rec_icon.add_css_class("gold-accent");
    let rec_title = gtk::Label::builder()
        .label("Recommendations")
        .css_classes(["heading"])
        .build();
    let rec_hint = gtk::Label::builder()
        .label("Grouped by destination folder")
        .hexpand(true)
        .xalign(1.0)
        .css_classes(["dim-label", "caption"])
        .build();
    let rec_header = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    rec_header.append(&rec_icon);
    rec_header.append(&rec_title);
    rec_header.append(&rec_hint);

    let rec_list = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .build();

    surface_inner.append(&sankey_header);
    surface_inner.append(&sankey);
    surface_inner.append(&sep);
    surface_inner.append(&rec_header);
    surface_inner.append(&rec_list);

    let content_page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .build();
    content_page.append(&stat_hero);
    content_page.append(&surface);
    stack.add_named(&content_page, Some("content"));

    // ── Status banner ─────────────────────────────────────────────────────────
    let (status_row, status_inner) = padded_card();
    status_inner.set_orientation(gtk::Orientation::Horizontal);
    status_inner.set_spacing(10);
    let status_icon = gtk::Image::from_icon_name("emblem-ok-symbolic");
    let status_label = gtk::Label::builder()
        .label("")
        .xalign(0.0)
        .wrap(true)
        .build();
    status_inner.append(&status_icon);
    status_inner.append(&status_label);
    status_row.set_visible(false);

    let bottom_spacer = gtk::Box::builder()
        .height_request(120)
        .visible(false)
        .build();

    // ── Scroll body ───────────────────────────────────────────────────────────
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .margin_top(16)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .css_classes(["fileid-tab"])
        .build();
    body.append(&header);
    body.append(&stack);
    body.append(&status_row);
    body.append(&bottom_spacer);

    let scroller = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&body)
        .css_classes(["fileid-tab"])
        .build();

    // ── Bottom bars (undo + apply), floating over the scroll body ─────────────
    let (undo_bar, undo_inner) = padded_card();
    undo_inner.set_orientation(gtk::Orientation::Horizontal);
    undo_inner.set_spacing(10);
    let undo_icon = gtk::Image::from_icon_name("edit-undo-symbolic");
    undo_icon.add_css_class("gold-accent");
    let undo_label = gtk::Label::builder()
        .label("Files were moved on disk — you can put them back.")
        .hexpand(true)
        .xalign(0.0)
        .css_classes(["dim-label"])
        .build();
    let undo_btn = gtk::Button::builder()
        .label("Undo last run")
        .css_classes(["gold-button"])
        .build();
    undo_inner.append(&undo_icon);
    undo_inner.append(&undo_label);
    undo_inner.append(&undo_btn);
    undo_bar.set_visible(false);

    let (apply_bar, apply_inner) = padded_card();
    apply_inner.set_spacing(6);
    let apply_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .build();
    let selected_count_label = gtk::Label::builder()
        .label("0 of 0 selected")
        .hexpand(true)
        .xalign(0.0)
        .css_classes(["heading"])
        .build();
    let symlink_label = gtk::Label::builder()
        .label("Use shortcuts (preview)")
        .css_classes(["dim-label", "caption"])
        .build();
    let symlink_switch = gtk::Switch::new();
    symlink_switch.set_valign(gtk::Align::Center);
    symlink_switch
        .set_tooltip_text(Some("On: leave originals in place and mirror the layout with shortcuts. Off: move files on disk (reversible with Undo)."));
    let apply_btn = gtk::Button::builder()
        .label("Apply")
        .sensitive(false)
        .css_classes(["gold-button"])
        .build();
    apply_row.append(&selected_count_label);
    apply_row.append(&symlink_label);
    apply_row.append(&symlink_switch);
    apply_row.append(&apply_btn);
    let apply_hint = gtk::Label::builder()
        .label("")
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dim-label", "caption"])
        .build();
    apply_inner.append(&apply_row);
    apply_inner.append(&apply_hint);
    apply_bar.set_visible(false);

    let bottom_bars = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .valign(gtk::Align::End)
        .halign(gtk::Align::Fill)
        .margin_start(16)
        .margin_end(16)
        .margin_bottom(16)
        .build();
    bottom_bars.append(&undo_bar);
    bottom_bars.append(&apply_bar);

    let root = gtk::Overlay::builder().css_classes(["fileid-tab"]).build();
    root.set_child(Some(&scroller));
    root.add_overlay(&bottom_bars);

    let ui = Rc::new(Ui {
        root: root.clone(),
        stack,
        empty_page,
        dest_label,
        pick_btn: pick_btn.clone(),
        stat_hero,
        sankey: sankey.clone(),
        sankey_stat,
        rec_list,
        status_row,
        status_icon,
        status_label,
        bottom_spacer,
        apply_bar,
        undo_bar,
        selected_count_label,
        apply_btn: apply_btn.clone(),
        symlink_switch: symlink_switch.clone(),
        apply_hint,
    });

    update_apply_hint(&ui, false);

    // ── Sankey rendering + interaction ────────────────────────────────────────
    sankey.set_draw_func(clone!(
        #[strong]
        state,
        move |_area, cr, w, h| {
            draw_sankey(&state, cr, w, h);
        }
    ));

    let motion = gtk::EventControllerMotion::new();
    motion.connect_motion(clone!(
        #[strong]
        state,
        #[weak]
        sankey,
        move |_, x, y| {
            let next = hit_test_ribbon(&state, &sankey, x, y);
            let changed = state.borrow().hovered_flow != next;
            if changed {
                state.borrow_mut().hovered_flow = next;
                sankey.queue_draw();
            }
        }
    ));
    motion.connect_leave(clone!(
        #[strong]
        state,
        #[weak]
        sankey,
        move |_| {
            if state.borrow().hovered_flow.is_some() {
                state.borrow_mut().hovered_flow = None;
                sankey.queue_draw();
            }
        }
    ));
    sankey.add_controller(motion);

    let click = gtk::GestureClick::new();
    click.connect_released(clone!(
        #[strong]
        state,
        #[strong]
        ui,
        #[weak]
        sankey,
        move |_, _, x, y| {
            if let Some(scope) = hit_test_node(&state, &sankey, x, y) {
                open_drilldown(&state, &ui, scope);
            }
        }
    ));
    sankey.add_controller(click);

    sankey.set_focusable(true);
    sankey.set_tooltip_text(Some(
        "Folder map. Use Left and Right to choose a folder, then Enter to open details.",
    ));
    let keyboard_node = Rc::new(Cell::new(0usize));
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(clone!(
        #[strong]
        state,
        #[strong]
        ui,
        #[strong]
        keyboard_node,
        #[weak]
        sankey,
        move |_, key, _, _| {
            let total = {
                let state = state.borrow();
                state.sankey.sources.len() + state.sankey.destinations.len()
            };
            if total == 0 {
                return glib::Propagation::Proceed;
            }
            match key {
                gtk::gdk::Key::Left | gtk::gdk::Key::Up => {
                    keyboard_node.set(keyboard_node.get().checked_sub(1).unwrap_or(total - 1));
                }
                gtk::gdk::Key::Right | gtk::gdk::Key::Down => {
                    keyboard_node.set((keyboard_node.get() + 1) % total);
                }
                gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter | gtk::gdk::Key::space => {
                    if let Some((scope, _)) = keyboard_node_scope(&state, keyboard_node.get()) {
                        open_drilldown(&state, &ui, scope);
                    }
                    return glib::Propagation::Stop;
                }
                _ => return glib::Propagation::Proceed,
            }
            if let Some((_, label)) = keyboard_node_scope(&state, keyboard_node.get()) {
                sankey.set_tooltip_text(Some(&format!("Selected folder map node: {label}")));
            }
            glib::Propagation::Stop
        }
    ));
    sankey.add_controller(keys);

    // ── Header pick button ────────────────────────────────────────────────────
    pick_btn.connect_clicked(clone!(
        #[strong]
        engine,
        #[strong]
        state,
        #[strong]
        ui,
        move |_| {
            let dialog = gtk::FileDialog::builder()
                .title("Pick a destination root")
                .modal(true)
                .build();
            let win = ui.root.root().and_downcast::<gtk::Window>();
            dialog.select_folder(
                win.as_ref(),
                gio::Cancellable::NONE,
                clone!(
                    #[strong]
                    engine,
                    #[strong]
                    state,
                    #[strong]
                    ui,
                    move |result| {
                        if let Ok(file) = result {
                            if let Some(path) = file.path() {
                                set_root_and_plan(
                                    &engine,
                                    &state,
                                    &ui,
                                    path.to_string_lossy().into_owned(),
                                );
                            }
                        }
                    }
                ),
            );
        }
    ));

    // ── Apply / symlink / undo wiring ─────────────────────────────────────────
    symlink_switch.connect_active_notify(clone!(
        #[strong]
        ui,
        move |sw| {
            update_apply_hint(&ui, sw.is_active());
        }
    ));

    apply_btn.connect_clicked(clone!(
        #[strong]
        engine,
        #[strong]
        state,
        #[strong]
        ui,
        move |_| {
            if state.borrow().applying {
                return;
            }
            let request = {
                let s = state.borrow();
                let (Some(plan), Some(root)) = (s.plan.as_ref(), s.root.clone()) else {
                    return;
                };
                if plan.truncated {
                    let Some(plan_id) = plan.plan_id.clone() else {
                        return;
                    };
                    ApplyRequest {
                        root,
                        moves: Vec::new(),
                        plan_id: Some(plan_id),
                        total: usize::try_from(plan.total_moves.unwrap_or(plan.moves.len() as u64))
                            .unwrap_or(usize::MAX),
                    }
                } else {
                    let moves: Vec<RestructureMove> = plan
                        .moves
                        .iter()
                        .filter(|m| s.selected.contains(&m.file_id))
                        .cloned()
                        .collect();
                    let total = moves.len();
                    ApplyRequest {
                        root,
                        moves,
                        plan_id: None,
                        total,
                    }
                }
            };
            if request.total == 0 {
                return;
            }
            confirm_apply(&engine, &state, &ui, request, ui.symlink_switch.is_active());
        }
    ));

    undo_btn.connect_clicked(clone!(
        #[strong]
        engine,
        #[strong]
        state,
        #[strong]
        ui,
        move |_| {
            if state.borrow().applying {
                return;
            }
            let Some(root) = state.borrow().root.clone() else {
                return;
            };
            {
                let mut s = state.borrow_mut();
                s.applying = true;
                s.undo_in_flight = true;
            }
            set_status(&ui, "Undoing the last restructure…", false);
            let sent =
                engine
                    .borrow_mut()
                    .send(CommandPayload::UndoRestructure(UndoRestructurePayload {
                        library_root: root,
                    }));
            if sent.is_err() {
                {
                    let mut s = state.borrow_mut();
                    s.applying = false;
                    s.undo_in_flight = false;
                }
                set_status(&ui, "Engine is unavailable — try again in a moment.", true);
            }
            update_apply_controls(&state, &ui);
        }
    ));

    // ── Engine events ─────────────────────────────────────────────────────────
    let ev_rx = engine.borrow_mut().subscribe();
    glib::MainContext::default().spawn_local(clone!(
        #[strong]
        engine,
        #[strong]
        state,
        #[strong]
        ui,
        async move {
            while let Ok(ev) = ev_rx.recv().await {
                match ev {
                    EngineEvent::RestructurePlan(plan) => on_plan(&state, &ui, plan),
                    EngineEvent::RestructureApplyResult(result) => {
                        on_apply_result(&engine, &state, &ui, result)
                    }
                    EngineEvent::Error(msg) => on_error(&state, &ui, &msg),
                    EngineEvent::Exited => on_exited(&state, &ui),
                    _ => {}
                }
            }
        }
    ));

    // ── Auto-default the destination root to the most recent scan ─────────────
    let rx = recent_root_async();
    glib::MainContext::default().spawn_local(clone!(
        #[strong]
        engine,
        #[strong]
        state,
        #[strong]
        ui,
        async move {
            if let Ok(Some(found)) = rx.recv().await {
                if state.borrow().root.is_none() {
                    set_root_and_plan(&engine, &state, &ui, found);
                }
            }
        }
    ));

    root.upcast()
}

fn build_loading_page() -> gtk::Box {
    let b = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .valign(gtk::Align::Center)
        .halign(gtk::Align::Center)
        .height_request(440)
        .build();
    let spinner = gtk::Spinner::builder()
        .spinning(true)
        .width_request(36)
        .height_request(36)
        .build();
    let l1 = gtk::Label::builder()
        .label("Computing proposals…")
        .css_classes(["title-4"])
        .build();
    let l2 = gtk::Label::builder()
        .label("Looking at every folder, classifying it, and picking a tidy home for every file.")
        .wrap(true)
        .justify(gtk::Justification::Center)
        .max_width_chars(48)
        .css_classes(["dim-label"])
        .build();
    b.append(&spinner);
    b.append(&l1);
    b.append(&l2);
    b
}

// ─── Plan request + lifecycle ────────────────────────────────────────────────

fn set_root_and_plan(
    engine: &Rc<RefCell<EngineClient>>,
    state: &Rc<RefCell<State>>,
    ui: &Rc<Ui>,
    root: String,
) {
    {
        let mut s = state.borrow_mut();
        s.root = Some(root.clone());
        s.prior_deselected.clear();
        // Undo is journal-keyed by the root that was applied. Switching to a
        // different destination invalidates the "Undo last run" affordance —
        // leaving it armed would send the NEW root to UndoRestructure and
        // silently no-op (or error) against the wrong journal. Clear it.
        s.can_undo = false;
    }
    ui.dest_label.set_text(&format!("Destination: {root}"));
    ui.dest_label.set_visible(true);
    ui.pick_btn.set_label("Change destination…");
    request_plan(engine, state, ui);
}

fn request_plan(engine: &Rc<RefCell<EngineClient>>, state: &Rc<RefCell<State>>, ui: &Rc<Ui>) {
    let Some(root) = state.borrow().root.clone() else {
        return;
    };
    // Preserve the user's per-file unchecks across a re-plan (computed under a
    // separate immutable borrow before the mutable one below).
    let dropped: HashSet<i64> = {
        let s = state.borrow();
        s.proposals
            .iter()
            .map(|p| p.file_id)
            .filter(|id| !s.selected.contains(id))
            .collect()
    };
    {
        let mut s = state.borrow_mut();
        s.prior_deselected = dropped;
        s.loading = true;
    }
    ui.stack.set_visible_child_name("loading");
    set_status(ui, "", false);
    update_bars(state, ui);
    let sent = engine
        .borrow_mut()
        .send(CommandPayload::PlanRestructure(PlanRestructurePayload {
            library_root: root,
            supports_paged_plans: true,
        }));
    if sent.is_err() {
        state.borrow_mut().loading = false;
        show_empty(
            ui,
            "Pick a destination root",
            "Choose a folder, then FileID proposes a tidier layout.",
        );
        set_status(ui, "Engine is starting — try again in a moment.", true);
        update_bars(state, ui);
    }
}

fn on_plan(state: &Rc<RefCell<State>>, ui: &Rc<Ui>, plan: RestructurePlan) {
    // Ignore a plan computed for a different destination root (the user may
    // have switched folders while a plan was in flight).
    {
        let s = state.borrow();
        let Some(root) = s.root.as_ref() else { return };
        if normalize(&plan.library_root) != normalize(root) {
            return;
        }
    }

    let truncated = plan.truncated;
    let total_moves = plan.total_moves.unwrap_or(plan.moves.len() as u64);
    let proposals = map_proposals(&plan);
    let summary = make_summary(&plan, &proposals);
    let sankey = build_sankey(&proposals);

    let mut selected: HashSet<i64> = proposals.iter().map(|p| p.file_id).collect();
    // Ask-confidence moves start UNchecked (RESTRUCTURE.md §6).
    for p in &proposals {
        if p.confidence.eq_ignore_ascii_case("ask") {
            selected.remove(&p.file_id);
        }
    }
    {
        let s = state.borrow();
        for id in &s.prior_deselected {
            selected.remove(id);
        }
    }

    let is_empty = proposals.is_empty();
    {
        let mut s = state.borrow_mut();
        s.plan = Some(plan);
        s.proposals = proposals;
        s.summary = summary;
        s.sankey = sankey;
        s.selected = selected;
        s.hovered_flow = None;
        s.loading = false;
    }

    if is_empty {
        show_empty(
            ui,
            "Nothing to move",
            "Your library is already organized — every folder is a recognized anchor.",
        );
    } else if truncated {
        show_empty(
            ui,
            "Large plan ready",
            &format!(
                "The engine stored all {total_moves} moves as one bounded, undoable plan. Apply runs the complete plan without loading every path into the app."
            ),
        );
    } else {
        ui.sankey_stat
            .set_text(&sankey_header_stat(&state.borrow().proposals));
        render_stat_hero(state, ui);
        render_recommendations(state, ui);
        ui.sankey.queue_draw();
        ui.stack.set_visible_child_name("content");
    }
    update_bars(state, ui);
    update_apply_controls(state, ui);
}

fn on_apply_result(
    engine: &Rc<RefCell<EngineClient>>,
    state: &Rc<RefCell<State>>,
    ui: &Rc<Ui>,
    result: fileid_engine::ipc::RestructureApplyResult,
) {
    let was_undo = {
        let mut s = state.borrow_mut();
        let u = s.undo_in_flight;
        s.undo_in_flight = false;
        s.applying = false;
        s.can_undo = if u {
            false
        } else {
            !s.last_apply_symlinks && result.applied > 0
        };
        u
    };
    match result.privilege_error.as_ref().filter(|p| !p.is_empty()) {
        Some(p) => set_status(ui, p, true),
        None if was_undo => set_status(
            ui,
            &format!(
                "Put back {} file{}",
                result.applied,
                plural(result.applied as usize)
            ),
            false,
        ),
        None => set_status(
            ui,
            &format!("{} moved · {} failed", result.applied, result.failed),
            false,
        ),
    }
    // Moved files now carry new paths — refresh the plan.
    request_plan(engine, state, ui);
    update_bars(state, ui);
}

fn on_error(state: &Rc<RefCell<State>>, ui: &Rc<Ui>, msg: &str) {
    let (was_loading, was_applying) = {
        let s = state.borrow();
        (s.loading, s.applying)
    };
    if was_applying {
        state.borrow_mut().applying = false;
        set_status(
            ui,
            "The engine reported an error — recheck your library and try again.",
            true,
        );
        update_apply_controls(state, ui);
    }
    if was_loading {
        state.borrow_mut().loading = false;
        show_empty(ui, "Couldn't compute a plan", "Please try again.");
        set_status(ui, &format!("Engine: {msg}"), true);
        update_bars(state, ui);
    }
}

fn on_exited(state: &Rc<RefCell<State>>, ui: &Rc<Ui>) {
    let (was_loading, was_applying) = {
        let s = state.borrow();
        (s.loading, s.applying)
    };
    if was_applying {
        {
            let mut s = state.borrow_mut();
            s.applying = false;
            s.undo_in_flight = false;
        }
        set_status(
            ui,
            "Engine restarted — apply interrupted. Recheck your library and try again.",
            true,
        );
    }
    if was_loading {
        state.borrow_mut().loading = false;
    }
    update_apply_controls(state, ui);
    update_bars(state, ui);
}

// ─── Apply ───────────────────────────────────────────────────────────────────

fn confirm_apply(
    engine: &Rc<RefCell<EngineClient>>,
    state: &Rc<RefCell<State>>,
    ui: &Rc<Ui>,
    request: ApplyRequest,
    use_symlinks: bool,
) {
    let n = request.total;
    let body = if use_symlinks {
        "FileID will create a browsable shortcut tree mirroring the new layout. Your original files stay exactly where they are."
    } else {
        "FileID will move the selected files into the new structure on disk and update its library. You can reverse the whole run with Undo last run right after — but review the structure first."
    };
    let heading = format!("Apply {n} move{}?", plural(n));
    let alert = adw::AlertDialog::new(Some(heading.as_str()), Some(body));
    alert.add_response("cancel", "Cancel");
    alert.add_response(
        "apply",
        if use_symlinks {
            "Create shortcuts"
        } else {
            "Apply real moves"
        },
    );
    alert.set_response_appearance(
        "apply",
        if use_symlinks {
            adw::ResponseAppearance::Suggested
        } else {
            adw::ResponseAppearance::Destructive
        },
    );
    alert.set_default_response(Some("cancel"));
    alert.set_close_response("cancel");
    alert.connect_response(
        None,
        clone!(
            #[strong]
            engine,
            #[strong]
            state,
            #[strong]
            ui,
            move |_, resp| {
                if resp != "apply" {
                    return;
                }
                do_apply(&engine, &state, &ui, request.clone(), use_symlinks);
            }
        ),
    );
    alert.present(Some(&ui.root));
}

fn do_apply(
    engine: &Rc<RefCell<EngineClient>>,
    state: &Rc<RefCell<State>>,
    ui: &Rc<Ui>,
    request: ApplyRequest,
    use_symlinks: bool,
) {
    {
        let mut s = state.borrow_mut();
        if s.applying {
            return;
        }
        s.applying = true;
        s.last_apply_symlinks = use_symlinks;
    }
    let sent =
        engine
            .borrow_mut()
            .send(CommandPayload::ApplyRestructure(ApplyRestructurePayload {
                library_root: request.root,
                plan_id: request.plan_id,
                moves: request.moves,
                use_symlinks,
            }));
    if sent.is_err() {
        state.borrow_mut().applying = false;
        set_status(ui, "Engine is unavailable — try again in a moment.", true);
    } else {
        set_status(ui, "Applying moves…", false);
    }
    update_apply_controls(state, ui);
}

// ─── Stat hero ───────────────────────────────────────────────────────────────

fn render_stat_hero(state: &Rc<RefCell<State>>, ui: &Rc<Ui>) {
    clear_box(&ui.stat_hero);
    let s = state.borrow();
    let sum = &s.summary;

    if sum.anchor_folders > 0 {
        ui.stat_hero.append(&stat_chip(
            "emblem-ok-symbolic",
            &format!(
                "Keep {} folder{}",
                sum.anchor_folders,
                plural(sum.anchor_folders as usize)
            ),
            "Untouched",
            "gold-accent",
        ));
    }
    if sum.mixed_folders > 0 || sum.moved_out_files > 0 {
        ui.stat_hero.append(&stat_chip(
            "view-sort-ascending-symbolic",
            &format!(
                "Tidy {} folder{}",
                sum.mixed_folders,
                plural(sum.mixed_folders as usize)
            ),
            &format!(
                "Move {} misplaced file{}",
                sum.moved_out_files,
                plural(sum.moved_out_files)
            ),
            "lavender-accent",
        ));
    }
    if sum.junk_folders > 0 || sum.dissolved_files > 0 {
        ui.stat_hero.append(&stat_chip(
            "view-refresh-symbolic",
            &format!(
                "Reorganize {} folder{}",
                sum.junk_folders,
                plural(sum.junk_folders as usize)
            ),
            &format!(
                "Sort {} file{}",
                sum.dissolved_files,
                plural(sum.dissolved_files)
            ),
            "gold-accent",
        ));
    }
}

fn stat_chip(icon: &str, big: &str, small: &str, accent: &str) -> gtk::Box {
    let (card, inner) = padded_card();
    card.set_hexpand(true);
    inner.set_orientation(gtk::Orientation::Horizontal);
    inner.set_spacing(10);
    let img = gtk::Image::from_icon_name(icon);
    img.add_css_class(accent);
    img.set_pixel_size(22);
    let vb = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(0)
        .build();
    vb.append(
        &gtk::Label::builder()
            .label(big)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["heading"])
            .build(),
    );
    vb.append(
        &gtk::Label::builder()
            .label(small)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["dim-label", "caption"])
            .build(),
    );
    inner.append(&img);
    inner.append(&vb);
    card
}

// ─── Recommendation cards (grouped by destination bucket) ────────────────────

fn render_recommendations(state: &Rc<RefCell<State>>, ui: &Rc<Ui>) {
    clear_box(&ui.rec_list);
    let groups = group_by_bucket(&state.borrow().proposals);
    for (bucket, props) in &groups {
        let card = build_bucket_card(state, ui, bucket, props, CARD_ROW_CAP, true);
        ui.rec_list.append(&card);
    }
    update_apply_controls(state, ui);
}

fn build_bucket_card(
    state: &Rc<RefCell<State>>,
    ui: &Rc<Ui>,
    bucket: &str,
    props: &[Proposal],
    cap: usize,
    allow_drill: bool,
) -> gtk::Box {
    let (card, inner) = padded_card();

    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(10)
        .build();
    let icon = gtk::Image::from_icon_name(bucket_icon(bucket));
    icon.add_css_class("gold-accent");
    icon.set_valign(gtk::Align::Center);
    let title_vb = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(1)
        .hexpand(true)
        .build();
    title_vb.append(
        &gtk::Label::builder()
            .label(bucket)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .css_classes(["heading"])
            .build(),
    );
    title_vb.append(
        &gtk::Label::builder()
            .label(format!(
                "Folder · {} file{} moving in",
                props.len(),
                plural(props.len())
            ))
            .xalign(0.0)
            .css_classes(["dim-label", "caption"])
            .build(),
    );

    let all_selected = {
        let s = state.borrow();
        props.iter().all(|p| s.selected.contains(&p.file_id))
    };
    let sel_btn = gtk::Button::builder()
        .label(if all_selected {
            "Deselect all"
        } else {
            "Select all"
        })
        .css_classes(["flat", "caption"])
        .build();
    let ids: Vec<i64> = props.iter().map(|p| p.file_id).collect();
    sel_btn.connect_clicked(clone!(
        #[strong]
        state,
        #[strong]
        ui,
        move |_| {
            {
                let mut s = state.borrow_mut();
                let all = ids.iter().all(|id| s.selected.contains(id));
                for id in &ids {
                    if all {
                        s.selected.remove(id);
                    } else {
                        s.selected.insert(*id);
                    }
                }
            }
            schedule_render(&state, &ui);
        }
    ));

    header.append(&icon);
    header.append(&title_vb);
    header.append(&sel_btn);

    if allow_drill {
        let open_btn = gtk::Button::builder()
            .icon_name("go-next-symbolic")
            .tooltip_text("See every file in this folder")
            .css_classes(["flat"])
            .build();
        let bkt = bucket.to_string();
        open_btn.connect_clicked(clone!(
            #[strong]
            state,
            #[strong]
            ui,
            move |_| {
                open_drilldown(&state, &ui, DrillScope::Bucket(bkt.clone()));
            }
        ));
        header.append(&open_btn);
    }
    inner.append(&header);

    for p in props.iter().take(cap) {
        inner.append(&build_proposal_row(state, ui, p));
    }
    if props.len() > cap {
        let bkt = bucket.to_string();
        let more_btn = gtk::Button::builder()
            .label(format!("See all {} files", props.len()))
            .css_classes(["flat", "caption"])
            .halign(gtk::Align::Start)
            .build();
        more_btn.connect_clicked(clone!(
            #[strong]
            state,
            #[strong]
            ui,
            move |_| {
                open_drilldown(&state, &ui, DrillScope::Bucket(bkt.clone()));
            }
        ));
        inner.append(&more_btn);
    }

    card
}

fn build_proposal_row(state: &Rc<RefCell<State>>, ui: &Rc<Ui>, p: &Proposal) -> gtk::Box {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();

    let check = gtk::CheckButton::new();
    check.set_active(state.borrow().selected.contains(&p.file_id));
    check.set_valign(gtk::Align::Center);
    let fid = p.file_id;
    check.connect_toggled(clone!(
        #[strong]
        state,
        #[strong]
        ui,
        move |c| {
            {
                let mut s = state.borrow_mut();
                if c.is_active() {
                    s.selected.insert(fid);
                } else {
                    s.selected.remove(&fid);
                }
            }
            update_apply_controls(&state, &ui);
        }
    ));
    row.append(&check);

    let icon = gtk::Image::from_icon_name(file_icon(&p.filename));
    icon.add_css_class("dim-label");
    icon.set_valign(gtk::Align::Center);
    row.append(&icon);

    let vb = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(1)
        .hexpand(true)
        .build();
    vb.append(
        &gtk::Label::builder()
            .label(p.filename.as_str())
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .single_line_mode(true)
            .build(),
    );
    vb.append(
        &gtk::Label::builder()
            .label(format!(
                "from {}",
                if p.source_name.is_empty() {
                    "root"
                } else {
                    &p.source_name
                }
            ))
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .css_classes(["dim-label", "caption"])
            .build(),
    );
    if let Some(reason) = p.reason.as_ref().filter(|r| !r.is_empty()) {
        vb.append(
            &gtk::Label::builder()
                .label(reason.as_str())
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["dim-label", "caption"])
                .build(),
        );
    }
    row.append(&vb);

    if let Some((icon_name, tip, css)) = confidence_badge(&p.confidence) {
        let badge = gtk::Image::from_icon_name(icon_name);
        badge.set_tooltip_text(Some(tip));
        badge.set_valign(gtk::Align::Center);
        if let Some(c) = css {
            badge.add_css_class(c);
        }
        row.append(&badge);
    }

    row
}

// ─── Drill-down dialog ───────────────────────────────────────────────────────

fn open_drilldown(state: &Rc<RefCell<State>>, ui: &Rc<Ui>, scope: DrillScope) {
    let (title, groups) = {
        let s = state.borrow();
        let matched: Vec<Proposal> = s
            .proposals
            .iter()
            .filter(|p| matches_scope(&scope, p))
            .cloned()
            .collect();
        (
            drill_title(&scope, matched.len()),
            group_by_bucket(&matched),
        )
    };

    let dialog = adw::Dialog::new();
    dialog.set_title(&title);
    dialog.set_content_width(700);
    dialog.set_content_height(580);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .css_classes(["fileid-tab"])
        .build();
    content.append(
        &gtk::Label::builder()
            .label(title.as_str())
            .xalign(0.0)
            .wrap(true)
            .css_classes(["title-4"])
            .build(),
    );

    let list = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .build();
    if groups.is_empty() {
        list.append(
            &adw::StatusPage::builder()
                .icon_name("object-select-symbolic")
                .title("Nothing to show")
                .build(),
        );
    } else {
        for (bucket, props) in &groups {
            list.append(&build_bucket_card(
                state,
                ui,
                bucket,
                props,
                DRILL_ROW_CAP,
                false,
            ));
        }
    }
    let scroller = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&list)
        .build();
    content.append(&scroller);

    toolbar.set_content(Some(&content));
    dialog.set_child(Some(&toolbar));

    // Toggling a checkbox inside the sheet writes the shared selection; rebuild
    // the main recommendation cards once the sheet closes so they stay in sync.
    dialog.connect_closed(clone!(
        #[strong]
        state,
        #[strong]
        ui,
        move |_| {
            schedule_render(&state, &ui);
        }
    ));
    dialog.present(Some(&ui.root));

    // Springy reveal on the shared brand spring (matches Library's preview).
    let content_weak = content.downgrade();
    let _ = crate::spring::animate(&content, 0.0, 1.0, move |v| {
        if let Some(c) = content_weak.upgrade() {
            c.set_opacity(v);
        }
    });
}

// ─── Shared UI updates ───────────────────────────────────────────────────────

fn update_apply_controls(state: &Rc<RefCell<State>>, ui: &Rc<Ui>) {
    let s = state.borrow();
    if let Some(plan) = s.plan.as_ref().filter(|plan| plan.truncated) {
        let total = plan.total_moves.unwrap_or(plan.moves.len() as u64);
        ui.selected_count_label
            .set_text(&format!("all {total} moves selected"));
        ui.apply_btn
            .set_sensitive(total > 0 && plan.plan_id.is_some() && !s.applying);
        return;
    }
    let total = s.proposals.len();
    let sel = s.selected.len();
    ui.selected_count_label
        .set_text(&format!("{sel} of {total} selected"));
    ui.apply_btn.set_sensitive(sel > 0 && !s.applying);
}

fn update_apply_hint(ui: &Rc<Ui>, use_symlinks: bool) {
    ui.apply_hint.set_text(if use_symlinks {
        "Originals stay put — a browsable shortcut tree mirrors the new layout."
    } else {
        "Files move on disk into the new structure. Permanent, but reversible right after with Undo last run."
    });
}

fn update_bars(state: &Rc<RefCell<State>>, ui: &Rc<Ui>) {
    let s = state.borrow();
    let show_apply = !s.proposals.is_empty() && !s.loading;
    ui.apply_bar.set_visible(show_apply);
    ui.undo_bar.set_visible(s.can_undo);
    ui.bottom_spacer.set_visible(show_apply || s.can_undo);
}

fn set_status(ui: &Rc<Ui>, msg: &str, is_error: bool) {
    if msg.is_empty() {
        ui.status_row.set_visible(false);
        return;
    }
    ui.status_label.set_text(msg);
    ui.status_icon.set_icon_name(Some(if is_error {
        "dialog-warning-symbolic"
    } else {
        "emblem-ok-symbolic"
    }));
    ui.status_icon.remove_css_class("gold-accent");
    if !is_error {
        ui.status_icon.add_css_class("gold-accent");
    }
    ui.status_row.set_visible(true);
}

fn show_empty(ui: &Rc<Ui>, title: &str, desc: &str) {
    ui.empty_page.set_title(title);
    ui.empty_page.set_description(Some(desc));
    ui.stack.set_visible_child_name("empty");
}

fn schedule_render(state: &Rc<RefCell<State>>, ui: &Rc<Ui>) {
    glib::idle_add_local_once(clone!(
        #[strong]
        state,
        #[strong]
        ui,
        move || {
            render_recommendations(&state, &ui);
        }
    ));
}

// ─── Sankey: pixel layout, drawing, hit-testing ──────────────────────────────

struct SankeyPixels {
    ribbon_x0: f64,
    ribbon_x1: f64,
    src: Vec<(f64, f64, f64, f64)>, // (x, y, w, h) aligned to model.sources
    dst: Vec<(f64, f64, f64, f64)>,
    src_mid: HashMap<String, f64>,
    dst_mid: HashMap<String, f64>,
}

fn layout_sankey(model: &SankeyModel, width: f64, height: f64) -> SankeyPixels {
    let node_w = (width * 0.32).clamp(96.0, 184.0);
    let margin = 6.0;
    let src_x = margin;
    let dst_x = (width - node_w - margin).max(src_x + node_w + 20.0);
    let ribbon_x0 = src_x + node_w;
    let ribbon_x1 = dst_x;

    let src_slots = compute_slots(&model.sources, height);
    let dst_slots = compute_slots(&model.destinations, height);

    let mut src = Vec::with_capacity(model.sources.len());
    let mut src_mid = HashMap::new();
    for (i, n) in model.sources.iter().enumerate() {
        let (y, h) = src_slots.get(i).copied().unwrap_or((0.0, 0.0));
        src.push((src_x, y, node_w, h));
        src_mid.insert(n.id.clone(), y + h / 2.0);
    }
    let mut dst = Vec::with_capacity(model.destinations.len());
    let mut dst_mid = HashMap::new();
    for (i, n) in model.destinations.iter().enumerate() {
        let (y, h) = dst_slots.get(i).copied().unwrap_or((0.0, 0.0));
        dst.push((dst_x, y, node_w, h));
        dst_mid.insert(n.id.clone(), y + h / 2.0);
    }
    SankeyPixels {
        ribbon_x0,
        ribbon_x1,
        src,
        dst,
        src_mid,
        dst_mid,
    }
}

/// Proportional slot heights for a column: each node's height tracks its file
/// count (with a floor) and the column scales to fit `total_height`.
fn compute_slots(nodes: &[SankeyNode], total_height: f64) -> Vec<(f64, f64)> {
    let n = nodes.len();
    if n == 0 || total_height <= 0.0 {
        return vec![];
    }
    let gap = 12.0;
    let buffer = 10.0;
    let layout_h = (total_height - buffer * 2.0).max(0.0);
    let avail = (layout_h - gap * (n as f64 - 1.0)).max(0.0);
    let min_h = 22.0;
    let total_count = nodes.iter().map(|x| x.count).sum::<usize>().max(1) as f64;

    let mut heights: Vec<f64> = nodes
        .iter()
        .map(|x| (avail * x.count as f64 / total_count).max(min_h))
        .collect();
    let sum: f64 = heights.iter().sum();
    if sum > avail && sum > 0.0 {
        let scale = avail / sum;
        for h in heights.iter_mut() {
            *h = (*h * scale).max(8.0);
        }
    }
    let mut out = Vec::with_capacity(n);
    let mut y = buffer;
    for h in &heights {
        out.push((y, *h));
        y += *h + gap;
    }
    out
}

fn ribbon_thickness(count: usize, total: usize, height: f64) -> f64 {
    let ratio = count as f64 / total.max(1) as f64;
    (ratio * height * 0.5).clamp(1.5, 9.0)
}

fn draw_sankey(state: &Rc<RefCell<State>>, cr: &gtk::cairo::Context, w: i32, h: i32) {
    if w <= 0 || h <= 0 {
        return;
    }
    let st = state.borrow();
    let model = &st.sankey;
    if model.sources.is_empty() || model.destinations.is_empty() {
        return;
    }
    let (wf, hf) = (w as f64, h as f64);
    let px = layout_sankey(model, wf, hf);
    let total = model.total_flow.max(1);
    let hovered = st.hovered_flow;

    // Ribbons under the nodes; hovered one drawn last so it sits on top.
    for (i, f) in model.flows.iter().enumerate() {
        if Some(i) == hovered {
            continue;
        }
        let opacity = if hovered.is_some() { 0.05 } else { 0.20 };
        draw_ribbon(cr, f, &px, total, hf, opacity);
    }
    if let Some(hi) = hovered {
        if let Some(f) = model.flows.get(hi) {
            draw_ribbon(cr, f, &px, total, hf, 0.95);
        }
    }

    for (i, n) in model.sources.iter().enumerate() {
        draw_node(cr, n, px.src[i]);
    }
    for (i, n) in model.destinations.iter().enumerate() {
        draw_node(cr, n, px.dst[i]);
    }
}

fn draw_ribbon(
    cr: &gtk::cairo::Context,
    flow: &SankeyFlow,
    px: &SankeyPixels,
    total: usize,
    height: f64,
    opacity: f64,
) {
    let (Some(&y0), Some(&y1)) = (px.src_mid.get(&flow.src_id), px.dst_mid.get(&flow.dst_id))
    else {
        return;
    };
    let (x0, x1) = (px.ribbon_x0, px.ribbon_x1);
    let dx = x1 - x0;
    cr.move_to(x0, y0);
    cr.curve_to(x0 + dx * 0.5, y0, x1 - dx * 0.5, y1, x1, y1);
    let (r, g, b) = flow.tint;
    cr.set_source_rgba(r, g, b, opacity);
    cr.set_line_width(ribbon_thickness(flow.count, total, height));
    cr.set_line_cap(gtk::cairo::LineCap::Round);
    let _ = cr.stroke();
}

fn draw_node(cr: &gtk::cairo::Context, n: &SankeyNode, rect: (f64, f64, f64, f64)) {
    let (x, y, w, h) = rect;
    if h <= 0.0 || w <= 0.0 {
        return;
    }
    let (tr, tg, tb) = n.tint;

    rounded_rect(cr, x, y, w, h, 8.0);
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.06);
    let _ = cr.fill_preserve();
    cr.set_source_rgba(tr, tg, tb, if n.is_rollup { 0.25 } else { 0.55 });
    cr.set_line_width(1.0);
    let _ = cr.stroke();

    // Colour swatch.
    let sw = 14.0_f64.min(h - 6.0).max(6.0);
    let sx = x + 9.0;
    let sy = y + (h - sw) / 2.0;
    rounded_rect(cr, sx, sy, sw, sw, 4.0);
    cr.set_source_rgba(tr, tg, tb, 0.9);
    let _ = cr.fill();

    let text_x = sx + sw + 8.0;
    let avail_chars = (((x + w) - text_x - 8.0) / 7.0).max(4.0) as usize;

    cr.select_font_face(
        "Sans",
        gtk::cairo::FontSlant::Normal,
        gtk::cairo::FontWeight::Bold,
    );
    cr.set_font_size(12.0);
    cr.set_source_rgba(1.0, 1.0, 1.0, if n.is_rollup { 0.70 } else { 0.92 });
    cr.move_to(text_x, y + h / 2.0 - 1.0);
    let _ = cr.show_text(&truncate(&n.label, avail_chars));

    cr.select_font_face(
        "Sans",
        gtk::cairo::FontSlant::Normal,
        gtk::cairo::FontWeight::Normal,
    );
    cr.set_font_size(9.5);
    cr.set_source_rgba(1.0, 1.0, 1.0, 0.5);
    cr.move_to(text_x, y + h / 2.0 + 11.0);
    let _ = cr.show_text(&format!("{} file{}", n.count, plural(n.count)));

    cr.new_path();
}

fn rounded_rect(cr: &gtk::cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -0.5 * PI, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, 0.5 * PI);
    cr.arc(x + r, y + h - r, r, 0.5 * PI, PI);
    cr.arc(x + r, y + r, r, PI, 1.5 * PI);
    cr.close_path();
}

/// Y of the horizontal cubic Bézier at a given cursor X (flat tangents at the
/// endpoints — matches the rendered ribbon).
fn bezier_y(cursor_x: f64, start_x: f64, end_x: f64, start_y: f64, end_y: f64) -> f64 {
    if (end_x - start_x).abs() < f64::EPSILON {
        return (start_y + end_y) / 2.0;
    }
    let t = ((cursor_x - start_x) / (end_x - start_x)).clamp(0.0, 1.0);
    let om = 1.0 - t;
    om * om * om * start_y
        + 3.0 * om * om * t * start_y
        + 3.0 * om * t * t * end_y
        + t * t * t * end_y
}

fn hit_test_ribbon(
    state: &Rc<RefCell<State>>,
    sankey: &gtk::DrawingArea,
    x: f64,
    y: f64,
) -> Option<usize> {
    let st = state.borrow();
    let model = &st.sankey;
    if model.flows.is_empty() {
        return None;
    }
    let (wf, hf) = (sankey.width() as f64, sankey.height() as f64);
    if wf <= 0.0 || hf <= 0.0 {
        return None;
    }
    let px = layout_sankey(model, wf, hf);
    if x < px.ribbon_x0 || x > px.ribbon_x1 {
        return None;
    }
    let total = model.total_flow.max(1);
    let mut best: Option<usize> = None;
    let mut best_d = f64::INFINITY;
    for (i, f) in model.flows.iter().enumerate() {
        let (Some(&y0), Some(&y1)) = (px.src_mid.get(&f.src_id), px.dst_mid.get(&f.dst_id)) else {
            continue;
        };
        let cy = bezier_y(x, px.ribbon_x0, px.ribbon_x1, y0, y1);
        let dy = (cy - y).abs();
        let prox = ribbon_thickness(f.count, total, hf) * 0.5 + 6.0;
        if dy < prox && dy < best_d {
            best_d = dy;
            best = Some(i);
        }
    }
    best
}

fn hit_test_node(
    state: &Rc<RefCell<State>>,
    sankey: &gtk::DrawingArea,
    x: f64,
    y: f64,
) -> Option<DrillScope> {
    let st = state.borrow();
    let model = &st.sankey;
    let (wf, hf) = (sankey.width() as f64, sankey.height() as f64);
    if wf <= 0.0 || hf <= 0.0 {
        return None;
    }
    let px = layout_sankey(model, wf, hf);
    for (i, n) in model.sources.iter().enumerate() {
        let (nx, ny, nw, nh) = px.src[i];
        if x >= nx && x <= nx + nw && y >= ny && y <= ny + nh {
            return Some(if n.is_rollup {
                DrillScope::SourceFolders(n.rollup_members.clone())
            } else {
                DrillScope::Source(n.identity.clone())
            });
        }
    }
    for (i, n) in model.destinations.iter().enumerate() {
        let (nx, ny, nw, nh) = px.dst[i];
        if x >= nx && x <= nx + nw && y >= ny && y <= ny + nh {
            return Some(if n.is_rollup {
                DrillScope::DestBuckets(n.rollup_members.clone())
            } else {
                DrillScope::Bucket(n.identity.clone())
            });
        }
    }
    None
}

fn keyboard_node_scope(state: &Rc<RefCell<State>>, index: usize) -> Option<(DrillScope, String)> {
    let state = state.borrow();
    let source_count = state.sankey.sources.len();
    let node = if index < source_count {
        state.sankey.sources.get(index)?
    } else {
        state.sankey.destinations.get(index - source_count)?
    };
    let scope = if index < source_count {
        if node.is_rollup {
            DrillScope::SourceFolders(node.rollup_members.clone())
        } else {
            DrillScope::Source(node.identity.clone())
        }
    } else if node.is_rollup {
        DrillScope::DestBuckets(node.rollup_members.clone())
    } else {
        DrillScope::Bucket(node.identity.clone())
    };
    Some((scope, format!("{} — {} files", node.label, node.count)))
}

// ─── Plan → view-model mapping ───────────────────────────────────────────────

fn map_proposals(plan: &RestructurePlan) -> Vec<Proposal> {
    plan.moves
        .iter()
        .map(|m| {
            let source_folder = parent_dir(&m.source);
            Proposal {
                file_id: m.file_id,
                bucket: bucket_label(&m.destination, &plan.library_root, &m.category),
                source_name: basename(&source_folder),
                source_folder,
                filename: basename(&m.source),
                kind: match m.tier.as_deref() {
                    Some(t) if t.eq_ignore_ascii_case("mixed") => ProposalKind::MovedOutAsOutlier,
                    _ => ProposalKind::Dissolved,
                },
                confidence: m.confidence.clone(),
                reason: m.reason.clone(),
            }
        })
        .collect()
}

fn make_summary(plan: &RestructurePlan, proposals: &[Proposal]) -> Summary {
    let moved_out = proposals
        .iter()
        .filter(|p| p.kind == ProposalKind::MovedOutAsOutlier)
        .count();
    let dissolved = proposals
        .iter()
        .filter(|p| p.kind == ProposalKind::Dissolved)
        .count();
    let (anchor_folders, mixed_folders, junk_folders) = match &plan.folder_classifications {
        Some(fc) => (fc.anchor_folders, fc.mixed_folders, fc.junk_folders),
        None => {
            let mixed = proposals
                .iter()
                .filter(|p| p.kind == ProposalKind::MovedOutAsOutlier)
                .map(|p| p.source_folder.as_str())
                .collect::<HashSet<_>>()
                .len() as u32;
            let junk = proposals
                .iter()
                .filter(|p| p.kind == ProposalKind::Dissolved)
                .map(|p| p.source_folder.as_str())
                .collect::<HashSet<_>>()
                .len() as u32;
            (0, mixed, junk)
        }
    };
    Summary {
        anchor_folders,
        mixed_folders,
        junk_folders,
        moved_out_files: moved_out,
        dissolved_files: dissolved,
    }
}

fn group_by_bucket(props: &[Proposal]) -> Vec<(String, Vec<Proposal>)> {
    let mut map: HashMap<String, Vec<Proposal>> = HashMap::new();
    for p in props {
        map.entry(p.bucket.clone()).or_default().push(p.clone());
    }
    let mut v: Vec<(String, Vec<Proposal>)> = map.into_iter().collect();
    v.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
    v
}

fn build_sankey(props: &[Proposal]) -> SankeyModel {
    if props.is_empty() {
        return SankeyModel::default();
    }

    let mut src_count: HashMap<String, usize> = HashMap::new();
    let mut src_junk: HashMap<String, bool> = HashMap::new();
    let mut dst_count: HashMap<String, usize> = HashMap::new();
    for p in props {
        *src_count.entry(p.source_folder.clone()).or_default() += 1;
        let junk = src_junk.entry(p.source_folder.clone()).or_insert(true);
        if p.kind != ProposalKind::Dissolved {
            *junk = false;
        }
        *dst_count.entry(p.bucket.clone()).or_default() += 1;
    }

    let mut all_srcs: Vec<(String, usize)> = src_count.into_iter().collect();
    all_srcs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let visible_srcs: Vec<(String, usize)> = all_srcs.iter().take(TOP_N).cloned().collect();
    let tail_srcs: Vec<(String, usize)> = all_srcs.iter().skip(TOP_N).cloned().collect();

    let mut all_dsts: Vec<(String, usize)> = dst_count.into_iter().collect();
    all_dsts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let visible_dsts: Vec<(String, usize)> = all_dsts.iter().take(TOP_N).cloned().collect();
    let tail_dsts: Vec<(String, usize)> = all_dsts.iter().skip(TOP_N).cloned().collect();

    let mut sources: Vec<SankeyNode> = visible_srcs
        .iter()
        .map(|(folder, count)| SankeyNode {
            id: format!("src:{folder}"),
            label: basename_or(folder),
            identity: folder.clone(),
            count: *count,
            is_rollup: false,
            tint: NEUTRAL,
            rollup_members: vec![],
        })
        .collect();
    if !tail_srcs.is_empty() {
        let count: usize = tail_srcs.iter().map(|(_, c)| *c).sum();
        sources.push(SankeyNode {
            id: SRC_OTHER.to_string(),
            label: format!(
                "+ {} more folder{}",
                tail_srcs.len(),
                plural(tail_srcs.len())
            ),
            identity: SRC_OTHER.to_string(),
            count,
            is_rollup: true,
            tint: NEUTRAL,
            rollup_members: tail_srcs.iter().map(|(f, _)| f.clone()).collect(),
        });
    }

    let mut destinations: Vec<SankeyNode> = visible_dsts
        .iter()
        .enumerate()
        .map(|(i, (bucket, count))| SankeyNode {
            id: format!("dst:{bucket}"),
            label: bucket.clone(),
            identity: bucket.clone(),
            count: *count,
            is_rollup: false,
            tint: OKABE_ITO[i % OKABE_ITO.len()],
            rollup_members: vec![],
        })
        .collect();
    if !tail_dsts.is_empty() {
        let count: usize = tail_dsts.iter().map(|(_, c)| *c).sum();
        destinations.push(SankeyNode {
            id: DST_OTHER.to_string(),
            label: format!(
                "+ {} more bucket{}",
                tail_dsts.len(),
                plural(tail_dsts.len())
            ),
            identity: DST_OTHER.to_string(),
            count,
            is_rollup: true,
            tint: NEUTRAL,
            rollup_members: tail_dsts.iter().map(|(b, _)| b.clone()).collect(),
        });
    }

    let visible_src_ids: HashSet<String> = sources.iter().map(|n| n.id.clone()).collect();
    let visible_dst_ids: HashSet<String> = destinations.iter().map(|n| n.id.clone()).collect();
    let tint_by_dst: HashMap<String, (f64, f64, f64)> = destinations
        .iter()
        .map(|n| (n.id.clone(), n.tint))
        .collect();

    let mut flow_map: HashMap<(String, String), SankeyFlow> = HashMap::new();
    for p in props {
        let raw_src = format!("src:{}", p.source_folder);
        let src_id = if visible_src_ids.contains(&raw_src) {
            raw_src
        } else {
            SRC_OTHER.to_string()
        };
        let raw_dst = format!("dst:{}", p.bucket);
        let dst_id = if visible_dst_ids.contains(&raw_dst) {
            raw_dst
        } else {
            DST_OTHER.to_string()
        };
        let tint = *tint_by_dst.get(&dst_id).unwrap_or(&NEUTRAL);
        let entry = flow_map
            .entry((src_id.clone(), dst_id.clone()))
            .or_insert(SankeyFlow {
                src_id,
                dst_id,
                tint,
                count: 0,
            });
        entry.count += 1;
    }
    let flows: Vec<SankeyFlow> = flow_map.into_values().collect();
    let total_flow: usize = flows.iter().map(|f| f.count).sum();

    // Barycentric ordering (2 passes) to cut ribbon crossings.
    let mut src_order: Vec<String> = sources.iter().map(|n| n.id.clone()).collect();
    let mut dst_order: Vec<String> = destinations.iter().map(|n| n.id.clone()).collect();
    for _ in 0..2 {
        let dst_index: HashMap<String, usize> = dst_order
            .iter()
            .enumerate()
            .map(|(i, id)| (id.clone(), i))
            .collect();
        let src_w = weighted_means(&flows, |f| &f.src_id, |f| &f.dst_id, &dst_index);
        src_order.sort_by(|a, b| {
            src_w
                .get(a)
                .unwrap_or(&0.0)
                .partial_cmp(src_w.get(b).unwrap_or(&0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let src_index: HashMap<String, usize> = src_order
            .iter()
            .enumerate()
            .map(|(i, id)| (id.clone(), i))
            .collect();
        let dst_w = weighted_means(&flows, |f| &f.dst_id, |f| &f.src_id, &src_index);
        dst_order.sort_by(|a, b| {
            dst_w
                .get(a)
                .unwrap_or(&0.0)
                .partial_cmp(dst_w.get(b).unwrap_or(&0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    let src_map: HashMap<String, SankeyNode> =
        sources.into_iter().map(|n| (n.id.clone(), n)).collect();
    let dst_map: HashMap<String, SankeyNode> = destinations
        .into_iter()
        .map(|n| (n.id.clone(), n))
        .collect();
    let mut sources: Vec<SankeyNode> = src_order
        .iter()
        .filter_map(|id| src_map.get(id).cloned())
        .collect();
    let mut destinations: Vec<SankeyNode> = dst_order
        .iter()
        .filter_map(|id| dst_map.get(id).cloned())
        .collect();

    // Pin the "+ N more" rollups to the bottom of their column.
    if let Some(i) = sources.iter().position(|n| n.id == SRC_OTHER) {
        let r = sources.remove(i);
        sources.push(r);
    }
    if let Some(i) = destinations.iter().position(|n| n.id == DST_OTHER) {
        let r = destinations.remove(i);
        destinations.push(r);
    }

    SankeyModel {
        sources,
        destinations,
        flows,
        total_flow,
    }
}

/// Per-key weighted mean of `index(other_endpoint)`, weighted by flow count.
fn weighted_means(
    flows: &[SankeyFlow],
    key: impl Fn(&SankeyFlow) -> &String,
    other: impl Fn(&SankeyFlow) -> &String,
    index: &HashMap<String, usize>,
) -> HashMap<String, f64> {
    let mut acc: HashMap<String, (f64, f64)> = HashMap::new();
    for f in flows {
        let idx = *index.get(other(f)).unwrap_or(&0) as f64;
        let e = acc.entry(key(f).clone()).or_insert((0.0, 0.0));
        e.0 += f.count as f64 * idx;
        e.1 += f.count as f64;
    }
    acc.into_iter()
        .map(|(k, (w, t))| (k, if t > 0.0 { w / t } else { 0.0 }))
        .collect()
}

// ─── Scope helpers ───────────────────────────────────────────────────────────

fn matches_scope(scope: &DrillScope, p: &Proposal) -> bool {
    match scope {
        DrillScope::Bucket(b) => &p.bucket == b,
        DrillScope::Source(f) => &p.source_folder == f,
        DrillScope::SourceFolders(fs) => fs.contains(&p.source_folder),
        DrillScope::DestBuckets(bs) => bs.contains(&p.bucket),
    }
}

fn drill_title(scope: &DrillScope, count: usize) -> String {
    let base = match scope {
        DrillScope::Bucket(b) => format!("Going to {b}"),
        DrillScope::Source(f) => format!("From {}", basename_or(f)),
        DrillScope::SourceFolders(fs) => {
            format!("From {} smaller folder{}", fs.len(), plural(fs.len()))
        }
        DrillScope::DestBuckets(bs) => {
            format!("Going to {} smaller bucket{}", bs.len(), plural(bs.len()))
        }
    };
    format!("{base} · {count} file{}", plural(count))
}

fn sankey_header_stat(props: &[Proposal]) -> String {
    let srcs: HashSet<&str> = props.iter().map(|p| p.source_folder.as_str()).collect();
    let dsts: HashSet<&str> = props.iter().map(|p| p.bucket.as_str()).collect();
    format!(
        "{} source{} → {} destination{}",
        srcs.len(),
        plural(srcs.len()),
        dsts.len(),
        plural(dsts.len())
    )
}

// ─── Small pure helpers ──────────────────────────────────────────────────────

fn parent_dir(p: &str) -> String {
    Path::new(p)
        .parent()
        .map(|x| x.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn basename(p: &str) -> String {
    Path::new(p)
        .file_name()
        .map(|x| x.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string())
}

fn basename_or(folder: &str) -> String {
    let b = basename(folder);
    if b.is_empty() {
        folder.to_string()
    } else {
        b
    }
}

/// Destination's parent directory relative to the library root, or the engine's
/// category label when the destination isn't under the root (mirror of macOS
/// `bucketLabel`).
fn bucket_label(destination: &str, root: &str, fallback: &str) -> String {
    let parent = parent_dir(destination);
    let root_norm = root.strip_suffix('/').unwrap_or(root);
    if parent == root_norm {
        return fallback.to_string();
    }
    let prefix = format!("{root_norm}/");
    if let Some(rel) = parent.strip_prefix(&prefix) {
        if !rel.is_empty() {
            return rel.to_string();
        }
    }
    fallback.to_string()
}

fn normalize(p: &str) -> &str {
    p.strip_suffix('/').unwrap_or(p)
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

fn truncate(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    let take = max.saturating_sub(1).max(1);
    let mut out: String = s.chars().take(take).collect();
    out.push('…');
    out
}

fn confidence_badge(conf: &str) -> Option<(&'static str, &'static str, Option<&'static str>)> {
    match conf.to_ascii_lowercase().as_str() {
        "auto" => Some((
            "emblem-ok-symbolic",
            "High confidence — safe to apply.",
            Some("gold-accent"),
        )),
        "review" => Some((
            "dialog-question-symbolic",
            "Medium confidence — worth a look.",
            None,
        )),
        "ask" => Some((
            "dialog-information-symbolic",
            "Low confidence — review before applying.",
            None,
        )),
        _ => None,
    }
}

fn bucket_icon(bucket: &str) -> &'static str {
    match bucket.split('/').next().unwrap_or("") {
        "People" => "system-users-symbolic",
        "Places" | "Travel" => "mark-location-symbolic",
        "Documents" | "Receipts" | "Forms" | "ID" => "x-office-document-symbolic",
        "Photos" | "Screenshots" | "Diagrams" => "image-x-generic-symbolic",
        _ => "folder-symbolic",
    }
}

fn file_icon(filename: &str) -> &'static str {
    let ext = Path::new(filename)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "jpg" | "jpeg" | "png" | "heic" | "heif" | "gif" | "tiff" | "tif" | "bmp" | "webp"
        | "raw" | "dng" => "image-x-generic-symbolic",
        "mp4" | "mov" | "m4v" | "avi" | "mkv" | "webm" => "video-x-generic-symbolic",
        "pdf" | "doc" | "docx" | "rtf" | "txt" | "md" | "pages" => "x-office-document-symbolic",
        "mp3" | "m4a" | "wav" | "flac" | "aac" | "ogg" => "audio-x-generic-symbolic",
        "zip" | "tar" | "gz" | "7z" => "package-x-generic-symbolic",
        _ => "text-x-generic-symbolic",
    }
}

fn padded_card() -> (gtk::Box, gtk::Box) {
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["glass-card"])
        .build();
    let inner = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(14)
        .margin_end(14)
        .build();
    card.append(&inner);
    (card, inner)
}

fn clear_box(b: &gtk::Box) {
    while let Some(child) = b.first_child() {
        b.remove(&child);
    }
}

// ─── Most-recent-scan root (auto-default the destination) ─────────────────────

/// Read the most recently started scan's root path directly from the engine's
/// SQLite DB (the same single-writer WAL DB the Library reads), so the tab loads
/// a plan immediately on open — mirror of macOS `store.recentSessions(limit:1)`.
fn recent_root() -> Option<String> {
    let db = fileid_engine::paths::db_path().ok()?;
    if !db.exists() {
        return None;
    }
    let conn = fileid_engine::db::open_read(&db).ok()?;
    conn.query_row(
        "SELECT root_path FROM scan_sessions ORDER BY started_at DESC LIMIT 1",
        rusqlite::params![],
        |r| r.get::<_, String>(0),
    )
    .ok()
    .filter(|p| Path::new(p).exists())
}

fn recent_root_async() -> async_channel::Receiver<Option<String>> {
    let (tx, rx) = async_channel::bounded::<Option<String>>(1);
    std::thread::spawn(move || {
        let _ = tx.send_blocking(recent_root());
    });
    rx
}
