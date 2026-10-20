// Restructure (DESIGN_V3_PORT "Restructure"): Preview · Review · Move step
// cells pinned above the plan, then the intro, metric row, notices,
// recommendations with their files grouped by destination, Staying put and
// the Review drill-down. Nothing moves until Move is confirmed.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use fileid_engine::ipc::{
    ApplyRestructurePayload, CommandPayload, Empty, PlanRestructurePayload, RestructureApplyResult, RestructureMove,
    RestructurePlan, UndoRestructurePayload,
};

use super::restructure_model::*;
use crate::engine_client::{EngineClient, EngineState};
use crate::shell::status::Tone;
use crate::v3;

const INLINE_ROWS: usize = 50;
const SEE_ALL_AFTER: usize = 30;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Operation {
    Planning,
    Applying,
    Undoing,
}

struct StepCell {
    number: usize,
    root: gtk::Box,
    badge: gtk::Label,
    title: gtk::Label,
    detail: gtk::Label,
}

struct Ui {
    engine: Rc<RefCell<EngineClient>>,
    root: RefCell<Option<String>>,
    plan: RefCell<Option<RestructurePlan>>,
    selected: RefCell<Vec<bool>>,
    skipped: RefCell<HashSet<Outcome>>,
    expanded: RefCell<Option<Outcome>>,
    operation: Cell<Option<Operation>>,
    stopping: Cell<bool>,
    moved: Cell<bool>,
    library: RefCell<Library>,
    cells: [StepCell; 3],
    plan_button: gtk::Button,
    plan_again: gtk::Button,
    planning_bar: gtk::ProgressBar,
    review_button: gtk::Button,
    move_button: gtk::Button,
    moving_bar: gtk::ProgressBar,
    cancel_button: gtk::Button,
    undo_button: gtk::Button,
    folder: gtk::Label,
    notice: gtk::Box,
    notice_title: gtk::Label,
    notice_body: gtk::Label,
    metrics: gtk::Box,
    metric_values: [gtk::Label; 3],
    metric_hints: [gtk::Label; 3],
    clip_notice: gtk::Box,
    clip_body: gtk::Label,
    clip_button: gtk::Button,
    recommendations: v3::Panel,
    staying: gtk::Box,
    staying_header: gtk::Button,
    staying_meta: gtk::Label,
    staying_chevron: gtk::Image,
    staying_list: gtk::Revealer,
    staying_rows: gtk::Box,
}

fn step_cell(index: usize, title: &str) -> StepCell {
    let root = v3::vbox(8);
    root.add_css_class("v3-step");
    root.set_hexpand(true);
    let head = v3::hbox(8);
    let badge = v3::label(&(index + 1).to_string(), &["v3-step-badge"]);
    badge.set_xalign(0.5);
    badge.set_valign(gtk::Align::Center);
    head.append(&badge);
    let title = v3::label(title, &["v3-step-title"]);
    head.append(&title);
    root.append(&head);
    let detail = v3::wrapped("", &["v3-meta"]);
    detail.set_lines(2);
    detail.set_ellipsize(gtk::pango::EllipsizeMode::End);
    root.append(&detail);
    StepCell { number: index + 1, root, badge, title, detail }
}

fn notice(tone: Tone) -> (gtk::Box, gtk::Label, gtk::Label) {
    let root = v3::vbox(4);
    root.add_css_class("v3-notice");
    root.add_css_class(tone.css());
    let title = v3::label("", &["v3-step-title"]);
    let body = v3::wrapped("", &["v3-body-125"]);
    root.append(&title);
    root.append(&body);
    (root, title, body)
}

pub fn build_restructure_tab(engine: Rc<RefCell<EngineClient>>) -> gtk::Widget {
    // ── Step strip ─────────────────────────────────────────────────────────
    let strip_outer = v3::vbox(0);
    strip_outer.add_css_class("v3-strip");
    let strip = gtk::Box::builder().homogeneous(true).css_classes(["v3-panel"]).build();
    let cells = [step_cell(0, "Preview"), step_cell(1, "Review"), step_cell(2, "Move")];
    let preview_actions = v3::hbox(8);
    preview_actions.add_css_class("v3-toggle-row");
    let plan_button = v3::primary("Plan", true);
    let plan_again = v3::ghost("Plan again");
    let planning_bar = v3::busy_bar(160);
    preview_actions.append(&plan_button);
    preview_actions.append(&plan_again);
    preview_actions.append(&planning_bar);
    cells[0].root.append(&preview_actions);
    let review_actions = v3::hbox(8);
    review_actions.add_css_class("v3-toggle-row");
    let review_button = v3::ghost("Review all moves");
    review_actions.append(&review_button);
    cells[1].root.append(&review_actions);
    let move_actions = v3::hbox(8);
    move_actions.add_css_class("v3-toggle-row");
    let move_button = v3::primary("Move files", true);
    let moving_bar = v3::busy_bar(120);
    let cancel_button = v3::danger("Cancel");
    cancel_button.set_visible(false);
    let undo_button = v3::accent("↺ Undo move", Tone::Gold, false);
    undo_button.set_visible(false);
    move_actions.append(&move_button);
    move_actions.append(&moving_bar);
    move_actions.append(&cancel_button);
    move_actions.append(&undo_button);
    cells[2].root.append(&move_actions);
    for (index, cell) in cells.iter().enumerate() {
        if index > 0 {
            strip.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        }
        strip.append(&cell.root);
    }
    strip_outer.append(&strip);

    // ── Content ────────────────────────────────────────────────────────────
    let content = v3::vbox(16);
    content.add_css_class("v3-page-pad");
    let intro = v3::vbox(4);
    intro.append(&v3::wrapped(
        "Preview the proposed moves: FileID keeps your well-named folders and proposes a tidier home for the rest. Nothing moves until you choose Move.",
        &["v3-body", "tone-text2"],
    ));
    let folder_row = v3::hbox(8);
    let folder = v3::mono("Choose a library folder to build a plan.", "s11");
    folder.set_ellipsize(gtk::pango::EllipsizeMode::Start);
    folder.add_css_class("tone-muted");
    folder.set_hexpand(true);
    folder_row.append(&folder);
    let change_folder = v3::ghost("Change folder…");
    folder_row.append(&change_folder);
    intro.append(&folder_row);
    content.append(&intro);
    let (notice_box, notice_title, notice_body) = notice(Tone::Done);
    notice_box.set_visible(false);
    content.append(&notice_box);

    let metrics = gtk::Box::builder().homogeneous(true).css_classes(["v3-panel"]).visible(false).build();
    let metric = |label: &str, tone: Tone| -> (gtk::Box, gtk::Label, gtk::Label) {
        let cell = v3::vbox(4);
        cell.add_css_class("v3-metric");
        let head = v3::hbox(6);
        head.append(&gtk::Box::builder().css_classes(["v3-dot", tone.css()]).valign(gtk::Align::Center).build());
        head.append(&v3::label(label, &["v3-meta"]));
        cell.append(&head);
        let value = v3::mono("0", "s20");
        value.add_css_class("numeric");
        cell.append(&value);
        let hint = v3::label("", &["v3-meta-115"]);
        cell.append(&hint);
        (cell, value, hint)
    };
    let (keep_cell, keep_value, keep_hint) = metric("Staying put", Tone::Done);
    let (tidy_cell, tidy_value, tidy_hint) = metric("Tidying", Tone::Warning);
    let (reorg_cell, reorg_value, reorg_hint) = metric("Reorganizing", Tone::Gold);
    metrics.append(&keep_cell);
    metrics.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    metrics.append(&tidy_cell);
    metrics.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    metrics.append(&reorg_cell);
    content.append(&metrics);

    let (clip_notice, clip_title, clip_body) = notice(Tone::Warning);
    clip_title.set_text("Photos were scanned without CLIP");
    let clip_button = v3::accent("Rescan", Tone::Warning, false);
    clip_button.set_halign(gtk::Align::Start);
    clip_notice.append(&clip_button);
    clip_notice.set_visible(false);
    content.append(&clip_notice);

    let recommendations = v3::panel("Recommendations");
    recommendations.trailing.append(&v3::meta("Skip a group to leave it out of the move"));
    recommendations.root.set_visible(false);
    content.append(&recommendations.root);

    let staying = v3::vbox(0);
    staying.add_css_class("v3-panel");
    staying.set_visible(false);
    let staying_head = v3::hbox(8);
    let staying_chevron = gtk::Image::from_icon_name("pan-end-symbolic");
    staying_head.append(&staying_chevron);
    let staying_title = v3::section("Staying put");
    staying_title.set_hexpand(true);
    staying_head.append(&staying_title);
    let staying_meta = v3::meta("");
    staying_head.append(&staying_meta);
    let staying_header = gtk::Button::builder().child(&staying_head).css_classes(["v3-staying-header"]).build();
    staying_header.update_state(&[gtk::accessible::State::Expanded(Some(false))]);
    staying.append(&staying_header);
    let staying_rows = v3::vbox(0);
    let staying_list = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .transition_duration(if crate::spring::animations_enabled() { 220 } else { 0 })
        .child(&staying_rows)
        .build();
    staying.append(&staying_list);
    content.append(&staying);

    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&content)
        .build();
    let page = v3::vbox(0);
    page.add_css_class("fileid-tab");
    page.append(&strip_outer);
    page.append(&scroll);

    let ui = Rc::new(Ui {
        engine,
        root: RefCell::new(crate::app_settings::last_folder().map(|path| path.to_string_lossy().into_owned())),
        plan: RefCell::new(None),
        selected: RefCell::new(Vec::new()),
        skipped: RefCell::new(HashSet::new()),
        expanded: RefCell::new(None),
        operation: Cell::new(None),
        stopping: Cell::new(false),
        moved: Cell::new(false),
        library: RefCell::new(Library::default()),
        cells,
        plan_button,
        plan_again,
        planning_bar,
        review_button,
        move_button,
        moving_bar,
        cancel_button,
        undo_button,
        folder,
        notice: notice_box,
        notice_title,
        notice_body,
        metrics,
        metric_values: [keep_value, tidy_value, reorg_value],
        metric_hints: [keep_hint, tidy_hint, reorg_hint],
        clip_notice,
        clip_body,
        clip_button,
        recommendations,
        staying,
        staying_header,
        staying_meta,
        staying_chevron,
        staying_list,
        staying_rows,
    });
    if let Some(root) = ui.root.borrow().clone() {
        ui.set_folder_label(&root);
    }
    wire(&ui, &change_folder);
    ui.refresh();
    page.upcast()
}

impl Ui {
    fn set_folder_label(&self, root: &str) {
        self.folder.set_text(root);
        self.folder.set_tooltip_text(Some(root));
        self.folder.update_property(&[gtk::accessible::Property::Label(&format!("Library folder {root}"))]);
    }

    fn moves(&self) -> Vec<RestructureMove> {
        self.plan.borrow().as_ref().map(|plan| plan.moves.clone()).unwrap_or_default()
    }

    /// Selected moves outside skipped groups: what Move applies.
    fn chosen(&self) -> Vec<RestructureMove> {
        let skipped = self.skipped.borrow();
        let selected = self.selected.borrow();
        self.moves()
            .into_iter()
            .enumerate()
            .filter(|(index, movement)| selected.get(*index).copied().unwrap_or(false) && !skipped.contains(&outcome(movement)))
            .map(|(_, movement)| movement)
            .collect()
    }

    fn progress(&self) -> Progress {
        Progress {
            planning: self.operation.get() == Some(Operation::Planning),
            has_plan: self.plan.borrow().as_ref().is_some_and(|plan| !plan.moves.is_empty()),
            selected: self.chosen().len(),
            moving: self.operation.get() == Some(Operation::Applying),
            moved: self.moved.get(),
        }
    }

    fn set_notice(&self, tone: Tone, title: &str, body: &str) {
        for other in Tone::ALL {
            self.notice.remove_css_class(other.css());
        }
        self.notice.add_css_class(tone.css());
        self.notice_title.set_text(title);
        self.notice_body.set_text(body);
        self.notice.set_visible(true);
    }

    fn refresh(&self) {
        let progress = self.progress();
        let states = steps(progress);
        let moves = self.moves();
        let sources: HashSet<String> = moves.iter().map(|m| v3::parent_name(&m.source)).collect();
        let buckets: HashSet<String> =
            moves.iter().map(|m| bucket(m, self.root.borrow().as_deref().unwrap_or(""))).collect();
        let details = [
            if progress.has_plan {
                format!(
                    "{} planned · {} → {}",
                    plural(moves.len(), "move"),
                    plural(sources.len(), "source"),
                    plural(buckets.len(), "destination")
                )
            } else {
                "Build a plan from the library folder. Nothing moves.".to_owned()
            },
            if progress.has_plan {
                format!("{} of {} selected", plural(progress.selected, "move"), crate::shell::grouped(moves.len() as u64))
            } else {
                "Check where each file would go.".to_owned()
            },
            if self.moved.get() {
                "Files moved. Undo puts every file back.".to_owned()
            } else {
                "Move the selected files. One undoable run.".to_owned()
            },
        ];
        for ((cell, state), detail) in self.cells.iter().zip(states).zip(details) {
            for class in ["current", "done"] {
                cell.root.remove_css_class(class);
                cell.badge.remove_css_class(class);
            }
            cell.title.remove_css_class("upcoming");
            match state {
                StepState::Current => {
                    cell.root.add_css_class("current");
                    cell.badge.add_css_class("current");
                }
                StepState::Done => cell.badge.add_css_class("done"),
                StepState::Upcoming => cell.title.add_css_class("upcoming"),
            }
            cell.badge.set_text(&if state == StepState::Done { "✓".to_owned() } else { cell.number.to_string() });
            let state_word = match state {
                StepState::Done => "done",
                StepState::Current => "current",
                StepState::Upcoming => "not started",
            };
            cell.root.update_property(&[
                gtk::accessible::Property::Label(&format!("Step {} of 3, {}, {state_word}", cell.number, cell.title.text())),
                gtk::accessible::Property::Description(&detail),
            ]);
            cell.detail.set_text(&detail);
        }
        let busy = self.operation.get().is_some();
        let has_root = self.root.borrow().is_some();
        self.plan_button.set_visible(!progress.has_plan);
        self.plan_button.set_sensitive(has_root && !busy);
        self.plan_again.set_visible(progress.has_plan);
        self.plan_again.set_sensitive(has_root && !busy);
        self.planning_bar.set_visible(progress.planning);
        self.review_button.set_sensitive(progress.has_plan && !busy);
        let count = progress.selected;
        self.move_button.set_label(&format!("Move {}", plural(count, "file")));
        self.move_button.set_visible(!progress.moving && !self.moved.get());
        self.move_button.set_sensitive(count > 0 && !busy);
        self.moving_bar.set_visible(progress.moving);
        self.cancel_button.set_visible(progress.moving);
        self.cancel_button.set_label(if self.stopping.get() { "Stopping…" } else { "Cancel" });
        self.cancel_button.set_sensitive(!self.stopping.get());
        self.undo_button.set_visible(self.moved.get() && !progress.moving);
        self.undo_button.set_sensitive(!busy);
    }

    fn render_plan(self: &Rc<Self>) {
        let plan = self.plan.borrow().clone();
        let Some(plan) = plan else {
            self.metrics.set_visible(false);
            self.recommendations.root.set_visible(false);
            self.staying.set_visible(false);
            return;
        };
        let moves = &plan.moves;
        let tidy = moves.iter().filter(|m| outcome(m) == Outcome::Tidy).count();
        let reorganize = moves.len() - tidy;
        let total_moves = plan.total_moves.unwrap_or(moves.len() as u64);
        let counts = plan.folder_classifications.clone();
        let (anchors, mixed, junk) = counts.map_or((0, 0, 0), |c| (c.anchor_folders, c.mixed_folders, c.junk_folders));
        let library = self.library.borrow().clone();
        let staying = library.indexed.saturating_sub(total_moves);
        self.metric_values[0].set_text(&crate::shell::grouped(staying));
        self.metric_values[1].set_text(&crate::shell::grouped(tidy as u64));
        self.metric_values[2].set_text(&crate::shell::grouped(reorganize as u64));
        self.metric_hints[0].set_text(&plural(anchors as usize, "kept folder"));
        self.metric_hints[1].set_text(&plural(mixed as usize, "mixed folder"));
        self.metric_hints[2].set_text(&plural(junk as usize, "generic folder"));
        self.metrics.set_visible(true);

        v3::clear(&self.recommendations.body);
        if anchors > 0 {
            self.recommendations.body.append(&self.recommendation(
                None,
                "folder-symbolic",
                Tone::Done,
                &format!("Keep {} as they are", plural(anchors as usize, "folder")),
                &format!("Clear names and matching contents. {} stay exactly where they are.", plural(staying as usize, "file")),
                staying as usize,
                anchors as usize,
            ));
        }
        if tidy > 0 {
            self.recommendations.body.append(&self.recommendation(
                Some(Outcome::Tidy),
                "edit-select-all-symbolic",
                Tone::Warning,
                &format!("Tidy {}: move {}", plural(mixed.max(1) as usize, "folder"), plural(tidy, "misplaced file")),
                "Mostly organized folders with a few files that don't fit. The folder stays; the misplaced files move to where they belong.",
                tidy,
                mixed as usize,
            ));
        }
        if reorganize > 0 {
            self.recommendations.body.append(&self.recommendation(
                Some(Outcome::Reorganize),
                "view-sort-ascending-symbolic",
                Tone::Gold,
                &format!("Reorganize {}: sort {}", plural(junk.max(1) as usize, "folder"), plural(reorganize, "file")),
                "Folders with generic names like \u{201c}Untitled\u{201d} or \u{201c}Camera Roll\u{201d}. Their files are sorted into People, Places, Documents, events or Photos by year.",
                reorganize,
                junk as usize,
            ));
        }
        self.recommendations.root.set_visible(self.recommendations.body.first_child().is_some());
        self.render_staying();
    }

    #[allow(clippy::too_many_arguments)]
    fn recommendation(
        self: &Rc<Self>,
        group: Option<Outcome>,
        icon: &str,
        tone: Tone,
        headline: &str,
        body: &str,
        files: usize,
        folders: usize,
    ) -> gtk::Box {
        let outer = v3::vbox(0);
        outer.add_css_class("v3-soft-row");
        let row = v3::hbox(12);
        row.add_css_class("v3-rec");
        let skipped = group.is_some_and(|g| self.skipped.borrow().contains(&g));
        if skipped {
            row.add_css_class("skipped");
        }
        let tile = gtk::Box::builder().css_classes(["v3-icon-tile", tone.css()]).valign(gtk::Align::Start).build();
        let glyph = gtk::Image::from_icon_name(icon);
        glyph.set_pixel_size(13);
        glyph.set_hexpand(true);
        glyph.set_vexpand(true);
        tile.append(&glyph);
        row.append(&tile);
        let text = v3::vbox(6);
        text.set_hexpand(true);
        text.append(&v3::wrapped(headline, &["v3-step-title"]));
        text.append(&v3::wrapped(body, &["v3-body-125"]));
        if let Some(group) = group {
            let actions = v3::hbox(8);
            let expanded = *self.expanded.borrow() == Some(group);
            let toggle = v3::secondary(if expanded { "Hide files ▾" } else { "Review files ▸" }, false);
            toggle.update_state(&[gtk::accessible::State::Expanded(Some(expanded))]);
            let weak = Rc::downgrade(self);
            toggle.connect_clicked(move |_| {
                let Some(ui) = weak.upgrade() else { return };
                let next = if *ui.expanded.borrow() == Some(group) { None } else { Some(group) };
                ui.expanded.replace(next);
                ui.render_plan();
            });
            actions.append(&toggle);
            let skip = if skipped { v3::accent("Include", tone, false) } else { v3::ghost("Skip these") };
            let weak = Rc::downgrade(self);
            skip.connect_clicked(move |_| {
                let Some(ui) = weak.upgrade() else { return };
                {
                    let mut set = ui.skipped.borrow_mut();
                    if !set.remove(&group) {
                        set.insert(group);
                    }
                }
                ui.render_plan();
                ui.refresh();
            });
            actions.append(&skip);
            text.append(&actions);
        }
        row.append(&text);
        let counts = v3::vbox(2);
        counts.set_valign(gtk::Align::Start);
        let count = v3::mono(&crate::shell::grouped(files as u64), "s20");
        count.add_css_class(tone.css());
        count.set_xalign(1.0);
        counts.append(&count);
        let folders_label = v3::label(&plural(folders, "folder"), &["v3-meta-115"]);
        folders_label.set_xalign(1.0);
        counts.append(&folders_label);
        row.append(&counts);
        outer.append(&row);
        if let Some(group) = group.filter(|g| *self.expanded.borrow() == Some(*g)) {
            outer.append(&self.inline_files(group));
        }
        outer
    }

    /// The group's files by destination bucket: 50 rows inline, then
    /// "+ N more" and "See all" for the drill-down.
    fn inline_files(self: &Rc<Self>, group: Outcome) -> gtk::Box {
        let list = v3::vbox(10);
        list.add_css_class("v3-expanded");
        list.add_css_class("v3-panel-pad");
        let root = self.root.borrow().clone().unwrap_or_default();
        let moves = self.moves();
        let mut buckets: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (index, movement) in moves.iter().enumerate().filter(|(_, m)| outcome(m) == group) {
            buckets.entry(bucket(movement, &root)).or_default().push(index);
        }
        let total: usize = buckets.values().map(Vec::len).sum();
        let mut shown = 0;
        for (name, indices) in &buckets {
            if shown >= INLINE_ROWS {
                break;
            }
            let take = (INLINE_ROWS - shown).min(indices.len());
            list.append(&self.bucket_section(name, &indices[..take], indices));
            shown += take;
        }
        if total > shown {
            list.append(&v3::meta(&format!("+ {} more", crate::shell::grouped((total - shown) as u64))));
        }
        if total > SEE_ALL_AFTER {
            let see_all = v3::ghost(&format!("See all {}", plural(total, "file")));
            see_all.set_halign(gtk::Align::Start);
            let weak = Rc::downgrade(self);
            see_all.connect_clicked(move |button| {
                if let Some(ui) = weak.upgrade() {
                    ui.open_review(button, Some(group));
                }
            });
            list.append(&see_all);
        }
        list
    }

    fn bucket_section(self: &Rc<Self>, name: &str, shown: &[usize], all: &[usize]) -> gtk::Box {
        let section = v3::vbox(2);
        let head = v3::hbox(8);
        let icon = gtk::Image::from_icon_name("folder-symbolic");
        icon.set_pixel_size(13);
        icon.add_css_class("tone-gold");
        head.append(&icon);
        let label = v3::mono(name, "s125");
        label.add_css_class("v3-medium");
        head.append(&label);
        let count = v3::meta(&format!("{} moving in", plural(all.len(), "file")));
        count.set_hexpand(true);
        head.append(&count);
        for (text, on) in [("Select all", true), ("Skip all", false)] {
            let button = v3::ghost(text);
            let weak = Rc::downgrade(self);
            let indices = all.to_vec();
            button.connect_clicked(move |_| {
                let Some(ui) = weak.upgrade() else { return };
                {
                    let mut selected = ui.selected.borrow_mut();
                    for index in &indices {
                        if let Some(slot) = selected.get_mut(*index) {
                            *slot = on;
                        }
                    }
                }
                ui.render_plan();
                ui.refresh();
            });
            head.append(&button);
        }
        section.append(&head);
        let rows = v3::vbox(0);
        rows.add_css_class("v3-guide");
        for index in shown {
            rows.append(&self.proposal_row(*index));
        }
        section.append(&rows);
        section
    }

    fn proposal_row(self: &Rc<Self>, index: usize) -> gtk::Button {
        let moves = self.plan.borrow();
        let movement = &moves.as_ref().expect("a plan is shown").moves[index];
        let root = self.root.borrow().clone().unwrap_or_default();
        let selected = self.selected.borrow().get(index).copied().unwrap_or(false);
        let row = v3::hbox(8);
        let check = gtk::CheckButton::builder().active(selected).can_focus(false).can_target(false).build();
        row.append(&check);
        let icon = gtk::Image::from_icon_name("text-x-generic-symbolic");
        icon.set_pixel_size(11);
        row.append(&icon);
        let text = v3::vbox(1);
        text.set_hexpand(true);
        let line = v3::hbox(6);
        let file = v3::mono(&v3::file_name(&movement.source), "s12");
        if !selected {
            file.add_css_class("tone-muted");
        }
        line.append(&file);
        let from = v3::mono(&format!("from {}", source_folder(movement)), "s11");
        from.add_css_class("tone-muted");
        line.append(&from);
        text.append(&line);
        if let Some(reason) = movement.reason.as_deref().filter(|r| !r.is_empty()) {
            let reason = v3::wrapped(reason, &["v3-reason"]);
            text.append(&reason);
        }
        row.append(&text);
        let (confidence, tone) = match movement.confidence.to_ascii_lowercase().as_str() {
            "auto" => ("Confident", Tone::Done),
            "review" => ("Review", Tone::Gold),
            "ask" => ("Ask", Tone::Warning),
            _ => ("Unrated", Tone::Muted),
        };
        row.append(&v3::pill(confidence, tone));
        let button = gtk::Button::builder().child(&row).css_classes(["v3-proposal"]).build();
        button.update_property(&[gtk::accessible::Property::Label(&format!(
            "Move {} from {} to {}",
            v3::file_name(&movement.source),
            source_folder(movement),
            bucket(movement, &root)
        ))]);
        button.update_state(&[gtk::accessible::State::Selected(Some(selected))]);
        drop(moves);
        let weak = Rc::downgrade(self);
        button.connect_clicked(move |button| {
            let Some(ui) = weak.upgrade() else { return };
            let now = {
                let mut selected = ui.selected.borrow_mut();
                let Some(slot) = selected.get_mut(index) else { return };
                *slot = !*slot;
                *slot
            };
            if let Some(check) = button.child().and_then(|c| c.first_child()).and_downcast::<gtk::CheckButton>() {
                check.set_active(now);
            }
            button.update_state(&[gtk::accessible::State::Selected(Some(now))]);
            ui.refresh();
        });
        button
    }

    fn render_staying(self: &Rc<Self>) {
        let library = self.library.borrow().clone();
        v3::clear(&self.staying_rows);
        let files: u64 = library.staying.iter().map(|(_, n)| n).sum();
        self.staying.set_visible(!library.staying.is_empty() && self.plan.borrow().is_some());
        self.staying_meta.set_text(&format!(
            "{} · {}",
            plural(library.staying.len(), "folder"),
            plural(files as usize, "file")
        ));
        for (folder, count) in library.staying.iter().take(500) {
            let row = v3::hbox(10);
            row.add_css_class("v3-row-pad");
            row.add_css_class("v3-soft-row");
            row.append(&gtk::Box::builder().css_classes(["v3-dot", "tone-done"]).valign(gtk::Align::Center).build());
            let name = v3::mono(folder, "s12");
            name.set_hexpand(true);
            row.append(&name);
            row.append(&v3::label(&plural(*count as usize, "file"), &["v3-meta-115"]));
            self.staying_rows.append(&row);
        }
    }

    /// Drill-down sheet with every move (or one group's), grouped by
    /// destination; Done (Return) closes it.
    fn open_review(self: &Rc<Self>, anchor: &impl IsA<gtk::Widget>, group: Option<Outcome>) {
        let dialog = adw::Dialog::builder().content_width(820).content_height(640).title("Review moves").build();
        let column = v3::vbox(0);
        let head = v3::hbox(16);
        head.add_css_class("v3-review-head");
        let titles = v3::vbox(6);
        titles.set_hexpand(true);
        titles.append(&v3::section("Restructure / Review"));
        titles.append(&v3::label(
            match group {
                Some(Outcome::Tidy) => "Tidying",
                Some(Outcome::Reorganize) => "Reorganizing",
                None => "All moves",
            },
            &["v3-serif-28"],
        ));
        head.append(&titles);
        let meta = v3::meta("");
        meta.set_valign(gtk::Align::End);
        head.append(&meta);
        let done = v3::primary("Done", false);
        done.set_valign(gtk::Align::End);
        done.set_tooltip_text(Some("Done (Return)"));
        head.append(&done);
        column.append(&head);
        column.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        let list = v3::vbox(12);
        list.add_css_class("v3-review-list");
        let root = self.root.borrow().clone().unwrap_or_default();
        let mut buckets: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (index, movement) in self.moves().iter().enumerate() {
            if group.map_or(true, |group| group == outcome(movement)) {
                buckets.entry(bucket(movement, &root)).or_default().push(index);
            }
        }
        let total: usize = buckets.values().map(Vec::len).sum();
        meta.set_text(&plural(total, "move"));
        for (name, indices) in &buckets {
            list.append(&self.bucket_section(name, indices, indices));
        }
        column.append(
            &gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).child(&list).build(),
        );
        dialog.set_child(Some(&column));
        let close = dialog.downgrade();
        done.connect_clicked(move |_| {
            if let Some(dialog) = close.upgrade() {
                dialog.close();
            }
        });
        let keys = gtk::EventControllerKey::new();
        let close = dialog.downgrade();
        keys.connect_key_pressed(move |_, key, _, _| {
            if matches!(key, gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter) {
                if let Some(dialog) = close.upgrade() {
                    dialog.close();
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        dialog.add_controller(keys);
        let weak = Rc::downgrade(self);
        dialog.connect_closed(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.render_plan();
                ui.refresh();
            }
        });
        dialog.present(Some(anchor));
    }

    fn load_library(self: &Rc<Self>) {
        let Some(root) = self.root.borrow().clone() else { return };
        let moved_from: Vec<String> = self.moves().iter().map(|m| v3::parent_name(&m.source)).collect();
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let library = fileid_engine::paths::db_path()
                .ok()
                .filter(|path| path.exists())
                .and_then(|path| fileid_engine::db::open_read(&path).ok())
                .map(|conn| library_under(&conn, &root, &moved_from))
                .unwrap_or_default();
            let _ = tx.send_blocking(library);
        });
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let Ok(library) = rx.recv().await else { return };
            let Some(ui) = weak.upgrade() else { return };
            let without_clip = library.images_without_clip;
            ui.library.replace(library);
            if without_clip > 0 {
                let installed = clip_model_installed();
                ui.clip_body.set_text(&format!(
                    "{} group by date only. {}",
                    plural(without_clip as usize, "photo"),
                    if installed { "Rescan to sort them by what they show." } else { "Install CLIP in Settings, then rescan." }
                ));
                ui.clip_button.set_label(if installed { "Rescan" } else { "Open Settings" });
                ui.clip_notice.set_visible(true);
            } else {
                ui.clip_notice.set_visible(false);
            }
            ui.render_plan();
        });
    }

    fn show_plan(self: &Rc<Self>, plan: RestructurePlan) {
        self.operation.set(None);
        self.moved.set(false);
        let anchors = plan.folder_classifications.as_ref().map_or(0, |c| c.anchor_folders);
        self.selected.replace(plan.moves.iter().map(|m| default_selected(&m.confidence)).collect());
        self.skipped.borrow_mut().clear();
        self.expanded.replace(None);
        if plan.moves.is_empty() {
            self.set_notice(Tone::Done, "Nothing to move", &zero_move_status(anchors));
        } else {
            self.notice.set_visible(false);
        }
        self.plan.replace(Some(plan));
        self.load_library();
        self.render_plan();
        self.refresh();
    }

    fn finish(self: &Rc<Self>, result: RestructureApplyResult) {
        let operation = self.operation.replace(None);
        self.stopping.set(false);
        if let Some(message) = result.privilege_error.as_deref().filter(|m| !m.is_empty()) {
            self.set_notice(Tone::Warning, "Something went wrong", message);
        } else if operation == Some(Operation::Undoing) {
            if result.failed == 0 {
                self.moved.set(false);
                self.set_notice(Tone::Done, "Undone", &format!("{} put back.", plural(result.applied as usize, "file")));
            } else {
                self.set_notice(
                    Tone::Warning,
                    "Undo incomplete",
                    &format!("{} put back; {} failed. You can retry.", plural(result.applied as usize, "file"), result.failed),
                );
            }
        } else {
            self.moved.set(result.applied > 0);
            let body = if result.cancelled {
                format!("Stopped after {}. Undo puts them back.", plural(result.applied as usize, "file"))
            } else if result.failed > 0 {
                format!("{} moved; {} failed. Undo puts the moved ones back.", plural(result.applied as usize, "file"), result.failed)
            } else {
                format!("{} moved. Undo puts every file back.", plural(result.applied as usize, "file"))
            };
            self.set_notice(if result.failed > 0 { Tone::Warning } else { Tone::Done }, "Done", &body);
            if result.applied > 0 {
                self.plan.replace(None);
                self.render_plan();
            }
        }
        self.refresh();
    }

    fn send(self: &Rc<Self>, operation: Operation, command: CommandPayload) {
        self.operation.set(Some(operation));
        self.refresh();
        let sent = self.engine.borrow_mut().send(command);
        if let Err(error) = sent {
            self.operation.set(None);
            self.set_notice(Tone::Warning, "Something went wrong", &error.to_string());
            self.refresh();
        }
    }

    fn confirm_move(self: &Rc<Self>, anchor: &gtk::Button) {
        let moves = self.chosen();
        if moves.is_empty() {
            return;
        }
        let dialog = adw::AlertDialog::new(
            Some(&format!("Move {}?", plural(moves.len(), "file"))),
            Some("They move inside the library folder as one undoable run. Nothing is deleted."),
        );
        dialog.add_responses(&[("cancel", "Cancel"), ("move", "Move files")]);
        dialog.set_response_appearance("move", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            let Some(ui) = weak.upgrade().filter(|_| response == "move") else { return };
            let Some(root) = ui.root.borrow().clone() else { return };
            let moves = ui.chosen();
            ui.send(
                Operation::Applying,
                CommandPayload::ApplyRestructure(ApplyRestructurePayload {
                    library_root: root,
                    plan_id: None,
                    moves,
                    use_symlinks: false,
                }),
            );
        });
        dialog.present(Some(anchor));
    }
}

fn clip_model_installed() -> bool {
    use fileid_engine::models::registry::{self, LookupResult};
    matches!(registry::lookup_full("mobileclip_s2"), LookupResult::Found(model) if registry::installation_complete(&model))
}

fn wire(ui: &Rc<Ui>, change_folder: &gtk::Button) {
    let weak = Rc::downgrade(ui);
    change_folder.connect_clicked(move |button| {
        let dialog = gtk::FileDialog::builder().title("Choose a library folder").modal(true).build();
        let parent = button.root().and_downcast::<gtk::Window>();
        let weak = weak.clone();
        dialog.select_folder(parent.as_ref(), gio::Cancellable::NONE, move |result| {
            let (Some(ui), Some(path)) = (weak.upgrade(), result.ok().and_then(|f| f.path())) else { return };
            let root = path.to_string_lossy().into_owned();
            crate::app_settings::remember_folder(&path);
            ui.root.replace(Some(root.clone()));
            ui.set_folder_label(&root);
            ui.plan.replace(None);
            ui.moved.set(false);
            ui.notice.set_visible(false);
            ui.render_plan();
            ui.refresh();
        });
    });
    for button in [&ui.plan_button, &ui.plan_again] {
        let weak = Rc::downgrade(ui);
        button.connect_clicked(move |_| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(root) = ui.root.borrow().clone() else { return };
            ui.notice.set_visible(false);
            ui.send(
                Operation::Planning,
                CommandPayload::PlanRestructure(PlanRestructurePayload { library_root: root, supports_paged_plans: false }),
            );
        });
    }
    let weak = Rc::downgrade(ui);
    ui.review_button.connect_clicked(move |button| {
        if let Some(ui) = weak.upgrade() {
            ui.open_review(button, None);
        }
    });
    let weak = Rc::downgrade(ui);
    ui.move_button.connect_clicked(move |button| {
        if let Some(ui) = weak.upgrade() {
            ui.confirm_move(button);
        }
    });
    let weak = Rc::downgrade(ui);
    ui.cancel_button.connect_clicked(move |_| {
        let Some(ui) = weak.upgrade() else { return };
        ui.stopping.set(true);
        let _ = ui.engine.borrow_mut().send(CommandPayload::CancelRestructure(Empty {}));
        ui.refresh();
    });
    let weak = Rc::downgrade(ui);
    ui.undo_button.connect_clicked(move |_| {
        let Some(ui) = weak.upgrade() else { return };
        let Some(root) = ui.root.borrow().clone() else { return };
        ui.send(
            Operation::Undoing,
            CommandPayload::UndoRestructure(UndoRestructurePayload { library_root: root, shortcut_undo_token: None }),
        );
    });
    let weak = Rc::downgrade(ui);
    ui.clip_button.connect_clicked(move |button| {
        let Some(ui) = weak.upgrade() else { return };
        if clip_model_installed() {
            let root = ui.root.borrow().clone();
            if let Some(root) = root {
                let started = ui.engine.borrow_mut().start_scan(&root);
                match started {
                    Ok(()) => ui.set_notice(Tone::Gold, "Rescanning", "Plan again when the scan finishes."),
                    Err(error) => ui.set_notice(Tone::Warning, "Something went wrong", &error.to_string()),
                }
            }
        } else {
            let _ = button.activate_action("win.navigate", Some(&"settings".to_variant()));
        }
    });
    let weak = Rc::downgrade(ui);
    ui.staying_header.connect_clicked(move |header| {
        let Some(ui) = weak.upgrade() else { return };
        let open = !ui.staying_list.reveals_child();
        ui.staying_list.set_reveal_child(open);
        ui.staying_chevron.set_icon_name(Some(if open { "pan-down-symbolic" } else { "pan-end-symbolic" }));
        header.update_state(&[gtk::accessible::State::Expanded(Some(open))]);
    });

    let events = ui.engine.borrow_mut().subscribe();
    let ui = ui.clone();
    glib::MainContext::default().spawn_local(async move {
        while let Ok(event) = events.recv().await {
            match event {
                EngineState::RestructurePlan(plan) => ui.show_plan(plan),
                EngineState::RestructureApplyResult(result) => ui.finish(result),
                EngineState::Error { kind, message, .. } if ui.operation.get().is_some() => {
                    ui.operation.set(None);
                    ui.stopping.set(false);
                    ui.set_notice(Tone::Warning, "Something went wrong", &format!("{kind}: {message}"));
                    ui.refresh();
                }
                EngineState::Exited => {
                    ui.operation.set(None);
                    ui.stopping.set(false);
                    ui.set_notice(Tone::Warning, "Engine stopped", "The FileID engine is restarting. Plan again when it's back.");
                    ui.refresh();
                }
                _ => {}
            }
        }
    });
}
