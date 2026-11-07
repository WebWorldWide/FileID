// Best Takes (DESIGN_V3_PORT "Best Takes"): the engine groups bursts and repeated
// clips by itself, scores each try without a model, and this page lets the
// user check the starred best try, keep or skip, or move the rest to Trash
// with Undo. Manual groups with a goal (YOUR GROUPS / MAKE A GROUP BY HAND)
// live in takes_groups.rs. The goal ranking panel is not offered: the shared
// engine reports it as unavailable.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use adw::prelude::*;
use fileid_engine::ipc::*;
use gtk::glib;

use super::takes_groups::Manual;
use super::takes_model as model;
use super::takes_widgets::{ComparePane, Shot, ShotView, Tile};
use super::workbench_state::{Requests, catalog_request};
use super::workbenches::Engine;
use crate::engine_client::{CancelToken, EngineState, ThumbReply};
use crate::v3;

#[path = "takes_proposals.rs"]
mod proposals;

const COMPARE_PX: i32 = 1200;
const TILE_PX: i32 = 192;
const FAN_PX: i32 = 96;
const SHOT_CACHE: usize = 16;

struct PendingTrash {
    group: CatalogTakeAutoGroup,
    doomed: Vec<CatalogTakeQuality>,
    kept_files: Vec<i64>,
    kept_tries: usize,
}

pub(super) struct Ui {
    pub(super) engine: Engine,
    pub(super) manual: Manual,
    pub(super) proposals: proposals::Proposals,
    requests: RefCell<Requests>,
    groups: RefCell<Vec<CatalogTakeAutoGroup>>,
    loaded: Cell<bool>,
    regroup: Cell<bool>,
    pub(super) current: RefCell<Option<CatalogTakeAutoGroup>>,
    resolved: RefCell<Option<(String, usize)>>,
    qualities: RefCell<Vec<CatalogTakeQuality>>,
    keep: RefCell<BTreeSet<i64>>,
    focused: Cell<usize>,
    zoomed: Cell<bool>,
    pending_decision: RefCell<Option<(CatalogTakeAutoGroup, String)>>,
    pending_trash: RefCell<Option<PendingTrash>>,
    restoring: RefCell<Option<Option<(CatalogTakeAutoGroup, Vec<i64>)>>>,
    tile_token: RefCell<CancelToken>,
    fan_token: RefCell<CancelToken>,
    shots: RefCell<HashMap<i64, Shot>>,
    tiles: RefCell<Vec<Tile>>,
    scroll_animation: RefCell<Option<adw::SpringAnimation>>,
    pub(super) root: adw::ToastOverlay,
    groups_header: gtk::Label,
    groups_bar: gtk::ProgressBar,
    look_again: gtk::Button,
    groups_list: gtk::Box,
    pub(super) stack: gtk::Stack,
    loading_label: gtk::Label,
    failed_title: gtk::Label,
    failed_text: gtk::Label,
    title: gtk::Label,
    date: gtk::Label,
    skipped: gtk::Label,
    trash_button: gtk::Button,
    keep_all: gtk::Button,
    skip: gtk::Button,
    action_bar: gtk::ProgressBar,
    best_pane: ComparePane,
    other_pane: ComparePane,
    strip_header: gtk::Label,
    zoom_button: gtk::Button,
    strip: gtk::Box,
    strip_scroll: gtk::ScrolledWindow,
    hint: gtk::Label,
}

pub fn takes(engine: Engine) -> gtk::Widget {
    build(engine, vec![], true).0
}

/// Best Takes over the clips a search found (the macOS sheet from Search &
/// Moments): the same page in a dialog, its clip pool seeded with `candidates`.
pub(super) fn open_dialog(
    engine: Engine,
    candidates: Vec<CatalogHit>,
    parent: &impl IsA<gtk::Widget>,
) {
    let (content, ui) = build(engine, candidates, false);
    let dialog = adw::Dialog::builder()
        .title("Best Takes")
        .content_width(1100)
        .content_height(760)
        .build();
    let header = adw::HeaderBar::new();
    header.set_show_end_title_buttons(false);
    let done = v3::secondary("Done", false);
    done.set_tooltip_text(Some("Done (Esc)"));
    let weak = dialog.downgrade();
    done.connect_clicked(move |_| {
        if let Some(dialog) = weak.upgrade() {
            dialog.close();
        }
    });
    header.pack_end(&done);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&content));
    dialog.set_child(Some(&toolbar));
    let owner = RefCell::new(Some(ui));
    dialog.connect_closed(move |_| {
        if let Some(ui) = owner.borrow_mut().take() {
            ui.best_pane.stop();
            ui.other_pane.stop();
            ui.manual.stop_cards();
        }
    });
    dialog.present(Some(parent));
}

fn build(engine: Engine, candidates: Vec<CatalogHit>, keep_alive: bool) -> (gtk::Widget, Rc<Ui>) {
    let manual = Manual::new(candidates);
    let related = proposals::Proposals::new();
    let page = v3::hbox(0);

    let pane = v3::vbox(0);
    pane.add_css_class("v3-pane");
    pane.set_size_request(280, -1);
    let top = v3::vbox(6);
    top.add_css_class("v3-pane-top");
    let header_row = v3::hbox(8);
    let groups_header = v3::section("POSSIBLE GROUPS");
    groups_header.add_css_class("tone-ai");
    groups_header.set_hexpand(true);
    header_row.append(&groups_header);
    let groups_bar = v3::busy_bar(40);
    header_row.append(&groups_bar);
    let look_again = v3::icon_button("view-refresh-symbolic", "Look again");
    header_row.append(&look_again);
    top.append(&header_row);
    top.append(&v3::wrapped(
        "Bursts and repeated clips from your scans. Biggest and newest first.",
        &["v3-meta-115"],
    ));
    let groups_list = v3::vbox(2);
    groups_list.set_margin_top(8);
    let pane_body = v3::vbox(0);
    pane_body.append(&related.section);
    pane_body.append(&top);
    pane_body.append(&groups_list);
    pane_body.append(&manual.pane);
    let list_scroll = v3::scroller(&pane_body);
    list_scroll.set_vexpand(true);
    pane.append(&list_scroll);
    pane.append(&manual.footer);
    page.append(&pane);
    page.append(&gtk::Separator::new(gtk::Orientation::Vertical));

    let stack = gtk::Stack::new();
    stack.set_hexpand(true);
    stack.set_vexpand(true);

    let empty = v3::dashed_notice(
        "No tries to compare yet",
        "After a scan, Best Takes groups bursts and repeated clips here by itself. Scan a folder with photo bursts, or make a group by hand on the left.",
    );
    empty.set_valign(gtk::Align::Start);
    empty.add_css_class("v3-page-pad-inner");
    stack.add_named(&empty, Some("empty"));
    let pick = v3::dashed_notice(
        "Pick a group",
        "Choose a possible group on the left to compare its tries.",
    );
    pick.set_valign(gtk::Align::Start);
    pick.add_css_class("v3-page-pad-inner");
    stack.add_named(&pick, Some("pick"));
    stack.add_named(&manual.page, Some("manual"));

    let loading = v3::vbox(10);
    loading.add_css_class("v3-page-pad");
    loading.set_valign(gtk::Align::Start);
    let loading_bar = v3::busy_bar(160);
    loading_bar.set_visible(true);
    loading_bar.set_halign(gtk::Align::Start);
    loading.append(&loading_bar);
    let loading_label = v3::label(
        "Looking for bursts and repeated clips…",
        &["v3-meta", "v3-meta-13"],
    );
    loading.append(&loading_label);
    stack.add_named(&loading, Some("loading"));

    let failed = v3::vbox(6);
    failed.add_css_class("v3-dashed");
    failed.add_css_class("v3-page-pad-inner");
    failed.set_valign(gtk::Align::Start);
    let failed_title = v3::label("Couldn't score these photos", &["v3-serif-28"]);
    failed.append(&failed_title);
    let failed_text = v3::wrapped("", &["v3-body", "tone-text2"]);
    failed.append(&failed_text);
    stack.add_named(&failed, Some("failed"));

    let review = v3::vbox(16);
    review.add_css_class("v3-page-pad");
    let header = v3::vbox(12);
    let heading = v3::hbox(10);
    let title = v3::label("", &["v3-serif-28"]);
    heading.append(&title);
    let date = v3::label("", &["v3-meta", "v3-meta-13", "tone-text2"]);
    date.set_valign(gtk::Align::Center);
    heading.append(&date);
    let skipped = v3::pill("SKIPPED", crate::shell::status::Tone::Muted);
    skipped.set_visible(false);
    heading.append(&related.relation);
    heading.append(&skipped);
    header.append(&heading);
    header.append(&related.intro);
    let actions = v3::hbox(8);
    let trash_button = v3::primary("Keep the best, move the other 0 to Trash", false);
    actions.append(&trash_button);
    let keep_all = v3::secondary("Keep all", false);
    actions.append(&keep_all);
    let skip = v3::ghost("Skip group");
    skip.set_tooltip_text(Some(
        "Decide later. Skipped groups move to the bottom of the list.",
    ));
    actions.append(&skip);
    actions.append(&related.save);
    let action_bar = v3::busy_bar(60);
    actions.append(&action_bar);
    header.append(&actions);
    review.append(&header);

    let compare = v3::hbox(12);
    compare.set_homogeneous(true);
    let best_pane = ComparePane::new();
    let other_pane = ComparePane::new();
    compare.append(&best_pane.root);
    compare.append(&other_pane.root);
    review.append(&compare);

    let filmstrip = v3::vbox(8);
    let strip_head = v3::hbox(10);
    let strip_header = v3::section("ALL 0 PHOTOS · 0 TO KEEP");
    strip_header.set_hexpand(true);
    strip_head.append(&strip_header);
    let zoom_button = v3::ghost("Zoom to face");
    zoom_button.set_tooltip_text(Some("Z"));
    strip_head.append(&zoom_button);
    filmstrip.append(&strip_head);
    let strip = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .focusable(true)
        .css_classes(["v3-filmstrip"])
        .build();
    let strip_scroll = gtk::ScrolledWindow::builder()
        .vscrollbar_policy(gtk::PolicyType::Never)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .child(&strip)
        .build();
    filmstrip.append(&strip_scroll);
    let hint = v3::label("", &["v3-meta-115"]);
    hint.set_wrap(true);
    filmstrip.append(&hint);
    review.append(&filmstrip);
    let review_scroll = v3::scroller(&review);
    stack.add_named(&review_scroll, Some("review"));
    stack.set_visible_child_name("empty");
    page.append(&stack);

    let root = adw::ToastOverlay::new();
    root.set_child(Some(&page));

    let ui = Rc::new(Ui {
        engine,
        manual,
        proposals: related,
        requests: RefCell::new(Requests::default()),
        groups: RefCell::new(vec![]),
        loaded: Cell::new(false),
        regroup: Cell::new(false),
        current: RefCell::new(None),
        resolved: RefCell::new(None),
        qualities: RefCell::new(vec![]),
        keep: RefCell::new(BTreeSet::new()),
        focused: Cell::new(0),
        zoomed: Cell::new(false),
        pending_decision: RefCell::new(None),
        pending_trash: RefCell::new(None),
        restoring: RefCell::new(None),
        tile_token: RefCell::new(CancelToken::default()),
        fan_token: RefCell::new(CancelToken::default()),
        shots: RefCell::new(HashMap::new()),
        tiles: RefCell::new(vec![]),
        scroll_animation: RefCell::new(None),
        root: root.clone(),
        groups_header,
        groups_bar,
        look_again: look_again.clone(),
        groups_list,
        stack,
        loading_label,
        failed_title,
        failed_text,
        title,
        date,
        skipped,
        trash_button: trash_button.clone(),
        keep_all: keep_all.clone(),
        skip: skip.clone(),
        action_bar,
        best_pane,
        other_pane,
        strip_header,
        zoom_button: zoom_button.clone(),
        strip: strip.clone(),
        strip_scroll,
        hint,
    });

    let weak = Rc::downgrade(&ui);
    let with = move |f: fn(&Rc<Ui>)| {
        let weak = weak.clone();
        move || {
            if let Some(ui) = weak.upgrade() {
                f(&ui);
            }
        }
    };
    let run = with(|ui| ui.request_groups());
    look_again.connect_clicked(move |_| run());
    let run = with(|ui| ui.confirm_trash());
    trash_button.connect_clicked(move |_| run());
    let run = with(|ui| ui.decide("keepAll"));
    keep_all.connect_clicked(move |_| run());
    let run = with(|ui| ui.decide("skip"));
    skip.connect_clicked(move |_| run());
    let run = with(|ui| ui.toggle_zoom());
    zoom_button.connect_clicked(move |_| run());
    for (best, pane) in [(true, &ui.best_pane), (false, &ui.other_pane)] {
        let weak = Rc::downgrade(&ui);
        pane.play.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.play(best);
            }
        });
    }
    let run = with(|ui| {
        ui.request_groups();
        ui.proposals.request(ui);
        if ui.engine.borrow().is_ready() && !ui.requests.borrow().busy("manual") {
            ui.manual.send(ui, "listEvents", |_| {});
        }
    });
    root.connect_map(move |_| run());
    let run = with(|ui| {
        ui.best_pane.stop();
        ui.other_pane.stop();
        ui.manual.stop_cards();
    });
    root.connect_unmap(move |_| run());

    let keys = gtk::EventControllerKey::new();
    let weak = Rc::downgrade(&ui);
    keys.connect_key_pressed(move |_, key, _, modifiers| {
        let Some(ui) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        if modifiers
            .intersects(gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::ALT_MASK)
        {
            return glib::Propagation::Proceed;
        }
        let focused = ui.focused.get();
        let count = ui.qualities.borrow().len();
        match key {
            gtk::gdk::Key::Left | gtk::gdk::Key::KP_Left => {
                ui.focus(model::step_focus(focused, -1, count))
            }
            gtk::gdk::Key::Right | gtk::gdk::Key::KP_Right => {
                ui.focus(model::step_focus(focused, 1, count))
            }
            gtk::gdk::Key::space => ui.toggle_keep(focused),
            gtk::gdk::Key::z | gtk::gdk::Key::Z => ui.toggle_zoom(),
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    });
    strip.add_controller(keys);

    let enter = gtk::EventControllerKey::new();
    let weak = Rc::downgrade(&ui);
    enter.connect_key_pressed(move |_, key, _, _| {
        let Some(ui) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        if matches!(key, gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter)
            && ui.trash_button.is_sensitive()
        {
            ui.confirm_trash();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    review.add_controller(enter);

    let weak = Rc::downgrade(&ui);
    glib::timeout_add_local(std::time::Duration::from_secs(2), move || {
        let Some(ui) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if ui.root.is_mapped()
            && ui.manual.wants_jobs()
            && ui.engine.borrow().is_ready()
            && !ui.requests.borrow().busy("jobs")
        {
            ui.send("jobs", catalog_request("jobs"));
        }
        glib::ControlFlow::Continue
    });

    super::takes_groups::wire(&ui);
    proposals::Proposals::wire(&ui);
    subscribe(&ui, keep_alive);
    (root.upcast(), ui)
}

impl Ui {
    pub(super) fn send(&self, slot: &str, request: CatalogRequest) -> bool {
        self.requests.borrow_mut().begin(slot, &request.request_id);
        let sent = self
            .engine
            .borrow_mut()
            .send(CommandPayload::CatalogRequest(Box::new(
                CatalogRequestPayload { request },
            )));
        if let Err(error) = sent {
            self.requests.borrow_mut().forget(slot);
            self.groups_bar.set_visible(false);
            self.look_again.set_sensitive(true);
            self.toast(&format!("Could not reach the helper: {error}"));
            return false;
        }
        true
    }

    pub(super) fn toast(&self, text: &str) {
        let toast = adw::Toast::builder().title(text).use_markup(false).build();
        self.root.add_toast(toast);
    }

    fn toast_undo(self: &Rc<Self>, text: &str, seconds: u32, undo: impl Fn(&Rc<Ui>) + 'static) {
        let toast = adw::Toast::builder()
            .title(text)
            .use_markup(false)
            .button_label("Undo")
            .timeout(seconds)
            .build();
        let weak = Rc::downgrade(self);
        toast.connect_button_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                undo(&ui);
            }
        });
        self.root.add_toast(toast);
    }

    fn request_groups(self: &Rc<Self>) {
        if !self.engine.borrow().is_ready() {
            return;
        }
        if self.requests.borrow().busy("groups") {
            self.regroup.set(true);
            return;
        }
        self.groups_bar.set_visible(true);
        self.look_again.set_sensitive(false);
        if !self.loaded.get() && self.current.borrow().is_none() && !self.manual.engaged() {
            self.loading_label
                .set_text("Looking for bursts and repeated clips…");
            self.stack.set_visible_child_name("loading");
        }
        let mut request = catalog_request("autoTakeGroups");
        request.limit = Some(100);
        self.send("groups", request);
    }

    /// Closes the open possible group (the manual editor is taking over).
    pub(super) fn leave_auto(&self) {
        self.tile_token
            .borrow()
            .store(true, std::sync::atomic::Ordering::Release);
        *self.tile_token.borrow_mut() = CancelToken::default();
        self.current.borrow_mut().take();
        self.best_pane.reset();
        self.other_pane.reset();
        let mut child = self.groups_list.first_child();
        while let Some(widget) = child {
            v3::set_selected(&widget, false);
            child = widget.next_sibling();
        }
        self.proposals.clear_selection();
    }

    /// The right side with nothing open: first load, no groups, or a hint.
    pub(super) fn show_idle(&self) {
        let name = if !self.loaded.get() && self.requests.borrow().busy("groups") {
            "loading"
        } else if self.groups.borrow().is_empty() {
            "empty"
        } else {
            "pick"
        };
        self.stack.set_visible_child_name(name);
    }

    fn groups_finished(self: &Rc<Self>) {
        self.groups_bar.set_visible(false);
        self.look_again.set_sensitive(true);
        if self.regroup.replace(false) {
            self.request_groups();
        }
    }

    fn resolve(&self, key: &str) {
        let position = self
            .groups
            .borrow()
            .iter()
            .position(|group| group.group_key == key)
            .unwrap_or(0);
        *self.resolved.borrow_mut() = Some((key.to_owned(), position));
    }

    fn render_groups(self: &Rc<Self>, groups: Vec<CatalogTakeAutoGroup>) {
        self.loaded.set(true);
        v3::clear(&self.groups_list);
        self.fan_token
            .borrow()
            .store(true, std::sync::atomic::Ordering::Release);
        *self.fan_token.borrow_mut() = CancelToken::default();
        self.groups_header
            .set_text(&format!("POSSIBLE GROUPS · {}", model::open_count(&groups)));
        let current_key = self.current.borrow().as_ref().map(|g| g.group_key.clone());
        let resolved = self.resolved.borrow_mut().take();
        let manual = (self.manual.engaged() && current_key.is_none()) || self.proposals.any_open();
        let target = if manual {
            None
        } else {
            model::next_selection(
                &groups,
                current_key.as_deref(),
                resolved
                    .as_ref()
                    .map(|(key, position)| (key.as_str(), *position)),
            )
        };
        let mut fans = Vec::new();
        for (index, group) in groups.iter().enumerate() {
            let row = v3::hbox(10);
            let fan = gtk::Fixed::builder()
                .accessible_role(gtk::AccessibleRole::Presentation)
                .width_request(60)
                .height_request(28)
                .valign(gtk::Align::Center)
                .build();
            let shots: Vec<_> = group.shots.iter().take(3).collect();
            let mut views = Vec::new();
            for (slot, shot) in shots.iter().enumerate().rev() {
                let view = ShotView::new(28, 28, 4.0, true, true);
                fan.put(&view.area, slot as f64 * 16.0, 0.0);
                views.push((shot.path.clone(), model::is_video(group), FAN_PX, view));
            }
            views.reverse();
            fans.extend(views);
            row.append(&fan);
            let text = v3::vbox(2);
            let title = v3::label(&model::group_title(group), &["v3-title-13"]);
            if !model::is_open(group) {
                title.add_css_class("tone-muted");
            }
            let date = date_text(group.captured_at);
            let meta = v3::label(
                &if model::is_open(group) {
                    date.clone()
                } else {
                    format!("{date} · Skipped")
                },
                &["v3-meta-115"],
            );
            meta.set_ellipsize(gtk::pango::EllipsizeMode::End);
            text.append(&title);
            text.append(&meta);
            row.append(&text);
            let button = v3::rail_row(&row);
            button.update_property(&[gtk::accessible::Property::Label(&model::group_spoken(
                group, &date,
            ))]);
            v3::set_selected(&button, target == Some(index));
            let weak = Rc::downgrade(self);
            let key = group.group_key.clone();
            button.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.select_key(&key);
                }
            });
            self.groups_list.append(&button);
        }
        if groups.is_empty() {
            let none = v3::wrapped(
                "None yet. Scan a folder with photo bursts or several tries of a clip.",
                &["v3-meta"],
            );
            none.set_margin_start(14);
            none.set_margin_end(14);
            self.groups_list.append(&none);
        }
        *self.groups.borrow_mut() = groups;
        load_in_order(&self.engine, fans, self.fan_token.borrow().clone());
        if manual {
            return;
        }
        let target_key = target.and_then(|index| {
            self.groups
                .borrow()
                .get(index)
                .map(|group| group.group_key.clone())
        });
        if target_key.is_some() && target_key == current_key {
            if let (Some(index), Some(group)) = (target, self.current.borrow_mut().as_mut()) {
                *group = self.groups.borrow()[index].clone();
            }
            self.refresh_header();
            return;
        }
        match target {
            Some(index) => self.select(index),
            None => {
                *self.current.borrow_mut() = None;
                self.best_pane.reset();
                self.other_pane.reset();
                self.show_idle();
            }
        }
    }

    fn select_key(self: &Rc<Self>, key: &str) {
        let index = self
            .groups
            .borrow()
            .iter()
            .position(|group| group.group_key == key);
        if let Some(index) = index {
            self.select(index);
        }
    }

    fn select(self: &Rc<Self>, index: usize) {
        let Some(group) = self.groups.borrow().get(index).cloned() else {
            return;
        };
        self.proposals.clear_selection();
        let mut child = self.groups_list.first_child();
        let mut position = 0;
        while let Some(widget) = child {
            v3::set_selected(&widget, position == index);
            position += 1;
            child = widget.next_sibling();
        }
        self.open_group(group);
    }

    fn open_proposal(self: &Rc<Self>, key: &str) {
        let Some(group) = self.proposals.take(key) else {
            return;
        };
        let mut child = self.groups_list.first_child();
        while let Some(widget) = child {
            v3::set_selected(&widget, false);
            child = widget.next_sibling();
        }
        self.open_group(group);
    }

    /// Nothing open on the right any more (a proposal vanished or the last one was resolved).
    fn close_review(&self) {
        *self.current.borrow_mut() = None;
        self.best_pane.reset();
        self.other_pane.reset();
        if self.manual.engaged() {
            return;
        }
        self.show_idle();
    }

    fn open_group(self: &Rc<Self>, group: CatalogTakeAutoGroup) {
        self.manual.leave();
        self.tile_token
            .borrow()
            .store(true, std::sync::atomic::Ordering::Release);
        *self.tile_token.borrow_mut() = CancelToken::default();
        self.shots.borrow_mut().clear();
        self.best_pane.reset();
        self.other_pane.reset();
        self.loading_label.set_text(&format!(
            "Checking sharpness, faces and exposure in {} {}…",
            group.shots.len(),
            model::noun(&group, group.shots.len())
        ));
        self.stack.set_visible_child_name("loading");
        let mut request = catalog_request("scoreTakes");
        request.file_ids = Some(group.file_ids());
        self.send("score", request);
        *self.current.borrow_mut() = Some(group);
        self.refresh_header();
    }

    fn render_review(self: &Rc<Self>, qualities: Vec<CatalogTakeQuality>) {
        let Some(group) = self.current.borrow().clone() else {
            return;
        };
        if qualities.is_empty() {
            self.score_failed("FileID didn't answer in time. Try again in a moment.");
            return;
        }
        *self.keep.borrow_mut() = model::default_keep(&qualities);
        self.focused.set(model::best_index(&qualities).unwrap_or(0));
        self.zoomed.set(false);
        *self.qualities.borrow_mut() = qualities;
        self.hint.set_text(&model::hint_line(&group));
        self.strip
            .update_property(&[gtk::accessible::Property::Label(&format!(
                "{}. Left and right arrows move, Space marks keep.",
                model::group_title(&group)
            ))]);
        self.keep_all.set_tooltip_text(Some(&format!(
            "Keep every {} here and stop suggesting this group",
            model::noun(&group, 1)
        )));
        self.rebuild_strip(&group);
        self.refresh();
        self.stack.set_visible_child_name("review");
        if self.root.is_mapped() {
            self.strip.grab_focus();
        }
    }

    fn score_failed(&self, message: &str) {
        let noun = self
            .current
            .borrow()
            .as_ref()
            .map_or("photos", |group| model::noun(group, 2));
        self.failed_title
            .set_text(&format!("Couldn't score these {noun}"));
        self.failed_text.set_text(message);
        self.stack.set_visible_child_name("failed");
    }

    fn rebuild_strip(self: &Rc<Self>, group: &CatalogTakeAutoGroup) {
        v3::clear(&self.strip);
        let video = model::is_video(group);
        let mut jobs = Vec::new();
        let mut tiles = Vec::new();
        for (index, quality) in self.qualities.borrow().iter().enumerate() {
            let tile = Tile::new(index + 1);
            let name = v3::file_name(&quality.path);
            tile.frame
                .set_tooltip_text(Some(&if quality.reasons.is_empty() {
                    name
                } else {
                    format!("{name} · {}", quality.reasons.join(", "))
                }));
            let click = gtk::GestureClick::new();
            let weak = Rc::downgrade(self);
            click.connect_pressed(move |_, presses, _, _| {
                let Some(ui) = weak.upgrade() else { return };
                ui.strip.grab_focus();
                if presses == 2 {
                    ui.toggle_keep(index);
                } else {
                    ui.focus(index);
                }
            });
            tile.frame.add_controller(click);
            self.strip.append(&tile.holder);
            jobs.push((quality.path.clone(), video, TILE_PX, tile.view.clone()));
            tiles.push(tile);
        }
        *self.tiles.borrow_mut() = tiles;
        load_in_order(&self.engine, jobs, self.tile_token.borrow().clone());
    }

    fn refresh_header(&self) {
        let Some(group) = self.current.borrow().clone() else {
            return;
        };
        self.title.set_text(&model::group_title(&group));
        self.date.set_text(&date_text(group.captured_at));
        self.skipped.set_visible(!model::is_open(&group));
        let trashing = self.pending_trash.borrow().is_some();
        self.proposals.render_header(&group, trashing);
        self.skip.set_sensitive(model::is_open(&group) && !trashing);
        self.keep_all.set_sensitive(!trashing);
        let button = model::trash_button(&group, &self.qualities.borrow(), &self.keep.borrow());
        self.trash_button.set_label(&button.label);
        self.trash_button.set_sensitive(button.enabled && !trashing);
        self.trash_button.set_tooltip_text(Some(&button.tooltip));
    }

    fn refresh(self: &Rc<Self>) {
        let Some(group) = self.current.borrow().clone() else {
            return;
        };
        self.refresh_header();
        let qualities = self.qualities.borrow().clone();
        let keep = self.keep.borrow().clone();
        let focused = self.focused.get();
        self.strip_header
            .set_text(&model::filmstrip_header(&group, &keep, qualities.len()));
        let best = model::best_index(&qualities);
        for (index, (tile, quality)) in self.tiles.borrow().iter().zip(&qualities).enumerate() {
            let is_best = best == Some(index);
            let kept = keep.contains(&quality.primary_file_id);
            tile.update(
                is_best,
                kept,
                index == focused,
                &model::tile_spoken(&group, quality, index, qualities.len(), is_best, kept),
                &quality.reasons.join(", "),
            );
        }
        self.refresh_zoom_button(&group);
        let Some((best_index, other_index)) = model::compare_pair(&qualities, focused) else {
            return;
        };
        let other_title = if other_index == focused {
            "THIS TRY"
        } else {
            "RUNNER-UP"
        };
        for (pane, index, title) in [
            (&self.best_pane, best_index, "BEST"),
            (&self.other_pane, other_index, other_title),
        ] {
            let quality = &qualities[index];
            let changed = pane.fill(
                &group,
                quality,
                title,
                index,
                qualities.len(),
                index == best_index,
                keep.contains(&quality.primary_file_id),
            );
            if changed {
                pane.view.set_region(self.region_for(quality), false);
                self.load_shot(quality, model::is_video(&group));
            }
        }
    }

    fn refresh_zoom_button(&self, group: &CatalogTakeAutoGroup) {
        let zoomed = self.zoomed.get();
        self.zoom_button
            .set_label(&model::zoom_label(group, zoomed));
        v3::set_selected(&self.zoom_button, zoomed);
        self.zoom_button
            .update_state(&[gtk::accessible::State::Selected(Some(zoomed))]);
    }

    fn region_for(&self, quality: &CatalogTakeQuality) -> CatalogTakeBox {
        if self.zoomed.get() {
            model::zoom_box(quality).unwrap_or(model::WHOLE)
        } else {
            model::WHOLE
        }
    }

    fn load_shot(self: &Rc<Self>, quality: &CatalogTakeQuality, video: bool) {
        let id = quality.primary_file_id;
        if let Some(shot) = self.shots.borrow().get(&id).cloned() {
            self.show_shot(id, shot);
            return;
        }
        let rx = self.engine.borrow().request_tile(
            quality.path.clone(),
            super::util::decode_px(&self.best_pane.view.area, COMPARE_PX),
            video,
            self.tile_token.borrow().clone(),
        );
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let Ok(ThumbReply::Image(decoded)) = rx.recv().await else {
                return;
            };
            let (Some(ui), Some(shot)) = (weak.upgrade(), Shot::from_decoded(decoded)) else {
                return;
            };
            let mut shots = ui.shots.borrow_mut();
            if shots.len() >= SHOT_CACHE {
                shots.clear();
            }
            shots.insert(id, shot.clone());
            drop(shots);
            ui.show_shot(id, shot);
        });
    }

    fn show_shot(&self, id: i64, shot: Shot) {
        for pane in [&self.best_pane, &self.other_pane] {
            if pane.shown.get() == Some(id) {
                pane.view.set_shot(Some(shot.clone()));
            }
        }
    }

    fn focus(self: &Rc<Self>, index: usize) {
        if index >= self.qualities.borrow().len() {
            return;
        }
        self.focused.set(index);
        self.refresh();
        self.scroll_to(index);
    }

    fn scroll_to(&self, index: usize) {
        let Some(frame) = self
            .tiles
            .borrow()
            .get(index)
            .map(|tile| tile.frame.clone())
        else {
            return;
        };
        let Some(bounds) = frame.compute_bounds(&self.strip) else {
            return;
        };
        let adjustment = self.strip_scroll.hadjustment();
        let page = adjustment.page_size();
        let target = (f64::from(bounds.x() + bounds.width() / 2.0) - page / 2.0).clamp(
            adjustment.lower(),
            (adjustment.upper() - page).max(adjustment.lower()),
        );
        if let Some(previous) = self.scroll_animation.take() {
            previous.pause();
        }
        let moving = adjustment.clone();
        let animation = crate::spring::animate(
            &self.strip_scroll,
            adjustment.value(),
            target,
            move |value| moving.set_value(value),
        );
        self.scroll_animation.replace(Some(animation));
    }

    fn toggle_keep(self: &Rc<Self>, index: usize) {
        let Some(quality) = self.qualities.borrow().get(index).cloned() else {
            return;
        };
        let kept = model::toggle(&mut self.keep.borrow_mut(), quality.primary_file_id);
        self.focused.set(index);
        self.refresh();
        self.strip.announce(
            &model::keep_announcement(&quality, kept),
            gtk::AccessibleAnnouncementPriority::Medium,
        );
    }

    fn toggle_zoom(self: &Rc<Self>) {
        let Some(group) = self.current.borrow().clone() else {
            return;
        };
        if self.qualities.borrow().is_empty() {
            return;
        }
        self.zoomed.set(!self.zoomed.get());
        self.refresh_zoom_button(&group);
        let qualities = self.qualities.borrow().clone();
        for pane in [&self.best_pane, &self.other_pane] {
            if let Some(quality) = qualities
                .iter()
                .find(|quality| Some(quality.primary_file_id) == pane.shown.get())
            {
                pane.view.set_region(self.region_for(quality), true);
            }
        }
    }

    fn play(&self, best: bool) {
        let pane = if best {
            &self.best_pane
        } else {
            &self.other_pane
        };
        let quality = self
            .qualities
            .borrow()
            .iter()
            .find(|quality| Some(quality.primary_file_id) == pane.shown.get())
            .cloned();
        if let Some(quality) = quality {
            for other in [&self.best_pane, &self.other_pane] {
                if !std::ptr::eq(other, pane) {
                    other.stop();
                    other.play.set_visible(true);
                }
            }
            pane.play(&quality);
        }
    }

    fn confirm_trash(self: &Rc<Self>) {
        let Some(group) = self.current.borrow().clone() else {
            return;
        };
        let qualities = self.qualities.borrow().clone();
        let keep = self.keep.borrow().clone();
        if !model::trash_button(&group, &qualities, &keep).enabled
            || self.pending_trash.borrow().is_some()
        {
            return;
        }
        let dialog = adw::AlertDialog::new(
            Some(model::confirm_heading(&group, &qualities, &keep).as_str()),
            Some(model::confirm_body(&qualities, &keep).as_str()),
        );
        dialog.add_responses(&[("cancel", "Cancel"), ("trash", "Move to Trash")]);
        dialog.set_response_appearance("trash", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            if let (Some(ui), "trash") = (weak.upgrade(), response) {
                ui.trash();
            }
        });
        dialog.present(Some(&self.root));
    }

    fn trash(self: &Rc<Self>) {
        let Some(group) = self.current.borrow().clone() else {
            return;
        };
        let qualities = self.qualities.borrow().clone();
        let keep = self.keep.borrow().clone();
        let ids = model::trash_ids(&qualities, &keep);
        if ids.is_empty() || self.pending_trash.borrow().is_some() {
            return;
        }
        let kept: Vec<&CatalogTakeQuality> = model::kept(&qualities, &keep).collect();
        *self.pending_trash.borrow_mut() = Some(PendingTrash {
            group,
            doomed: model::victims(&qualities, &keep).cloned().collect(),
            kept_files: kept
                .iter()
                .flat_map(|q| q.file_ids.iter().copied())
                .collect(),
            kept_tries: kept.len(),
        });
        self.action_bar.set_visible(true);
        self.refresh_header();
        let sent = self
            .engine
            .borrow_mut()
            .send(CommandPayload::TrashFiles(TrashFilesPayload {
                file_ids: ids,
                exact_identities: None,
            }));
        if sent.is_err() {
            self.end_trash();
            self.toast("FileID is still getting ready. Try again in a moment.");
        }
    }

    fn end_trash(&self) -> Option<PendingTrash> {
        let pending = self.pending_trash.borrow_mut().take();
        self.action_bar.set_visible(false);
        self.refresh_header();
        pending
    }

    fn finish_trash(self: &Rc<Self>, result: BulkActionResult) {
        let Some(pending) = self.end_trash() else {
            return;
        };
        if result.succeeded == 0 {
            let reason = result
                .messages
                .iter()
                .find_map(|item| item.message.clone())
                .unwrap_or_else(|| "Nothing was moved.".to_owned());
            self.toast(&format!("Couldn't move to Trash. {reason}"));
            return;
        }
        let reopen = (pending.kept_tries >= 2).then(|| pending.kept_files.clone());
        if let Some(files) = &reopen {
            let mut request = catalog_request("decideTakeGroup");
            request.take_decision = Some(model::decision_for(
                &pending.group,
                "keepAll",
                files.clone(),
            ));
            self.send("keep-rest", request);
        }
        let moved = model::moved_tries(&pending.doomed, &result);
        let text = model::moved_message(&pending.group, moved, result.failed);
        let batch = result
            .action
            .split_once(':')
            .map(|(_, batch)| batch.to_owned());
        match batch {
            Some(batch_id) => {
                let group = pending.group.clone();
                self.toast_undo(&text, 10, move |ui| {
                    *ui.restoring.borrow_mut() =
                        Some(reopen.clone().map(|files| (group.clone(), files)));
                    let sent = ui
                        .engine
                        .borrow_mut()
                        .send(CommandPayload::RestoreFromTrash(RestoreFromTrashPayload {
                            batch_id: batch_id.clone(),
                        }));
                    if sent.is_err() {
                        ui.restoring.borrow_mut().take();
                        ui.toast("FileID is still getting ready. Try again in a moment.");
                    }
                });
            }
            None => self.toast(&text),
        }
        self.after_resolve(&pending.group.group_key);
    }

    fn finish_restore(self: &Rc<Self>, result: BulkActionResult) {
        let Some(follow_up) = self.restoring.borrow_mut().take() else {
            return;
        };
        if let Some((group, files)) = follow_up {
            let mut request = catalog_request("decideTakeGroup");
            request.take_decision = Some(model::decision_for(&group, "reopen", files));
            self.send("reopen", request);
        }
        self.toast(&model::restored_message(&result));
        self.request_groups();
        self.proposals.request(self);
    }

    fn decide(self: &Rc<Self>, kind: &str) {
        let Some(group) = self.current.borrow().clone() else {
            return;
        };
        if self.requests.borrow().busy("decision") || self.pending_trash.borrow().is_some() {
            return;
        }
        let mut request = catalog_request("decideTakeGroup");
        request.take_decision = Some(model::decision(&group, kind));
        if self.send("decision", request) {
            *self.pending_decision.borrow_mut() = Some((group, kind.to_owned()));
        }
    }

    fn finish_decision(self: &Rc<Self>, ok: bool) {
        let Some((group, kind)) = self.pending_decision.borrow_mut().take() else {
            return;
        };
        if !ok {
            self.toast("That didn't save. Try again.");
            return;
        }
        let reopen = group.clone();
        self.toast_undo(&model::decided_message(&group, &kind), 8, move |ui| {
            let mut request = catalog_request("decideTakeGroup");
            request.take_decision = Some(model::decision(&reopen, "reopen"));
            ui.send("reopen", request);
        });
        self.after_resolve(&group.group_key);
    }

    /// Reads both lists again after a group is kept, skipped or trashed; review
    /// flows on to the next group in whichever list it came from.
    fn after_resolve(self: &Rc<Self>, key: &str) {
        if self.proposals.is_open(key) {
            self.proposals.resolve(key);
        } else {
            self.resolve(key);
        }
        self.request_groups();
        self.proposals.request(self);
    }
}

/// Decodes thumbnails one at a time in list order, so a long list never
/// overflows the tile queue (which drops its oldest jobs) and the first rows
/// arrive first. A cancelled token stops the walk.
pub(super) fn load_in_order(
    engine: &Engine,
    jobs: Vec<(String, bool, i32, ShotView)>,
    token: CancelToken,
) {
    let engine = Rc::downgrade(engine);
    glib::MainContext::default().spawn_local(async move {
        for (path, video, dim, view) in jobs {
            if token.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            let dim = super::util::decode_px(&view.area, dim);
            let Some(rx) = engine.upgrade().map(|engine| {
                engine
                    .borrow()
                    .request_tile(path, dim, video, token.clone())
            }) else {
                return;
            };
            if let Ok(ThumbReply::Image(decoded)) = rx.recv().await
                && !token.load(std::sync::atomic::Ordering::Acquire)
            {
                view.set_shot(Shot::from_decoded(decoded));
            }
        }
    });
}

fn date_text(seconds: f64) -> String {
    glib::DateTime::from_unix_local(seconds as i64)
        .ok()
        .and_then(|date| date.format("%a %-d %b, %-I:%M %p").ok())
        .map_or_else(String::new, |text| text.to_string())
}

/// The page in the main window lives as long as the app (`keep_alive`); the
/// dialog copy stops listening once the dialog lets go of it.
fn subscribe(ui: &Rc<Ui>, keep_alive: bool) {
    let rx = ui.engine.borrow_mut().subscribe();
    let strong = keep_alive.then(|| ui.clone());
    let weak = Rc::downgrade(ui);
    glib::MainContext::default().spawn_local(async move {
        let _strong = strong;
        while let Ok(event) = rx.recv().await {
            let Some(ui) = weak.upgrade() else { return };
            match event {
                EngineState::CatalogResponse(r) if super::takes_proposals_model::is_job_push(&r) => {
                    ui.proposals.set_job(&ui, super::takes_proposals_model::job(&r.jobs));
                }
                EngineState::CatalogResponse(r) => {
                    let Some(slot) = ui.requests.borrow_mut().accept(&r.request_id, true) else {
                        continue;
                    };
                    let ok = r.status == "ok";
                    match slot.as_str() {
                        "groups" => {
                            if ok {
                                ui.render_groups(r.take_groups.unwrap_or_default());
                            } else {
                                ui.toast(r.message.as_deref().unwrap_or("Couldn't look for groups."));
                                if ui.current.borrow().is_none() {
                                    ui.stack.set_visible_child_name("empty");
                                }
                            }
                            ui.groups_finished();
                        }
                        "score" if ok => ui.render_review(r.take_qualities.unwrap_or_default()),
                        "score" => ui.score_failed(
                            r.message
                                .as_deref()
                                .unwrap_or("FileID didn't answer in time. Try again in a moment."),
                        ),
                        "decision" => ui.finish_decision(ok),
                        "reopen" | "keep-rest" => {
                            ui.request_groups();
                            ui.proposals.request(&ui);
                        }
                        "proposals" | "grouping" | "grouping-control" => ui.proposals.consume(&ui, &slot, r),
                        "manual" => ui.manual.consume(&ui, r),
                        "jobs" if ok => ui.manual.set_jobs(&ui, r.jobs),
                        "job-control" => {
                            ui.send("jobs", catalog_request("jobs"));
                        }
                        _ => {}
                    }
                }
                EngineState::BulkActionResult(result) if result.action.starts_with("trashFiles") => {
                    ui.finish_trash(result)
                }
                EngineState::BulkActionResult(result) if result.action.starts_with("restoreFromTrash") => {
                    ui.finish_restore(result)
                }
                EngineState::Ready => {
                    ui.request_groups();
                    ui.proposals.request(&ui);
                    if ui.root.is_mapped() && !ui.requests.borrow().busy("manual") {
                        ui.manual.send(&ui, "listEvents", |_| {});
                    }
                }
                EngineState::ScanComplete(_) => ui.request_groups(),
                EngineState::Failed(message) => {
                    ui.requests.borrow_mut().clear();
                    ui.manual.request_failed();
                    ui.pending_decision.borrow_mut().take();
                    ui.groups_finished();
                    ui.proposals.finished_request(&ui);
                    if ui.end_trash().is_some() || ui.root.is_mapped() {
                        ui.toast(&message);
                    }
                }
                EngineState::Exited => {
                    ui.requests.borrow_mut().clear();
                    ui.manual.request_failed();
                    ui.pending_decision.borrow_mut().take();
                    ui.restoring.borrow_mut().take();
                    ui.groups_finished();
                    if ui.end_trash().is_some() {
                        ui.toast("FileID restarted before the move to Trash finished. Check Trash, then try again.");
                    }
                }
                _ => {}
            }
        }
    });
}
