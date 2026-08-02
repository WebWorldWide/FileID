// People tab — face-cluster viewer over `persons` + `face_prints`, the 1:1 port
// of macOS `PeopleView.swift` (+ PersonDetailSheet / SuggestedMergesSheet /
// MergeTargetPicker).
//
//   * a `gtk::FlowBox` of person cards (representative face crop + name +
//     "N photos · M faces"), read directly from the DB (the engine is the
//     single writer; the app is a reader, mirroring `library.rs`),
//   * "Group photos by face" → `runFaceClustering`,
//   * a person-detail `adw::Dialog` (structured-name `adw::EntryRow`s + photo
//     grid + "I don't know who this is") that saves via `renamePerson` /
//     `markPersonsAsUnknown` on close,
//   * "Merge people" (checkbox mode → target picker → `mergeClusters`),
//   * "Mark unknown" (checkbox mode → `markPersonsAsUnknown`), and
//   * "Suggest merges" — the shared engine's corroborated candidate pairs,
//     presented in a sheet whose Merge buttons fan out `mergeClusters`.
//
// Live-scan behaviour mirrors `library.rs`: the tab subscribes to engine events
// and throttle-reloads as face detection lands during a scan, then reloads on
// completion.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::Hash;
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::glib;

use crate::engine_client::{texture_from_decoded, DecodedImage, EngineClient, EngineEvent};
use fileid_engine::ipc::{
    BulkActionResult, CommandPayload, Empty, MarkPersonsAsUnknownPayload, MergeClustersPayload,
    RenamePersonPayload,
};

const CARD_THUMB_PX: i32 = 256;
const PHOTO_THUMB_PX: i32 = 240;
const PERSON_FILE_LIMIT: i64 = 500;
const PERSON_THUMB_CACHE_CAP: usize = 256;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    Merge,
    Unknown,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct PersonThumbKey {
    path: String,
    bbox: Option<String>,
    size_bytes: i64,
    modified_bits: Option<u64>,
    file_ref: Option<i64>,
    content_hash: Option<Vec<u8>>,
}

#[derive(Clone, Default)]
struct PersonRow {
    id: i64,
    title: Option<String>,
    first_name: Option<String>,
    middle_name: Option<String>,
    last_name: Option<String>,
    suffix: Option<String>,
    name: Option<String>,
    is_unknown: bool,
    file_count: i64,
    face_count: i64,
    rep_path: Option<String>,
    rep_bbox: Option<String>,
    rep_size_bytes: i64,
    rep_modified: Option<f64>,
    rep_file_ref: Option<i64>,
    rep_content_hash: Option<Vec<u8>>,
}

impl PersonRow {
    fn structured(&self) -> String {
        [
            &self.title,
            &self.first_name,
            &self.middle_name,
            &self.last_name,
            &self.suffix,
        ]
        .iter()
        .filter_map(|o| o.as_ref())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
    }

    fn display_name(&self) -> String {
        let s = self.structured();
        if !s.is_empty() {
            return s;
        }
        if let Some(n) = self
            .name
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            return n.to_string();
        }
        if self.is_unknown {
            "Unknown person".to_string()
        } else {
            "Unnamed person".to_string()
        }
    }

    fn has_any_name(&self) -> bool {
        !self.structured().is_empty()
            || self
                .name
                .as_ref()
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false)
    }

    fn counts(&self) -> String {
        format!(
            "{} photo{} · {} face{}",
            self.file_count,
            plural(self.file_count),
            self.face_count,
            plural(self.face_count)
        )
    }
}

#[derive(Clone, Copy)]
struct Candidate {
    a: i64,
    b: i64,
    sim: f32,
}

#[derive(Default)]
struct Snapshot {
    all_with_faces: Vec<PersonRow>,
    total_faces: i64,
}

struct BoundedLru<K, V> {
    values: HashMap<K, V>,
    order: VecDeque<K>,
    capacity: usize,
}

impl<K, V> BoundedLru<K, V>
where
    K: Clone + Eq + Hash,
    V: Clone,
{
    fn new(capacity: usize) -> Self {
        Self {
            values: HashMap::with_capacity(capacity),
            order: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    fn get(&mut self, key: &K) -> Option<V> {
        let value = self.values.get(key)?.clone();
        self.order.retain(|existing| existing != key);
        self.order.push_back(key.clone());
        Some(value)
    }

    fn insert(&mut self, key: K, value: V) {
        if self.capacity == 0 {
            return;
        }
        if self.values.insert(key.clone(), value).is_some() {
            self.order.retain(|existing| existing != &key);
        }
        self.order.push_back(key);
        while self.values.len() > self.capacity {
            if let Some(evicted) = self.order.pop_front() {
                self.values.remove(&evicted);
            }
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.values.len()
    }
}

struct PendingSuggestion {
    row: glib::WeakRef<gtk::Box>,
    button: glib::WeakRef<gtk::Button>,
    dialog: glib::WeakRef<adw::Dialog>,
}

#[derive(Debug, Default)]
struct FaceClusteringLifecycle {
    engine_ready: bool,
    next_generation: u64,
    active_generation: Option<u64>,
}

impl FaceClusteringLifecycle {
    fn new(engine_ready: bool) -> Self {
        Self {
            engine_ready,
            ..Self::default()
        }
    }

    fn begin(&mut self) -> Result<u64, &'static str> {
        if !self.engine_ready {
            return Err("The engine is still starting.");
        }
        if self.active_generation.is_some() {
            return Err("Face grouping is already in progress.");
        }
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        self.active_generation = Some(self.next_generation);
        Ok(self.next_generation)
    }

    fn finish_active(&mut self) -> Option<u64> {
        self.active_generation.take()
    }

    fn finish_if(&mut self, generation: u64) -> bool {
        if self.active_generation == Some(generation) {
            self.active_generation = None;
            true
        } else {
            false
        }
    }

    fn on_ready(&mut self) {
        self.engine_ready = true;
    }

    fn on_unavailable(&mut self) -> Option<u64> {
        self.engine_ready = false;
        self.finish_active()
    }

    fn is_active(&self) -> bool {
        self.active_generation.is_some()
    }

    fn can_start(&self) -> bool {
        self.engine_ready && !self.is_active()
    }
}

#[derive(Default)]
struct PersonActionGate {
    active: RefCell<HashSet<&'static str>>,
}

impl PersonActionGate {
    fn begin(&self, action: &'static str) -> bool {
        self.active.borrow_mut().insert(action)
    }

    fn finish(&self, action: &'static str) {
        self.active.borrow_mut().remove(action);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum PersonDialogOperation {
    #[default]
    Idle,
    Renaming,
    MarkingUnknown,
    Complete,
}

#[derive(Default)]
struct PersonDialogLifecycle {
    operation: Cell<PersonDialogOperation>,
}

impl PersonDialogLifecycle {
    fn begin(&self, operation: PersonDialogOperation) -> bool {
        if self.operation.get() != PersonDialogOperation::Idle {
            return false;
        }
        self.operation.set(operation);
        true
    }

    fn reset(&self) {
        self.operation.set(PersonDialogOperation::Idle);
    }

    fn complete(&self) {
        self.operation.set(PersonDialogOperation::Complete);
    }
}

#[derive(Debug, PartialEq, Eq)]
enum RenameTerminal {
    Ignore,
    Success,
    Failure,
}

fn classify_person_terminal(
    result: &BulkActionResult,
    action: &str,
    person_id: i64,
) -> RenameTerminal {
    if result.action != action {
        return RenameTerminal::Ignore;
    }
    let ids: Vec<i64> = result
        .messages
        .iter()
        .filter_map(|item| item.file_id)
        .collect();
    if !ids.is_empty() && !ids.contains(&person_id) {
        return RenameTerminal::Ignore;
    }
    if result.failed == 0
        && result.succeeded > 0
        && ids.contains(&person_id)
        && result.messages.iter().any(|item| item.ok)
    {
        RenameTerminal::Success
    } else if result.failed > 0 && (ids.is_empty() || ids.contains(&person_id)) {
        RenameTerminal::Failure
    } else {
        RenameTerminal::Ignore
    }
}

fn classify_rename_terminal(result: &BulkActionResult, person_id: i64) -> RenameTerminal {
    classify_person_terminal(result, "renamePerson", person_id)
}

struct Ui {
    engine: Rc<RefCell<EngineClient>>,

    persons: RefCell<Vec<PersonRow>>,
    person_by_id: RefCell<HashMap<i64, PersonRow>>,
    suggestions: RefCell<Vec<Candidate>>,
    pending_suggestions: RefCell<VecDeque<PendingSuggestion>>,
    merge_results_pending: Cell<usize>,
    person_actions: PersonActionGate,
    total_faces: Cell<i64>,
    hidden_unknown: Cell<i64>,
    show_hidden: Cell<bool>,
    mode: Cell<Mode>,
    merge_checked: RefCell<HashSet<i64>>,
    unknown_checked: RefCell<HashSet<i64>>,
    reload_gen: Cell<u64>,
    face_clustering: RefCell<FaceClusteringLifecycle>,
    // Keyed by (representative photo path, face bbox): two people can share a
    // representative photo but crop different faces from it, so the path alone
    // would make the second card reuse the first card's face crop.
    thumb_cache: RefCell<BoundedLru<PersonThumbKey, gtk::gdk::MemoryTexture>>,

    count_label: gtk::Label,
    status_label: gtk::Label,
    actions_box: gtk::Box,
    bulk_strip: gtk::Box,
    bulk_label: gtk::Label,
    bulk_button: gtk::Button,
    grid: gtk::FlowBox,
    grid_scroller: gtk::ScrolledWindow,
    empty_page: adw::StatusPage,
    no_clusters_page: adw::StatusPage,
    group_button: gtk::Button,
    cluster_spinner: gtk::Spinner,
    footer: gtk::Box,
    footer_label: gtk::Label,
    footer_button: gtk::Button,
    anchor: gtk::Box,
}

pub fn build(engine: Rc<RefCell<EngineClient>>) -> gtk::Widget {
    // ── Header ────────────────────────────────────────────────────────────────
    let title = gtk::Label::builder()
        .label("People")
        .xalign(0.0)
        .css_classes(["title-1"])
        .build();
    let count_label = gtk::Label::builder()
        .label("")
        .xalign(0.0)
        .css_classes(["dim-label"])
        .build();
    let actions_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .build();
    let title_spacer = gtk::Box::builder().hexpand(true).build();
    let title_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .build();
    title_row.append(&title);
    title_row.append(&count_label);
    title_row.append(&title_spacer);
    title_row.append(&actions_box);

    let status_label = gtk::Label::builder()
        .label("")
        .xalign(0.0)
        .visible(false)
        .css_classes(["dim-label"])
        .build();

    let bulk_label = gtk::Label::builder()
        .label("")
        .css_classes(["dim-label"])
        .build();
    let bulk_spacer = gtk::Box::builder().hexpand(true).build();
    let bulk_button = gtk::Button::builder()
        .label("")
        .css_classes(["gold-button"])
        .build();
    let bulk_strip = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(10)
        .visible(false)
        .build();
    bulk_strip.append(&bulk_label);
    bulk_strip.append(&bulk_spacer);
    bulk_strip.append(&bulk_button);

    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .build();
    header.append(&title_row);
    header.append(&status_label);
    header.append(&bulk_strip);

    // ── Content: grid / empty / no-clusters ──────────────────────────────────
    let grid = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .max_children_per_line(16)
        .min_children_per_line(2)
        .column_spacing(14)
        .row_spacing(14)
        .homogeneous(true)
        .valign(gtk::Align::Start)
        .build();
    let grid_scroller = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .child(&grid)
        .css_classes(["fileid-tab"])
        .build();

    let empty_page = adw::StatusPage::builder()
        .icon_name("system-users-symbolic")
        .title("No people yet")
        .description(
            "Pick a folder and Start Scan. As the scan runs, faces in your photos get \
             detected — they'll appear here as cards you can name.",
        )
        .vexpand(true)
        .build();

    let group_btn = gtk::Button::builder()
        .label("Group photos by face")
        .halign(gtk::Align::Center)
        .css_classes(["gold-button", "pill"])
        .build();
    let cluster_spinner = gtk::Spinner::new();
    let no_clusters_inner = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .halign(gtk::Align::Center)
        .build();
    no_clusters_inner.append(&group_btn);
    no_clusters_inner.append(&cluster_spinner);
    let no_clusters_page = adw::StatusPage::builder()
        .icon_name("view-grid-symbolic")
        .title("Ready to group faces")
        .description(
            "Click Group photos by face. The app compares every face to every other and \
             creates one card per person. Once grouped, open a card to add a name.",
        )
        .child(&no_clusters_inner)
        .vexpand(true)
        .build();

    // ── Hidden-unknowns footer ───────────────────────────────────────────────
    let footer_label = gtk::Label::builder()
        .label("")
        .css_classes(["dim-label"])
        .build();
    let footer_spacer = gtk::Box::builder().hexpand(true).build();
    let footer_button = gtk::Button::builder()
        .label("Show them")
        .css_classes(["flat"])
        .build();
    let footer = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .visible(false)
        .build();
    footer.append(&footer_label);
    footer.append(&footer_spacer);
    footer.append(&footer_button);

    // ── Root ─────────────────────────────────────────────────────────────────
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(16)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .css_classes(["fileid-tab"])
        .build();
    root.append(&header);
    root.append(&grid_scroller);
    root.append(&empty_page);
    root.append(&no_clusters_page);
    root.append(&footer);

    let engine_ready = engine.borrow().is_ready();
    let ui = Rc::new(Ui {
        engine: engine.clone(),
        persons: RefCell::new(Vec::new()),
        person_by_id: RefCell::new(HashMap::new()),
        suggestions: RefCell::new(Vec::new()),
        pending_suggestions: RefCell::new(VecDeque::new()),
        merge_results_pending: Cell::new(0),
        person_actions: PersonActionGate::default(),
        total_faces: Cell::new(0),
        hidden_unknown: Cell::new(0),
        show_hidden: Cell::new(false),
        mode: Cell::new(Mode::Normal),
        merge_checked: RefCell::new(HashSet::new()),
        unknown_checked: RefCell::new(HashSet::new()),
        reload_gen: Cell::new(0),
        face_clustering: RefCell::new(FaceClusteringLifecycle::new(engine_ready)),
        thumb_cache: RefCell::new(BoundedLru::new(PERSON_THUMB_CACHE_CAP)),
        count_label: count_label.clone(),
        status_label: status_label.clone(),
        actions_box: actions_box.clone(),
        bulk_strip: bulk_strip.clone(),
        bulk_label: bulk_label.clone(),
        bulk_button: bulk_button.clone(),
        grid: grid.clone(),
        grid_scroller: grid_scroller.clone(),
        empty_page: empty_page.clone(),
        no_clusters_page: no_clusters_page.clone(),
        group_button: group_btn.clone(),
        cluster_spinner: cluster_spinner.clone(),
        footer: footer.clone(),
        footer_label: footer_label.clone(),
        footer_button: footer_button.clone(),
        anchor: root.clone(),
    });

    {
        let ui = ui.clone();
        bulk_button.connect_clicked(move |_| on_bulk_clicked(&ui));
    }
    {
        let ui = ui.clone();
        group_btn.connect_clicked(move |_| start_clustering(&ui));
    }
    {
        let ui = ui.clone();
        footer_button.connect_clicked(move |_| {
            ui.show_hidden.set(!ui.show_hidden.get());
            reload(&ui);
        });
    }
    // Live-scan reloads (faces land during the scan): throttle on batches, final
    // on completion — same coalescing as `library.rs`.
    let ev_rx = ui.engine.borrow_mut().subscribe();
    {
        let ui = ui.clone();
        glib::MainContext::default().spawn_local(async move {
            let mut last = Instant::now() - Duration::from_secs(10);
            while let Ok(ev) = ev_rx.recv().await {
                match ev {
                    EngineEvent::Spawning => {
                        if ui.face_clustering.borrow_mut().on_unavailable().is_some() {
                            set_status(
                                &ui,
                                "Grouping stopped because the engine restarted.".to_string(),
                            );
                        }
                        refresh_view(&ui);
                    }
                    EngineEvent::Ready => {
                        ui.face_clustering.borrow_mut().on_ready();
                        refresh_view(&ui);
                    }
                    EngineEvent::BatchLanded(_) => {
                        if last.elapsed() >= Duration::from_millis(1200) {
                            last = Instant::now();
                            reload(&ui);
                        }
                    }
                    EngineEvent::ScanComplete(_) => reload(&ui),
                    EngineEvent::FaceClusteringComplete(result) => {
                        ui.face_clustering.borrow_mut().finish_active();
                        set_status(
                            &ui,
                            format!(
                                "Grouped {} faces into {} people.",
                                result.face_count, result.person_count
                            ),
                        );
                        reload(&ui);
                    }
                    EngineEvent::FaceClusteringFailed(message) => {
                        ui.face_clustering.borrow_mut().finish_active();
                        set_status(&ui, format!("Grouping failed: {message}"));
                        refresh_view(&ui);
                    }
                    EngineEvent::FaceClusteringBusy(message) => {
                        if ui.face_clustering.borrow_mut().finish_active().is_some() {
                            set_status(&ui, format!("Grouping could not start: {message}"));
                        }
                        refresh_view(&ui);
                    }
                    EngineEvent::Exited => {
                        if ui.face_clustering.borrow_mut().on_unavailable().is_some() {
                            set_status(
                                &ui,
                                "Grouping stopped because the engine exited.".to_string(),
                            );
                        }
                        refresh_view(&ui);
                    }
                    EngineEvent::BulkActionResult(result) if result.action == "mergeClusters" => {
                        let outstanding = ui.merge_results_pending.get();
                        if outstanding == 0 {
                            continue;
                        }
                        ui.merge_results_pending.set(outstanding - 1);
                        if let Some(pending) = ui.pending_suggestions.borrow_mut().pop_front() {
                            if result.failed == 0 && result.succeeded > 0 {
                                if let Some(row) = pending.row.upgrade() {
                                    row.set_visible(false);
                                }
                                if let Some(dialog) = pending.dialog.upgrade() {
                                    dialog.close();
                                }
                                ui.suggestions.borrow_mut().clear();
                                set_status(&ui, "Merge complete.".to_string());
                                schedule_reload_burst(&ui);
                            } else {
                                if let Some(button) = pending.button.upgrade() {
                                    button.set_sensitive(true);
                                    button.set_label("Merge");
                                }
                                let detail = result
                                    .messages
                                    .iter()
                                    .find_map(|item| {
                                        (!item.ok).then_some(item.message.as_deref()).flatten()
                                    })
                                    .unwrap_or("the engine rejected the merge");
                                set_status(&ui, format!("Merge failed: {detail}"));
                            }
                        } else if result.failed > 0 {
                            let detail = result
                                .messages
                                .iter()
                                .find_map(|item| {
                                    (!item.ok).then_some(item.message.as_deref()).flatten()
                                })
                                .unwrap_or("the engine rejected a merge");
                            set_status(&ui, format!("Merge failed: {detail}"));
                        }
                        if ui.merge_results_pending.get() == 0 {
                            schedule_reload_burst(&ui);
                        }
                    }
                    _ => {}
                }
            }
        });
    }

    reload(&ui);
    refresh_view(&ui);
    // Re-read whenever the tab becomes visible: the startup read can race the
    // engine's own DB open (transient BUSY → empty snapshot), and clustering
    // finished on another tab must show up on switch — not only on scan events.
    {
        let ui = ui.clone();
        root.connect_map(move |_| reload(&ui));
    }
    root.upcast()
}

// ── View refresh ──────────────────────────────────────────────────────────────

fn refresh_view(ui: &Rc<Ui>) {
    let clustering = ui.face_clustering.borrow().is_active();
    let (has_persons, count_text, persons_len) = {
        let persons = ui.persons.borrow();
        (
            !persons.is_empty(),
            count_line(&persons, ui.total_faces.get(), clustering),
            persons.len(),
        )
    };
    ui.count_label.set_text(&count_text);

    let has_faces = ui.total_faces.get() > 0;
    ui.grid_scroller.set_visible(has_persons);
    ui.empty_page.set_visible(!has_persons && !has_faces);
    ui.no_clusters_page.set_visible(!has_persons && has_faces);
    if !has_persons && has_faces {
        ui.no_clusters_page.set_title(&format!(
            "{} faces detected — ready to group",
            ui.total_faces.get()
        ));
    }

    let can_start_clustering = ui.face_clustering.borrow().can_start();
    ui.group_button.set_sensitive(can_start_clustering);
    ui.cluster_spinner.set_visible(clustering);
    if clustering {
        ui.cluster_spinner.start();
    } else {
        ui.cluster_spinner.stop();
    }

    rebuild_actions(ui, persons_len);
    update_bulk_strip(ui);

    let show_footer = has_persons && ui.hidden_unknown.get() > 0;
    ui.footer.set_visible(show_footer);
    if show_footer {
        let n = ui.hidden_unknown.get();
        if ui.show_hidden.get() {
            ui.footer_label
                .set_text(&format!("{n} marked unknown — currently visible"));
            ui.footer_button.set_label("Hide them");
        } else {
            ui.footer_label.set_text(&format!("{n} hidden as unknown"));
            ui.footer_button.set_label("Show them");
        }
    }

    if has_persons {
        rebuild_grid(ui);
    }
}

fn count_line(persons: &[PersonRow], total_faces: i64, clustering: bool) -> String {
    let p = persons.len();
    let unnamed = persons.iter().filter(|x| !x.has_any_name()).count();
    if p == 0 && total_faces == 0 {
        return String::new();
    }
    if p == 0 {
        return if clustering {
            format!("{total_faces} faces · clustering…")
        } else {
            format!("{total_faces} faces · not grouped yet")
        };
    }
    if unnamed > 0 {
        format!("{p} people · {unnamed} still unnamed")
    } else {
        format!("{p} people · all named")
    }
}

fn rebuild_actions(ui: &Rc<Ui>, person_count: usize) {
    while let Some(child) = ui.actions_box.first_child() {
        ui.actions_box.remove(&child);
    }
    if ui.mode.get() != Mode::Normal {
        let cancel = gtk::Button::with_label("Cancel");
        let ui2 = ui.clone();
        cancel.connect_clicked(move |_| set_mode(&ui2, Mode::Normal));
        ui.actions_box.append(&cancel);
        return;
    }
    if person_count >= 2 {
        let suggest = gtk::Button::builder()
            .label("Suggest merges")
            .css_classes(["gold-button"])
            .build();
        let ui2 = ui.clone();
        suggest.connect_clicked(move |b| on_suggest_clicked(&ui2, b));

        let merge = gtk::Button::with_label("Merge people");
        let ui3 = ui.clone();
        merge.connect_clicked(move |_| set_mode(&ui3, Mode::Merge));

        let unknown = gtk::Button::with_label("Mark unknown");
        let ui4 = ui.clone();
        unknown.connect_clicked(move |_| set_mode(&ui4, Mode::Unknown));

        ui.actions_box.append(&suggest);
        ui.actions_box.append(&merge);
        ui.actions_box.append(&unknown);
    }
}

fn update_bulk_strip(ui: &Rc<Ui>) {
    let mode = ui.mode.get();
    ui.bulk_strip.set_visible(mode != Mode::Normal);
    match mode {
        Mode::Merge => {
            let n = ui.merge_checked.borrow().len();
            ui.bulk_label.set_text(&format!("{n} selected to merge"));
            ui.bulk_button.set_label(&format!("Merge {n} selected"));
            ui.bulk_button.set_sensitive(n >= 2);
        }
        Mode::Unknown => {
            let n = ui.unknown_checked.borrow().len();
            ui.bulk_label
                .set_text(&format!("{n} selected to mark unknown"));
            ui.bulk_button.set_label(&format!("Mark {n} as unknown"));
            ui.bulk_button.set_sensitive(n >= 1);
        }
        Mode::Normal => {}
    }
}

fn set_mode(ui: &Rc<Ui>, mode: Mode) {
    ui.mode.set(mode);
    ui.merge_checked.borrow_mut().clear();
    ui.unknown_checked.borrow_mut().clear();
    refresh_view(ui);
}

fn on_bulk_clicked(ui: &Rc<Ui>) {
    match ui.mode.get() {
        Mode::Merge => {
            if ui.merge_checked.borrow().len() >= 2 {
                open_merge_target_picker(ui);
            }
        }
        Mode::Unknown => {
            let ids: Vec<i64> = ui.unknown_checked.borrow().iter().copied().collect();
            if ids.is_empty() {
                return;
            }
            if !begin_person_action(ui, "markPersonsAsUnknown") {
                set_status(
                    ui,
                    "Another Mark Unknown action is still saving.".to_string(),
                );
                return;
            }
            let events = ui.engine.borrow_mut().subscribe();
            ui.bulk_button.set_sensitive(false);
            if !send_cmd(
                ui,
                CommandPayload::MarkPersonsAsUnknown(MarkPersonsAsUnknownPayload {
                    person_ids: ids.clone(),
                }),
            ) {
                finish_person_action(ui, "markPersonsAsUnknown");
                update_bulk_strip(ui);
                return;
            }
            set_status(ui, "Saving…".to_string());
            let ui = ui.clone();
            glib::MainContext::default().spawn_local(async move {
                while let Ok(event) = events.recv().await {
                    match event {
                        EngineEvent::BulkActionResult(result)
                            if result.action == "markPersonsAsUnknown" =>
                        {
                            finish_person_action(&ui, "markPersonsAsUnknown");
                            if result.failed == 0 && result.succeeded > 0 {
                                set_status(
                                    &ui,
                                    format!(
                                        "Marked {} cluster{} as unknown.",
                                        result.succeeded,
                                        plural(result.succeeded as i64)
                                    ),
                                );
                                set_mode(&ui, Mode::Normal);
                                schedule_reload_burst(&ui);
                            } else {
                                let detail = result
                                    .messages
                                    .iter()
                                    .find(|item| !item.ok)
                                    .and_then(|item| item.message.as_deref())
                                    .unwrap_or("the engine rejected the change");
                                set_status(&ui, format!("Couldn't mark as unknown: {detail}"));
                                update_bulk_strip(&ui);
                            }
                            break;
                        }
                        EngineEvent::Exited => {
                            finish_person_action(&ui, "markPersonsAsUnknown");
                            set_status(
                                &ui,
                                "Couldn't mark as unknown: the engine exited.".to_string(),
                            );
                            update_bulk_strip(&ui);
                            break;
                        }
                        _ => {}
                    }
                }
            });
        }
        Mode::Normal => {}
    }
}

// ── Person grid + cards ───────────────────────────────────────────────────────

fn rebuild_grid(ui: &Rc<Ui>) {
    ui.grid.remove_all();
    let persons = ui.persons.borrow();
    for p in persons.iter() {
        let card = build_card(ui, p);
        ui.grid.append(&card);
    }
}

fn build_card(ui: &Rc<Ui>, p: &PersonRow) -> gtk::Widget {
    let pid = p.id;
    let mode = ui.mode.get();
    let checked = match mode {
        Mode::Merge => ui.merge_checked.borrow().contains(&pid),
        Mode::Unknown => ui.unknown_checked.borrow().contains(&pid),
        Mode::Normal => false,
    };

    let pic = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Cover)
        .height_request(150)
        .hexpand(true)
        .css_classes(["tile-thumb"])
        .build();

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&pic));
    let check = gtk::Image::builder()
        .icon_name(if checked {
            "object-select-symbolic"
        } else {
            "checkbox-symbolic"
        })
        .pixel_size(22)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Start)
        .margin_top(6)
        .margin_start(6)
        .build();
    check.set_visible(mode != Mode::Normal);
    if checked {
        check.add_css_class("gold-accent");
    }
    overlay.add_overlay(&check);

    let name = gtk::Label::builder()
        .label(p.display_name())
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .single_line_mode(true)
        .css_classes(["heading"])
        .build();
    if p.has_any_name() {
        name.add_css_class("gold-accent");
    }
    let caption = gtk::Label::builder()
        .label(p.counts())
        .xalign(0.0)
        .css_classes(["tile-caption"])
        .build();

    let vbox = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .width_request(150)
        .css_classes(["file-tile"])
        .build();
    vbox.append(&overlay);
    vbox.append(&name);
    vbox.append(&caption);
    if checked {
        vbox.add_css_class("file-tile-selected");
    }

    let action = if ui.mode.get() == Mode::Normal {
        "Open"
    } else {
        "Select"
    };
    let button = gtk::ToggleButton::builder()
        .has_frame(false)
        .active(checked)
        .tooltip_text(format!("{action} {}", p.display_name()))
        .child(&vbox)
        .build();
    let vbox_weak = vbox.downgrade();
    let check_weak = check.downgrade();
    let ui_g = ui.clone();
    button.connect_clicked(move |button| {
        let (Some(vbox), Some(check)) = (vbox_weak.upgrade(), check_weak.upgrade()) else {
            return;
        };
        match ui_g.mode.get() {
            Mode::Normal => {
                button.set_active(false);
                open_person_detail(&ui_g, pid);
            }
            Mode::Merge => {
                let on = toggle(&ui_g.merge_checked, pid);
                button.set_active(on);
                update_check_visual(&check, &vbox, on);
                update_bulk_strip(&ui_g);
            }
            Mode::Unknown => {
                let on = toggle(&ui_g.unknown_checked, pid);
                button.set_active(on);
                update_check_visual(&check, &vbox, on);
                update_bulk_strip(&ui_g);
            }
        }
    });

    match p.rep_path.clone() {
        Some(path) => load_card_thumb(
            ui,
            &pic,
            PersonThumbKey {
                path,
                bbox: p.rep_bbox.clone(),
                size_bytes: p.rep_size_bytes,
                modified_bits: p.rep_modified.map(f64::to_bits),
                file_ref: p.rep_file_ref,
                content_hash: p.rep_content_hash.clone(),
            },
        ),
        None => pic.set_paintable(person_icon().as_ref()),
    }

    button.upcast()
}

fn toggle(set: &RefCell<HashSet<i64>>, pid: i64) -> bool {
    let mut s = set.borrow_mut();
    if s.contains(&pid) {
        s.remove(&pid);
        false
    } else {
        s.insert(pid);
        true
    }
}

fn update_check_visual(check: &gtk::Image, vbox: &gtk::Box, on: bool) {
    check.set_icon_name(Some(if on {
        "object-select-symbolic"
    } else {
        "checkbox-symbolic"
    }));
    if on {
        check.add_css_class("gold-accent");
        vbox.add_css_class("file-tile-selected");
    } else {
        check.remove_css_class("gold-accent");
        vbox.remove_css_class("file-tile-selected");
    }
}

fn load_card_thumb(ui: &Rc<Ui>, pic: &gtk::Picture, key: PersonThumbKey) {
    if let Some(tex) = ui.thumb_cache.borrow_mut().get(&key) {
        pic.set_paintable(Some(&tex));
        return;
    }
    let rx = ui
        .engine
        .borrow()
        .request_thumbnail_with(key.path.clone(), {
            let bbox = key.bbox.clone();
            move |bytes| cropped_texture(bytes, bbox.as_deref(), CARD_THUMB_PX)
        });
    let pic_weak = pic.downgrade();
    let ui = ui.clone();
    glib::MainContext::default().spawn_local(async move {
        let Ok(Some(decoded)) = rx.recv().await else {
            return;
        };
        let tex = texture_from_decoded(&decoded);
        ui.thumb_cache.borrow_mut().insert(key, tex.clone());
        if let Some(pic) = pic_weak.upgrade() {
            pic.set_paintable(Some(&tex));
        }
    });
}

// ── Reload (DB → caches → view) ───────────────────────────────────────────────

fn reload(ui: &Rc<Ui>) {
    let g = ui.reload_gen.get().wrapping_add(1);
    ui.reload_gen.set(g);
    let rx = read_snapshot_async();
    let ui = ui.clone();
    glib::MainContext::default().spawn_local(async move {
        let snap = rx.recv().await.unwrap_or_default();
        // Latest-wins: a slower earlier read can't clobber a newer one.
        if ui.reload_gen.get() != g {
            return;
        }
        ui.total_faces.set(snap.total_faces);
        let with_faces: Vec<PersonRow> = snap
            .all_with_faces
            .into_iter()
            .filter(|p| p.face_count > 0)
            .collect();
        let hidden = with_faces.iter().filter(|p| p.is_unknown).count() as i64;
        ui.hidden_unknown.set(hidden);

        let show_hidden = ui.show_hidden.get();
        let visible: Vec<PersonRow> = with_faces
            .iter()
            .filter(|p| !p.is_unknown || show_hidden)
            .cloned()
            .collect();
        let mut map = HashMap::with_capacity(visible.len());
        for p in &visible {
            map.insert(p.id, p.clone());
        }
        *ui.person_by_id.borrow_mut() = map;
        *ui.persons.borrow_mut() = visible;

        refresh_view(&ui);
    });
}

fn schedule_reload_burst(ui: &Rc<Ui>) {
    for ms in [500u64, 1500, 3000] {
        let ui = ui.clone();
        glib::timeout_add_local_once(Duration::from_millis(ms), move || reload(&ui));
    }
}

fn begin_person_action(ui: &Rc<Ui>, action: &'static str) -> bool {
    ui.person_actions.begin(action)
}

fn finish_person_action(ui: &Rc<Ui>, action: &'static str) {
    ui.person_actions.finish(action);
}

fn send_cmd(ui: &Rc<Ui>, payload: CommandPayload) -> bool {
    match ui.engine.borrow_mut().send(payload) {
        Ok(()) => true,
        Err(error) => {
            set_status(ui, format!("Command could not be sent: {error}"));
            false
        }
    }
}

fn set_status(ui: &Rc<Ui>, msg: String) {
    ui.status_label.set_text(&msg);
    ui.status_label.set_visible(true);
}

// ── Clustering ────────────────────────────────────────────────────────────────

fn start_clustering(ui: &Rc<Ui>) {
    let generation = match ui.face_clustering.borrow_mut().begin() {
        Ok(generation) => generation,
        Err(message) => {
            set_status(ui, message.to_string());
            refresh_view(ui);
            return;
        }
    };
    set_status(ui, "Grouping faces into people…".to_string());
    refresh_view(ui);
    if !send_cmd(ui, CommandPayload::RunFaceClustering(Empty {})) {
        ui.face_clustering.borrow_mut().finish_if(generation);
        refresh_view(ui);
    }
}

// ── Person-detail dialog ──────────────────────────────────────────────────────

fn set_person_dialog_busy(
    group: &adw::PreferencesGroup,
    done_button: &gtk::Button,
    mark_button: &gtk::Button,
    busy: bool,
) {
    group.set_sensitive(!busy);
    done_button.set_sensitive(!busy);
    mark_button.set_sensitive(!busy);
}

fn open_person_detail(ui: &Rc<Ui>, pid: i64) {
    let person = match ui.person_by_id.borrow().get(&pid).cloned() {
        Some(p) => p,
        None => return,
    };

    let dialog = adw::Dialog::new();
    dialog.set_title(&person.display_name());
    dialog.set_content_width(760);
    dialog.set_content_height(640);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());

    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(16)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .build();

    let subtitle = gtk::Label::builder()
        .label(person.counts())
        .xalign(0.0)
        .css_classes(["dim-label"])
        .build();
    body.append(&subtitle);

    let group = adw::PreferencesGroup::new();
    let title_row = adw::EntryRow::builder()
        .title("Title (Uncle, Grandma…)")
        .build();
    let first_row = adw::EntryRow::builder().title("First name").build();
    let middle_row = adw::EntryRow::builder().title("Middle name").build();
    let last_row = adw::EntryRow::builder().title("Last name").build();
    let suffix_row = adw::EntryRow::builder().title("Suffix (Jr, III…)").build();
    title_row.set_text(person.title.as_deref().unwrap_or(""));
    first_row.set_text(
        person
            .first_name
            .as_deref()
            .or(person.name.as_deref())
            .unwrap_or(""),
    );
    middle_row.set_text(person.middle_name.as_deref().unwrap_or(""));
    last_row.set_text(person.last_name.as_deref().unwrap_or(""));
    suffix_row.set_text(person.suffix.as_deref().unwrap_or(""));
    group.add(&title_row);
    group.add(&first_row);
    group.add(&middle_row);
    group.add(&last_row);
    group.add(&suffix_row);
    body.append(&group);

    let btn_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    let mark_btn = gtk::Button::with_label("I don't know who this is");
    let btn_spacer = gtk::Box::builder().hexpand(true).build();
    let done_btn = gtk::Button::builder()
        .label("Done")
        .css_classes(["gold-button"])
        .build();
    btn_row.append(&mark_btn);
    btn_row.append(&btn_spacer);
    btn_row.append(&done_btn);
    body.append(&btn_row);

    let photos = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .max_children_per_line(6)
        .min_children_per_line(2)
        .column_spacing(8)
        .row_spacing(8)
        .homogeneous(true)
        .valign(gtk::Align::Start)
        .build();
    let photos_scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hexpand(true)
        .child(&photos)
        .build();
    body.append(&photos_scroll);

    toolbar.set_content(Some(&body));
    dialog.set_child(Some(&toolbar));

    dialog.set_can_close(false);
    let lifecycle = Rc::new(PersonDialogLifecycle::default());

    {
        let ui = ui.clone();
        let lifecycle = lifecycle.clone();
        let group = group.clone();
        let done_btn = done_btn.clone();
        let mark_btn = mark_btn.clone();
        let (t, f, m, l, s) = (
            title_row.clone(),
            first_row.clone(),
            middle_row.clone(),
            last_row.clone(),
            suffix_row.clone(),
        );
        dialog.connect_close_attempt(move |dialog| {
            if !lifecycle.begin(PersonDialogOperation::Renaming) {
                return;
            }
            if !begin_person_action(&ui, "renamePerson") {
                lifecycle.reset();
                set_status(&ui, "Another person rename is still saving.".to_string());
                return;
            }
            let payload = RenamePersonPayload {
                person_id: pid,
                title: norm(t.text().as_str()),
                first_name: norm(f.text().as_str()),
                middle_name: norm(m.text().as_str()),
                last_name: norm(l.text().as_str()),
                suffix: norm(s.text().as_str()),
            };
            let events = ui.engine.borrow_mut().subscribe();
            set_person_dialog_busy(&group, &done_btn, &mark_btn, true);
            if !send_cmd(&ui, CommandPayload::RenamePerson(payload)) {
                finish_person_action(&ui, "renamePerson");
                lifecycle.reset();
                set_person_dialog_busy(&group, &done_btn, &mark_btn, false);
                return;
            }
            set_status(&ui, "Saving person…".to_string());
            let ui = ui.clone();
            let lifecycle = lifecycle.clone();
            let dialog = dialog.clone();
            let group = group.clone();
            let done_btn = done_btn.clone();
            let mark_btn = mark_btn.clone();
            glib::MainContext::default().spawn_local(async move {
                let mut handled = false;
                while let Ok(event) = events.recv().await {
                    match event {
                        EngineEvent::BulkActionResult(result) => {
                            match classify_rename_terminal(&result, pid) {
                                RenameTerminal::Ignore => continue,
                                RenameTerminal::Success => {
                                    finish_person_action(&ui, "renamePerson");
                                    lifecycle.complete();
                                    set_status(&ui, "Person saved.".to_string());
                                    schedule_reload_burst(&ui);
                                    dialog.set_can_close(true);
                                    dialog.close();
                                }
                                RenameTerminal::Failure => {
                                    finish_person_action(&ui, "renamePerson");
                                    lifecycle.reset();
                                    set_person_dialog_busy(&group, &done_btn, &mark_btn, false);
                                    let detail = result
                                        .messages
                                        .iter()
                                        .find(|item| !item.ok)
                                        .and_then(|item| item.message.as_deref())
                                        .unwrap_or("the engine rejected the change");
                                    set_status(&ui, format!("Couldn't save person: {detail}"));
                                }
                            }
                            handled = true;
                            break;
                        }
                        EngineEvent::Exited => {
                            finish_person_action(&ui, "renamePerson");
                            lifecycle.reset();
                            set_person_dialog_busy(&group, &done_btn, &mark_btn, false);
                            set_status(&ui, "Couldn't save person: the engine exited.".to_string());
                            handled = true;
                            break;
                        }
                        _ => {}
                    }
                }
                if !handled {
                    finish_person_action(&ui, "renamePerson");
                    lifecycle.reset();
                    set_person_dialog_busy(&group, &done_btn, &mark_btn, false);
                    set_status(
                        &ui,
                        "Couldn't save person: the engine connection closed.".to_string(),
                    );
                }
            });
        });
    }
    {
        let dialog = dialog.clone();
        done_btn.connect_clicked(move |_| {
            dialog.close();
        });
    }
    {
        let ui = ui.clone();
        let lifecycle = lifecycle.clone();
        let dialog = dialog.clone();
        let group = group.clone();
        let done_btn = done_btn.clone();
        let mark_btn = mark_btn.clone();
        mark_btn.clone().connect_clicked(move |_| {
            if !lifecycle.begin(PersonDialogOperation::MarkingUnknown) {
                return;
            }
            if !begin_person_action(&ui, "markPersonsAsUnknown") {
                lifecycle.reset();
                set_status(
                    &ui,
                    "Another Mark Unknown action is still saving.".to_string(),
                );
                return;
            }
            let events = ui.engine.borrow_mut().subscribe();
            set_person_dialog_busy(&group, &done_btn, &mark_btn, true);
            if !send_cmd(
                &ui,
                CommandPayload::MarkPersonsAsUnknown(MarkPersonsAsUnknownPayload {
                    person_ids: vec![pid],
                }),
            ) {
                finish_person_action(&ui, "markPersonsAsUnknown");
                lifecycle.reset();
                set_person_dialog_busy(&group, &done_btn, &mark_btn, false);
                return;
            }
            set_status(&ui, "Saving…".to_string());
            let ui = ui.clone();
            let lifecycle = lifecycle.clone();
            let dialog = dialog.clone();
            let group = group.clone();
            let done_btn = done_btn.clone();
            let mark_btn = mark_btn.clone();
            glib::MainContext::default().spawn_local(async move {
                let mut handled = false;
                while let Ok(event) = events.recv().await {
                    match event {
                        EngineEvent::BulkActionResult(result) => {
                            match classify_person_terminal(&result, "markPersonsAsUnknown", pid) {
                                RenameTerminal::Ignore => continue,
                                RenameTerminal::Success => {
                                    finish_person_action(&ui, "markPersonsAsUnknown");
                                    lifecycle.complete();
                                    set_status(&ui, "Marked as unknown.".to_string());
                                    schedule_reload_burst(&ui);
                                    dialog.set_can_close(true);
                                    dialog.close();
                                }
                                RenameTerminal::Failure => {
                                    finish_person_action(&ui, "markPersonsAsUnknown");
                                    lifecycle.reset();
                                    set_person_dialog_busy(&group, &done_btn, &mark_btn, false);
                                    let message = result
                                        .messages
                                        .iter()
                                        .find_map(|item| {
                                            (!item.ok).then_some(item.message.as_deref()).flatten()
                                        })
                                        .unwrap_or("The engine did not confirm the change.");
                                    set_status(&ui, format!("Couldn't mark as unknown: {message}"));
                                }
                            }
                            handled = true;
                            break;
                        }
                        EngineEvent::Exited => {
                            finish_person_action(&ui, "markPersonsAsUnknown");
                            lifecycle.reset();
                            set_person_dialog_busy(&group, &done_btn, &mark_btn, false);
                            set_status(
                                &ui,
                                "Couldn't mark as unknown: the engine exited.".to_string(),
                            );
                            handled = true;
                            break;
                        }
                        _ => {}
                    }
                }
                if !handled {
                    finish_person_action(&ui, "markPersonsAsUnknown");
                    lifecycle.reset();
                    set_person_dialog_busy(&group, &done_btn, &mark_btn, false);
                    set_status(
                        &ui,
                        "Couldn't mark as unknown: the engine connection closed.".to_string(),
                    );
                }
            });
        });
    }

    dialog.present(Some(&ui.anchor));

    // Photos load off the main loop, then tiles stream in.
    let rx = read_person_files_async(pid);
    let ui = ui.clone();
    let photos_weak = photos.downgrade();
    glib::MainContext::default().spawn_local(async move {
        let files = rx.recv().await.unwrap_or_default();
        let Some(photos) = photos_weak.upgrade() else {
            return;
        };
        for (_id, path) in files {
            let tile = build_photo_tile(&ui, &path);
            photos.append(&tile);
        }
    });
}

fn build_photo_tile(ui: &Rc<Ui>, path: &str) -> gtk::Widget {
    let pic = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Cover)
        .height_request(110)
        .width_request(110)
        .css_classes(["tile-thumb"])
        .build();
    let name = gtk::Label::builder()
        .label(basename(path))
        .xalign(0.5)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .max_width_chars(14)
        .single_line_mode(true)
        .css_classes(["tile-caption"])
        .build();
    let vbox = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .css_classes(["file-tile"])
        .build();
    vbox.append(&pic);
    vbox.append(&name);

    let rx = ui
        .engine
        .borrow()
        .request_scaled_thumbnail(path.to_string(), PHOTO_THUMB_PX);
    let pic_weak = pic.downgrade();
    glib::MainContext::default().spawn_local(async move {
        let Ok(Some(decoded)) = rx.recv().await else {
            return;
        };
        if let Some(pic) = pic_weak.upgrade() {
            pic.set_paintable(Some(&texture_from_decoded(&decoded)));
        }
    });
    vbox.upcast()
}

// ── Merge-target picker (manual merge mode) ───────────────────────────────────

fn open_merge_target_picker(ui: &Rc<Ui>) {
    let ids: Vec<i64> = ui.merge_checked.borrow().iter().copied().collect();
    if ids.len() < 2 {
        return;
    }
    let mut candidates: Vec<PersonRow> = {
        let map = ui.person_by_id.borrow();
        ids.iter().filter_map(|id| map.get(id).cloned()).collect()
    };
    // Named first (alphabetical), then unnamed (by photo count desc).
    candidates.sort_by(|a, b| {
        b.has_any_name()
            .cmp(&a.has_any_name())
            .then(b.file_count.cmp(&a.file_count))
            .then(a.id.cmp(&b.id))
    });

    let dialog = adw::Dialog::new();
    dialog.set_title("Merge into…");
    dialog.set_content_width(480);
    dialog.set_content_height(440);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .margin_top(16)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .build();
    let info = gtk::Label::builder()
        .label(
            "Pick which person becomes the primary. The others are absorbed — their photos \
             move in, and the source clusters disappear.",
        )
        .wrap(true)
        .xalign(0.0)
        .css_classes(["dim-label"])
        .build();
    body.append(&info);

    let listbox = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    for cand in &candidates {
        let row = adw::ActionRow::builder()
            .title(cand.display_name())
            .subtitle(cand.counts())
            .activatable(true)
            .build();
        let icon = gtk::Image::from_icon_name("avatar-default-symbolic");
        icon.add_css_class("gold-accent");
        row.add_prefix(&icon);

        let target_id = cand.id;
        let ui2 = ui.clone();
        let ids2 = ids.clone();
        let dialog2 = dialog.clone();
        row.connect_activated(move |_| {
            if ui2.merge_results_pending.get() > 0 {
                set_status(&ui2, "Wait for the current merge to finish.".to_string());
                return;
            }
            let sources: Vec<i64> = ids2
                .iter()
                .copied()
                .filter(|source| *source != target_id)
                .collect();
            let sent = sources
                .iter()
                .take_while(|source| {
                    send_cmd(
                        &ui2,
                        CommandPayload::MergeClusters(MergeClustersPayload {
                            source_person_id: **source,
                            destination_person_id: target_id,
                        }),
                    )
                })
                .count();
            ui2.merge_results_pending.set(sent);
            if sent > 0 {
                let status = if sent == sources.len() {
                    format!("Merging {} clusters into one…", ids2.len())
                } else {
                    format!(
                        "Sent {sent} of {} merges; reloading after a connection failure.",
                        sources.len()
                    )
                };
                set_status(&ui2, status);
                set_mode(&ui2, Mode::Normal);
                dialog2.close();
            }
        });
        listbox.append(&row);
    }
    let scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&listbox)
        .build();
    body.append(&scroll);
    toolbar.set_content(Some(&body));
    dialog.set_child(Some(&toolbar));
    dialog.present(Some(&ui.anchor));
}

// ── Suggested merges ──────────────────────────────────────────────────────────

fn on_suggest_clicked(ui: &Rc<Ui>, btn: &gtk::Button) {
    btn.set_sensitive(false);
    btn.set_label("Scanning…");
    let rx = ui.engine.borrow_mut().subscribe();
    if !send_cmd(ui, CommandPayload::FindMergeSuggestions(Empty {})) {
        btn.set_sensitive(true);
        btn.set_label("Suggest merges");
        return;
    }
    let ui = ui.clone();
    let btn = btn.clone();
    glib::MainContext::default().spawn_local(async move {
        let cands = loop {
            match rx.recv().await {
                Ok(EngineEvent::MergeSuggestions(result)) => {
                    break result
                        .pairs
                        .into_iter()
                        .map(|pair| Candidate {
                            a: pair.source_person_id,
                            b: pair.destination_person_id,
                            sim: pair.similarity,
                        })
                        .collect();
                }
                Ok(EngineEvent::Exited) | Err(_) => break Vec::new(),
                _ => {}
            }
        };
        btn.set_sensitive(true);
        btn.set_label("Suggest merges");
        // Don't preempt whatever the user navigated to while the scan ran.
        if ui.mode.get() != Mode::Normal {
            return;
        }
        *ui.suggestions.borrow_mut() = cands;
        open_suggested_merges(&ui);
    });
}

fn open_suggested_merges(ui: &Rc<Ui>) {
    let dialog = adw::Dialog::new();
    dialog.set_title("Suggested merges");
    dialog.set_content_width(640);
    dialog.set_content_height(560);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(16)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .build();

    let displayable: Vec<Candidate> = {
        let map = ui.person_by_id.borrow();
        ui.suggestions
            .borrow()
            .iter()
            .copied()
            .filter(|c| map.contains_key(&c.a) && map.contains_key(&c.b))
            .collect()
    };

    let sub = gtk::Label::builder()
        .label(format!(
            "{} cluster pair{} are similar enough to review. Compare each pair before merging.",
            displayable.len(),
            plural(displayable.len() as i64)
        ))
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dim-label"])
        .build();
    body.append(&sub);

    let listbox = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .build();
    if displayable.is_empty() {
        let none = gtk::Label::builder()
            .label("No merge-review candidates found.")
            .wrap(true)
            .xalign(0.0)
            .css_classes(["dim-label"])
            .build();
        listbox.append(&none);
    } else {
        let map = ui.person_by_id.borrow();
        for c in &displayable {
            let pa = &map[&c.a];
            let pb = &map[&c.b];
            let (target, source) = preferred(pa, pb);
            let target_id = target.id;
            let source_id = source.id;
            let (row, merge_btn) = build_suggestion_row(pa, pb, c.sim);
            let ui2 = ui.clone();
            let row_weak = row.downgrade();
            let button_weak = merge_btn.downgrade();
            let dialog_weak = dialog.downgrade();
            merge_btn.connect_clicked(move |button| {
                if ui2.merge_results_pending.get() > 0 {
                    set_status(&ui2, "Wait for the current merge to finish.".to_string());
                    return;
                }
                if send_cmd(
                    &ui2,
                    CommandPayload::MergeClusters(MergeClustersPayload {
                        source_person_id: source_id,
                        destination_person_id: target_id,
                    }),
                ) {
                    button.set_sensitive(false);
                    button.set_label("Merging…");
                    ui2.merge_results_pending.set(1);
                    ui2.pending_suggestions
                        .borrow_mut()
                        .push_back(PendingSuggestion {
                            row: row_weak.clone(),
                            button: button_weak.clone(),
                            dialog: dialog_weak.clone(),
                        });
                    set_status(&ui2, "Merging…".to_string());
                }
            });
            listbox.append(&row);
        }
    }
    let scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&listbox)
        .build();
    body.append(&scroll);

    toolbar.set_content(Some(&body));
    dialog.set_child(Some(&toolbar));
    dialog.present(Some(&ui.anchor));
}

fn build_suggestion_row(pa: &PersonRow, pb: &PersonRow, sim: f32) -> (gtk::Box, gtk::Button) {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .margin_top(10)
        .margin_bottom(10)
        .margin_start(10)
        .margin_end(10)
        .css_classes(["glass-card"])
        .build();
    row.append(&mini_person(pa));
    let arrow = gtk::Label::builder()
        .label("↔")
        .css_classes(["dim-label"])
        .build();
    row.append(&arrow);
    row.append(&mini_person(pb));
    let spacer = gtk::Box::builder().hexpand(true).build();
    row.append(&spacer);

    let right = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .halign(gtk::Align::End)
        .build();
    let sim_label = gtk::Label::new(None);
    sim_label.set_markup(&sim_markup(sim));
    let merge_btn = gtk::Button::builder()
        .label("Merge")
        .css_classes(["gold-button"])
        .build();
    right.append(&sim_label);
    right.append(&merge_btn);
    row.append(&right);
    (row, merge_btn)
}

fn mini_person(p: &PersonRow) -> gtk::Box {
    let b = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .width_request(170)
        .halign(gtk::Align::Start)
        .build();
    let icon = gtk::Image::from_icon_name("avatar-default-symbolic");
    icon.set_pixel_size(28);
    icon.add_css_class("gold-accent");
    let v = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(1)
        .build();
    let name = gtk::Label::builder()
        .label(p.display_name())
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .single_line_mode(true)
        .css_classes(["heading"])
        .build();
    let cnt = gtk::Label::builder()
        .label(format!("{} photo{}", p.file_count, plural(p.file_count)))
        .xalign(0.0)
        .css_classes(["tile-caption"])
        .build();
    v.append(&name);
    v.append(&cnt);
    b.append(&icon);
    b.append(&v);
    b
}

/// Named cluster wins (so a typed name sticks); else the larger photo count.
/// Returns `(target, source)`.
fn preferred<'a>(a: &'a PersonRow, b: &'a PersonRow) -> (&'a PersonRow, &'a PersonRow) {
    let (an, bn) = (a.has_any_name(), b.has_any_name());
    if an && !bn {
        return (a, b);
    }
    if bn && !an {
        return (b, a);
    }
    if a.file_count >= b.file_count {
        (a, b)
    } else {
        (b, a)
    }
}

// ── DB reads (mirror of macOS/Windows ReadStore) ──────────────────────────────

fn read_snapshot_async() -> async_channel::Receiver<Snapshot> {
    let (tx, rx) = async_channel::bounded::<Snapshot>(1);
    std::thread::spawn(move || {
        let snap = read_snapshot().unwrap_or_else(|error| {
            // A read failure must be loud: swallowing it renders the tab as
            // "no people yet" even when the DB is full of faces.
            tracing::warn!(target: "people", %error, "person snapshot read failed");
            Snapshot::default()
        });
        let _ = tx.send_blocking(snap);
    });
    rx
}

fn read_person_files_async(pid: i64) -> async_channel::Receiver<Vec<(i64, String)>> {
    let (tx, rx) = async_channel::bounded::<Vec<(i64, String)>>(1);
    std::thread::spawn(move || {
        let files = read_person_files(pid).unwrap_or_default();
        let _ = tx.send_blocking(files);
    });
    rx
}

// The representative face: the persisted pick when it is still active, else
// the lowest-id active face. Two correlated WHERE-clause lookups — SQLite
// rejects an outer reference (`p.representative_face_id`) inside a scalar
// subquery's ORDER BY with "no such column", which silently emptied the whole
// People tab (the error was swallowed into a default snapshot).
const PERSON_SNAPSHOT_SQL: &str = "\
    SELECT p.id, p.title, p.first_name, p.middle_name, p.last_name, p.suffix, p.name, \
           COALESCE(p.is_unknown, 0), \
           (SELECT COUNT(DISTINCT fp.file_id) FROM face_prints fp JOIN files af ON af.id = fp.file_id WHERE fp.person_id = p.id AND af.failed = 0) AS active_file_count, \
           (SELECT COUNT(*) FROM face_prints fp JOIN files af ON af.id = fp.file_id WHERE fp.person_id = p.id AND af.failed = 0), \
           f.path_text, rf.bbox, COALESCE(f.size_bytes, 0), f.modified_at, f.file_ref, f.content_hash \
    FROM persons p \
    LEFT JOIN face_prints rf ON rf.id = COALESCE( \
        (SELECT fp1.id FROM face_prints fp1 \
         JOIN files af1 ON af1.id = fp1.file_id \
         WHERE fp1.id = p.representative_face_id AND fp1.person_id = p.id AND af1.failed = 0), \
        (SELECT MIN(fp2.id) FROM face_prints fp2 \
         JOIN files af2 ON af2.id = fp2.file_id \
         WHERE fp2.person_id = p.id AND af2.failed = 0)) \
    LEFT JOIN files f ON f.id = rf.file_id AND f.failed = 0 \
    WHERE EXISTS (SELECT 1 FROM face_prints active_fp JOIN files active_f ON active_f.id = active_fp.file_id WHERE active_fp.person_id = p.id AND active_f.failed = 0) \
    ORDER BY \
      CASE WHEN TRIM(COALESCE(p.title,'') || COALESCE(p.first_name,'') || \
           COALESCE(p.last_name,'') || COALESCE(p.name,'')) = '' THEN 1 ELSE 0 END, \
      active_file_count DESC, p.id ASC";

fn read_snapshot() -> anyhow::Result<Snapshot> {
    let Ok(db_path) = fileid_engine::paths::db_path() else {
        return Ok(Snapshot::default());
    };
    if !db_path.exists() {
        return Ok(Snapshot::default());
    }
    let conn = fileid_engine::db::open_read(&db_path)?;
    let total_faces: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM face_prints fp JOIN files f ON f.id = fp.file_id WHERE f.failed = 0",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    let mut stmt = conn.prepare(PERSON_SNAPSHOT_SQL)?;
    let rows = stmt
        .query_map([], map_person)?
        .collect::<rusqlite::Result<Vec<PersonRow>>>()?;
    Ok(Snapshot {
        all_with_faces: rows,
        total_faces,
    })
}

fn map_person(row: &rusqlite::Row<'_>) -> rusqlite::Result<PersonRow> {
    Ok(PersonRow {
        id: row.get(0)?,
        title: row.get(1)?,
        first_name: row.get(2)?,
        middle_name: row.get(3)?,
        last_name: row.get(4)?,
        suffix: row.get(5)?,
        name: row.get(6)?,
        is_unknown: row.get::<_, i64>(7)? != 0,
        file_count: row.get(8)?,
        face_count: row.get(9)?,
        rep_path: row.get(10)?,
        rep_bbox: row.get(11)?,
        rep_size_bytes: row.get(12)?,
        rep_modified: row.get(13)?,
        rep_file_ref: row.get(14)?,
        rep_content_hash: row.get(15)?,
    })
}

fn read_person_files(pid: i64) -> anyhow::Result<Vec<(i64, String)>> {
    let Ok(db_path) = fileid_engine::paths::db_path() else {
        return Ok(Vec::new());
    };
    if !db_path.exists() {
        return Ok(Vec::new());
    }
    let conn = fileid_engine::db::open_read(&db_path)?;
    let mut stmt = conn.prepare(
        "SELECT f.id, f.path_text FROM files f \
         JOIN face_prints fp ON fp.file_id = f.id \
         WHERE fp.person_id = ?1 \
         GROUP BY f.id ORDER BY f.scanned_at DESC LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![pid, PERSON_FILE_LIMIT], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<(i64, String)>>>()?;
    Ok(rows)
}

// ── Thumbnail decode + face crop ──────────────────────────────────────────────

/// Decode raw image bytes, crop to the (pixel, top-left-origin) face bbox with
/// 20% padding like macOS `cropFace`, then scale down to `max_px`. Decoding
/// full-res is required because the bbox is in the original image's pixel space
/// and the DB stores no dimensions to normalize against. Any failure falls back
/// to the uncropped frame, then to `None` (icon placeholder).
fn cropped_texture(bytes: Vec<u8>, bbox: Option<&str>, max_px: i32) -> Option<DecodedImage> {
    let gbytes = glib::Bytes::from_owned(bytes);
    let stream = gio::MemoryInputStream::from_bytes(&gbytes);
    let full = gtk::gdk_pixbuf::Pixbuf::from_stream(&stream, gio::Cancellable::NONE).ok()?;
    let cropped = bbox.and_then(|b| crop_to_bbox(&full, b)).unwrap_or(full);

    let (w, h) = (cropped.width(), cropped.height());
    if w <= 0 || h <= 0 {
        return None;
    }
    let longest = w.max(h);
    let scaled = if longest > max_px {
        let s = max_px as f64 / longest as f64;
        cropped.scale_simple(
            ((w as f64) * s).round() as i32,
            ((h as f64) * s).round() as i32,
            gtk::gdk_pixbuf::InterpType::Bilinear,
        )?
    } else {
        cropped
    };
    Some(DecodedImage::from_pixbuf(&scaled))
}

fn crop_to_bbox(full: &gtk::gdk_pixbuf::Pixbuf, bbox: &str) -> Option<gtk::gdk_pixbuf::Pixbuf> {
    let v: serde_json::Value = serde_json::from_str(bbox).ok()?;
    let x = v.get("x")?.as_f64()?;
    let y = v.get("y")?.as_f64()?;
    let w = v.get("w")?.as_f64()?;
    let h = v.get("h")?.as_f64()?;
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let iw = full.width() as f64;
    let ih = full.height() as f64;
    let pad = 0.20;
    let cx = (x - w * pad).max(0.0);
    let cy = (y - h * pad).max(0.0);
    let mut cw = w * (1.0 + 2.0 * pad);
    let mut ch = h * (1.0 + 2.0 * pad);
    if cx + cw > iw {
        cw = iw - cx;
    }
    if cy + ch > ih {
        ch = ih - cy;
    }
    let (cx, cy, cw, ch) = (cx as i32, cy as i32, cw as i32, ch as i32);
    if cw <= 4 || ch <= 4 || cx < 0 || cy < 0 || cx + cw > full.width() || cy + ch > full.height() {
        return None;
    }
    Some(full.new_subpixbuf(cx, cy, cw, ch))
}

fn person_icon() -> Option<gtk::IconPaintable> {
    let display = gtk::gdk::Display::default()?;
    let theme = gtk::IconTheme::for_display(&display);
    Some(theme.lookup_icon(
        "avatar-default-symbolic",
        &[],
        96,
        1,
        gtk::TextDirection::None,
        gtk::IconLookupFlags::empty(),
    ))
}

// ── Small helpers ─────────────────────────────────────────────────────────────

fn norm(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

fn plural(n: i64) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

fn basename(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

fn sim_markup(s: f32) -> String {
    format!("<span foreground='#A0E2EA' weight='bold'>Similarity {s:.2}</span>")
}

#[cfg(test)]
mod tests {
    use super::{
        classify_rename_terminal, BoundedLru, FaceClusteringLifecycle, PersonActionGate,
        PersonDialogLifecycle, PersonDialogOperation, RenameTerminal,
    };
    use fileid_engine::ipc::{BulkActionItem, BulkActionResult};

    // PR #106 shipped a snapshot query SQLite can't prepare (outer reference
    // inside a scalar subquery's ORDER BY) and the swallowed error blanked the
    // whole tab. Preparing against the real migrated schema catches any drift.
    #[test]
    fn person_snapshot_sql_prepares_against_current_schema() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        fileid_engine::db::migrations::apply(&conn).unwrap();
        conn.prepare(super::PERSON_SNAPSHOT_SQL).unwrap();
    }

    #[test]
    fn person_actions_serialize_by_terminal_action() {
        let gate = PersonActionGate::default();
        assert!(gate.begin("renamePerson"));
        assert!(!gate.begin("renamePerson"));
        assert!(gate.begin("markPersonsAsUnknown"));
        gate.finish("renamePerson");
        assert!(gate.begin("renamePerson"));
    }

    #[test]
    fn face_clustering_lifecycle_is_generation_owned_and_terminal_driven() {
        let mut lifecycle = FaceClusteringLifecycle::new(false);
        assert_eq!(lifecycle.begin(), Err("The engine is still starting."));
        lifecycle.on_ready();
        let first = lifecycle.begin().unwrap();
        assert!(lifecycle.is_active());
        assert_eq!(
            lifecycle.begin(),
            Err("Face grouping is already in progress.")
        );
        assert!(!lifecycle.finish_if(first.wrapping_add(1)));
        assert!(lifecycle.is_active());
        assert_eq!(lifecycle.finish_active(), Some(first));
        let second = lifecycle.begin().unwrap();
        assert!(second > first);
        assert_eq!(lifecycle.on_unavailable(), Some(second));
        assert!(!lifecycle.can_start());
        lifecycle.on_ready();
        assert!(lifecycle.can_start());
        assert!(lifecycle.begin().unwrap() > second);
    }

    #[test]
    fn stale_face_send_rollback_cannot_clear_a_later_generation() {
        let mut lifecycle = FaceClusteringLifecycle::new(true);
        let first = lifecycle.begin().unwrap();
        assert!(lifecycle.finish_if(first));
        let second = lifecycle.begin().unwrap();
        assert!(!lifecycle.finish_if(first));
        assert!(lifecycle.is_active());
        assert!(lifecycle.finish_if(second));
    }

    #[test]
    fn face_busy_rejection_releases_the_rejected_attempt() {
        let mut lifecycle = FaceClusteringLifecycle::new(true);
        assert!(lifecycle.begin().is_ok());
        assert!(lifecycle.finish_active().is_some());
        assert!(lifecycle.can_start());
    }

    fn bulk_result(
        action: &str,
        succeeded: u32,
        failed: u32,
        person_id: Option<i64>,
        ok: bool,
    ) -> BulkActionResult {
        BulkActionResult {
            action: action.into(),
            succeeded,
            failed,
            messages: vec![BulkActionItem {
                file_id: person_id,
                ok,
                message: None,
            }],
        }
    }

    #[test]
    fn rename_terminal_requires_matching_action_and_person() {
        assert_eq!(
            classify_rename_terminal(&bulk_result("applyTags", 1, 0, Some(11), true), 11),
            RenameTerminal::Ignore
        );
        assert_eq!(
            classify_rename_terminal(&bulk_result("renamePerson", 1, 0, Some(22), true), 11),
            RenameTerminal::Ignore
        );
        assert_eq!(
            classify_rename_terminal(&bulk_result("renamePerson", 1, 0, Some(11), true), 11),
            RenameTerminal::Success
        );
        assert_eq!(
            classify_rename_terminal(&bulk_result("renamePerson", 0, 1, None, false), 11),
            RenameTerminal::Failure
        );
        assert_eq!(
            classify_rename_terminal(&bulk_result("renamePerson", 1, 0, None, true), 11),
            RenameTerminal::Ignore
        );
    }

    #[test]
    fn dialog_lifecycle_keeps_rejected_or_failed_edits_retryable() {
        let lifecycle = PersonDialogLifecycle::default();
        assert!(lifecycle.begin(PersonDialogOperation::Renaming));
        assert!(!lifecycle.begin(PersonDialogOperation::Renaming));
        assert!(!lifecycle.begin(PersonDialogOperation::MarkingUnknown));
        lifecycle.reset();
        assert!(lifecycle.begin(PersonDialogOperation::Renaming));
        lifecycle.complete();
        assert!(!lifecycle.begin(PersonDialogOperation::Renaming));
    }

    #[test]
    fn people_thumbnail_cache_never_exceeds_capacity() {
        let mut cache = BoundedLru::new(3);
        for value in 0..100 {
            cache.insert(value, value);
            assert!(cache.len() <= 3);
        }
        assert_eq!(cache.len(), 3);
        assert!(cache.get(&0).is_none());
        assert_eq!(cache.get(&99), Some(99));
    }

    #[test]
    fn people_thumbnail_cache_refreshes_recently_used_entry() {
        let mut cache = BoundedLru::new(2);
        cache.insert("a", 1);
        cache.insert("b", 2);
        assert_eq!(cache.get(&"a"), Some(1));
        cache.insert("c", 3);
        assert_eq!(cache.get(&"a"), Some(1));
        assert!(cache.get(&"b").is_none());
        assert_eq!(cache.get(&"c"), Some(3));
    }
}
