// Describe Files ▸ Name style (macOS smartNamesCard "Name style" + Preview
// names): the saved style, a proposeNames preview of the Library selection or
// the files with suggestions, editable names, and Rename through renameFiles
// with an Undo toast.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use fileid_engine::ipc::{
    CatalogHit, CatalogNaming, CatalogRequest, CatalogRequestPayload, CommandPayload, RenameEntry,
    RenameFilesPayload,
};
use gtk::glib;

use super::chat_handoff;
use super::util::glass_card;
use super::workbench_state::{catalog_request, Requests};
use crate::app_settings::{self, NamingSettings};
use crate::engine_client::{EngineClient, EngineState};
use crate::spring::{animate_with, animations_enabled, Spring};

pub(super) const STYLES: [(&str, &str); 3] =
    [("short", "Short"), ("descriptive", "Descriptive"), ("dateFirst", "Date first")];
pub(super) const MIN_LENGTH: u32 = 4;
pub(super) const MAX_LENGTH: u32 = 120;
pub(super) const MAX_FILES: usize = 1_000;
pub(super) const PREVIEW_DELAY: Duration = Duration::from_millis(300);
const SLOT: &str = "names";
const NOT_READY: &str = "FileID is still getting ready. Try again in a moment.";

pub(super) fn date_toggle_enabled(style: &str) -> bool {
    style != "dateFirst"
}

pub(super) fn naming(settings: &NamingSettings) -> CatalogNaming {
    let style = STYLES
        .iter()
        .find(|(id, _)| *id == settings.style)
        .map_or("short", |(id, _)| id);
    CatalogNaming {
        style: style.to_owned(),
        max_length: settings.max_length.clamp(MIN_LENGTH, MAX_LENGTH),
        include_people: settings.include_people,
        include_event: settings.include_event,
        include_date: settings.include_date && date_toggle_enabled(style),
    }
}

/// The Library selection when there is one, otherwise `fallback`;
/// deduplicated and capped at the engine's 1,000.
pub(super) fn file_ids(selection: &[i64], fallback: &[i64]) -> Vec<i64> {
    let source = if selection.is_empty() { fallback } else { selection };
    let mut seen = HashSet::new();
    source
        .iter()
        .copied()
        .filter(|id| *id > 0 && seen.insert(*id))
        .take(MAX_FILES)
        .collect()
}

pub(super) fn request(file_ids: Vec<i64>, naming: CatalogNaming) -> CatalogRequest {
    let mut request = catalog_request("proposeNames");
    request.limit = None;
    request.file_ids = Some(file_ids);
    request.naming = Some(naming);
    request
}

pub(super) fn reason_label(reason: &str) -> &'static str {
    match reason {
        "chapter" => "From your chapter",
        "proposedName" => "From the AI name",
        "description" => "From the description",
        "tags" => "From tags",
        "event" => "From your event",
        "unchanged" => "Already well named",
        "budget" => "Too long for the length limit",
        "reserved" => "Not allowed as a file name",
        "collision" => "No free name in this folder",
        _ => "Nothing to name it from yet",
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Row {
    pub file_id: i64,
    pub path: String,
    pub proposed: String,
    pub reason: String,
    pub edited: String,
    pub checked: bool,
}

impl Row {
    pub fn can_check(&self) -> bool {
        !self.proposed.is_empty()
    }
    pub fn current_name(&self) -> &str {
        fileid_engine::util::display::basename(&self.path)
    }
}

/// Rows for a reply. A file unticked in the last preview stays unticked;
/// rows without a proposed name can't be ticked.
pub(super) fn rows(hits: &[CatalogHit], previous: &[Row]) -> Vec<Row> {
    let unticked: HashSet<i64> = previous
        .iter()
        .filter(|row| row.can_check() && !row.checked)
        .map(|row| row.file_id)
        .collect();
    hits.iter()
        .filter(|hit| hit.kind == "proposedName")
        .map(|hit| Row {
            file_id: hit.file_id,
            path: hit.path.clone(),
            proposed: hit.text.clone(),
            reason: hit.evidence_id.clone().unwrap_or_else(|| "noEvidence".to_owned()),
            edited: hit.text.clone(),
            checked: !hit.text.is_empty() && !unticked.contains(&hit.file_id),
        })
        .collect()
}

pub(super) fn valid_name(text: &str) -> Option<String> {
    let name = text.trim();
    (!name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\0'])).then(|| name.to_owned())
}

pub(super) fn renames(rows: &[Row]) -> Vec<RenameEntry> {
    rows.iter()
        .filter(|row| row.checked && row.can_check())
        .filter_map(|row| {
            let name = valid_name(&row.edited)?;
            (name != row.current_name()).then_some(RenameEntry { file_id: row.file_id, new_name: name })
        })
        .collect()
}

pub(super) fn accessible_label(row: &Row) -> String {
    let reason = reason_label(&row.reason);
    let because = reason[..1].to_lowercase() + &reason[1..];
    if row.can_check() {
        format!("{}, rename to {}, {because}", row.current_name(), row.edited.trim())
    } else {
        format!("{}, keeps its name, {because}", row.current_name())
    }
}

struct PendingRename {
    ids: HashSet<i64>,
    old: HashMap<i64, String>,
    undoing: bool,
}

struct NamingUi {
    engine: Rc<RefCell<EngineClient>>,
    overlay: adw::ToastOverlay,
    fallback: Box<dyn Fn() -> Vec<i64>>,
    requests: RefCell<Requests>,
    rows: RefCell<Vec<Row>>,
    store: gtk::gio::ListStore,
    animated: RefCell<HashSet<(usize, usize)>>,
    style: adw::ComboRow,
    length: adw::SpinRow,
    people: adw::SwitchRow,
    event: adw::SwitchRow,
    date: adw::SwitchRow,
    apply: gtk::Button,
    status: gtk::Label,
    view: gtk::ColumnView,
    shown: Cell<bool>,
    syncing: Cell<bool>,
    debounce: Cell<u64>,
    pending: RefCell<Option<PendingRename>>,
}

/// The "Name style" card. `fallback` names the files to preview when the
/// Library has no selection.
pub(super) fn build(
    engine: Rc<RefCell<EngineClient>>,
    overlay: adw::ToastOverlay,
    fallback: impl Fn() -> Vec<i64> + 'static,
) -> gtk::Box {
    new_ui(engine, overlay, fallback).0
}

fn new_ui(
    engine: Rc<RefCell<EngineClient>>,
    overlay: adw::ToastOverlay,
    fallback: impl Fn() -> Vec<i64> + 'static,
) -> (gtk::Box, Rc<NamingUi>) {
    let saved = app_settings::naming();
    let titles: Vec<&str> = STYLES.iter().map(|(_, title)| *title).collect();
    let style = adw::ComboRow::builder()
        .title("Style")
        .model(&gtk::StringList::new(&titles))
        .selected(STYLES.iter().position(|(id, _)| *id == saved.style).unwrap_or(0) as u32)
        .build();
    let length = adw::SpinRow::with_range(f64::from(MIN_LENGTH), f64::from(MAX_LENGTH), 1.0);
    length.set_title("Longest name");
    length.set_value(f64::from(saved.max_length));
    length.set_subtitle(&format!("{} characters", saved.max_length));
    let people = adw::SwitchRow::builder().title("Add confirmed people").active(saved.include_people).build();
    let event = adw::SwitchRow::builder().title("Add event").active(saved.include_event).build();
    let date = adw::SwitchRow::builder()
        .title("Add date")
        .active(saved.include_date)
        .sensitive(date_toggle_enabled(&saved.style))
        .build();
    let group = adw::PreferencesGroup::builder()
        .title("Name style")
        .description("How FileID builds new names from what it knows: chapters, descriptions, tags, events and the people you named. Nothing is renamed until you press Rename.")
        .build();
    for row in [style.upcast_ref::<gtk::Widget>(), length.upcast_ref(), people.upcast_ref(), event.upcast_ref(), date.upcast_ref()] {
        group.add(row);
    }

    let preview = gtk::Button::builder()
        .label("Preview names")
        .css_classes(["gold-button"])
        .tooltip_text("Show the new names before anything is renamed")
        .build();
    let apply = gtk::Button::builder().label("Rename").css_classes(["pill"]).sensitive(false).build();
    let actions = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(10).build();
    actions.append(&preview);
    actions.append(&apply);
    let status = gtk::Label::builder().xalign(0.0).wrap(true).css_classes(["dim-label"]).visible(false).build();

    let store = gtk::gio::ListStore::new::<glib::BoxedAnyObject>();
    let view = gtk::ColumnView::builder()
        .model(&gtk::NoSelection::new(Some(store.clone())))
        .show_row_separators(true)
        .visible(false)
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_height(0)
        .max_content_height(440)
        .propagate_natural_height(true)
        .child(&view)
        .build();

    let card = glass_card();
    card.append(&group);
    card.append(&actions);
    card.append(&status);
    card.append(&scroller);

    let ui = Rc::new(NamingUi {
        engine,
        overlay,
        fallback: Box::new(fallback),
        requests: RefCell::default(),
        rows: RefCell::default(),
        store,
        animated: RefCell::default(),
        style,
        length,
        people,
        event,
        date,
        apply,
        status,
        view,
        shown: Cell::new(false),
        syncing: Cell::new(false),
        debounce: Cell::new(0),
        pending: RefCell::default(),
    });
    add_columns(&ui);
    wire(&ui, &preview);
    subscribe(&ui);
    (card, ui)
}

fn settings(ui: &NamingUi) -> NamingSettings {
    NamingSettings {
        style: STYLES.get(ui.style.selected() as usize).map_or("short", |(id, _)| id).to_owned(),
        max_length: (ui.length.value().round() as u32).clamp(MIN_LENGTH, MAX_LENGTH),
        include_people: ui.people.is_active(),
        include_event: ui.event.is_active(),
        include_date: ui.date.is_active(),
    }
}

fn wire(ui: &Rc<NamingUi>, preview: &gtk::Button) {
    let changed = {
        let ui = Rc::downgrade(ui);
        move || {
            if let Some(ui) = ui.upgrade() {
                style_changed(&ui);
            }
        }
    };
    let on_change = Rc::new(changed);
    let f = on_change.clone();
    ui.style.connect_selected_notify(move |_| f());
    let f = on_change.clone();
    ui.length.connect_value_notify(move |_| f());
    for switch in [&ui.people, &ui.event, &ui.date] {
        let f = on_change.clone();
        switch.connect_active_notify(move |_| f());
    }
    let weak = Rc::downgrade(ui);
    preview.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            run_preview(&ui);
        }
    });
    let weak = Rc::downgrade(ui);
    ui.apply.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            apply(&ui);
        }
    });
}

fn style_changed(ui: &Rc<NamingUi>) {
    if ui.syncing.get() {
        return;
    }
    let current = settings(ui);
    ui.date.set_sensitive(date_toggle_enabled(&current.style));
    ui.length.set_subtitle(&format!("{} characters", current.max_length));
    app_settings::remember_naming(&current);
    if !ui.shown.get() {
        return;
    }
    let generation = ui.debounce.get().wrapping_add(1);
    ui.debounce.set(generation);
    let weak = Rc::downgrade(ui);
    glib::timeout_add_local_once(PREVIEW_DELAY, move || {
        if let Some(ui) = weak.upgrade()
            && ui.debounce.get() == generation
        {
            run_preview(&ui);
        }
    });
}

fn set_status(ui: &NamingUi, text: &str) {
    ui.status.set_label(text);
    ui.status.set_visible(!text.is_empty());
}

fn run_preview(ui: &Rc<NamingUi>) {
    ui.shown.set(true);
    let ids = file_ids(&chat_handoff::selected_files(), &(ui.fallback)());
    if ids.is_empty() {
        set_status(ui, "Choose files in Library first, or describe some files so they have suggested names.");
        return;
    }
    let request = request(ids, naming(&settings(ui)));
    ui.requests.borrow_mut().begin(SLOT, &request.request_id);
    set_status(ui, "Working out names…");
    let sent = ui
        .engine
        .borrow_mut()
        .send(CommandPayload::CatalogRequest(Box::new(CatalogRequestPayload { request })));
    if sent.is_err() {
        ui.requests.borrow_mut().forget(SLOT);
        set_status(ui, NOT_READY);
    }
}

fn show_rows(ui: &Rc<NamingUi>, hits: &[CatalogHit]) {
    let next = rows(hits, &ui.rows.borrow());
    let ready = next.iter().filter(|row| row.can_check()).count();
    let keep = next.len() - ready;
    let count = next.len();
    ui.rows.replace(next);
    ui.animated.borrow_mut().clear();
    let items: Vec<glib::BoxedAnyObject> = (0..count).map(glib::BoxedAnyObject::new).collect();
    ui.store.splice(0, ui.store.n_items(), &items);
    ui.view.set_visible(count > 0);
    let mut text = format!("{} new {}", crate::shell::grouped(ready as u64), if ready == 1 { "name" } else { "names" });
    if keep > 0 {
        text.push_str(&format!(" · {} keep their names", crate::shell::grouped(keep as u64)));
    }
    set_status(ui, &format!("{text}."));
    sync_apply(ui);
}

fn sync_apply(ui: &NamingUi) {
    let count = renames(&ui.rows.borrow()).len();
    ui.apply.set_sensitive(count > 0 && ui.pending.borrow().is_none());
    ui.apply.set_label(&match count {
        0 => "Rename".to_owned(),
        1 => "Rename 1 file".to_owned(),
        n => format!("Rename {} files", crate::shell::grouped(n as u64)),
    });
}

fn apply(ui: &Rc<NamingUi>) {
    if ui.pending.borrow().is_some() {
        return;
    }
    let rows = ui.rows.borrow().clone();
    let entries = renames(&rows);
    if entries.is_empty() {
        return;
    }
    let old = rows.iter().map(|row| (row.file_id, row.current_name().to_owned())).collect();
    let ids = entries.iter().map(|entry| entry.file_id).collect();
    send_renames(ui, entries, PendingRename { ids, old, undoing: false });
}

fn send_renames(ui: &Rc<NamingUi>, renames: Vec<RenameEntry>, pending: PendingRename) {
    let sent = ui
        .engine
        .borrow_mut()
        .send(CommandPayload::RenameFiles(RenameFilesPayload { renames }));
    if sent.is_err() {
        set_status(ui, NOT_READY);
        return;
    }
    set_status(ui, if pending.undoing { "Putting names back…" } else { "Renaming…" });
    ui.pending.replace(Some(pending));
    sync_apply(ui);
}

fn renamed(ui: &Rc<NamingUi>, result: &fileid_engine::ipc::BulkActionResult) {
    let reported: HashSet<i64> = result.messages.iter().filter_map(|item| item.file_id).collect();
    let matches = ui
        .pending
        .borrow()
        .as_ref()
        .is_some_and(|pending| !reported.is_disjoint(&pending.ids) || reported.is_empty());
    if !matches {
        return;
    }
    let Some(pending) = ui.pending.take() else { return };
    let done: Vec<i64> = result
        .messages
        .iter()
        .filter(|item| item.ok)
        .filter_map(|item| item.file_id)
        .filter(|id| pending.ids.contains(id))
        .collect();
    let failed = pending.ids.len() - done.len();
    let first_error = result.messages.iter().find(|item| !item.ok).and_then(|item| item.message.clone());
    let toast = if pending.undoing {
        adw::Toast::builder()
            .title(format!("Put back {} names.", crate::shell::grouped(done.len() as u64)))
            .use_markup(false)
            .build()
    } else {
        let mut title = format!(
            "Renamed {} {}",
            crate::shell::grouped(done.len() as u64),
            if done.len() == 1 { "file" } else { "files" }
        );
        if failed > 0 {
            title.push_str(&format!(" · {} couldn't be renamed", crate::shell::grouped(failed as u64)));
            if let Some(error) = &first_error {
                title.push_str(&format!(": {error}"));
            }
        }
        let back: Vec<RenameEntry> = done
            .iter()
            .filter_map(|id| pending.old.get(id).map(|name| RenameEntry { file_id: *id, new_name: name.clone() }))
            .collect();
        let toast = adw::Toast::builder().title(title).use_markup(false).timeout(10).build();
        if !back.is_empty() {
            toast.set_button_label(Some("Undo"));
            let weak = Rc::downgrade(ui);
            toast.connect_button_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    let ids = back.iter().map(|entry| entry.file_id).collect();
                    send_renames(&ui, back.clone(), PendingRename { ids, old: HashMap::new(), undoing: true });
                }
            });
        }
        toast
    };
    ui.overlay.add_toast(toast);
    set_status(ui, "");
    sync_apply(ui);
    run_preview(ui);
}

fn subscribe(ui: &Rc<NamingUi>) {
    let rx = ui.engine.borrow_mut().subscribe();
    let weak = Rc::downgrade(ui);
    glib::MainContext::default().spawn_local(async move {
        while let Ok(event) = rx.recv().await {
            let Some(ui) = weak.upgrade() else { break };
            match event {
                EngineState::CatalogResponse(response) => {
                    if ui.requests.borrow_mut().accept(&response.request_id, true).as_deref() != Some(SLOT) {
                        continue;
                    }
                    if response.status == "ok" {
                        show_rows(&ui, &response.hits);
                    } else {
                        set_status(&ui, response.message.as_deref().unwrap_or("FileID couldn't suggest names right now."));
                    }
                }
                EngineState::BulkActionResult(result) if result.action == "renameFiles" => renamed(&ui, &result),
                EngineState::Failed(_) | EngineState::Exited => {
                    ui.requests.borrow_mut().clear();
                    if ui.pending.take().is_some() {
                        set_status(&ui, "FileID stopped before the renames were confirmed. Check the files before trying again.");
                    }
                    sync_apply(&ui);
                }
                _ => {}
            }
        }
    });
}

fn item_index(item: &gtk::ListItem) -> Option<usize> {
    item.item()
        .and_downcast::<glib::BoxedAnyObject>()
        .map(|object| *object.borrow::<usize>())
}

fn spring_in(ui: &NamingUi, key: (usize, usize), widget: &gtk::Widget) {
    if !ui.animated.borrow_mut().insert(key) || !animations_enabled() {
        return;
    }
    widget.set_opacity(0.0);
    let weak = widget.downgrade();
    let _ = animate_with(Spring::Tight, widget, 0.0, 1.0, move |value| {
        if let Some(widget) = weak.upgrade() {
            widget.set_opacity(value.clamp(0.0, 1.0));
        }
    });
}

fn column(
    ui: &Rc<NamingUi>,
    title: &str,
    expand: bool,
    setup: impl Fn(&Rc<NamingUi>, &gtk::ListItem) -> gtk::Widget + 'static,
    bind: impl Fn(&NamingUi, &Row, &gtk::Widget) + 'static,
) {
    let position = ui.view.columns().n_items() as usize;
    let factory = gtk::SignalListItemFactory::new();
    let weak = Rc::downgrade(ui);
    factory.connect_setup(move |_, item| {
        let (Some(ui), Some(item)) = (weak.upgrade(), item.downcast_ref::<gtk::ListItem>()) else { return };
        let widget = setup(&ui, item);
        item.set_child(Some(&widget));
    });
    let weak = Rc::downgrade(ui);
    factory.connect_bind(move |_, item| {
        let (Some(ui), Some(item)) = (weak.upgrade(), item.downcast_ref::<gtk::ListItem>()) else { return };
        let (Some(index), Some(widget)) = (item_index(item), item.child()) else { return };
        let Some(row) = ui.rows.borrow().get(index).cloned() else { return };
        ui.syncing.set(true);
        bind(&ui, &row, &widget);
        ui.syncing.set(false);
        widget.set_opacity(if row.can_check() { 1.0 } else { 0.5 });
        if row.can_check() {
            spring_in(&ui, (index, position), &widget);
        }
    });
    let column = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    column.set_expand(expand);
    column.set_resizable(true);
    ui.view.append_column(&column);
}

fn add_columns(ui: &Rc<NamingUi>) {
    column(
        ui,
        "Original",
        true,
        |_, _| {
            gtk::Label::builder()
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .build()
                .upcast()
        },
        |_, row, widget| {
            let Some(label) = widget.downcast_ref::<gtk::Label>() else { return };
            label.set_label(row.current_name());
            label.set_tooltip_text(Some(&row.path));
        },
    );
    column(
        ui,
        "Proposed",
        true,
        |ui, item| {
            let editable = gtk::EditableLabel::new("");
            editable.add_css_class("gold-accent");
            let weak = Rc::downgrade(ui);
            let item = item.downgrade();
            editable.connect_text_notify(move |editable| {
                let (Some(ui), Some(item)) = (weak.upgrade(), item.upgrade()) else { return };
                if ui.syncing.get() {
                    return;
                }
                let Some(index) = item_index(&item) else { return };
                let label = {
                    let mut rows = ui.rows.borrow_mut();
                    let Some(row) = rows.get_mut(index).filter(|row| row.can_check()) else { return };
                    row.edited = editable.text().to_string();
                    accessible_label(row)
                };
                editable.update_property(&[gtk::accessible::Property::Description(&label)]);
                sync_apply(&ui);
            });
            editable.upcast()
        },
        |_, row, widget| {
            let Some(editable) = widget.downcast_ref::<gtk::EditableLabel>() else { return };
            if row.can_check() {
                editable.set_text(&row.edited);
                editable.set_editable(true);
                editable.add_css_class("gold-accent");
                editable.remove_css_class("dim-label");
            } else {
                editable.set_text("Keeps its name");
                editable.set_editable(false);
                editable.remove_css_class("gold-accent");
                editable.add_css_class("dim-label");
            }
            editable.update_property(&[
                gtk::accessible::Property::Label(&format!("New name for {}", row.current_name())),
                gtk::accessible::Property::Description(&accessible_label(row)),
            ]);
        },
    );
    column(
        ui,
        "Reason",
        false,
        |_, _| gtk::Label::builder().css_classes(["chip", "reason-chip"]).build().upcast(),
        |_, row, widget| {
            if let Some(label) = widget.downcast_ref::<gtk::Label>() {
                label.set_label(reason_label(&row.reason));
            }
        },
    );
    column(
        ui,
        "Rename",
        false,
        |ui, item| {
            let check = gtk::CheckButton::builder().halign(gtk::Align::Center).build();
            let weak = Rc::downgrade(ui);
            let item = item.downgrade();
            check.connect_toggled(move |check| {
                let (Some(ui), Some(item)) = (weak.upgrade(), item.upgrade()) else { return };
                if ui.syncing.get() {
                    return;
                }
                let Some(index) = item_index(&item) else { return };
                if let Some(row) = ui.rows.borrow_mut().get_mut(index).filter(|row| row.can_check()) {
                    row.checked = check.is_active();
                }
                sync_apply(&ui);
            });
            check.upcast()
        },
        |_, row, widget| {
            let Some(check) = widget.downcast_ref::<gtk::CheckButton>() else { return };
            check.set_active(row.checked);
            check.set_sensitive(row.can_check());
            check.update_property(&[gtk::accessible::Property::Label(&accessible_label(row))]);
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(id: i64, name: &str, text: &str, reason: &str) -> CatalogHit {
        CatalogHit {
            file_id: id,
            path: format!("/photos/{name}"),
            kind: "proposedName".into(),
            text: text.into(),
            evidence_id: Some(reason.into()),
            start_seconds: None,
            page: None,
        }
    }

    #[test]
    fn reason_labels_cover_every_engine_reason() {
        let expected = [
            ("chapter", "From your chapter"),
            ("proposedName", "From the AI name"),
            ("description", "From the description"),
            ("tags", "From tags"),
            ("event", "From your event"),
            ("unchanged", "Already well named"),
            ("budget", "Too long for the length limit"),
            ("reserved", "Not allowed as a file name"),
            ("collision", "No free name in this folder"),
            ("noEvidence", "Nothing to name it from yet"),
        ];
        for (reason, label) in expected {
            assert_eq!(reason_label(reason), label);
        }
        let corpus = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../shared/test-corpus/naming/style-cases.json");
        let cases: serde_json::Value = serde_json::from_slice(&std::fs::read(corpus).unwrap()).unwrap();
        for case in cases.as_array().unwrap() {
            let reason = case["reason"].as_str().unwrap();
            assert!(expected.iter().any(|(known, _)| *known == reason), "{reason}");
        }
    }

    #[test]
    fn defaults_and_request() {
        let defaults = NamingSettings::default();
        assert_eq!(
            naming(&defaults),
            CatalogNaming {
                style: "short".into(),
                max_length: 40,
                include_people: true,
                include_event: true,
                include_date: false
            }
        );
        let loud = NamingSettings { style: "loud".into(), max_length: 900, ..defaults };
        assert_eq!((naming(&loud).style.as_str(), naming(&loud).max_length), ("short", 120));
        let request = request(vec![3, 4], naming(&NamingSettings::default()));
        assert_eq!(request.action, "proposeNames");
        assert_eq!(request.file_ids, Some(vec![3, 4]));
        assert_eq!(request.limit, None);
        assert_eq!(request.naming.as_ref().map(|n| n.max_length), Some(40));
        let wire = serde_json::to_value(&request).unwrap();
        assert_eq!(wire["naming"]["style"], "short");
        assert_eq!(PREVIEW_DELAY, Duration::from_millis(300));
    }

    #[test]
    fn date_first_disables_add_date() {
        assert!(!date_toggle_enabled("dateFirst"));
        assert!(date_toggle_enabled("short") && date_toggle_enabled("descriptive"));
        let date_first = NamingSettings { style: "dateFirst".into(), include_date: true, ..NamingSettings::default() };
        assert!(!naming(&date_first).include_date);
    }

    #[test]
    fn file_ids_prefer_the_selection_and_cap_at_one_thousand() {
        assert_eq!(file_ids(&[5, 5, 0, 6], &[9]), vec![5, 6]);
        assert_eq!(file_ids(&[], &[9, 8]), vec![9, 8]);
        assert_eq!(file_ids(&(1..=1_500).collect::<Vec<_>>(), &[]).len(), MAX_FILES);
    }

    #[test]
    fn only_rows_with_text_can_be_checked_and_unticked_rows_stay_unticked() {
        let mut keyword = hit(3, "x", "nope", "tags");
        keyword.kind = "keyword".into();
        let hits = [hit(1, "IMG_0001.jpg", "sam-baseball-hit.jpg", "proposedName"), hit(2, "Beach.jpg", "", "unchanged"), keyword];
        let mut first = rows(&hits, &[]);
        assert_eq!(first.iter().map(|row| row.file_id).collect::<Vec<_>>(), vec![1, 2]);
        assert!(first[0].can_check() && first[0].checked);
        assert!(!first[1].can_check() && !first[1].checked);
        first[0].checked = false;
        assert!(!rows(&hits, &first)[0].checked);
    }

    #[test]
    fn renames_send_the_edited_text_for_checked_rows_only() {
        let mut list = rows(
            &[
                hit(1, "IMG_0001.jpg", "sam-baseball-hit.jpg", "proposedName"),
                hit(2, "IMG_0002.jpg", "beach.jpg", "tags"),
                hit(3, "IMG_0003.jpg", "", "budget"),
                hit(4, "IMG_0004.jpg", "cake.jpg", "description"),
                hit(5, "IMG_0005.jpg", "dog.jpg", "tags"),
            ],
            &[],
        );
        list[0].edited = "  sam-home-run.jpg ".into();
        list[1].checked = false;
        list[2].checked = true;
        list[3].edited = "bad/name.jpg".into();
        list[4].edited = "IMG_0005.jpg".into();
        let sent = renames(&list);
        assert_eq!(sent.len(), 1);
        assert_eq!((sent[0].file_id, sent[0].new_name.as_str()), (1, "sam-home-run.jpg"));
    }

    #[test]
    #[ignore = "requires a display; run alone with XDG_DATA_HOME pointing at scratch"]
    fn card_lists_rows_and_greys_out_rows_without_names() {
        adw::init().unwrap();
        let engine = Rc::new(RefCell::new(EngineClient::new()));
        let (card, ui) = new_ui(engine, adw::ToastOverlay::new(), Vec::new);
        let window = gtk::Window::builder().child(&card).default_width(720).default_height(600).build();
        window.present();
        assert!(!ui.date.is_sensitive() || date_toggle_enabled(&settings(&ui).style));
        ui.style.set_selected(2);
        assert!(!ui.date.is_sensitive());
        ui.style.set_selected(0);
        assert!(ui.date.is_sensitive());
        show_rows(&ui, &[hit(1, "IMG_0001.jpg", "sam-baseball-hit.jpg", "proposedName"), hit(2, "Beach.jpg", "", "unchanged")]);
        assert_eq!(ui.store.n_items(), 2);
        assert_eq!(ui.view.columns().n_items(), 4);
        assert_eq!(ui.apply.label().as_deref(), Some("Rename 1 file"));
        assert!(ui.apply.is_sensitive());
        for _ in 0..100 {
            glib::MainContext::default().iteration(false);
        }
        window.close();
    }

    #[test]
    fn accessible_label_reads_name_target_and_reason() {
        let list = rows(&[hit(1, "IMG_0001.jpg", "sam-baseball-hit.jpg", "proposedName"), hit(2, "Beach.jpg", "", "unchanged")], &[]);
        assert_eq!(accessible_label(&list[0]), "IMG_0001.jpg, rename to sam-baseball-hit.jpg, from the AI name");
        assert_eq!(accessible_label(&list[1]), "Beach.jpg, keeps its name, already well named");
    }
}
