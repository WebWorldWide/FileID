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
        UNSCANNED_COMPANION => "Its partner file isn't scanned yet",
        _ => "Nothing to name it from yet",
    }
}

pub(super) const UNSCANNED_COMPANION: &str = "unscannedCompanion";
const GROUP_MARKER: &str = "|group:";

/// Splits a proposeNames evidenceID into its reason and the companion
/// group's leader file ID ("proposedName|group:11").
pub(super) fn parse_reason(evidence: Option<&str>) -> (String, Option<i64>) {
    let text = evidence.unwrap_or("noEvidence");
    match text.split_once(GROUP_MARKER) {
        Some((reason, leader)) => (reason.to_owned(), leader.parse().ok()),
        None => (text.to_owned(), None),
    }
}

/// The name before its last dot, like the engines' split.
pub(super) fn stem(name: &str) -> &str {
    match name.rfind('.') {
        Some(dot) if dot > 0 && dot + 1 < name.len() => &name[..dot],
        _ => name,
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
    pub group: Option<i64>,
    pub suffix: String,
}

impl Row {
    pub fn can_check(&self) -> bool {
        !self.proposed.is_empty()
    }
    pub fn group_key(&self) -> i64 {
        self.group.unwrap_or(self.file_id)
    }
    pub fn is_leader(&self) -> bool {
        self.group == Some(self.file_id)
    }
    pub fn is_follower(&self) -> bool {
        self.group.is_some_and(|group| group != self.file_id)
    }
    pub fn current_name(&self) -> &str {
        fileid_engine::util::display::basename(&self.path)
    }
}

/// Rows for a reply, companions kept together leader first. A file (or
/// group) unticked in the last preview stays unticked; rows without a
/// proposed name can't be ticked.
pub(super) fn rows(hits: &[CatalogHit], previous: &[Row]) -> Vec<Row> {
    let unticked: HashSet<i64> = previous
        .iter()
        .filter(|row| row.can_check() && !row.checked)
        .map(Row::group_key)
        .collect();
    let mut rows: Vec<Row> = hits
        .iter()
        .filter(|hit| hit.kind == "proposedName")
        .map(|hit| {
            let (reason, group) = parse_reason(hit.evidence_id.as_deref());
            Row {
                file_id: hit.file_id,
                path: hit.path.clone(),
                proposed: hit.text.clone(),
                reason,
                edited: hit.text.clone(),
                checked: !hit.text.is_empty() && !unticked.contains(&group.unwrap_or(hit.file_id)),
                group,
                suffix: String::new(),
            }
        })
        .collect();
    let leader_stems: HashMap<i64, String> = rows
        .iter()
        .filter(|row| row.is_leader())
        .map(|row| (row.file_id, stem(&row.proposed).to_owned()))
        .collect();
    for row in &mut rows {
        if let Some(leader_stem) = row.group.and_then(|group| leader_stems.get(&group)) {
            row.suffix = row.proposed.strip_prefix(leader_stem.as_str()).unwrap_or_default().to_owned();
        }
    }
    rows
}

/// Ticking any member of a companion group ticks the whole group; returns
/// the indices changed.
pub(super) fn set_checked(rows: &mut [Row], file_id: i64, checked: bool) -> Vec<usize> {
    let Some(key) = rows.iter().find(|row| row.file_id == file_id).map(Row::group_key) else { return Vec::new() };
    let mut changed = Vec::new();
    for (index, row) in rows.iter_mut().enumerate() {
        if row.group_key() == key && row.can_check() {
            row.checked = checked;
            changed.push(index);
        }
    }
    changed
}

/// Editing the leader's name gives every companion the same stem; returns
/// the follower indices changed.
pub(super) fn edit(rows: &mut [Row], file_id: i64, text: &str) -> Vec<usize> {
    let Some(at) = rows.iter().position(|row| row.file_id == file_id && !row.is_follower()) else { return Vec::new() };
    rows[at].edited = text.to_owned();
    if !rows[at].is_leader() {
        return Vec::new();
    }
    let new_stem = stem(text.trim()).to_owned();
    let mut changed = Vec::new();
    for (index, row) in rows.iter_mut().enumerate() {
        if row.is_follower() && row.group == Some(file_id) && row.can_check() {
            row.edited = format!("{new_stem}{}", row.suffix);
            changed.push(index);
        }
    }
    changed
}

/// The leader's chip naming its companions' extensions: "+ ARW, XMP".
pub(super) fn companion_chip(row: &Row, rows: &[Row]) -> Option<String> {
    if !row.is_leader() {
        return None;
    }
    let extensions: Vec<String> = rows
        .iter()
        .filter(|other| other.is_follower() && other.group == Some(row.file_id))
        .map(|other| {
            let name = other.current_name();
            name.get(stem(name).len() + 1..).unwrap_or_default().to_uppercase()
        })
        .collect();
    (!extensions.is_empty()).then(|| format!("+ {}", extensions.join(", ")))
}

pub(super) fn valid_name(text: &str) -> Option<String> {
    let name = text.trim();
    (!name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\0'])).then(|| name.to_owned())
}

/// Ticked renames in row order, as one batch. A companion group goes whole
/// or not at all: one invalid member name holds every member back.
pub(super) fn renames(rows: &[Row]) -> Vec<RenameEntry> {
    let blocked: HashSet<i64> = rows
        .iter()
        .filter(|row| row.group.is_some() && row.checked && row.can_check() && valid_name(&row.edited).is_none())
        .map(Row::group_key)
        .collect();
    rows.iter()
        .filter(|row| row.checked && row.can_check() && !blocked.contains(&row.group_key()))
        .filter_map(|row| {
            let name = valid_name(&row.edited)?;
            (name != row.current_name()).then_some(RenameEntry { file_id: row.file_id, new_name: name })
        })
        .collect()
}

pub(super) fn accessible_label(row: &Row, rows: &[Row]) -> String {
    let reason = reason_label(&row.reason);
    let because = reason[..1].to_lowercase() + &reason[1..];
    let others: Vec<&str> = match row.group {
        Some(group) => rows
            .iter()
            .filter(|other| other.group == Some(group) && other.file_id != row.file_id)
            .map(Row::current_name)
            .collect(),
        None => Vec::new(),
    };
    let together = if others.is_empty() { String::new() } else { format!(", together with {}", others.join(", ")) };
    if row.can_check() {
        format!("{}, rename to {}, {because}{together}", row.current_name(), row.edited.trim())
    } else {
        format!("{}, keeps its name, {because}{together}", row.current_name())
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

fn refresh_rows(ui: &NamingUi, indices: &[usize]) {
    for &index in indices {
        ui.store.items_changed(index as u32, 1, 1);
    }
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
            if row.is_follower() {
                label.set_label(&format!("\u{21B3} {}", row.current_name()));
                label.set_margin_start(16);
            } else {
                label.set_label(row.current_name());
                label.set_margin_start(0);
            }
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
                let (followers, label) = {
                    let mut rows = ui.rows.borrow_mut();
                    let Some(file_id) = rows.get(index).filter(|row| row.can_check()).map(|row| row.file_id) else { return };
                    let followers = edit(&mut rows, file_id, &editable.text());
                    (followers, accessible_label(&rows[index], &rows))
                };
                editable.update_property(&[gtk::accessible::Property::Description(&label)]);
                refresh_rows(&ui, &followers);
                sync_apply(&ui);
            });
            editable.upcast()
        },
        |ui, row, widget| {
            let Some(editable) = widget.downcast_ref::<gtk::EditableLabel>() else { return };
            if row.can_check() {
                editable.set_text(&row.edited);
                editable.set_editable(!row.is_follower());
                editable.add_css_class("gold-accent");
                editable.remove_css_class("dim-label");
                editable.set_tooltip_text(row.is_follower().then_some("Follows the name above"));
            } else {
                editable.set_text("Keeps its name");
                editable.set_editable(false);
                editable.remove_css_class("gold-accent");
                editable.add_css_class("dim-label");
                editable.set_tooltip_text(None);
            }
            editable.update_property(&[
                gtk::accessible::Property::Label(&format!("New name for {}", row.current_name())),
                gtk::accessible::Property::Description(&accessible_label(row, &ui.rows.borrow())),
            ]);
        },
    );
    column(
        ui,
        "Reason",
        false,
        |_, _| {
            let chips = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(6).build();
            let companions = gtk::Label::builder()
                .css_classes(["chip", "gold-accent"])
                .tooltip_text("Renamed together with this file")
                .visible(false)
                .build();
            chips.append(&companions);
            chips.append(&gtk::Label::builder().css_classes(["chip", "reason-chip"]).build());
            chips.upcast()
        },
        |ui, row, widget| {
            let Some(companions) = widget.first_child().and_downcast::<gtk::Label>() else { return };
            let chip = companion_chip(row, &ui.rows.borrow());
            companions.set_visible(chip.is_some());
            companions.set_label(chip.as_deref().unwrap_or_default());
            if let Some(reason) = companions.next_sibling().and_downcast::<gtk::Label>() {
                reason.set_label(reason_label(&row.reason));
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
                let others: Vec<usize> = {
                    let mut rows = ui.rows.borrow_mut();
                    let Some(file_id) = rows.get(index).filter(|row| row.can_check()).map(|row| row.file_id) else { return };
                    set_checked(&mut rows, file_id, check.is_active()).into_iter().filter(|i| *i != index).collect()
                };
                refresh_rows(&ui, &others);
                sync_apply(&ui);
            });
            check.upcast()
        },
        |ui, row, widget| {
            let Some(check) = widget.downcast_ref::<gtk::CheckButton>() else { return };
            check.set_active(row.checked);
            check.set_sensitive(row.can_check());
            check.update_property(&[gtk::accessible::Property::Label(&accessible_label(row, &ui.rows.borrow()))]);
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
        assert_eq!(accessible_label(&list[0], &list), "IMG_0001.jpg, rename to sam-baseball-hit.jpg, from the AI name");
        assert_eq!(accessible_label(&list[1], &list), "Beach.jpg, keeps its name, already well named");
    }

    fn pair() -> Vec<CatalogHit> {
        vec![
            hit(11, "DSC_1.JPG", "beach-sunset_2.JPG", "proposedName|group:11"),
            hit(10, "DSC_1.ARW", "beach-sunset_2.ARW", "proposedName|group:11"),
            hit(12, "DSC_1.ARW.xmp", "beach-sunset_2.ARW.xmp", "proposedName|group:11"),
            hit(14, "DSC_3.ARW", "lone-hill.ARW", "proposedName"),
        ]
    }

    #[test]
    fn companions_arrive_together_leader_first() {
        let list = rows(&pair(), &[]);
        assert_eq!(list.iter().map(|row| row.file_id).collect::<Vec<_>>(), vec![11, 10, 12, 14]);
        assert!(list.iter().all(|row| row.reason == "proposedName"));
        assert!(list[0].is_leader() && list[1].is_follower() && list[2].is_follower() && list[3].group.is_none());
        assert_eq!(list.iter().map(|row| row.suffix.as_str()).collect::<Vec<_>>(), vec![".JPG", ".ARW", ".ARW.xmp", ""]);
        assert_eq!(companion_chip(&list[0], &list).as_deref(), Some("+ ARW, XMP"));
        assert_eq!(companion_chip(&list[1], &list), None);
        assert_eq!(reason_label("unscannedCompanion"), "Its partner file isn't scanned yet");
        assert_eq!(
            accessible_label(&list[1], &list),
            "DSC_1.ARW, rename to beach-sunset_2.ARW, from the AI name, together with DSC_1.JPG, DSC_1.ARW.xmp"
        );
        assert_eq!(parse_reason(Some("collision|group:30")), ("collision".to_owned(), Some(30)));
        assert_eq!(parse_reason(None), ("noEvidence".to_owned(), None));
    }

    #[test]
    fn ticking_the_leader_ticks_the_whole_group_and_stays_unticked() {
        let mut list = rows(&pair(), &[]);
        assert_eq!(set_checked(&mut list, 11, false), vec![0, 1, 2]);
        assert_eq!(list.iter().map(|row| row.checked).collect::<Vec<_>>(), vec![false, false, false, true]);
        assert_eq!(rows(&pair(), &list).iter().map(|row| row.checked).collect::<Vec<_>>(), vec![false, false, false, true]);
        set_checked(&mut list, 10, true);
        assert!(list.iter().all(|row| row.checked));
    }

    #[test]
    fn editing_the_leader_gives_every_companion_the_same_stem() {
        let mut list = rows(&pair(), &[]);
        assert_eq!(edit(&mut list, 11, " sunset-walk.JPG "), vec![1, 2]);
        assert_eq!(
            list.iter().map(|row| row.edited.as_str()).collect::<Vec<_>>(),
            vec![" sunset-walk.JPG ", "sunset-walk.ARW", "sunset-walk.ARW.xmp", "lone-hill.ARW"]
        );
        assert!(edit(&mut list, 10, "rogue.ARW").is_empty());
        assert_eq!(list[1].edited, "sunset-walk.ARW");
        let sent: Vec<(i64, String)> = renames(&list).into_iter().map(|entry| (entry.file_id, entry.new_name)).collect();
        assert_eq!(
            sent,
            vec![
                (11, "sunset-walk.JPG".to_owned()),
                (10, "sunset-walk.ARW".to_owned()),
                (12, "sunset-walk.ARW.xmp".to_owned()),
                (14, "lone-hill.ARW".to_owned()),
            ]
        );
        edit(&mut list, 11, "bad/name.JPG");
        assert_eq!(renames(&list).into_iter().map(|entry| entry.file_id).collect::<Vec<_>>(), vec![14]);
    }
}
