// Library bulk rename and bulk tag for the selected files, with "Undo last
// rename" and "Undo last tags" (macOS `BulkRenameSheet`, `BulkTagSheet`, and
// the Library toolbar). Renames go through `renameFiles`, tags through
// `applyTags`; both undo paths use the same commands and the journals in
// `library_bulk_model`.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use gtk::glib::clone;

use super::library_bulk_model as model;
use super::util::{icon_for_kind, icon_paintable};
use crate::engine_client::{EngineClient, EngineEvent, FileRow, read_db, texture_from_decoded};
use fileid_engine::ipc::{
    ApplyTagsPayload, BulkActionResult, CommandPayload, RenameEntry, RenameFilesPayload, TagMode,
};

type Engine = Rc<RefCell<EngineClient>>;

struct Subscription(crate::engine_client::EventReceiver);

impl Drop for Subscription {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// Sends `payload` and waits for the `action` result that covers `ids`
/// (any result when the engine names no files). `None` when the engine is
/// not ready, restarts, or stays silent for `timeout_secs`.
async fn bulk_result(
    engine: &Engine,
    payload: CommandPayload,
    action: &str,
    ids: &HashSet<i64>,
    timeout_secs: u32,
) -> Option<BulkActionResult> {
    let subscription = Subscription(engine.borrow_mut().subscribe());
    engine.borrow_mut().send(payload).ok()?;
    let closer = subscription.0.closer();
    glib::timeout_add_seconds_local_once(timeout_secs, move || {
        closer.close();
    });
    loop {
        let Ok(event) = subscription.0.recv().await else { return None };
        match &*event {
            EngineEvent::BulkActionResult(result) if result.action == action => {
                let reported: HashSet<i64> =
                    result.messages.iter().filter_map(|m| m.file_id).collect();
                if reported.is_empty() || !reported.is_disjoint(ids) {
                    return Some(result.clone());
                }
            }
            EngineEvent::Exited => return None,
            _ => {}
        }
    }
}

fn first_error(result: &BulkActionResult) -> Option<String> {
    result
        .messages
        .iter()
        .find(|m| !m.ok)
        .and_then(|m| m.message.clone())
}

fn landed(result: &BulkActionResult, ids: &HashSet<i64>) -> HashSet<i64> {
    result
        .messages
        .iter()
        .filter(|m| m.ok)
        .filter_map(|m| m.file_id)
        .filter(|id| ids.contains(id))
        .collect()
}

fn id_list(ids: &[i64]) -> String {
    ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
}

// ── Toolbar ──────────────────────────────────────────────────────────────────

pub(super) struct BulkBar {
    pub root: gtk::Box,
    engine: Engine,
    selected: Rc<dyn Fn() -> Vec<FileRow>>,
    reload: Rc<dyn Fn()>,
    clear_selection: Rc<dyn Fn()>,
    rename: gtk::Button,
    tag: gtk::Button,
    undo_rename: gtk::Button,
    undo_tags: gtk::Button,
    status: gtk::Label,
    busy: Cell<bool>,
}

impl BulkBar {
    pub fn new(
        engine: Engine,
        selected: Rc<dyn Fn() -> Vec<FileRow>>,
        reload: Rc<dyn Fn()>,
        clear_selection: Rc<dyn Fn()>,
    ) -> Rc<Self> {
        let rename = crate::v3::secondary("Rename selected", false);
        rename.set_tooltip_text(Some("Apply FileID's suggested names to the chosen files"));
        let tag = crate::v3::secondary("Tag selected", false);
        tag.set_tooltip_text(Some(
            "Click files to choose them first, then add tags to all of them",
        ));
        let undo_rename =
            crate::v3::accent("Undo last rename", crate::shell::status::Tone::Gold, false);
        undo_rename.set_tooltip_text(Some("Reverse the most recent rename batch"));
        let undo_tags =
            crate::v3::accent("Undo last tags", crate::shell::status::Tone::Gold, false);
        undo_tags.set_tooltip_text(Some(
            "Remove only the tags FileID added in the most recent bulk tag batch",
        ));
        let status = crate::v3::meta("");
        status.set_visible(false);
        status.set_wrap(true);
        status.set_xalign(0.0);

        let root = crate::v3::hbox(8);
        for widget in [&rename, &tag, &undo_rename, &undo_tags] {
            root.append(widget);
        }
        root.append(&status);

        let bar = Rc::new(Self {
            root,
            engine,
            selected,
            reload,
            clear_selection,
            rename,
            tag,
            undo_rename,
            undo_tags,
            status,
            busy: Cell::new(false),
        });
        bar.selection_changed(0);
        bar.refresh_undo();
        bar.rename.connect_clicked(clone!(
            #[strong]
            bar,
            move |button| open_rename_sheet(&bar, button)
        ));
        bar.tag.connect_clicked(clone!(
            #[strong]
            bar,
            move |button| open_tag_sheet(&bar, button)
        ));
        bar.undo_rename.connect_clicked(clone!(
            #[strong]
            bar,
            move |_| run_undo_rename(&bar)
        ));
        bar.undo_tags.connect_clicked(clone!(
            #[strong]
            bar,
            move |_| run_undo_tags(&bar)
        ));
        bar
    }

    pub fn selection_changed(&self, count: usize) {
        self.rename.set_sensitive(count > 0);
        self.tag.set_sensitive(count > 0);
    }

    pub fn refresh_undo(&self) {
        let has_rename = model::rename_journal_path()
            .and_then(|path| model::load_rename_batch(&path, now_secs()))
            .is_some();
        let has_tags = model::tag_journal_path()
            .and_then(|path| model::load_tag_batch(&path))
            .is_some();
        self.undo_rename.set_visible(has_rename);
        self.undo_tags.set_visible(has_tags);
    }

    fn say(&self, text: &str) {
        self.status.set_label(text);
        self.status.set_visible(!text.is_empty());
    }

    fn finished(&self) {
        self.busy.set(false);
        self.refresh_undo();
        (self.reload)();
    }
}

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

// ── Rename sheet ─────────────────────────────────────────────────────────────

struct RenameSheet {
    bar: Rc<BulkBar>,
    ids: Vec<i64>,
    plans: RefCell<Vec<model::RenamePlan>>,
    checked: RefCell<HashSet<i64>>,
    list: gtk::Box,
    count: gtk::Label,
    apply: gtk::Button,
    all: gtk::Button,
    none: gtk::Button,
    note: gtk::Label,
    status: gtk::Label,
    stack: gtk::Stack,
    in_flight: Cell<bool>,
    generation: Cell<u64>,
}

fn open_rename_sheet(bar: &Rc<BulkBar>, anchor: &gtk::Button) {
    let rows = (bar.selected)();
    if rows.is_empty() {
        return;
    }
    let dialog = adw::Dialog::new();
    dialog.set_title("Suggested names");
    dialog.set_content_width(720);
    dialog.set_content_height(520);

    let head = crate::v3::vbox(2);
    head.append(&crate::v3::label("Suggested names", &["title-3"]));
    let blurb = crate::v3::wrapped(
        "FileID suggested a clearer name for each file based on what it shows. Tick the ones you want, then press Rename.",
        &["meta"],
    );
    head.append(&blurb);
    let done = crate::v3::primary("Done", false);
    let header = crate::v3::hbox(12);
    header.append(&head);
    head.set_hexpand(true);
    header.append(&done);

    let count = crate::v3::mono("", "v3-mono-11");
    let all = crate::v3::secondary("Select all", false);
    let none = crate::v3::secondary("Clear", false);
    let apply = crate::v3::primary("Rename", false);
    let actions = crate::v3::hbox(8);
    actions.append(&count);
    actions.append(&crate::v3::spacer());
    actions.append(&all);
    actions.append(&none);
    actions.append(&apply);

    let status = crate::v3::meta("");
    status.set_wrap(true);
    status.set_xalign(0.0);
    status.set_visible(false);
    let note = crate::v3::meta("");
    note.set_wrap(true);
    note.set_xalign(0.0);
    note.set_visible(false);

    let list = crate::v3::vbox(4);
    let empty = adw::StatusPage::builder()
        .icon_name("document-edit-symbolic")
        .title("No suggested names yet")
        .description(
            "Describe your files first (Describe Files page). Suggested names appear here.",
        )
        .vexpand(true)
        .build();
    let stack = gtk::Stack::new();
    stack.add_named(&crate::v3::scroller(&list), Some("list"));
    stack.add_named(&empty, Some("empty"));
    stack.set_visible_child_name("empty");
    stack.set_vexpand(true);

    let body = crate::v3::vbox(10);
    body.set_margin_top(16);
    body.set_margin_bottom(16);
    body.set_margin_start(16);
    body.set_margin_end(16);
    body.append(&header);
    body.append(&actions);
    body.append(&note);
    body.append(&status);
    body.append(&stack);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&body));
    dialog.set_child(Some(&toolbar));

    let sheet = Rc::new(RenameSheet {
        bar: bar.clone(),
        ids: rows.iter().map(|row| row.id).collect(),
        plans: RefCell::new(Vec::new()),
        checked: RefCell::new(HashSet::new()),
        list,
        count,
        apply: apply.clone(),
        all: all.clone(),
        none: none.clone(),
        note,
        status,
        stack,
        in_flight: Cell::new(false),
        generation: Cell::new(0),
    });
    done.connect_clicked(clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    all.connect_clicked(clone!(
        #[strong]
        sheet,
        move |_| {
            let ids = sheet
                .plans
                .borrow()
                .iter()
                .map(|plan| plan.file_id)
                .collect();
            sheet.checked.replace(ids);
            sheet.rebuild();
        }
    ));
    none.connect_clicked(clone!(
        #[strong]
        sheet,
        move |_| {
            sheet.checked.borrow_mut().clear();
            sheet.rebuild();
        }
    ));
    apply.connect_clicked(clone!(
        #[strong]
        sheet,
        move |button| sheet.attempt_apply(button)
    ));
    dialog.connect_closed(clone!(
        #[strong]
        sheet,
        move |_| {
            sheet.generation.set(sheet.generation.get().wrapping_add(1));
            sheet.bar.refresh_undo();
        }
    ));
    sheet.load(true);
    dialog.present(Some(anchor));
}

impl RenameSheet {
    /// Re-reads the chosen files' current paths and suggestions and plans their names.
    fn load(self: &Rc<Self>, select_all: bool) {
        let ids = self.ids.clone();
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        let rx = read_db(move |conn| {
            let mut candidates = Vec::new();
            let sql = format!(
                "SELECT id, path_text, vlm_proposed_name, kind FROM files WHERE failed = 0 AND id IN ({}) ORDER BY id",
                id_list(&ids)
            );
            if let Ok(mut stmt) = conn.prepare(&sql)
                && let Ok(rows) = stmt.query_map([], |row| {
                    Ok(model::Candidate {
                        file_id: row.get(0)?,
                        path: row.get(1)?,
                        proposed: row.get(2)?,
                        kind: row.get(3)?,
                    })
                })
            {
                candidates.extend(rows.flatten());
            }
            let (plans, unnamed) = model::plan_renames(&candidates, model::taken_on_disk);
            (plans, unnamed, candidates.len())
        });
        let this = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let Ok((plans, unnamed, found)) = rx.recv().await else {
                return;
            };
            if this.generation.get() != generation {
                return;
            }
            let missing = this.ids.len().saturating_sub(found);
            let skipped = unnamed + missing;
            this.note.set_label(&format!(
                "{skipped} of the {} chosen file{} {} no new suggested name, so {} left out.",
                this.ids.len(),
                if this.ids.len() == 1 { "" } else { "s" },
                if skipped == 1 { "has" } else { "have" },
                if skipped == 1 { "it is" } else { "they are" },
            ));
            this.note.set_visible(skipped > 0 && !plans.is_empty());
            if select_all {
                this.checked
                    .replace(plans.iter().map(|plan| plan.file_id).collect());
            } else {
                let keep: HashSet<i64> = plans.iter().map(|plan| plan.file_id).collect();
                this.checked.borrow_mut().retain(|id| keep.contains(id));
            }
            this.plans.replace(plans);
            this.rebuild();
        });
    }

    fn rebuild(self: &Rc<Self>) {
        crate::v3::clear(&self.list);
        let plans = self.plans.borrow().clone();
        self.stack
            .set_visible_child_name(if plans.is_empty() { "empty" } else { "list" });
        for plan in &plans {
            self.list.append(&self.row(plan));
        }
        self.sync();
    }

    fn sync(&self) {
        let total = self.plans.borrow().len();
        let chosen = self.checked.borrow().len();
        self.count
            .set_label(&format!("{chosen} of {total} selected"));
        self.apply
            .set_label(&model::rename_label(chosen, self.in_flight.get()));
        self.apply
            .set_sensitive(chosen > 0 && !self.in_flight.get());
        self.all.set_sensitive(!self.in_flight.get());
        self.none.set_sensitive(!self.in_flight.get());
    }

    fn row(self: &Rc<Self>, plan: &model::RenamePlan) -> gtk::Box {
        let on = self.checked.borrow().contains(&plan.file_id);
        let check = gtk::CheckButton::new();
        check.set_active(on);
        check.update_property(&[gtk::accessible::Property::Label(&format!(
            "Rename {}",
            plan.old_name()
        ))]);

        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Cover)
            .width_request(64)
            .height_request(64)
            .css_classes(["tile-thumb"])
            .build();
        match plan.kind.as_str() {
            "image" | "video" => {
                let video = plan.kind == "video";
                let rx = if video {
                    self.bar
                        .engine
                        .borrow()
                        .request_video_thumbnail(plan.old_path.clone(), 128)
                } else {
                    self.bar
                        .engine
                        .borrow()
                        .request_scaled_thumbnail(plan.old_path.clone(), 128)
                };
                let weak = picture.downgrade();
                glib::MainContext::default().spawn_local(async move {
                    if let (Ok(Some(decoded)), Some(picture)) = (rx.recv().await, weak.upgrade()) {
                        picture.set_paintable(Some(&texture_from_decoded(decoded)));
                    }
                });
            }
            _ => {
                picture.set_paintable(icon_paintable(icon_for_kind(&plan.kind), 32).as_ref());
            }
        }

        let old = crate::v3::mono(&plan.old_name(), "v3-mono-11");
        old.add_css_class("dim-label");
        old.set_xalign(0.0);
        let new = crate::v3::mono(&format!("→ {}", plan.new_name()), "v3-mono-11");
        new.add_css_class("gold-accent");
        new.set_xalign(0.0);
        let names = crate::v3::vbox(2);
        names.set_hexpand(true);
        names.set_valign(gtk::Align::Center);
        names.append(&old);
        names.append(&new);

        let row = crate::v3::list_row(10);
        row.append(&check);
        row.append(&picture);
        row.append(&names);

        let id = plan.file_id;
        let sheet = Rc::downgrade(self);
        check.connect_toggled(move |check| {
            let Some(sheet) = sheet.upgrade() else { return };
            if check.is_active() {
                sheet.checked.borrow_mut().insert(id);
            } else {
                sheet.checked.borrow_mut().remove(&id);
            }
            sheet.sync();
        });
        let click = gtk::GestureClick::new();
        click.connect_released(clone!(
            #[weak]
            check,
            move |_, _, _, _| check.set_active(!check.is_active())
        ));
        names.add_controller(click);
        row
    }

    fn attempt_apply(self: &Rc<Self>, anchor: &gtk::Button) {
        let chosen = self.checked.borrow().len();
        if chosen == 0 || self.in_flight.get() {
            return;
        }
        if chosen < model::LARGE_BATCH {
            self.apply_selected();
            return;
        }
        let dialog = adw::AlertDialog::new(
            Some(&format!("Rename {chosen} files?")),
            Some(&model::rename_confirm_body(chosen)),
        );
        let confirm = format!("Rename {chosen} files");
        dialog.add_responses(&[("cancel", "Cancel"), ("rename", confirm.as_str())]);
        dialog.set_response_appearance("rename", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let this = self.clone();
        dialog.connect_response(None, move |_, response| {
            if response == "rename" {
                this.apply_selected();
            }
        });
        dialog.present(Some(anchor));
    }

    fn apply_selected(self: &Rc<Self>) {
        let checked = self.checked.borrow().clone();
        let chosen: Vec<model::RenamePlan> = self
            .plans
            .borrow()
            .iter()
            .filter(|plan| checked.contains(&plan.file_id))
            .cloned()
            .collect();
        if chosen.is_empty() || self.in_flight.replace(true) {
            return;
        }
        self.status.set_visible(false);
        self.sync();
        let this = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let (renamed, failed, error) = rename_plans(&this.bar.engine, &chosen).await;
            if let Some(path) = model::rename_journal_path() {
                model::save_rename_batch(&path, &renamed, now_secs());
            }
            this.in_flight.set(false);
            this.status.set_label(&model::rename_status(
                renamed.len(),
                failed,
                error.as_deref(),
            ));
            this.status.set_visible(true);
            this.bar.refresh_undo();
            (this.bar.reload)();
            this.load(true);
        });
    }
}

/// Sends the renames in chunks. Returns what landed, how many didn't, and the first reason.
async fn rename_plans(
    engine: &Engine,
    plans: &[model::RenamePlan],
) -> (Vec<model::RenameOutcome>, usize, Option<String>) {
    let mut renamed = Vec::new();
    let mut failed = 0;
    let mut error = None;
    for (index, chunk) in plans.chunks(model::RENAME_CHUNK).enumerate() {
        let ids: HashSet<i64> = chunk.iter().map(|plan| plan.file_id).collect();
        let renames = chunk
            .iter()
            .map(|plan| RenameEntry {
                file_id: plan.file_id,
                new_name: plan.new_name(),
            })
            .collect();
        let timeout = 60 + (chunk.len() / 4) as u32;
        let result = bulk_result(
            engine,
            CommandPayload::RenameFiles(RenameFilesPayload { renames }),
            "renameFiles",
            &ids,
            timeout,
        )
        .await;
        let Some(result) = result else {
            failed += plans.len() - index * model::RENAME_CHUNK;
            error.get_or_insert_with(|| "FileID didn't answer. Try again in a moment.".to_owned());
            break;
        };
        let done = landed(&result, &ids);
        renamed.extend(
            chunk
                .iter()
                .filter(|plan| done.contains(&plan.file_id))
                .map(|plan| model::RenameOutcome {
                    file_id: plan.file_id,
                    old_path: plan.old_path.clone(),
                    new_path: plan.new_path.clone(),
                }),
        );
        failed += chunk.len() - done.len();
        if error.is_none() {
            error = first_error(&result);
        }
    }
    (renamed, failed, error)
}

fn run_undo_rename(bar: &Rc<BulkBar>) {
    let Some(path) = model::rename_journal_path() else {
        return;
    };
    let Some(batch) = model::load_rename_batch(&path, now_secs()) else {
        bar.refresh_undo();
        return;
    };
    if bar.busy.replace(true) {
        return;
    }
    bar.say("Putting names back…");
    let ids: Vec<i64> = batch.iter().map(|entry| entry.file_id).collect();
    let rx = read_db(move |conn| {
        let mut current: HashMap<i64, String> = HashMap::new();
        if let Ok(mut stmt) = conn.prepare(&format!(
            "SELECT id, path_text FROM files WHERE id IN ({})",
            id_list(&ids)
        )) && let Ok(rows) = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        }) {
            current.extend(rows.flatten());
        }
        let ready: Vec<model::RenameOutcome> = batch
            .iter()
            .rev()
            .filter(|entry| {
                model::undo_blocker(
                    entry,
                    current.get(&entry.file_id).map(String::as_str),
                    |p: &Path| p.exists(),
                    |a: &Path, b: &Path| matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y),
                )
                .is_none()
            })
            .cloned()
            .collect();
        (ready, batch.len())
    });
    let bar = bar.clone();
    glib::MainContext::default().spawn_local(async move {
        let Ok((ready, total)) = rx.recv().await else {
            bar.busy.set(false);
            return;
        };
        let mut undone = 0;
        let mut failed = 0;
        for (index, chunk) in ready.chunks(model::RENAME_CHUNK).enumerate() {
            let ids: HashSet<i64> = chunk.iter().map(|entry| entry.file_id).collect();
            let renames = chunk
                .iter()
                .map(|entry| RenameEntry {
                    file_id: entry.file_id,
                    new_name: Path::new(&entry.old_path)
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                })
                .collect();
            let result = bulk_result(
                &bar.engine,
                CommandPayload::RenameFiles(RenameFilesPayload { renames }),
                "renameFiles",
                &ids,
                60 + (chunk.len() / 4) as u32,
            )
            .await;
            let Some(result) = result else {
                failed += ready.len() - index * model::RENAME_CHUNK;
                break;
            };
            let back = landed(&result, &ids).len();
            undone += back;
            failed += chunk.len() - back;
        }
        let skipped = total - ready.len();
        if failed == 0 && skipped == 0 {
            model::clear_journal(&path);
        }
        bar.say(&model::undo_rename_status(undone, skipped, failed));
        bar.finished();
    });
}

// ── Tag sheet ────────────────────────────────────────────────────────────────

fn open_tag_sheet(bar: &Rc<BulkBar>, anchor: &gtk::Button) {
    let rows = (bar.selected)();
    if rows.is_empty() {
        return;
    }
    let ids: Vec<i64> = rows.iter().map(|row| row.id).collect();
    let dialog = adw::Dialog::new();
    dialog.set_title(&model::tag_title(ids.len()));
    dialog.set_content_width(520);

    let entry = crate::v3::text_field("e.g. Vacation, Family, Important");
    entry.set_hexpand(true);
    entry.update_property(&[gtk::accessible::Property::Label(
        "Tags, separated by commas",
    )]);
    let status = crate::v3::meta("");
    status.set_wrap(true);
    status.set_xalign(0.0);
    status.set_visible(false);
    let apply = crate::v3::primary("Apply", false);
    apply.set_sensitive(false);
    let cancel = crate::v3::secondary("Cancel", false);
    let buttons = crate::v3::hbox(8);
    buttons.append(&crate::v3::spacer());
    buttons.append(&cancel);
    buttons.append(&apply);

    let body = crate::v3::vbox(12);
    body.set_margin_top(16);
    body.set_margin_bottom(16);
    body.set_margin_start(16);
    body.set_margin_end(16);
    body.append(&crate::v3::label(
        &model::tag_title(ids.len()),
        &["title-3"],
    ));
    body.append(&crate::v3::wrapped(
        "Comma-separated. Existing tags on each file are preserved — only new tags get added.",
        &["meta"],
    ));
    body.append(&entry);
    body.append(&status);
    body.append(&buttons);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&body));
    dialog.set_child(Some(&toolbar));

    entry.connect_changed(clone!(
        #[weak]
        apply,
        move |entry| apply.set_sensitive(!model::parse_tags(entry.text().as_str()).is_empty())
    ));
    cancel.connect_clicked(clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    let in_flight = Rc::new(Cell::new(false));
    let start: Rc<dyn Fn()> = Rc::new(clone!(
        #[strong]
        bar,
        #[strong]
        in_flight,
        #[weak]
        dialog,
        #[weak]
        entry,
        #[weak]
        apply,
        #[weak]
        status,
        move || {
            let tags = model::parse_tags(entry.text().as_str());
            if tags.is_empty() || in_flight.replace(true) {
                return;
            }
            apply.set_label("Tagging…");
            apply.set_sensitive(false);
            status.set_visible(false);
            let (bar, in_flight, ids) = (bar.clone(), in_flight.clone(), ids.clone());
            glib::MainContext::default().spawn_local(async move {
                let (line, clean) = tag_files(&bar, &ids, &tags).await;
                in_flight.set(false);
                apply.set_label("Apply");
                apply.set_sensitive(true);
                bar.refresh_undo();
                if clean {
                    bar.say(&line);
                    (bar.clear_selection)();
                    (bar.reload)();
                    dialog.close();
                } else {
                    status.set_label(&line);
                    status.set_visible(true);
                    (bar.reload)();
                }
            });
        }
    ));
    apply.connect_clicked(clone!(
        #[strong]
        start,
        move |_| start()
    ));
    entry.connect_activate(clone!(
        #[strong]
        start,
        move |_| start()
    ));
    dialog.set_focus(Some(&entry));
    dialog.present(Some(anchor));
}

/// Adds `tags` to `ids` and journals what was actually new. Returns the
/// status line and whether nothing failed.
async fn tag_files(bar: &Rc<BulkBar>, ids: &[i64], tags: &[String]) -> (String, bool) {
    let read_ids = ids.to_vec();
    let existing = read_db(move |conn| {
        let mut have: HashMap<i64, HashSet<String>> = HashMap::new();
        if let Ok(mut stmt) = conn.prepare(&format!(
            "SELECT file_id, tag FROM tags WHERE source = 'user' AND file_id IN ({})",
            id_list(&read_ids)
        )) && let Ok(rows) = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        }) {
            for (file, tag) in rows.flatten() {
                have.entry(file).or_default().insert(tag);
            }
        }
        have
    })
    .recv()
    .await
    .unwrap_or_default();
    let id_set: HashSet<i64> = ids.iter().copied().collect();
    let result = bulk_result(
        &bar.engine,
        CommandPayload::ApplyTags(ApplyTagsPayload {
            file_ids: ids.to_vec(),
            tags: tags.to_vec(),
            mode: TagMode::Add,
        }),
        "applyTags",
        &id_set,
        30,
    )
    .await;
    let Some(result) = result else {
        return (
            "FileID didn't finish adding tags. Try again in a moment.".to_owned(),
            false,
        );
    };
    let ok = landed(&result, &id_set);
    let empty = HashSet::new();
    let mut outcomes = Vec::new();
    let mut unchanged = 0;
    for id in ids.iter().filter(|id| ok.contains(id)) {
        let added = model::new_tags(tags, existing.get(id).unwrap_or(&empty));
        if added.is_empty() {
            unchanged += 1;
        } else {
            outcomes.push(model::TagOutcome {
                file_id: *id,
                tags: added,
            });
        }
    }
    let failed = ids.len() - ok.len();
    if let Some(path) = model::tag_journal_path() {
        model::record_tag_batch(&path, &outcomes, failed);
    }
    (
        model::tag_status(
            outcomes.len(),
            unchanged,
            failed,
            first_error(&result).as_deref(),
        ),
        failed == 0,
    )
}

fn run_undo_tags(bar: &Rc<BulkBar>) {
    let Some(path) = model::tag_journal_path() else {
        return;
    };
    let Some(batch) = model::load_tag_batch(&path) else {
        bar.refresh_undo();
        return;
    };
    if bar.busy.replace(true) {
        return;
    }
    bar.say("Removing tags…");
    let bar = bar.clone();
    glib::MainContext::default().spawn_local(async move {
        let mut by_tag: Vec<(String, Vec<i64>)> = Vec::new();
        for outcome in &batch {
            for tag in &outcome.tags {
                match by_tag.iter_mut().find(|(name, _)| name == tag) {
                    Some((_, files)) => files.push(outcome.file_id),
                    None => by_tag.push((tag.clone(), vec![outcome.file_id])),
                }
            }
        }
        let mut undone: HashSet<i64> = HashSet::new();
        let mut failed: HashSet<i64> = HashSet::new();
        let mut error = None;
        for (tag, files) in by_tag {
            let ids: HashSet<i64> = files.iter().copied().collect();
            let result = bulk_result(
                &bar.engine,
                CommandPayload::ApplyTags(ApplyTagsPayload {
                    file_ids: files,
                    tags: vec![tag],
                    mode: TagMode::Remove,
                }),
                "applyTags",
                &ids,
                30,
            )
            .await;
            let Some(result) = result else {
                failed.extend(ids);
                error.get_or_insert_with(|| "FileID didn't answer".to_owned());
                break;
            };
            let ok = landed(&result, &ids);
            failed.extend(ids.difference(&ok));
            undone.extend(ok);
            if error.is_none() {
                error = first_error(&result);
            }
        }
        undone.retain(|id| !failed.contains(id));
        if failed.is_empty() {
            model::clear_journal(&path);
        }
        bar.say(&model::undo_tag_status(
            undone.len(),
            failed.len(),
            error.as_deref(),
        ));
        bar.finished();
    });
}
