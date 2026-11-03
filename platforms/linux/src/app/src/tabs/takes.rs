// Best Takes (DESIGN_V3_PORT "Best Takes"): the engine groups bursts and repeated
// clips by itself, scores each try without a model, and this page lets the
// user check the starred best try, keep or skip, or move the rest to Trash
// with Undo. The goal ranking panel is not offered: the shared engine
// reports it as unavailable.

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::rc::Rc;

use adw::prelude::*;
use fileid_engine::ipc::*;
use gtk::glib;

use super::takes_model as model;
use super::workbench_state::{catalog_request, Requests};
use super::workbenches::Engine;
use crate::engine_client::{texture_from_decoded, CancelToken, EngineState, ThumbReply};
use crate::shell::status::Tone;
use crate::v3;

struct Ui {
    engine: Engine,
    requests: RefCell<Requests>,
    groups: RefCell<Vec<CatalogTakeAutoGroup>>,
    current: RefCell<Option<CatalogTakeAutoGroup>>,
    qualities: RefCell<Vec<CatalogTakeQuality>>,
    keep: RefCell<BTreeSet<i64>>,
    focused: Cell<i64>,
    last_batch: RefCell<Option<String>>,
    undo_decision: RefCell<Option<CatalogTakeDecision>>,
    trashing: Cell<bool>,
    tile_token: RefCell<CancelToken>,
    root: gtk::Box,
    groups_header: gtk::Label,
    groups_bar: gtk::ProgressBar,
    groups_list: gtk::Box,
    stack: gtk::Stack,
    loading_label: gtk::Label,
    failed_text: gtk::Label,
    title: gtk::Label,
    date: gtk::Label,
    skipped: gtk::Label,
    trash_button: gtk::Button,
    keep_all: gtk::Button,
    skip: gtk::Button,
    action_bar: gtk::ProgressBar,
    notice: gtk::Label,
    undo: gtk::Button,
    best_panel: v3::Panel,
    best_picture: gtk::Picture,
    other_panel: v3::Panel,
    other_picture: gtk::Picture,
    strip_header: gtk::Label,
    strip: gtk::Box,
}

pub fn takes(engine: Engine) -> gtk::Widget {
    build(engine)
}

fn picture(height: i32) -> gtk::Picture {
    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Contain)
        .css_classes(["v3-black"])
        .hexpand(true)
        .build();
    picture.set_size_request(-1, height);
    picture
}

fn build(engine: Engine) -> gtk::Widget {
    let root = v3::hbox(0);

    let pane = v3::vbox(0);
    pane.add_css_class("v3-pane");
    pane.set_size_request(280, -1);
    let top = v3::vbox(6);
    top.add_css_class("v3-pane-top");
    let header_row = v3::hbox(8);
    let groups_header = v3::section("POSSIBLE GROUPS");
    groups_header.set_hexpand(true);
    header_row.append(&groups_header);
    let groups_bar = v3::busy_bar(40);
    header_row.append(&groups_bar);
    let look_again = v3::icon_button("view-refresh-symbolic", "Look again");
    header_row.append(&look_again);
    top.append(&header_row);
    top.append(&v3::wrapped(
        "Bursts and repeated clips from your scans. Biggest and newest first.",
        &["v3-meta"],
    ));
    pane.append(&top);
    let groups_list = v3::vbox(2);
    groups_list.set_margin_top(8);
    groups_list.set_margin_bottom(8);
    let list_scroll = v3::scroller(&groups_list);
    list_scroll.set_vexpand(true);
    pane.append(&list_scroll);
    root.append(&pane);

    let stack = gtk::Stack::new();
    stack.set_hexpand(true);
    stack.set_vexpand(true);

    let empty = v3::dashed_notice(
        "No tries to compare yet",
        "After a scan, Best Takes groups bursts and repeated clips here by itself. Scan a folder with photo bursts or several tries of a clip.",
    );
    empty.add_css_class("v3-page-pad");
    stack.add_named(&empty, Some("empty"));

    let loading = v3::vbox(10);
    loading.add_css_class("v3-page-pad");
    loading.set_valign(gtk::Align::Center);
    loading.set_halign(gtk::Align::Center);
    let loading_bar = v3::busy_bar(160);
    loading_bar.set_visible(true);
    loading.append(&loading_bar);
    let loading_label = v3::label("Looking for bursts and repeated clips…", &["v3-body"]);
    loading.append(&loading_label);
    stack.add_named(&loading, Some("loading"));

    let failed = v3::dashed_notice("Couldn't score these photos", "");
    failed.add_css_class("v3-page-pad");
    let failed_text = v3::wrapped("", &["v3-body", "tone-text2"]);
    failed.append(&failed_text);
    stack.add_named(&failed, Some("failed"));

    let review = v3::vbox(16);
    review.add_css_class("v3-page-pad");
    let heading = v3::hbox(12);
    let title = v3::label("", &["v3-serif-28"]);
    heading.append(&title);
    let date = v3::label("", &["v3-meta"]);
    date.set_valign(gtk::Align::End);
    heading.append(&date);
    let skipped = v3::pill("SKIPPED", Tone::Muted);
    skipped.set_visible(false);
    skipped.set_valign(gtk::Align::Center);
    heading.append(&skipped);
    review.append(&heading);
    review.append(&v3::wrapped(
        "Tries of the same shot, taken moments apart. FileID starred the one that looks best. Check it, then keep it and clear out the rest.",
        &["v3-body", "tone-text3"],
    ));
    let actions = v3::hbox(8);
    let trash_button = v3::primary("Keep the best, move the other 0 to Trash", false);
    actions.append(&trash_button);
    let keep_all = v3::secondary("Keep all", false);
    actions.append(&keep_all);
    let skip = v3::ghost("Skip group");
    actions.append(&skip);
    let action_bar = v3::busy_bar(60);
    actions.append(&action_bar);
    review.append(&actions);
    let status_row = v3::hbox(8);
    let notice = v3::wrapped("", &["v3-meta"]);
    notice.set_hexpand(true);
    status_row.append(&notice);
    let undo = v3::accent("Undo", Tone::Info, false);
    undo.set_visible(false);
    status_row.append(&undo);
    review.append(&status_row);

    let compare = v3::hbox(12);
    compare.set_homogeneous(true);
    let best_panel = v3::panel("BEST");
    let best_picture = picture(340);
    best_panel.body.append(&best_picture);
    let other_panel = v3::panel("THIS TRY");
    let other_picture = picture(340);
    other_panel.body.append(&other_picture);
    compare.append(&best_panel.root);
    compare.append(&other_panel.root);
    review.append(&compare);

    let strip_header = v3::section("ALL 0 PHOTOS · 0 TO KEEP");
    review.append(&strip_header);
    let strip = v3::hbox(8);
    let strip_scroll = gtk::ScrolledWindow::builder()
        .vscrollbar_policy(gtk::PolicyType::Never)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .child(&strip)
        .build();
    review.append(&strip_scroll);
    let review_scroll = v3::scroller(&review);
    stack.add_named(&review_scroll, Some("review"));
    stack.set_visible_child_name("empty");
    root.append(&stack);

    let ui = Rc::new(Ui {
        engine,
        requests: RefCell::new(Requests::default()),
        groups: RefCell::new(vec![]),
        current: RefCell::new(None),
        qualities: RefCell::new(vec![]),
        keep: RefCell::new(BTreeSet::new()),
        focused: Cell::new(0),
        last_batch: RefCell::new(None),
        undo_decision: RefCell::new(None),
        trashing: Cell::new(false),
        tile_token: RefCell::new(CancelToken::default()),
        root: root.clone(),
        groups_header,
        groups_bar,
        groups_list,
        stack,
        loading_label,
        failed_text,
        title,
        date,
        skipped,
        trash_button: trash_button.clone(),
        keep_all: keep_all.clone(),
        skip: skip.clone(),
        action_bar,
        notice,
        undo: undo.clone(),
        best_panel,
        best_picture,
        other_panel,
        other_picture,
        strip_header,
        strip,
    });

    {
        let ui = ui.clone();
        look_again.connect_clicked(move |_| ui.request_groups());
    }
    {
        let ui = ui.clone();
        trash_button.connect_clicked(move |_| ui.confirm_trash());
    }
    {
        let ui = ui.clone();
        keep_all.connect_clicked(move |_| {
            ui.decide("keepAll", "Kept all. This group won't be suggested again.")
        });
    }
    {
        let ui = ui.clone();
        skip.connect_clicked(move |_| {
            ui.decide("skip", "Skipped. It's at the bottom of the list.")
        });
    }
    {
        let ui = ui.clone();
        undo.connect_clicked(move |_| ui.undo_last());
    }
    {
        let ui = ui.clone();
        root.connect_map(move |_| ui.request_groups());
    }
    subscribe(&ui);
    ui.root.clone().upcast()
}

impl Ui {
    fn send(&self, slot: &str, request: CatalogRequest) {
        self.requests.borrow_mut().begin(slot, &request.request_id);
        if let Err(error) = self
            .engine
            .borrow_mut()
            .send(CommandPayload::CatalogRequest(Box::new(
                CatalogRequestPayload { request },
            )))
        {
            self.requests.borrow_mut().forget(slot);
            self.groups_bar.set_visible(false);
            self.set_notice(&format!("Could not reach the helper: {error}"), false);
        }
    }

    fn set_notice(&self, text: &str, undo: bool) {
        self.notice.set_text(text);
        self.undo.set_visible(undo);
    }

    fn request_groups(self: &Rc<Self>) {
        if !self.engine.borrow().is_ready() || self.requests.borrow().busy("groups") {
            return;
        }
        self.groups_bar.set_visible(true);
        let mut request = catalog_request("autoTakeGroups");
        request.limit = Some(100);
        self.send("groups", request);
    }

    fn render_groups(self: &Rc<Self>, groups: Vec<CatalogTakeAutoGroup>) {
        self.groups_bar.set_visible(false);
        v3::clear(&self.groups_list);
        self.groups_header
            .set_text(&format!("POSSIBLE GROUPS · {}", model::open_count(&groups)));
        let current_key = self
            .current
            .borrow()
            .as_ref()
            .map(|group| group.group_key.clone());
        for group in &groups {
            let row = v3::vbox(2);
            let title = v3::label(&model::group_title(group), &["v3-medium"]);
            title.set_halign(gtk::Align::Start);
            if !model::is_open(group) {
                title.add_css_class("tone-text3");
            }
            let date = date_text(group.captured_at);
            let meta = v3::meta(&if model::is_open(group) {
                date.clone()
            } else {
                format!("{date} · Skipped")
            });
            meta.set_halign(gtk::Align::Start);
            row.append(&title);
            row.append(&meta);
            let button = v3::rail_row(&row);
            button.update_property(&[gtk::accessible::Property::Label(&model::group_spoken(
                group, &date,
            ))]);
            v3::set_selected(
                &button,
                current_key.as_deref() == Some(group.group_key.as_str()),
            );
            let ui = self.clone();
            let key = group.group_key.clone();
            button.connect_clicked(move |_| ui.select_key(&key));
            self.groups_list.append(&button);
        }
        if groups.is_empty() {
            self.groups_list.append(&v3::wrapped(
                "None yet. Scan a folder with photo bursts or several tries of a clip.",
                &["v3-meta"],
            ));
        }
        *self.groups.borrow_mut() = groups;
        let still_there = current_key.as_deref().is_some_and(|key| {
            self.groups
                .borrow()
                .iter()
                .any(|group| group.group_key == key)
        });
        if !still_there {
            *self.current.borrow_mut() = None;
            let next = model::first_open(&self.groups.borrow());
            match next {
                Some(index) => self.select(index),
                None => self.stack.set_visible_child_name("empty"),
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
        self.tile_token
            .borrow()
            .store(true, std::sync::atomic::Ordering::Release);
        *self.tile_token.borrow_mut() = CancelToken::default();
        self.set_notice("", false);
        self.loading_label.set_text(&format!(
            "Checking sharpness, faces and exposure in {} {}…",
            group.shots.len(),
            model::noun(&group, group.shots.len())
        ));
        self.stack.set_visible_child_name("loading");
        let mut request = catalog_request("scoreTakes");
        request.file_ids = Some(group.file_ids());
        self.send("score", request);
        let mut child = self.groups_list.first_child();
        let mut position = 0;
        while let Some(widget) = child {
            v3::set_selected(&widget, position == index);
            position += 1;
            child = widget.next_sibling();
        }
        *self.current.borrow_mut() = Some(group);
    }

    fn render_review(self: &Rc<Self>, qualities: Vec<CatalogTakeQuality>) {
        let Some(group) = self.current.borrow().clone() else {
            return;
        };
        if qualities.is_empty() {
            self.failed_text
                .set_text("The engine returned no scores for this group.");
            self.stack.set_visible_child_name("failed");
            return;
        }
        self.title.set_text(&model::group_title(&group));
        self.date.set_text(&date_text(group.captured_at));
        self.skipped.set_visible(!model::is_open(&group));
        self.skip.set_sensitive(model::is_open(&group));
        *self.keep.borrow_mut() = model::default_keep(&qualities);
        self.focused
            .set(model::best(&qualities).map_or(0, |best| best.primary_file_id));
        *self.qualities.borrow_mut() = qualities;
        self.rebuild_strip(&group);
        self.refresh(&group);
        self.stack.set_visible_child_name("review");
    }

    fn rebuild_strip(self: &Rc<Self>, group: &CatalogTakeAutoGroup) {
        v3::clear(&self.strip);
        let video = group.kind != "photos";
        let best_id = model::best(&self.qualities.borrow()).map(|best| best.primary_file_id);
        for (number, quality) in self.qualities.borrow().iter().enumerate() {
            let tile = v3::vbox(4);
            let thumb = gtk::Picture::builder()
                .content_fit(gtk::ContentFit::Cover)
                .css_classes(["v3-black"])
                .build();
            thumb.set_size_request(112, 76);
            tile.append(&thumb);
            let star = if Some(quality.primary_file_id) == best_id {
                " ★"
            } else {
                ""
            };
            let badge = v3::mono(&format!("{}{star}", number + 1), "mono-10");
            badge.set_halign(gtk::Align::Start);
            tile.append(&badge);
            let button = v3::rail_row(&tile);
            button.update_property(&[gtk::accessible::Property::Label(&format!(
                "Try {}{}, {}",
                number + 1,
                if star.is_empty() { "" } else { ", the best" },
                v3::file_name(&quality.path)
            ))]);
            let id = quality.primary_file_id;
            let ui = self.clone();
            button.connect_clicked(move |_| ui.focus(id));
            let ui = self.clone();
            let click = gtk::GestureClick::builder().button(3).build();
            click.connect_pressed(move |_, _, _, _| ui.toggle_keep(id));
            button.add_controller(click);
            self.strip.append(&button);
            self.load(&quality.path, video, 160, &thumb, false);
        }
    }

    fn load(&self, path: &str, video: bool, dim: i32, target: &gtk::Picture, preview: bool) {
        let rx = if preview {
            self.engine
                .borrow()
                .request_preview(path.to_owned(), dim, video)
        } else {
            self.engine.borrow().request_tile(
                path.to_owned(),
                dim,
                video,
                self.tile_token.borrow().clone(),
            )
        };
        let target = target.downgrade();
        glib::MainContext::default().spawn_local(async move {
            if let (Ok(ThumbReply::Image(decoded)), Some(target)) =
                (rx.recv().await, target.upgrade())
            {
                target.set_paintable(Some(&texture_from_decoded(decoded)));
            }
        });
    }

    fn focus(self: &Rc<Self>, primary_file_id: i64) {
        self.focused.set(primary_file_id);
        if let Some(group) = self.current.borrow().clone() {
            self.refresh(&group);
        }
    }

    fn toggle_keep(self: &Rc<Self>, primary_file_id: i64) {
        model::toggle(&mut self.keep.borrow_mut(), primary_file_id);
        if let Some(group) = self.current.borrow().clone() {
            self.refresh(&group);
        }
    }

    fn refresh(self: &Rc<Self>, group: &CatalogTakeAutoGroup) {
        let qualities = self.qualities.borrow().clone();
        let keep = self.keep.borrow().clone();
        let (label, reason) = model::trash_button(group, &qualities, &keep);
        self.trash_button.set_label(&label);
        self.trash_button
            .set_sensitive(reason.is_none() && !self.trashing.get());
        self.trash_button.set_tooltip_text(reason);
        self.strip_header
            .set_text(&model::filmstrip_header(group, &keep, qualities.len()));
        let video = group.kind != "photos";
        if let Some((best_index, other_index)) = model::compare_pair(&qualities, self.focused.get())
        {
            let best = &qualities[best_index];
            let other = &qualities[other_index];
            self.best_panel
                .set_title(&format!("BEST · {} of {}", best_index + 1, qualities.len()));
            self.other_panel.set_title(&format!(
                "{} · {} of {}",
                if other.rank == 2 && self.focused.get() == best.primary_file_id {
                    "RUNNER-UP"
                } else {
                    "THIS TRY"
                },
                other_index + 1,
                qualities.len()
            ));
            self.best_panel.trailing.set_visible(true);
            self.load(&best.path, video, 1200, &self.best_picture, true);
            self.load(&other.path, video, 1200, &self.other_picture, true);
            v3::clear(&self.best_panel.footer);
            v3::clear(&self.other_panel.footer);
            for (panel, quality) in [(&self.best_panel, best), (&self.other_panel, other)] {
                let reasons = if quality.reasons.is_empty() {
                    "No clear differences.".to_owned()
                } else {
                    quality.reasons.join(" · ")
                };
                panel
                    .show_footer()
                    .append(&v3::wrapped(&reasons, &["v3-meta"]));
                let mark = v3::ghost(if keep.contains(&quality.primary_file_id) {
                    "Keeping ✓"
                } else {
                    "Keep this one"
                });
                let ui = self.clone();
                let id = quality.primary_file_id;
                mark.connect_clicked(move |_| ui.toggle_keep(id));
                panel.show_footer().append(&mark);
            }
        }
    }

    fn confirm_trash(self: &Rc<Self>) {
        let Some(group) = self.current.borrow().clone() else {
            return;
        };
        let qualities = self.qualities.borrow().clone();
        let keep = self.keep.borrow().clone();
        if model::trash_ids(&qualities, &keep).is_empty() {
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
        let ui = self.clone();
        dialog.connect_response(None, move |_, response| {
            if response == "trash" {
                ui.trash();
            }
        });
        dialog.present(Some(&self.root));
    }

    fn trash(self: &Rc<Self>) {
        let ids = model::trash_ids(&self.qualities.borrow(), &self.keep.borrow());
        if ids.is_empty() || self.trashing.replace(true) {
            return;
        }
        self.trash_button.set_sensitive(false);
        self.keep_all.set_sensitive(false);
        self.skip.set_sensitive(false);
        self.action_bar.set_visible(true);
        self.set_notice("Moving to Trash…", false);
        let sent = self
            .engine
            .borrow_mut()
            .send(CommandPayload::TrashFiles(TrashFilesPayload {
                file_ids: ids,
                exact_identities: None,
            }));
        if let Err(error) = sent {
            self.finish_trash_failed(&format!("Could not send the Trash command: {error}"));
        }
    }

    fn finish_trash_failed(&self, message: &str) {
        self.trashing.set(false);
        self.action_bar.set_visible(false);
        self.trash_button.set_sensitive(true);
        self.keep_all.set_sensitive(true);
        self.skip.set_sensitive(true);
        self.set_notice(message, false);
    }

    fn finish_trash(self: &Rc<Self>, result: BulkActionResult) {
        if !self.trashing.replace(false) {
            return;
        }
        self.action_bar.set_visible(false);
        self.keep_all.set_sensitive(true);
        if result.succeeded == 0 {
            self.skip.set_sensitive(true);
            self.trash_button.set_sensitive(true);
            let reason = result
                .messages
                .iter()
                .find_map(|item| item.message.clone())
                .unwrap_or_else(|| "Nothing was moved.".to_owned());
            self.set_notice(&format!("Couldn't move to Trash. {reason}"), false);
            return;
        }
        *self.last_batch.borrow_mut() = result
            .action
            .split_once(':')
            .map(|(_, batch)| batch.to_owned());
        *self.undo_decision.borrow_mut() = None;
        let failed = if result.failed > 0 {
            format!(" · {} couldn't be moved", result.failed)
        } else {
            String::new()
        };
        self.set_notice(
            &format!("Moved {} to Trash{failed}", result.succeeded),
            self.last_batch.borrow().is_some(),
        );
        self.request_groups();
    }

    fn decide(self: &Rc<Self>, kind: &str, message: &str) {
        let Some(group) = self.current.borrow().clone() else {
            return;
        };
        *self.undo_decision.borrow_mut() = Some(model::decision(&group, "reopen"));
        *self.last_batch.borrow_mut() = None;
        let mut request = catalog_request("decideTakeGroup");
        request.take_decision = Some(model::decision(&group, kind));
        self.send("decision", request);
        self.set_notice(message, true);
    }

    fn undo_last(self: &Rc<Self>) {
        if let Some(decision) = self.undo_decision.borrow_mut().take() {
            let mut request = catalog_request("decideTakeGroup");
            request.take_decision = Some(decision);
            self.send("decision", request);
            self.set_notice("Put back in the list.", false);
            return;
        }
        if let Some(batch_id) = self.last_batch.borrow_mut().take() {
            self.set_notice("Putting the files back…", false);
            let sent = self
                .engine
                .borrow_mut()
                .send(CommandPayload::RestoreFromTrash(RestoreFromTrashPayload {
                    batch_id,
                }));
            if let Err(error) = sent {
                self.set_notice(
                    &format!("Could not send the restore command: {error}"),
                    false,
                );
            }
        }
    }
}

fn date_text(seconds: f64) -> String {
    glib::DateTime::from_unix_local(seconds as i64)
        .ok()
        .and_then(|date| date.format("%a %-d %b, %-I:%M %p").ok())
        .map_or_else(String::new, |text| text.to_string())
}

fn subscribe(ui: &Rc<Ui>) {
    let rx = ui.engine.borrow_mut().subscribe();
    let ui = ui.clone();
    glib::MainContext::default().spawn_local(async move {
        while let Ok(event) = rx.recv().await {
            match event {
                EngineState::CatalogResponse(r) => {
                    let Some(slot) = ui.requests.borrow_mut().accept(&r.request_id, true) else { continue };
                    match slot.as_str() {
                        "groups" if r.status == "ok" => ui.render_groups(r.take_groups.unwrap_or_default()),
                        "groups" => {
                            ui.groups_bar.set_visible(false);
                            ui.set_notice(r.message.as_deref().unwrap_or("Couldn't look for groups."), false);
                        }
                        "score" if r.status == "ok" => ui.render_review(r.take_qualities.unwrap_or_default()),
                        "score" => {
                            ui.failed_text.set_text(r.message.as_deref().unwrap_or("The engine could not score these files."));
                            ui.stack.set_visible_child_name("failed");
                        }
                        "decision" => ui.request_groups(),
                        _ => {}
                    }
                }
                EngineState::BulkActionResult(result) if result.action.starts_with("trashFiles") => ui.finish_trash(result),
                EngineState::BulkActionResult(result) if result.action == "restoreFromTrash" => {
                    ui.set_notice(&format!("Put back {} file{}", result.succeeded, if result.succeeded == 1 { "" } else { "s" }), false);
                    ui.request_groups();
                }
                EngineState::Ready | EngineState::ScanComplete(_) => ui.request_groups(),
                EngineState::Failed(message) => {
                    ui.requests.borrow_mut().clear();
                    ui.groups_bar.set_visible(false);
                    ui.trashing.set(false);
                    ui.action_bar.set_visible(false);
                    ui.set_notice(&message, false);
                }
                EngineState::Exited => {
                    ui.requests.borrow_mut().clear();
                    ui.groups_bar.set_visible(false);
                    if ui.trashing.replace(false) {
                        ui.action_bar.set_visible(false);
                        ui.set_notice("FileID restarted before the move to Trash finished. Check Trash, then try again.", false);
                    }
                }
                _ => {}
            }
        }
    });
}
