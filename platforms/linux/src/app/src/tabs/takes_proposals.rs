// Library-wide related tries (macOS RelatedTriesSection): Find related tries
// queues the takeGrouping job, its row shows progress with pause/resume/cancel,
// and the stored proposals open in the same review page as possible groups,
// with a relation chip, the evidence and Save as group.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use adw::prelude::*;
use fileid_engine::ipc::{CatalogJob, CatalogResponse, CatalogTakeAutoGroup};

use super::{FAN_PX, Ui, date_text, load_in_order};
use crate::engine_client::CancelToken;
use crate::shell::status::Tone;
use crate::spring::{Spring, animate_with, animations_enabled};
use crate::tabs::takes_model as model;
use crate::tabs::takes_proposals_model as proposals;
use crate::tabs::takes_widgets::ShotView;
use crate::v3;

const RELATION_TONES: [Tone; 3] = [Tone::Gold, Tone::Info, Tone::Ai];

pub(crate) struct Proposals {
    groups: RefCell<Vec<CatalogTakeAutoGroup>>,
    loaded: Cell<bool>,
    key: RefCell<Option<String>>,
    resolved: RefCell<Option<(String, usize)>>,
    job: RefCell<Option<CatalogJob>>,
    reload: Cell<bool>,
    fan_token: RefCell<CancelToken>,
    seen: RefCell<HashSet<String>>,
    pub(super) section: gtk::Box,
    header: gtk::Label,
    find: gtk::Button,
    job_box: gtk::Box,
    empty: gtk::Label,
    list: gtk::Box,
    pub(super) relation: gtk::Label,
    pub(super) intro: gtk::Label,
    pub(super) save: gtk::Button,
}

impl Proposals {
    pub(super) fn new() -> Self {
        let section = v3::vbox(2);
        let top = v3::vbox(6);
        top.add_css_class("v3-pane-top");
        let header_row = v3::hbox(8);
        let header = v3::section("RELATED TRIES · 0");
        header.add_css_class("tone-gold");
        header.set_hexpand(true);
        header_row.append(&header);
        let find = v3::accent("Find related tries", Tone::Gold, false);
        find.set_tooltip_text(Some("Look through the whole library for related tries. Runs on this computer."));
        find.update_property(&[gtk::accessible::Property::Label("Find related tries across the library")]);
        header_row.append(&find);
        top.append(&header_row);
        top.append(&v3::wrapped(
            "Tries and other angles of the same moment, found across your whole library. Suggestions never change your files.",
            &["v3-meta-115"],
        ));
        section.append(&top);
        let job_box = v3::vbox(0);
        job_box.set_margin_start(8);
        job_box.set_margin_end(8);
        job_box.set_visible(false);
        section.append(&job_box);
        let empty = v3::wrapped("", &["v3-meta"]);
        empty.set_margin_start(14);
        empty.set_margin_end(14);
        empty.set_visible(false);
        section.append(&empty);
        let list = v3::vbox(2);
        list.update_property(&[gtk::accessible::Property::Label("Related tries")]);
        section.append(&list);
        section.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let relation = v3::pill("", Tone::Ai);
        relation.set_visible(false);
        let intro = v3::wrapped("", &["v3-meta", "v3-meta-13"]);
        let save = v3::accent("Save as group", Tone::Gold, false);
        save.set_tooltip_text(Some("Keep these together as a group with a name and a goal"));
        save.set_visible(false);
        Self {
            groups: RefCell::new(vec![]),
            loaded: Cell::new(false),
            key: RefCell::new(None),
            resolved: RefCell::new(None),
            job: RefCell::new(None),
            reload: Cell::new(false),
            fan_token: RefCell::new(CancelToken::default()),
            seen: RefCell::new(HashSet::new()),
            section,
            header,
            find,
            job_box,
            empty,
            list,
            relation,
            intro,
            save,
        }
    }

    pub(super) fn wire(ui: &Rc<Ui>) {
        let weak = Rc::downgrade(ui);
        ui.proposals.find.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.send("grouping", proposals::enqueue_request());
            }
        });
        let weak = Rc::downgrade(ui);
        ui.proposals.save.connect_clicked(move |_| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(group) = ui.current.borrow().clone() else { return };
            if ui.proposals.is_open(&group.group_key) {
                ui.manual.review_hits(&ui, proposals::save_hits(&group), &date_text(group.captured_at));
            }
        });
    }

    pub(super) fn is_open(&self, key: &str) -> bool {
        self.key.borrow().as_deref() == Some(key)
    }

    pub(super) fn any_open(&self) -> bool {
        self.key.borrow().is_some()
    }

    pub(crate) fn request(&self, ui: &Rc<Ui>) {
        if !ui.engine.borrow().is_ready() {
            return;
        }
        if ui.requests.borrow().busy("proposals") {
            self.reload.set(true);
            return;
        }
        ui.send("proposals", proposals::proposals_request());
    }

    pub(super) fn finished_request(&self, ui: &Rc<Ui>) {
        if self.reload.replace(false) {
            self.request(ui);
        }
    }

    /// The job's latest state, from a reply or a push while it runs; a finished run reloads the list.
    pub(super) fn set_job(&self, ui: &Rc<Ui>, job: Option<CatalogJob>) {
        let Some(job) = job else { return };
        let previous = self.job.replace(Some(job.clone()));
        self.render_job(ui);
        if proposals::finished(previous.as_ref(), Some(&job)) {
            self.request(ui);
        }
    }

    pub(super) fn consume(&self, ui: &Rc<Ui>, slot: &str, response: CatalogResponse) {
        let ok = response.status == "ok";
        match slot {
            "proposals" => {
                if ok {
                    if let Some(job) = proposals::job(&response.jobs) {
                        *self.job.borrow_mut() = Some(job);
                    }
                    self.render_job(ui);
                    self.render(ui, response.take_groups.unwrap_or_default());
                }
                self.finished_request(ui);
            }
            _ => {
                if let Some(message) = response.message.as_deref().filter(|m| !m.is_empty()) {
                    ui.toast(message);
                } else if !ok {
                    ui.toast("That didn't work. Try again in a moment.");
                }
                if ok {
                    self.set_job(ui, proposals::job(&response.jobs));
                }
            }
        }
    }

    /// Marks `key` resolved so the reload opens the proposal that followed it.
    pub(super) fn resolve(&self, key: &str) {
        let position = self.groups.borrow().iter().position(|group| group.group_key == key).unwrap_or(0);
        *self.resolved.borrow_mut() = Some((key.to_owned(), position));
    }

    pub(super) fn clear_selection(&self) {
        self.key.borrow_mut().take();
        mark(&self.list, None);
    }

    fn render(&self, ui: &Rc<Ui>, groups: Vec<CatalogTakeAutoGroup>) {
        self.loaded.set(true);
        v3::clear(&self.list);
        self.fan_token.borrow().store(true, std::sync::atomic::Ordering::Release);
        *self.fan_token.borrow_mut() = CancelToken::default();
        self.header.set_text(&proposals::header(&groups));
        let job = self.job.borrow().clone();
        self.empty.set_text(proposals::empty_text(job.as_ref()));
        self.empty
            .set_visible(groups.is_empty() && job.as_ref().is_none_or(|job| job.state != "running"));
        let current = self.key.borrow().clone();
        let resolved = self.resolved.borrow_mut().take();
        let target = match (&current, &resolved) {
            (Some(key), Some((done, position))) if key == done => {
                model::next_selection(&groups, None, Some((done.as_str(), *position)))
            }
            (Some(key), _) => groups.iter().position(|group| &group.group_key == key),
            _ => None,
        };
        let mut fans = Vec::new();
        let mut seen = self.seen.borrow_mut();
        let first_render = seen.is_empty();
        for (index, group) in groups.iter().enumerate() {
            let (button, views) = row(group);
            fans.extend(views);
            v3::set_selected(&button, target == Some(index));
            let weak = Rc::downgrade(ui);
            let key = group.group_key.clone();
            button.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.open_proposal(&key);
                }
            });
            if seen.insert(group.group_key.clone()) && !first_render {
                spring_in(button.upcast_ref());
            }
            self.list.append(&button);
        }
        drop(seen);
        *self.groups.borrow_mut() = groups;
        load_in_order(&ui.engine, fans, self.fan_token.borrow().clone());
        let Some(current) = current else { return };
        let target_key = target.and_then(|index| self.groups.borrow().get(index).map(|g| g.group_key.clone()));
        match target_key {
            Some(key) if key == current => {
                if let Some(index) = target {
                    *ui.current.borrow_mut() = Some(self.groups.borrow()[index].clone());
                }
                ui.refresh_header();
            }
            Some(key) => ui.open_proposal(&key),
            None => {
                self.key.borrow_mut().take();
                ui.close_review();
            }
        }
    }

    /// Opens the stored proposal `key` and marks its row; None when it is gone.
    pub(super) fn take(&self, key: &str) -> Option<CatalogTakeAutoGroup> {
        let groups = self.groups.borrow();
        let index = groups.iter().position(|group| group.group_key == key)?;
        *self.key.borrow_mut() = Some(key.to_owned());
        mark(&self.list, Some(index));
        Some(groups[index].clone())
    }

    fn render_job(&self, ui: &Rc<Ui>) {
        let job = self.job.borrow().clone();
        let can_find = proposals::can_find(job.as_ref());
        self.find.set_sensitive(can_find);
        self.find.set_tooltip_text(Some(if can_find {
            "Look through the whole library for related tries. Runs on this computer."
        } else {
            "Already looking"
        }));
        let was_visible = self.job_box.is_visible();
        v3::clear(&self.job_box);
        let Some(job) = job.filter(|job| proposals::show_job(Some(job))) else {
            self.job_box.set_visible(false);
            return;
        };
        let panel = gtk::Box::builder().orientation(gtk::Orientation::Vertical).css_classes(["v3-panel"]).build();
        let row = v3::vbox(6);
        row.add_css_class("v3-job-row");
        let head = v3::hbox(8);
        let title = v3::label(super::super::moments::job_title(&job.kind), &["v3-title-13"]);
        title.set_hexpand(true);
        head.append(&title);
        let (state, tone) = super::super::moments::job_state(&job.state);
        head.append(&v3::label(state, &["v3-state", tone.css()]));
        head.append(&v3::mono(&format!("{:.0}%", job.progress.clamp(0.0, 1.0) * 100.0), "s115"));
        row.append(&head);
        if let Some(error) = &job.error {
            row.append(&v3::wrapped(error, &["v3-meta"]));
        }
        let bar = crate::shell::GrainBar::new(true);
        bar.set(job.progress, tone);
        bar.bar.update_property(&[gtk::accessible::Property::Label("Related tries progress")]);
        row.append(&bar.bar);
        let controls = v3::hbox(6);
        for (action, name) in proposals::controls(&job) {
            let button = match name {
                "Resume" | "Retry" => v3::accent(name, Tone::Gold, false),
                "Cancel" => v3::ghost(name),
                _ => v3::secondary(name, false),
            };
            button.update_property(&[gtk::accessible::Property::Label(&format!("{name} finding related tries"))]);
            let weak = Rc::downgrade(ui);
            let job = job.clone();
            button.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.send("grouping-control", proposals::control_request(action, &job));
                }
            });
            controls.append(&button);
        }
        if controls.first_child().is_some() {
            row.append(&controls);
        }
        panel.append(&row);
        panel.update_property(&[gtk::accessible::Property::Label(&format!("Related tries, {state}"))]);
        self.job_box.append(&panel);
        self.job_box.set_visible(true);
        if !was_visible {
            spring_in(panel.upcast_ref());
        }
    }

    /// The review header pieces that differ for a proposal: the relation chip, the evidence and Save as group.
    pub(super) fn render_header(&self, group: &CatalogTakeAutoGroup, trashing: bool) {
        let relation = group.relation.as_deref();
        match proposals::relation_label(relation) {
            Some(label) => {
                self.relation.set_text(label);
                for tone in RELATION_TONES {
                    self.relation.remove_css_class(tone.css());
                }
                self.relation.add_css_class(proposals::relation_tone(relation).css());
                let help = proposals::relation_help(relation).unwrap_or_default();
                self.relation.set_tooltip_text(Some(help));
                self.relation.update_property(&[
                    gtk::accessible::Property::Label(label),
                    gtk::accessible::Property::Description(help),
                ]);
                self.relation.set_visible(true);
            }
            None => self.relation.set_visible(false),
        }
        self.intro.set_text(&proposals::summary(group));
        let open = self.is_open(&group.group_key);
        self.save.set_visible(open);
        self.save.set_sensitive(open && !trashing);
        self.save.update_property(&[gtk::accessible::Property::Label(&format!(
            "Save these {} as a group",
            model::noun(group, 2)
        ))]);
    }
}

fn row(group: &CatalogTakeAutoGroup) -> (gtk::Button, Vec<(String, bool, i32, ShotView)>) {
    let row = v3::hbox(10);
    let fan = gtk::Fixed::builder()
        .accessible_role(gtk::AccessibleRole::Presentation)
        .width_request(60)
        .height_request(28)
        .valign(gtk::Align::Start)
        .build();
    let mut views = Vec::new();
    for (slot, shot) in group.shots.iter().take(3).enumerate().rev() {
        let view = ShotView::new(28, 28, 4.0, true, true);
        fan.put(&view.area, slot as f64 * 16.0, 0.0);
        views.push((shot.path.clone(), model::is_video(group), FAN_PX, view));
    }
    views.reverse();
    row.append(&fan);
    let text = v3::vbox(2);
    let top = v3::hbox(6);
    let title = v3::label(&model::group_title(group), &["v3-title-13"]);
    if !model::is_open(group) {
        title.add_css_class("tone-muted");
    }
    top.append(&title);
    let relation = group.relation.as_deref();
    if let Some(label) = proposals::relation_label(relation) {
        let pill = v3::pill(label, proposals::relation_tone(relation));
        pill.set_tooltip_text(proposals::relation_help(relation));
        top.append(&pill);
    }
    text.append(&top);
    let date = date_text(group.captured_at);
    let meta = v3::label(
        &if model::is_open(group) { date.clone() } else { format!("{date} · Skipped") },
        &["v3-meta-115"],
    );
    meta.set_ellipsize(gtk::pango::EllipsizeMode::End);
    text.append(&meta);
    let evidence = v3::wrapped(&proposals::evidence_text(group.evidence.as_deref()), &["v3-meta-115", "tone-text2"]);
    evidence.set_lines(2);
    evidence.set_ellipsize(gtk::pango::EllipsizeMode::End);
    text.append(&evidence);
    row.append(&text);
    let button = v3::rail_row(&row);
    button.update_property(&[gtk::accessible::Property::Label(&proposals::spoken(group, &date))]);
    (button, views)
}

fn mark(list: &gtk::Box, index: Option<usize>) {
    let mut child = list.first_child();
    let mut position = 0;
    while let Some(widget) = child {
        v3::set_selected(&widget, index == Some(position));
        position += 1;
        child = widget.next_sibling();
    }
}

fn spring_in(widget: &gtk::Widget) {
    if !animations_enabled() {
        return;
    }
    widget.set_opacity(0.0);
    let weak = widget.downgrade();
    let _ = animate_with(Spring::Standard, widget, 0.0, 1.0, move |value| {
        if let Some(widget) = weak.upgrade() {
            widget.set_opacity(value.clamp(0.0, 1.0));
        }
    });
}
