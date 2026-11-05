// Manual Best Takes groups (DESIGN_V3_PORT "Best Takes", manual groups; macOS
// TakeWorkbench): YOUR GROUPS, MAKE A GROUP BY HAND with clip search and Find
// related, the group editor (Ctrl+S saves), the scoring job, the
// recommendation and the take cards with Met / Missed / Unclear / Prefer /
// Undo review. Lives inside the Best Takes page (takes.rs).

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::rc::Rc;

use adw::prelude::*;
use fileid_engine::ipc::*;
use gtk::glib;

use super::takes::Ui;
use super::takes_groups_model as model;
use super::takes_groups_model::Outcome;
use super::takes_widgets::ShotView;
use super::workbench_state::{catalog_request, request_id};
use crate::engine_client::CancelToken;
use crate::shell::status::Tone;
use crate::v3;

const CARD_PX: i32 = 480;

struct Card {
    file_id: i64,
    well: gtk::Stack,
    video: gtk::Video,
}

pub(super) struct Manual {
    candidates: Vec<CatalogHit>,
    events: RefCell<Vec<CatalogEvent>>,
    selected_event: RefCell<Option<CatalogEvent>>,
    active: Cell<bool>,
    takes: RefCell<Vec<CatalogTake>>,
    recommendation: RefCell<Option<CatalogTakeRecommendation>>,
    suggestions: RefCell<Vec<CatalogTakeGroupSuggestion>>,
    found: RefCell<Vec<CatalogHit>>,
    chosen: RefCell<BTreeSet<i64>>,
    last_deleted: RefCell<Option<String>>,
    action: RefCell<Option<String>>,
    goal_job: RefCell<Option<String>>,
    goal_event: RefCell<Option<String>>,
    goal_requested: RefCell<BTreeSet<i64>>,
    previous_jobs: RefCell<BTreeSet<String>>,
    jobs: RefCell<Vec<CatalogJob>>,
    failure_shown: RefCell<Option<String>>,
    syncing: Cell<bool>,
    clip_ids: RefCell<Vec<i64>>,
    clip_checks: RefCell<Vec<(i64, gtk::CheckButton)>>,
    cards: RefCell<Vec<Card>>,
    card_token: RefCell<CancelToken>,
    owner: RefCell<std::rc::Weak<Ui>>,
    pub(super) pane: gtk::Box,
    pub(super) footer: gtk::Box,
    pub(super) page: gtk::ScrolledWindow,
    events_header: gtk::Label,
    events_list: gtk::Box,
    search: gtk::Entry,
    find_related: gtk::Button,
    new_group: gtk::Button,
    suggestions_box: gtk::Box,
    undo_delete: gtk::Button,
    panel: v3::Panel,
    busy: gtk::ProgressBar,
    undo_edit: gtk::Button,
    delete: gtk::Button,
    title_entry: gtk::Entry,
    goal_entry: gtk::Entry,
    clips: gtk::Box,
    clips_header: gtk::Label,
    choose: gtk::Button,
    clip_rows: gtk::Box,
    save: gtk::Button,
    score: gtk::Button,
    refresh: gtk::Button,
    note: gtk::Label,
    job_box: gtk::Box,
    recommendation_box: gtk::Box,
    grid: gtk::FlowBox,
    intro: gtk::Box,
}

fn soft_hairline() -> gtk::Separator {
    let line = gtk::Separator::new(gtk::Orientation::Horizontal);
    line.add_css_class("v3-soft-line");
    line.set_margin_top(8);
    line.set_margin_bottom(8);
    line
}

fn field_row(label: &str, field: &gtk::Entry) -> gtk::Box {
    let row = v3::hbox(12);
    let caption = gtk::Label::builder()
        .label(label)
        .xalign(0.0)
        .width_request(44)
        .accessible_role(gtk::AccessibleRole::Presentation)
        .css_classes(["v3-output-label"])
        .build();
    row.append(&caption);
    field.update_property(&[gtk::accessible::Property::Label(label)]);
    row.append(field);
    row
}

impl Manual {
    pub(super) fn new(candidates: Vec<CatalogHit>) -> Self {
        // ── Pane sections (below POSSIBLE GROUPS) ──────────────────────────
        let pane = v3::vbox(2);
        pane.append(&soft_hairline());
        let events_header = v3::section("YOUR GROUPS · 0");
        events_header.set_margin_start(14);
        events_header.set_margin_end(14);
        events_header.set_margin_bottom(4);
        pane.append(&events_header);
        let events_list = v3::vbox(2);
        pane.append(&events_list);
        pane.append(&soft_hairline());
        let make = v3::vbox(10);
        make.set_margin_start(14);
        make.set_margin_end(14);
        make.set_margin_bottom(10);
        make.append(&v3::section("Make a group by hand"));
        let search = gtk::Entry::builder()
            .placeholder_text("Search your videos")
            .primary_icon_name("system-search-symbolic")
            .css_classes(["v3-field"])
            .build();
        search.update_property(&[gtk::accessible::Property::Label("Search your videos")]);
        make.append(&search);
        let buttons = v3::hbox(6);
        let find_related = v3::secondary("Find related", false);
        buttons.append(&find_related);
        let new_group = v3::secondary("+ New group", false);
        buttons.append(&new_group);
        make.append(&buttons);
        let suggestions_box = v3::vbox(8);
        make.append(&suggestions_box);
        pane.append(&make);

        let footer = v3::hbox(8);
        footer.add_css_class("v3-pane-footer");
        let deleted = v3::meta("Group deleted");
        deleted.set_hexpand(true);
        footer.append(&deleted);
        let undo_delete = v3::accent("Undo", Tone::Gold, false);
        footer.append(&undo_delete);
        footer.set_visible(false);

        // ── Page: group panel, scoring job, recommendation, take cards ────
        let body = v3::vbox(16);
        body.add_css_class("v3-page-pad");
        let panel = v3::panel("New group");
        let busy = v3::busy_bar(60);
        panel.trailing.append(&busy);
        let undo_edit = v3::ghost("Undo edit");
        panel.trailing.append(&undo_edit);
        let delete = v3::ghost("Delete");
        delete.add_css_class("tone-danger");
        panel.trailing.append(&delete);
        let fields = v3::vbox(10);
        fields.add_css_class("v3-panel-pad");
        let title_entry = v3::text_field("Group name, e.g. Alex at bat");
        title_entry.add_css_class("h28");
        let goal_entry = v3::text_field("What should happen, e.g. Alex gets a hit");
        goal_entry.add_css_class("h28");
        fields.append(&field_row("Name", &title_entry));
        fields.append(&field_row("Goal", &goal_entry));
        panel.body.append(&fields);
        let clips = v3::vbox(0);
        clips.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        let clips_head = v3::hbox(8);
        clips_head.set_margin_start(14);
        clips_head.set_margin_end(14);
        clips_head.set_margin_top(6);
        let clips_header = v3::section("Clips · 0 of 0 chosen");
        clips_header.set_hexpand(true);
        clips_head.append(&clips_header);
        let choose = v3::ghost("Choose all");
        clips_head.append(&choose);
        clips.append(&clips_head);
        let clip_rows = v3::vbox(0);
        clip_rows.set_margin_bottom(6);
        clips.append(&clip_rows);
        panel.body.append(&clips);
        let footer_row = panel.show_footer();
        let save = v3::primary("Save group", true);
        footer_row.append(&save);
        let score = v3::secondary("Score takes", false);
        score.set_tooltip_text(Some(
            "Check each clip against the goal. Uses the description model from Describe Files.",
        ));
        footer_row.append(&score);
        let refresh = v3::ghost("Refresh scores");
        footer_row.append(&refresh);
        footer_row.append(&v3::spacer());
        let note = v3::label("", &["v3-meta-115"]);
        note.set_wrap(true);
        note.set_lines(2);
        note.set_ellipsize(gtk::pango::EllipsizeMode::End);
        footer_row.append(&note);
        body.append(&panel.root);
        let job_box = v3::vbox(0);
        body.append(&job_box);
        let recommendation_box = v3::vbox(0);
        body.append(&recommendation_box);
        let grid = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .homogeneous(true)
            .min_children_per_line(1)
            .max_children_per_line(4)
            .column_spacing(12)
            .row_spacing(12)
            .valign(gtk::Align::Start)
            .build();
        body.append(&grid);
        let intro = v3::dashed_notice(
            "Make a group by hand",
            "1. Search for your clips on the left.  2. Tick two or more tries of the same shot and say what should happen.  3. Save, then score each try against that goal.",
        );
        body.append(&intro);
        let page = v3::scroller(&body);

        Self {
            candidates,
            events: RefCell::new(vec![]),
            selected_event: RefCell::new(None),
            active: Cell::new(false),
            takes: RefCell::new(vec![]),
            recommendation: RefCell::new(None),
            suggestions: RefCell::new(vec![]),
            found: RefCell::new(vec![]),
            chosen: RefCell::new(BTreeSet::new()),
            last_deleted: RefCell::new(None),
            action: RefCell::new(None),
            goal_job: RefCell::new(None),
            goal_event: RefCell::new(None),
            goal_requested: RefCell::new(BTreeSet::new()),
            previous_jobs: RefCell::new(BTreeSet::new()),
            jobs: RefCell::new(vec![]),
            failure_shown: RefCell::new(None),
            syncing: Cell::new(false),
            clip_ids: RefCell::new(vec![]),
            clip_checks: RefCell::new(vec![]),
            cards: RefCell::new(vec![]),
            card_token: RefCell::new(CancelToken::default()),
            owner: RefCell::new(std::rc::Weak::new()),
            pane,
            footer,
            page,
            events_header,
            events_list,
            search,
            find_related,
            new_group,
            suggestions_box,
            undo_delete,
            panel,
            busy,
            undo_edit,
            delete,
            title_entry,
            goal_entry,
            clips,
            clips_header,
            choose,
            clip_rows,
            save,
            score,
            refresh,
            note,
            job_box,
            recommendation_box,
            grid,
            intro,
        }
    }

    /// True while the page shows the manual editor rather than a possible group.
    pub(super) fn engaged(&self) -> bool {
        self.active.get() || self.selected_event.borrow().is_some()
    }

    fn pool(&self) -> Vec<CatalogHit> {
        model::pool(&self.candidates, &self.found.borrow())
    }

    fn open_event_id(&self) -> Option<String> {
        self.selected_event.borrow().as_ref().map(|e| e.id.clone())
    }

    fn goal_job_record(&self) -> Option<CatalogJob> {
        let id = self.goal_job.borrow().clone()?;
        if *self.goal_event.borrow() != self.open_event_id() {
            return None;
        }
        self.jobs.borrow().iter().find(|job| job.id == id).cloned()
    }

    pub(super) fn send(&self, ui: &Rc<Ui>, action: &str, fill: impl FnOnce(&mut CatalogRequest)) {
        let mut request = catalog_request(action);
        fill(&mut request);
        *self.action.borrow_mut() = Some(action.to_owned());
        if !ui.send("manual", request) {
            self.action.borrow_mut().take();
        }
        self.render_editor();
    }

    /// Leaves the manual editor because a possible group was opened.
    pub(super) fn leave(&self) {
        self.active.set(false);
        self.selected_event.borrow_mut().take();
        self.stop_cards();
        self.render_events_selection(false);
    }

    fn start_manual(&self, ui: &Rc<Ui>) {
        ui.leave_auto();
        self.active.set(true);
        self.show(ui);
    }

    pub(super) fn show(&self, ui: &Rc<Ui>) {
        if self.engaged() {
            self.render_editor();
            ui.stack.set_visible_child_name("manual");
        } else {
            ui.show_idle();
        }
        self.render_events_selection(ui.current.borrow().is_some());
    }

    fn new_group_state(&self) {
        self.selected_event.borrow_mut().take();
        self.takes.borrow_mut().clear();
        self.recommendation.borrow_mut().take();
        self.chosen.borrow_mut().clear();
        self.syncing.set(true);
        self.title_entry.set_text("");
        self.goal_entry.set_text("");
        self.syncing.set(false);
        self.stop_cards();
        self.render_takes(None);
    }

    fn open(&self, ui: &Rc<Ui>, event: CatalogEvent) {
        ui.leave_auto();
        self.active.set(false);
        self.last_deleted.borrow_mut().take();
        self.footer.set_visible(false);
        self.adopt(&event);
        self.stop_cards();
        let id = event.id.clone();
        self.send(ui, "takeGroup", |r| r.event_id = Some(id));
        self.show(ui);
    }

    fn adopt(&self, event: &CatalogEvent) {
        *self.chosen.borrow_mut() = event.file_ids.iter().copied().collect();
        *self.selected_event.borrow_mut() = Some(event.clone());
        self.syncing.set(true);
        self.title_entry.set_text(&event.title);
        self.goal_entry.set_text(&event.goal);
        self.syncing.set(false);
        self.sync_checks();
    }

    fn save(&self, ui: &Rc<Ui>) {
        let (title, goal) = (self.title_entry.text(), self.goal_entry.text());
        if !model::can_save(&title, &goal, self.chosen.borrow().len()) {
            return;
        }
        self.last_deleted.borrow_mut().take();
        self.footer.set_visible(false);
        let event = model::event_to_save(
            self.selected_event.borrow().as_ref(),
            request_id(),
            &title,
            &goal,
            &self.chosen.borrow(),
        );
        self.send(ui, "saveEvent", |r| r.event = Some(event));
    }

    fn score_takes(&self, ui: &Rc<Ui>) {
        let Some(event) = self.selected_event.borrow().clone() else {
            return;
        };
        *self.goal_event.borrow_mut() = Some(event.id.clone());
        *self.goal_requested.borrow_mut() = event.file_ids.iter().copied().collect();
        *self.previous_jobs.borrow_mut() =
            self.jobs.borrow().iter().map(|j| j.id.clone()).collect();
        self.send(ui, "enqueueTimeline", |r| {
            r.file_ids = Some(event.file_ids.clone());
            r.timeline_mode = Some("moments".into());
        });
    }

    fn feedback(&self, ui: &Rc<Ui>, feedback: CatalogTakeFeedback) {
        self.send(ui, "setTakeFeedback", |r| r.take_feedback = Some(feedback));
    }

    pub(super) fn wants_jobs(&self) -> bool {
        self.goal_job.borrow().is_some() || self.selected_event.borrow().is_some()
    }

    /// One catalog reply for the manual flow (macOS `consume`).
    pub(super) fn consume(&self, ui: &Rc<Ui>, response: CatalogResponse) {
        let action = self.action.borrow_mut().take().unwrap_or_default();
        let ok = response.status == "ok";
        let message = response.message.clone().unwrap_or_else(|| {
            if ok {
                String::new()
            } else {
                "The request failed.".into()
            }
        });
        if !message.is_empty() {
            ui.toast(&message);
        }
        if !ok {
            if action == "deleteEvent" {
                self.last_deleted.borrow_mut().take();
                self.footer.set_visible(false);
            }
            self.render_editor();
            return;
        }
        match action.as_str() {
            "search" => {
                let found = model::found_videos(&response.hits, &self.candidates);
                if found.is_empty() {
                    ui.toast(&format!(
                        "No videos matched “{}”",
                        self.search.text().trim()
                    ));
                }
                *self.found.borrow_mut() = found;
                self.render_editor();
                return;
            }
            "suggestTakeGroups" => {
                let groups = response.suggested_take_groups.clone().unwrap_or_default();
                if groups.is_empty() && message.is_empty() {
                    ui.toast("No related takes found in these clips");
                }
                *self.suggestions.borrow_mut() = groups;
                self.render_suggestions(ui);
            }
            "enqueueTimeline" => {
                let job = model::new_goal_job(
                    &response.jobs,
                    &self.goal_requested.borrow(),
                    &self.previous_jobs.borrow(),
                )
                .map(|job| job.id.clone());
                *self.goal_job.borrow_mut() = job;
                *self.jobs.borrow_mut() = response.jobs.clone();
                self.refresh_goal_job(ui);
            }
            _ => {}
        }
        if let Some(events) = response.events.clone() {
            if model::replies_with_open_group(&action) {
                if let Some(event) = events.first() {
                    self.adopt(event);
                }
            } else {
                *self.events.borrow_mut() = events;
                if action == "deleteEvent" {
                    self.new_group_state();
                    self.show(ui);
                }
                self.render_events(ui);
            }
        }
        if let Some(takes) = response.takes.clone() {
            *self.takes.borrow_mut() = takes;
            *self.recommendation.borrow_mut() = response.recommendation.clone();
            self.render_takes(Some(ui));
        }
        if model::relists(&action) {
            if action == "undoEventEdit" {
                self.last_deleted.borrow_mut().take();
                self.footer.set_visible(false);
            }
            self.send(ui, "listEvents", |_| {});
        }
        self.render_editor();
        self.render_events_selection(ui.current.borrow().is_some());
    }

    pub(super) fn request_failed(&self) {
        self.action.borrow_mut().take();
        self.render_editor();
    }

    pub(super) fn set_jobs(&self, ui: &Rc<Ui>, jobs: Vec<CatalogJob>) {
        *self.jobs.borrow_mut() = jobs;
        self.refresh_goal_job(ui);
    }

    fn refresh_goal_job(&self, ui: &Rc<Ui>) {
        self.render_job(ui);
        let Some(job) = self.goal_job_record() else {
            return;
        };
        let idle = self.action.borrow().is_none();
        if job.state == "completed" && idle {
            self.goal_job.borrow_mut().take();
            if let Some(id) = self.goal_event.borrow().clone() {
                self.send(ui, "takeGroup", |r| r.event_id = Some(id));
            }
            self.render_job(ui);
        } else if job.state == "failed"
            && self.failure_shown.replace(Some(job.id.clone())).as_deref() != Some(job.id.as_str())
        {
            ui.toast(
                job.error
                    .as_deref()
                    .unwrap_or("Scoring didn't finish. Scores you already reviewed are kept."),
            );
        }
        self.render_editor();
    }

    // ── Rendering ─────────────────────────────────────────────────────────

    fn render_events(&self, ui: &Rc<Ui>) {
        v3::clear(&self.events_list);
        let events = self.events.borrow().clone();
        self.events_header
            .set_text(&format!("YOUR GROUPS · {}", events.len()));
        for event in events {
            let text = v3::vbox(3);
            let title = v3::label(&event.title, &["v3-title-13"]);
            title.set_ellipsize(gtk::pango::EllipsizeMode::End);
            text.append(&title);
            let meta = v3::wrapped(&model::event_meta(&event), &["v3-meta-115"]);
            meta.set_lines(2);
            meta.set_ellipsize(gtk::pango::EllipsizeMode::End);
            text.append(&meta);
            let row = v3::rail_row(&text);
            row.set_widget_name(&event.id);
            row.update_property(&[gtk::accessible::Property::Label(&model::event_spoken(
                &event,
            ))]);
            let weak = Rc::downgrade(ui);
            row.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.manual.open(&ui, event.clone());
                }
            });
            self.events_list.append(&row);
        }
        if self.events_list.first_child().is_none() {
            let empty = v3::wrapped(
                "Groups you make by hand, with a goal, show up here.",
                &["v3-meta"],
            );
            empty.set_margin_start(14);
            empty.set_margin_end(14);
            self.events_list.append(&empty);
        }
        self.render_events_selection(ui.current.borrow().is_some());
    }

    fn render_events_selection(&self, auto_open: bool) {
        let open = self.open_event_id();
        let mut child = self.events_list.first_child();
        while let Some(widget) = child {
            let on = !auto_open && open.as_deref() == Some(widget.widget_name().as_str());
            v3::set_selected(&widget, on);
            child = widget.next_sibling();
        }
    }

    fn render_suggestions(&self, ui: &Rc<Ui>) {
        v3::clear(&self.suggestions_box);
        for suggestion in self.suggestions.borrow().iter().cloned() {
            let card = v3::vbox(6);
            card.add_css_class("v3-suggestion-card");
            let names = model::suggestion_names(&suggestion);
            let list = v3::wrapped(&names.join(", "), &["v3-mono", "s115"]);
            list.set_lines(2);
            list.set_ellipsize(gtk::pango::EllipsizeMode::End);
            card.append(&list);
            let row = v3::hbox(8);
            let meta = v3::label(
                &model::suggestion_meta(&suggestion),
                &["v3-meta-115", "tone-ai"],
            );
            meta.set_hexpand(true);
            row.append(&meta);
            let review = v3::accent("Review", Tone::Ai, false);
            review.set_tooltip_text(Some(&suggestion.reason));
            row.append(&review);
            card.append(&row);
            card.update_property(&[gtk::accessible::Property::Label(&format!(
                "Possible group: {}",
                names.join(", ")
            ))]);
            let weak = Rc::downgrade(ui);
            review.connect_clicked(move |_| {
                let Some(ui) = weak.upgrade() else { return };
                let manual = &ui.manual;
                manual.start_manual(&ui);
                manual.new_group_state();
                manual
                    .found
                    .borrow_mut()
                    .extend(model::suggestion_hits(&suggestion));
                *manual.chosen.borrow_mut() =
                    suggestion.members.iter().map(|m| m.file_id).collect();
                manual.render_editor();
                ui.toast("Name the group and describe the outcome, then save");
            });
            self.suggestions_box.append(&card);
        }
    }

    fn render_editor(&self) {
        let event = self.selected_event.borrow().clone();
        let busy = self.action.borrow().is_some();
        self.panel.set_title(if event.is_some() {
            "Group"
        } else {
            "New group"
        });
        self.busy.set_visible(busy);
        self.undo_edit.set_visible(event.is_some());
        self.delete.set_visible(event.is_some());
        let pool = self.pool();
        self.find_related.set_sensitive(pool.len() >= 2);
        self.find_related.set_tooltip_text(Some(if pool.len() < 2 {
            "Search for at least two videos first"
        } else {
            "Suggest groups of clips that look alike and were filmed close together"
        }));
        let ids: Vec<i64> = pool.iter().map(|hit| hit.file_id).collect();
        if *self.clip_ids.borrow() != ids {
            self.rebuild_clips(&pool);
            *self.clip_ids.borrow_mut() = ids;
        }
        self.clips.set_visible(!pool.is_empty());
        let chosen = self.chosen.borrow().clone();
        self.clips_header
            .set_text(&model::clips_header(chosen.len(), pool.len()));
        self.choose.set_label(model::choose_label(&chosen, &pool));
        self.sync_checks();
        let can_save = model::can_save(
            &self.title_entry.text(),
            &self.goal_entry.text(),
            chosen.len(),
        );
        self.save.set_sensitive(can_save);
        self.save
            .set_tooltip_text(Some(model::save_tooltip(can_save)));
        self.score.set_visible(event.is_some());
        self.refresh.set_visible(event.is_some());
        if let Some(event) = &event {
            let job = self.goal_job_record();
            self.score.set_sensitive(model::can_score(
                event,
                &self.goal_entry.text(),
                &chosen,
                &self.takes.borrow(),
                busy,
                job.as_ref(),
            ));
        }
        self.note.set_text(model::editor_note(pool.is_empty()));
        self.intro
            .set_visible(self.takes.borrow().is_empty() && event.is_none());
    }

    fn rebuild_clips(&self, pool: &[CatalogHit]) {
        v3::clear(&self.clip_rows);
        let mut checks = Vec::new();
        for hit in pool {
            let name = v3::mono(&v3::file_name(&hit.path), "s12");
            let check = gtk::CheckButton::builder().child(&name).build();
            check.set_margin_start(14);
            check.set_margin_end(14);
            check.set_tooltip_text(Some(&hit.path));
            check.update_property(&[gtk::accessible::Property::Label(&v3::file_name(&hit.path))]);
            let weak = self.owner.borrow().clone();
            let file_id = hit.file_id;
            check.connect_toggled(move |check| {
                let Some(ui) = weak.upgrade() else { return };
                if ui.manual.syncing.get() {
                    return;
                }
                if check.is_active() {
                    ui.manual.chosen.borrow_mut().insert(file_id);
                } else {
                    ui.manual.chosen.borrow_mut().remove(&file_id);
                }
                ui.manual.render_editor();
            });
            self.clip_rows.append(&check);
            checks.push((hit.file_id, check));
        }
        *self.clip_checks.borrow_mut() = checks;
    }

    fn sync_checks(&self) {
        let chosen = self.chosen.borrow().clone();
        self.syncing.set(true);
        for (file_id, check) in self.clip_checks.borrow().iter() {
            check.set_active(chosen.contains(file_id));
        }
        self.syncing.set(false);
    }

    fn render_job(&self, ui: &Rc<Ui>) {
        v3::clear(&self.job_box);
        let Some(job) = self.goal_job_record() else {
            return;
        };
        let panel = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["v3-panel"])
            .build();
        let row = v3::vbox(6);
        row.add_css_class("v3-job-row");
        let head = v3::hbox(8);
        let title = v3::label(super::moments::job_title(&job.kind), &["v3-title-13"]);
        title.set_hexpand(true);
        head.append(&title);
        let (state, tone) = super::moments::job_state(&job.state);
        head.append(&v3::label(state, &["v3-state", tone.css()]));
        head.append(&v3::mono(
            &format!("{:.0}%", job.progress.clamp(0.0, 1.0) * 100.0),
            "s115",
        ));
        row.append(&head);
        if let Some(error) = &job.error {
            row.append(&v3::wrapped(error, &["v3-meta"]));
        }
        let bar = crate::shell::GrainBar::new(true);
        bar.set(job.progress, tone);
        row.append(&bar.bar);
        let controls = v3::hbox(6);
        for (action, name) in super::workbenches::job_controls(&job) {
            let button = match name {
                "Resume" => v3::accent(name, Tone::Gold, false),
                "Cancel" => v3::ghost(name),
                _ => v3::secondary(name, false),
            };
            let weak = Rc::downgrade(ui);
            let job = job.clone();
            button.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.send("job-control", super::workbenches::job_request(action, &job));
                }
            });
            controls.append(&button);
        }
        if controls.first_child().is_some() {
            row.append(&controls);
        }
        panel.append(&row);
        self.job_box.append(&panel);
    }

    fn render_takes(&self, ui: Option<&Rc<Ui>>) {
        self.stop_cards();
        self.card_token
            .borrow()
            .store(true, std::sync::atomic::Ordering::Release);
        *self.card_token.borrow_mut() = CancelToken::default();
        v3::clear(&self.recommendation_box);
        self.grid.remove_all();
        self.cards.borrow_mut().clear();
        let Some(ui) = ui else {
            return;
        };
        let recommendation = self.recommendation.borrow().clone();
        if let Some(recommendation) = &recommendation {
            self.recommendation_box
                .append(&recommendation_row(recommendation));
        }
        let event_id = self.open_event_id();
        let mut jobs = Vec::new();
        for (index, take) in self.takes.borrow().iter().enumerate() {
            let recommended = model::is_recommended(recommendation.as_ref(), take);
            let (card, view, well, video) =
                take_card(ui, index + 1, take, recommended, event_id.as_deref());
            self.grid.append(&card);
            if let Some(child) = self.grid.last_child() {
                child.set_focusable(false);
            }
            jobs.push((
                take.path.clone(),
                model::is_video_path(&take.path),
                CARD_PX,
                view,
            ));
            self.cards.borrow_mut().push(Card {
                file_id: take.file_id,
                well,
                video,
            });
        }
        super::takes::load_in_order(&ui.engine, jobs, self.card_token.borrow().clone());
    }

    fn play(&self, ui: &Rc<Ui>, take: &CatalogTake) {
        if !super::workbenches::is_playable_media(&take.path) {
            v3::open_file(&ui.root, &take.path);
            return;
        }
        self.stop_cards();
        let cards = self.cards.borrow();
        let Some(card) = cards.iter().find(|card| card.file_id == take.file_id) else {
            return;
        };
        let media = gtk::MediaFile::for_filename(&take.path);
        card.video.set_media_stream(Some(&media));
        card.well.set_visible_child_name("video");
        media.play();
    }

    pub(super) fn stop_cards(&self) {
        for card in self.cards.borrow().iter() {
            if let Some(stream) = card.video.media_stream() {
                stream.pause();
            }
            card.video.set_media_stream(None::<&gtk::MediaStream>);
            card.well.set_visible_child_name("image");
        }
    }
}

fn recommendation_row(recommendation: &CatalogTakeRecommendation) -> gtk::Box {
    let tone = model::recommendation_tone(recommendation);
    let row = v3::hbox(12);
    row.add_css_class("v3-panel");
    row.add_css_class("v3-rec-rail");
    row.add_css_class(tone.css());
    let text = v3::vbox(4);
    text.set_margin_top(12);
    text.set_margin_bottom(12);
    text.set_margin_start(14);
    text.set_margin_end(14);
    let header = v3::section(&model::recommendation_header(recommendation));
    header.add_css_class(tone.css());
    text.append(&header);
    text.append(&v3::wrapped(&recommendation.reason, &["v3-text-13"]));
    row.append(&text);
    row.update_property(&[gtk::accessible::Property::Label(&format!(
        "{}. {}",
        model::recommendation_header(recommendation),
        recommendation.reason
    ))]);
    row
}

fn take_card(
    ui: &Rc<Ui>,
    index: usize,
    take: &CatalogTake,
    recommended: bool,
    event_id: Option<&str>,
) -> (gtk::Box, ShotView, gtk::Stack, gtk::Video) {
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .accessible_role(gtk::AccessibleRole::Group)
        .css_classes(["v3-panel"])
        .overflow(gtk::Overflow::Hidden)
        .width_request(260)
        .build();
    if recommended {
        card.add_css_class("v3-take-best");
    }
    card.update_property(&[gtk::accessible::Property::Label(&model::card_spoken(
        index,
        take,
        recommended,
    ))]);
    let header = v3::hbox(8);
    header.add_css_class("v3-take-head");
    let title = v3::section(&format!("Take {index}"));
    if recommended {
        title.add_css_class("tone-gold");
    }
    header.append(&title);
    if recommended {
        let star = v3::label("★", &["v3-take-star"]);
        star.update_property(&[gtk::accessible::Property::Label("Recommended")]);
        header.append(&star);
    }
    header.append(&v3::spacer());
    let (tone, tooltip, spoken) = model::review_dot(take);
    let dot = v3::dot(tone.css());
    dot.set_tooltip_text(Some(tooltip));
    dot.update_property(&[gtk::accessible::Property::Label(spoken)]);
    header.append(&dot);
    card.append(&header);

    let view = ShotView::new(160, 90, 0.0, false, false);
    view.area.set_hexpand(true);
    let video = gtk::Video::builder().autoplay(false).build();
    let well = gtk::Stack::new();
    well.add_named(&view.area, Some("image"));
    well.add_named(&video, Some("video"));
    let play = gtk::Button::builder()
        .icon_name("media-playback-start-symbolic")
        .css_classes(["v3-play-circle"])
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .tooltip_text(format!("Play take {index}"))
        .build();
    play.update_property(&[gtk::accessible::Property::Label(&format!(
        "Play take {index}"
    ))]);
    let image_overlay = gtk::Overlay::builder().child(&well).build();
    image_overlay.add_overlay(&play);
    let frame = gtk::AspectFrame::builder()
        .ratio(16.0 / 9.0)
        .obey_child(false)
        .child(&image_overlay)
        .css_classes(["v3-take-well"])
        .build();
    card.append(&frame);
    {
        let weak = Rc::downgrade(ui);
        let take = take.clone();
        let button = play.clone();
        play.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.manual.play(&ui, &take);
                button.set_visible(false);
            }
        });
    }

    let body = v3::vbox(8);
    body.add_css_class("v3-take-body");
    let name = v3::mono(&v3::file_name(&take.path), "s12");
    name.add_css_class("v3-medium");
    name.set_tooltip_text(Some(&take.path));
    body.append(&name);
    let (headline, tone) = model::headline(take);
    body.append(&v3::label(
        &headline,
        &["v3-title-13", "v3-semibold", tone.css()],
    ));
    if let Some(why) = take.explanation.as_deref().filter(|why| !why.is_empty()) {
        let why = v3::wrapped(why, &["v3-meta"]);
        why.set_lines(3);
        why.set_ellipsize(gtk::pango::EllipsizeMode::End);
        body.append(&why);
    }
    if let Some(quality) = take.quality_score {
        let row = v3::hbox(8);
        let label = v3::label("Sharpness", &["v3-meta-115", "v3-medium"]);
        label.set_width_request(64);
        row.append(&label);
        let bar = crate::shell::GrainBar::new(true);
        bar.bar.set_hexpand(true);
        bar.set(quality, Tone::Muted);
        row.append(&bar.bar);
        let value = v3::mono(&model::percent(quality).to_string(), "s11");
        value.add_css_class("tone-muted");
        value.set_width_request(26);
        value.set_xalign(1.0);
        row.append(&value);
        row.update_property(&[gtk::accessible::Property::Label(&format!(
            "Sharpness {} percent",
            model::percent(quality)
        ))]);
        body.append(&row);
    }
    card.append(&body);

    if let Some(event_id) = event_id {
        let footer = v3::hbox(6);
        footer.add_css_class("v3-take-card-footer");
        let selected = model::selected_outcome(take);
        for outcome in Outcome::ALL {
            let on = selected == Some(outcome);
            let button = if on {
                v3::primary(outcome.label(), true)
            } else {
                v3::secondary(outcome.label(), false)
            };
            button.update_property(&[gtk::accessible::Property::Label(&outcome.spoken())]);
            button.update_state(&[gtk::accessible::State::Selected(Some(on))]);
            let weak = Rc::downgrade(ui);
            let feedback = model::outcome_feedback(event_id, take, outcome);
            button.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.manual.feedback(&ui, feedback.clone());
                }
            });
            footer.append(&button);
        }
        footer.append(&v3::spacer());
        let prefer = if take.preferred {
            v3::accent("★ Prefer", Tone::Gold, false)
        } else {
            v3::secondary("☆ Prefer", false)
        };
        prefer.update_property(&[gtk::accessible::Property::Label(if take.preferred {
            "Preferred"
        } else {
            "Prefer this take"
        })]);
        prefer.update_state(&[gtk::accessible::State::Selected(Some(take.preferred))]);
        let weak = Rc::downgrade(ui);
        let feedback = model::prefer_feedback(event_id, take);
        prefer.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.manual.feedback(&ui, feedback.clone());
            }
        });
        footer.append(&prefer);
        let undo = v3::icon_button("edit-undo-symbolic", "Undo review");
        let weak = Rc::downgrade(ui);
        let (event, file_id) = (event_id.to_owned(), take.file_id);
        undo.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                let event = event.clone();
                ui.manual.send(&ui, "undoTakeFeedback", |r| {
                    r.event_id = Some(event);
                    r.file_id = Some(file_id);
                });
            }
        });
        footer.append(&undo);
        card.append(&footer);
    }
    (card, view, well, video)
}

/// Connects the manual flow's controls once the page's `Ui` exists.
pub(super) fn wire(ui: &Rc<Ui>) {
    let manual = &ui.manual;
    *manual.owner.borrow_mut() = Rc::downgrade(ui);
    let on = |button: &gtk::Button, f: fn(&Rc<Ui>)| {
        let weak = Rc::downgrade(ui);
        button.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                f(&ui);
            }
        });
    };
    on(&manual.new_group, |ui| {
        ui.manual.start_manual(ui);
        ui.manual.new_group_state();
        ui.manual.render_editor();
    });
    on(&manual.find_related, |ui| {
        let ids: Vec<i64> = ui.manual.pool().iter().map(|hit| hit.file_id).collect();
        ui.manual
            .send(ui, "suggestTakeGroups", |r| r.file_ids = Some(ids));
    });
    on(&manual.save, |ui| ui.manual.save(ui));
    on(&manual.score, |ui| ui.manual.score_takes(ui));
    on(&manual.refresh, |ui| {
        if let Some(id) = ui.manual.open_event_id() {
            ui.manual.send(ui, "takeGroup", |r| r.event_id = Some(id));
        }
    });
    on(&manual.undo_edit, |ui| {
        if let Some(id) = ui.manual.open_event_id() {
            ui.manual
                .send(ui, "undoEventEdit", |r| r.event_id = Some(id));
        }
    });
    on(&manual.delete, |ui| {
        if let Some(id) = ui.manual.open_event_id() {
            *ui.manual.last_deleted.borrow_mut() = Some(id.clone());
            ui.manual.footer.set_visible(true);
            ui.manual.send(ui, "deleteEvent", |r| r.event_id = Some(id));
        }
    });
    on(&manual.undo_delete, |ui| {
        if let Some(id) = ui.manual.last_deleted.borrow().clone() {
            ui.manual
                .send(ui, "undoEventEdit", |r| r.event_id = Some(id));
        }
    });
    on(&manual.choose, |ui| {
        let next = model::toggle_all(&ui.manual.chosen.borrow(), &ui.manual.pool());
        *ui.manual.chosen.borrow_mut() = next;
        ui.manual.render_editor();
    });
    let weak = Rc::downgrade(ui);
    manual.search.connect_activate(move |entry| {
        let Some(ui) = weak.upgrade() else { return };
        let query = entry.text().trim().to_owned();
        if query.is_empty() {
            return;
        }
        ui.manual.start_manual(&ui);
        ui.manual.send(&ui, "search", |r| r.query = Some(query));
    });
    for entry in [&manual.title_entry, &manual.goal_entry] {
        let weak = Rc::downgrade(ui);
        entry.connect_changed(move |_| {
            if let Some(ui) = weak.upgrade()
                && !ui.manual.syncing.get()
            {
                ui.manual.render_editor();
            }
        });
    }
    let shortcuts = gtk::ShortcutController::new();
    let weak = Rc::downgrade(ui);
    shortcuts.add_shortcut(gtk::Shortcut::new(
        gtk::ShortcutTrigger::parse_string("<Control>s"),
        Some(gtk::CallbackAction::new(move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.manual.save(&ui);
            }
            glib::Propagation::Stop
        })),
    ));
    manual.page.add_controller(shortcuts);
    manual.render_editor();
}
