// People review queues beyond single faces (IPC 1.25): suggested merges between
// people, suggested splits of one person, and the face refresh backlog shown in
// the People header. Wording matches the macOS and WinUI apps.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use super::face_review::{
    PAGE_SIZE, crop_picture, load_face, percent, refusal_text, request_id, reveal,
};
use super::workbench_state::catalog_request;
use crate::engine_client::{EngineClient, EngineEvent};
use fileid_engine::ipc::{
    CatalogJob, CatalogRequest, CatalogRequestPayload, CatalogResponse, CommandPayload,
    FaceMergeSuggestion, FaceRefreshStatus, FaceReviewPage, FaceSplitSuggestion,
    ListFaceReviewPayload, ResolveFaceReviewPayload, UndoFaceReviewPayload,
};

pub(super) const REFRESH_JOB: &str = "face-refresh";
const REFRESH_KIND: &str = "faceRefresh";
const PREVIEW_FACES: usize = 6;
const STRIP_PX: i32 = 44;
pub(super) const QUEUE_TITLES: [&str; 3] = ["Faces", "Merges", "Splits"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Queue {
    Merges,
    Splits,
}

impl Queue {
    pub fn wire(self) -> &'static str {
        match self {
            Queue::Merges => "merges",
            Queue::Splits => "splits",
        }
    }

    pub fn empty_text(self) -> &'static str {
        match self {
            Queue::Merges => "No people look like the same person right now.",
            Queue::Splits => "No one looks like two different people right now.",
        }
    }

    pub fn header_text(self, total: i64) -> String {
        match self {
            Queue::Merges => format!("Possibly the same person, {total} waiting"),
            Queue::Splits => format!("Possibly two people, {total} waiting"),
        }
    }
}

pub(super) fn tab_text(title: &str, total: i64) -> String {
    format!("{title} ({total})")
}

pub(super) fn short_name(name: Option<&str>) -> String {
    name.map(str::trim)
        .filter(|name| !name.is_empty())
        .map_or_else(|| "Unnamed".to_string(), str::to_string)
}

pub(super) fn face_count_text(count: i64) -> String {
    if count == 1 {
        "1 face".to_string()
    } else {
        format!("{count} faces")
    }
}

fn kept(merge: &FaceMergeSuggestion) -> String {
    short_name(merge.person_name.as_deref())
}

fn merged(merge: &FaceMergeSuggestion) -> String {
    short_name(merge.other_name.as_deref())
}

pub(super) fn merge_label(merge: &FaceMergeSuggestion) -> String {
    format!("Merge {} into {}", merged(merge), kept(merge))
}

pub(super) fn not_same_label(merge: &FaceMergeSuggestion) -> String {
    format!(
        "{} and {} are not the same person",
        kept(merge),
        merged(merge)
    )
}

pub(super) fn dismiss_merge_label(merge: &FaceMergeSuggestion) -> String {
    format!(
        "Dismiss the suggestion to merge {} into {}",
        merged(merge),
        kept(merge)
    )
}

pub(super) fn merge_sentence(merge: &FaceMergeSuggestion) -> String {
    format!(
        "Might be the same person ({}% alike).",
        percent(merge.score)
    )
}

pub(super) fn merge_row_label(merge: &FaceMergeSuggestion) -> String {
    format!(
        "{} ({}) and {} ({}) might be the same person, {}% alike.",
        kept(merge),
        face_count_text(merge.face_count),
        merged(merge),
        face_count_text(merge.other_face_count),
        percent(merge.score)
    )
}

pub(super) fn merge_done_text(resolution: &str, merge: &FaceMergeSuggestion) -> String {
    match resolution {
        "merge" => format!("Merged {} into {}.", merged(merge), kept(merge)),
        "notSamePerson" => format!(
            "Noted: {} and {} are different people.",
            kept(merge),
            merged(merge)
        ),
        _ => "Dismissed this suggestion.".to_string(),
    }
}

fn split_name(split: &FaceSplitSuggestion) -> String {
    short_name(split.person_name.as_deref())
}

pub(super) fn split_label(split: &FaceSplitSuggestion) -> String {
    format!("Split {} into two people", split_name(split))
}

pub(super) fn dismiss_split_label(split: &FaceSplitSuggestion) -> String {
    format!("Dismiss the suggestion to split {}", split_name(split))
}

pub(super) fn keep_text(split: &FaceSplitSuggestion) -> String {
    format!("Stays ({})", split.keep_count)
}

pub(super) fn moves_text(split: &FaceSplitSuggestion) -> String {
    format!("Moves to a new person ({})", split.split_count)
}

pub(super) fn split_sentence(split: &FaceSplitSuggestion) -> String {
    format!(
        "{}'s faces look like two different people.",
        split_name(split)
    )
}

pub(super) fn split_row_label(split: &FaceSplitSuggestion) -> String {
    format!(
        "{} {} stay, {} would move to a new person.",
        split_sentence(split),
        face_count_text(split.keep_count),
        face_count_text(split.split_count)
    )
}

pub(super) fn split_done_text(resolution: &str, split: &FaceSplitSuggestion) -> String {
    match resolution {
        "split" => format!("Split {} into two people.", split_name(split)),
        _ => "Dismissed this suggestion.".to_string(),
    }
}

pub(super) fn list_payload(id: &str, queue: Queue, offset: i64) -> ListFaceReviewPayload {
    ListFaceReviewPayload {
        request_id: id.to_string(),
        person_id: None,
        queue: Some(queue.wire().to_string()),
        offset: Some(offset),
        limit: Some(PAGE_SIZE),
    }
}

pub(super) fn merge_payload(
    id: &str,
    merge: &FaceMergeSuggestion,
    resolution: &str,
) -> ResolveFaceReviewPayload {
    ResolveFaceReviewPayload {
        request_id: id.to_string(),
        face_id: None,
        person_id: merge.person_id,
        other_person_id: Some(merge.other_person_id),
        resolution: resolution.to_string(),
    }
}

pub(super) fn split_payload(
    id: &str,
    split: &FaceSplitSuggestion,
    resolution: &str,
) -> ResolveFaceReviewPayload {
    ResolveFaceReviewPayload {
        request_id: id.to_string(),
        face_id: None,
        person_id: split.person_id,
        other_person_id: None,
        resolution: resolution.to_string(),
    }
}

pub(super) fn append_unique<T: Clone, K: PartialEq>(
    items: &mut Vec<T>,
    page: &[T],
    key: impl Fn(&T) -> K,
) {
    for item in page {
        if !items.iter().any(|known| key(known) == key(item)) {
            items.push(item.clone());
        }
    }
}

// ── Face refresh status ──────────────────────────────────────────────────────

pub(super) fn refresh_list_payload(id: &str) -> ListFaceReviewPayload {
    ListFaceReviewPayload {
        request_id: id.to_string(),
        person_id: None,
        queue: Some("refresh".to_string()),
        offset: None,
        limit: None,
    }
}

pub(super) fn refresh_request(action: &str) -> CatalogRequest {
    let mut request = catalog_request(action);
    request.limit = None;
    if action != "enqueueFaceRefresh" {
        request.job_id = Some(REFRESH_JOB.to_string());
    }
    request
}

pub(super) fn is_refresh_job(job: &CatalogJob) -> bool {
    job.kind == REFRESH_KIND || job.id == REFRESH_JOB
}

pub(super) fn apply_job(status: &mut FaceRefreshStatus, job: &CatalogJob) {
    status.job_id = Some(job.id.clone());
    status.state = Some(job.state.clone());
    status.progress = Some(job.progress.clamp(0.0, 1.0));
}

fn is_active(state: Option<&str>) -> bool {
    matches!(state, Some("running" | "queued" | "paused"))
}

pub(super) fn refresh_percent(status: &FaceRefreshStatus) -> i64 {
    let fraction = match status.progress {
        Some(progress) => progress,
        None if status.total > 0 => status.done as f64 / status.total as f64,
        None => 0.0,
    };
    (fraction.clamp(0.0, 1.0) * 100.0).floor() as i64
}

pub(super) fn problems_text(status: &FaceRefreshStatus) -> String {
    [
        (status.missing, "missing"),
        (status.offline, "offline"),
        (status.failed, "failed"),
    ]
    .iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, word)| format!(" · {count} {word}"))
    .collect()
}

#[derive(Debug, PartialEq)]
pub(super) struct RefreshView {
    pub text: String,
    pub controls: Vec<(&'static str, &'static str)>,
}

pub(super) fn refresh_view(status: &FaceRefreshStatus) -> Option<RefreshView> {
    let state = status.state.as_deref();
    let problems = problems_text(status);
    if is_active(state) {
        let paused = if state == Some("paused") {
            " (paused)"
        } else {
            ""
        };
        let toggle = if state == Some("paused") {
            ("resumeJob", "Resume")
        } else {
            ("pauseJob", "Pause")
        };
        return Some(RefreshView {
            text: format!(
                "Updating faces: {}%{paused}{problems}",
                refresh_percent(status)
            ),
            controls: vec![toggle, ("cancelJob", "Cancel")],
        });
    }
    if status.model.is_empty() {
        return None;
    }
    let update = ("enqueueFaceRefresh", "Update faces");
    if status.outdated > 0 {
        return Some(RefreshView {
            text: format!("Faces need updating ({}){problems}", status.outdated),
            controls: vec![update],
        });
    }
    if status.job_id.is_some() && !problems.is_empty() {
        return Some(RefreshView {
            text: format!("Faces updated{problems}"),
            controls: vec![update],
        });
    }
    None
}

pub(super) fn control_label(action: &str, status: &FaceRefreshStatus) -> String {
    match action {
        "pauseJob" => "Pause updating faces".to_string(),
        "resumeJob" => "Resume updating faces".to_string(),
        "cancelJob" => "Cancel updating faces".to_string(),
        _ if status.outdated > 0 => format!(
            "Update {} that need updating for the current face model",
            face_count_text(status.outdated)
        ),
        _ => "Try updating faces again".to_string(),
    }
}

pub(super) fn control_failed_text(response: &CatalogResponse) -> String {
    response
        .message
        .clone()
        .unwrap_or_else(|| "FileID couldn't update faces. Try again.".to_string())
}

pub(super) struct RefreshBar {
    engine: Rc<RefCell<EngineClient>>,
    status: RefCell<Option<FaceRefreshStatus>>,
    listing: RefCell<Option<String>>,
    dirty: Cell<bool>,
    control: RefCell<Option<String>>,
    pub root: gtk::Box,
    label: gtk::Label,
    buttons: gtk::Box,
    message: gtk::Label,
}

impl RefreshBar {
    pub fn new(engine: Rc<RefCell<EngineClient>>) -> Rc<Self> {
        let label = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .wrap(true)
            .css_classes(["dim-label"])
            .build();
        let buttons = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .valign(gtk::Align::Center)
            .build();
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(10)
            .build();
        row.append(&label);
        row.append(&buttons);
        let message = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .visible(false)
            .css_classes(["error"])
            .build();
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .visible(false)
            .build();
        root.append(&row);
        root.append(&message);
        root.set_accessible_role(gtk::AccessibleRole::Group);
        root.update_property(&[gtk::accessible::Property::Label("Face update")]);
        let bar = Rc::new(Self {
            engine,
            status: RefCell::new(None),
            listing: RefCell::new(None),
            dirty: Cell::new(false),
            control: RefCell::new(None),
            root,
            label,
            buttons,
            message,
        });
        bar.listen();
        bar
    }

    fn listen(self: &Rc<Self>) {
        let events = self.engine.borrow_mut().subscribe();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            while let Ok(event) = events.recv().await {
                let Some(bar) = weak.upgrade() else { return };
                match event {
                    EngineEvent::FaceReview(page) => bar.on_page(page),
                    EngineEvent::CatalogResponse(response) => bar.on_catalog(&response),
                    EngineEvent::Ready | EngineEvent::FaceClusteringComplete(_) => bar.request(),
                    EngineEvent::Exited => {
                        bar.listing.borrow_mut().take();
                        bar.control.borrow_mut().take();
                        bar.dirty.set(false);
                    }
                    _ => {}
                }
            }
        });
    }

    pub fn request(&self) {
        if !self.engine.borrow().is_ready() {
            return;
        }
        if self.listing.borrow().is_some() {
            self.dirty.set(true);
            return;
        }
        let id = request_id();
        let payload = CommandPayload::ListFaceReview(refresh_list_payload(&id));
        if self.engine.borrow_mut().send(payload).is_ok() {
            *self.listing.borrow_mut() = Some(id);
        }
    }

    fn on_page(self: &Rc<Self>, page: FaceReviewPage) {
        if self.listing.borrow().as_deref() != Some(page.request_id.as_str()) {
            return;
        }
        self.listing.borrow_mut().take();
        if page.status == "ok"
            && let Some(refresh) = page.refresh
        {
            *self.status.borrow_mut() = Some(refresh);
            self.render();
        }
        if self.dirty.replace(false) {
            self.request();
        }
    }

    fn on_catalog(self: &Rc<Self>, response: &CatalogResponse) {
        let mine = self.control.borrow().as_deref() == Some(response.request_id.as_str());
        if mine {
            self.control.borrow_mut().take();
            self.buttons.set_sensitive(true);
            if response.status == "ok" {
                self.message.set_visible(false);
            } else {
                self.message.set_text(&control_failed_text(response));
                self.message.set_visible(true);
                self.root.set_visible(true);
            }
        }
        let job = response.jobs.iter().find(|job| is_refresh_job(job));
        if let Some(job) = job
            && let Some(status) = self.status.borrow_mut().as_mut()
        {
            apply_job(status, job);
        }
        if mine || job.is_some() {
            self.render();
            self.request();
        }
    }

    fn send_control(&self, action: &'static str) {
        if self.control.borrow().is_some() {
            return;
        }
        let request = refresh_request(action);
        let id = request.request_id.clone();
        let payload = CommandPayload::CatalogRequest(Box::new(CatalogRequestPayload { request }));
        match self.engine.borrow_mut().send(payload) {
            Ok(()) => {
                *self.control.borrow_mut() = Some(id);
                self.buttons.set_sensitive(false);
            }
            Err(error) => {
                self.message
                    .set_text(&format!("Command could not be sent: {error}"));
                self.message.set_visible(true);
            }
        }
    }

    fn render(self: &Rc<Self>) {
        let status = self.status.borrow().clone();
        while let Some(child) = self.buttons.first_child() {
            self.buttons.remove(&child);
        }
        let view = status.as_ref().and_then(refresh_view);
        let was_visible = self.root.is_visible();
        self.root
            .set_visible(view.is_some() || self.message.is_visible());
        let (Some(view), Some(status)) = (view, status) else {
            self.label.set_text("");
            return;
        };
        self.label.set_text(&view.text);
        for (action, title) in view.controls {
            let button = gtk::Button::with_label(title);
            if action == "enqueueFaceRefresh" {
                button.add_css_class("gold-button");
            }
            let label = control_label(action, &status);
            button.update_property(&[gtk::accessible::Property::Label(&label)]);
            button.set_tooltip_text(Some(&label));
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(bar) = weak.upgrade() {
                    bar.send_control(action);
                }
            });
            self.buttons.append(&button);
        }
        if !was_visible {
            reveal(&self.root);
        }
    }
}

// ── Merge and split queues ───────────────────────────────────────────────────

enum Pending {
    Reload,
    More,
    Resolve(String),
    Undo,
}

pub(super) struct QueuePanel {
    engine: Rc<RefCell<EngineClient>>,
    queue: Queue,
    merges: RefCell<Vec<FaceMergeSuggestion>>,
    splits: RefCell<Vec<FaceSplitSuggestion>>,
    total: Cell<i64>,
    pending: RefCell<HashMap<String, Pending>>,
    busy: Cell<bool>,
    on_total: RefCell<Vec<Box<dyn Fn(i64)>>>,
    on_changed: RefCell<Vec<Box<dyn Fn()>>>,
    pub root: adw::ToastOverlay,
    header: gtk::Label,
    message: gtk::Label,
    list: gtk::Box,
    empty: gtk::Label,
    more: gtk::Button,
}

impl QueuePanel {
    pub fn new(engine: Rc<RefCell<EngineClient>>, queue: Queue) -> Rc<Self> {
        let header = gtk::Label::builder()
            .xalign(0.0)
            .css_classes(["heading"])
            .build();
        let message = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .visible(false)
            .css_classes(["error"])
            .build();
        let list = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .build();
        let empty = gtk::Label::builder()
            .label(queue.empty_text())
            .xalign(0.0)
            .wrap(true)
            .visible(false)
            .css_classes(["dim-label"])
            .build();
        let more = gtk::Button::builder()
            .label("Show more")
            .halign(gtk::Align::Start)
            .visible(false)
            .build();
        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .build();
        body.append(&header);
        body.append(&message);
        body.append(&list);
        body.append(&empty);
        body.append(&more);
        let root = adw::ToastOverlay::new();
        root.set_child(Some(&body));
        let panel = Rc::new(Self {
            engine,
            queue,
            merges: RefCell::new(Vec::new()),
            splits: RefCell::new(Vec::new()),
            total: Cell::new(0),
            pending: RefCell::new(HashMap::new()),
            busy: Cell::new(false),
            on_total: RefCell::new(Vec::new()),
            on_changed: RefCell::new(Vec::new()),
            root,
            header,
            message,
            list,
            empty,
            more,
        });
        let weak = Rc::downgrade(&panel);
        panel.more.connect_clicked(move |_| {
            if let Some(panel) = weak.upgrade() {
                panel.load_more();
            }
        });
        panel.listen();
        panel.render();
        panel
    }

    pub fn connect_total(&self, callback: impl Fn(i64) + 'static) {
        self.on_total.borrow_mut().push(Box::new(callback));
    }

    pub fn connect_changed(&self, callback: impl Fn() + 'static) {
        self.on_changed.borrow_mut().push(Box::new(callback));
    }

    fn listen(self: &Rc<Self>) {
        let events = self.engine.borrow_mut().subscribe();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            while let Ok(event) = events.recv().await {
                let Some(panel) = weak.upgrade() else { return };
                match event {
                    EngineEvent::FaceReview(page) => panel.on_page(page),
                    EngineEvent::FaceClusteringComplete(_) => panel.reload(),
                    EngineEvent::Exited => {
                        panel.pending.borrow_mut().clear();
                        panel.set_busy(false);
                    }
                    _ => {}
                }
            }
        });
    }

    fn send(&self, payload: CommandPayload, id: String, pending: Pending) -> bool {
        match self.engine.borrow_mut().send(payload) {
            Ok(()) => {
                self.pending.borrow_mut().insert(id, pending);
                true
            }
            Err(error) => {
                self.show_message(&format!("Command could not be sent: {error}"));
                false
            }
        }
    }

    fn shown(&self) -> i64 {
        match self.queue {
            Queue::Merges => self.merges.borrow().len() as i64,
            Queue::Splits => self.splits.borrow().len() as i64,
        }
    }

    pub fn reload(&self) {
        let id = request_id();
        let payload = CommandPayload::ListFaceReview(list_payload(&id, self.queue, 0));
        self.send(payload, id, Pending::Reload);
    }

    fn load_more(&self) {
        let id = request_id();
        let payload = CommandPayload::ListFaceReview(list_payload(&id, self.queue, self.shown()));
        self.send(payload, id, Pending::More);
    }

    fn resolve(&self, payload: ResolveFaceReviewPayload, done: String) {
        if self.busy.get() {
            return;
        }
        let id = payload.request_id.clone();
        if self.send(
            CommandPayload::ResolveFaceReview(payload),
            id,
            Pending::Resolve(done),
        ) {
            self.set_busy(true);
        }
    }

    fn undo(&self, operation: String) {
        let id = request_id();
        let payload = CommandPayload::UndoFaceReview(UndoFaceReviewPayload {
            request_id: id.clone(),
            operation_id: operation,
        });
        self.send(payload, id, Pending::Undo);
    }

    fn on_page(self: &Rc<Self>, page: FaceReviewPage) {
        let Some(pending) = self.pending.borrow_mut().remove(&page.request_id) else {
            return;
        };
        let ok = page.status == "ok";
        if !ok {
            self.show_message(&refusal_text(&page));
        }
        match pending {
            Pending::Reload | Pending::More if ok => {
                let fresh = matches!(pending, Pending::Reload);
                if fresh {
                    self.message.set_visible(false);
                }
                let merges = page.merges.unwrap_or_default();
                let splits = page.splits.unwrap_or_default();
                match self.queue {
                    Queue::Merges => {
                        let mut known = self.merges.borrow_mut();
                        if fresh {
                            known.clear();
                        }
                        append_unique(&mut known, &merges, |m| (m.person_id, m.other_person_id));
                    }
                    Queue::Splits => {
                        let mut known = self.splits.borrow_mut();
                        if fresh {
                            known.clear();
                        }
                        append_unique(&mut known, &splits, |s| s.person_id);
                    }
                }
                self.total.set(page.total);
            }
            Pending::Resolve(done) => {
                self.set_busy(false);
                if ok {
                    self.message.set_visible(false);
                    self.toast(&done, page.operation_id.clone());
                    self.changed();
                }
                self.reload();
            }
            Pending::Undo if ok => {
                self.message.set_visible(false);
                self.changed();
                self.reload();
            }
            _ => {}
        }
        self.render();
    }

    fn changed(&self) {
        for callback in self.on_changed.borrow().iter() {
            callback();
        }
    }

    fn toast(self: &Rc<Self>, title: &str, operation: Option<String>) {
        let toast = adw::Toast::builder()
            .title(title)
            .use_markup(false)
            .timeout(10)
            .build();
        if let Some(operation) = operation {
            toast.set_button_label(Some("Undo"));
            let weak = Rc::downgrade(self);
            toast.connect_button_clicked(move |_| {
                if let Some(panel) = weak.upgrade() {
                    panel.undo(operation.clone());
                }
            });
        }
        self.root.add_toast(toast);
    }

    fn show_message(&self, text: &str) {
        self.message.set_text(text);
        self.message.set_visible(true);
    }

    fn set_busy(&self, busy: bool) {
        self.busy.set(busy);
        self.list.set_sensitive(!busy);
    }

    fn render(self: &Rc<Self>) {
        let total = self.total.get();
        self.header.set_text(&self.queue.header_text(total));
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let rows: Vec<gtk::Box> = match self.queue {
            Queue::Merges => {
                let merges = self.merges.borrow().clone();
                merges.iter().map(|merge| self.merge_row(merge)).collect()
            }
            Queue::Splits => {
                let splits = self.splits.borrow().clone();
                splits.iter().map(|split| self.split_row(split)).collect()
            }
        };
        self.empty.set_visible(rows.is_empty());
        for row in &rows {
            self.list.append(row);
        }
        self.more.set_visible(total > self.shown());
        for callback in self.on_total.borrow().iter() {
            callback(total);
        }
    }

    fn action_button(
        self: &Rc<Self>,
        title: &str,
        label: &str,
        gold: bool,
        payload: ResolveFaceReviewPayload,
        done: String,
    ) -> gtk::Button {
        let button = gtk::Button::with_label(title);
        if gold {
            button.add_css_class("gold-button");
        }
        button.update_property(&[gtk::accessible::Property::Label(label)]);
        button.set_tooltip_text(Some(label));
        let weak = Rc::downgrade(self);
        button.connect_clicked(move |_| {
            if let Some(panel) = weak.upgrade() {
                let mut payload = payload.clone();
                payload.request_id = request_id();
                panel.resolve(payload, done.clone());
            }
        });
        button
    }

    fn buttons(&self) -> gtk::Box {
        gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .valign(gtk::Align::Center)
            .build()
    }

    fn merge_row(self: &Rc<Self>, merge: &FaceMergeSuggestion) -> gtk::Box {
        let kept_crop = crop_picture(&format!("Face of {}", kept(merge)));
        let merged_crop = crop_picture(&format!("Face of {}", merged(merge)));
        load_face(&self.engine, &kept_crop, merge.representative_face_id, None);
        load_face(
            &self.engine,
            &merged_crop,
            merge.other_representative_face_id,
            None,
        );
        let text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .hexpand(true)
            .build();
        for (name, named, count) in [
            (kept(merge), merge.person_name.is_some(), merge.face_count),
            (
                merged(merge),
                merge.other_name.is_some(),
                merge.other_face_count,
            ),
        ] {
            let line = gtk::Label::builder()
                .label(format!("{name} · {}", face_count_text(count)))
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["heading"])
                .build();
            if named {
                line.add_css_class("gold-accent");
            }
            text.append(&line);
        }
        text.append(
            &gtk::Label::builder()
                .label(merge_sentence(merge))
                .xalign(0.0)
                .wrap(true)
                .css_classes(["dim-label"])
                .build(),
        );
        let id = String::new();
        let buttons = self.buttons();
        for (title, resolution, label, gold) in [
            ("Dismiss", "dismiss", dismiss_merge_label(merge), false),
            (
                "Not the same person",
                "notSamePerson",
                not_same_label(merge),
                false,
            ),
            ("Merge", "merge", merge_label(merge), true),
        ] {
            buttons.append(&self.action_button(
                title,
                &label,
                gold,
                merge_payload(&id, merge, resolution),
                merge_done_text(resolution, merge),
            ));
        }
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .css_classes(["glass-card"])
            .build();
        row.append(&kept_crop);
        row.append(&merged_crop);
        row.append(&text);
        row.append(&buttons);
        row.set_accessible_role(gtk::AccessibleRole::Group);
        row.update_property(&[gtk::accessible::Property::Label(&merge_row_label(merge))]);
        reveal(&row);
        row
    }

    fn strip(&self, caption: &str, faces: &[i64], name: &str) -> gtk::Box {
        let crops = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .build();
        for face in faces.iter().take(PREVIEW_FACES) {
            let picture = crop_picture(&format!("Face of {name}"));
            picture.set_width_request(STRIP_PX);
            picture.set_height_request(STRIP_PX);
            load_face(&self.engine, &picture, Some(*face), None);
            crops.append(&picture);
        }
        let strip = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .build();
        strip.append(
            &gtk::Label::builder()
                .label(caption)
                .xalign(0.0)
                .css_classes(["tile-caption"])
                .build(),
        );
        strip.append(&crops);
        strip.set_accessible_role(gtk::AccessibleRole::Group);
        strip.update_property(&[gtk::accessible::Property::Label(caption)]);
        strip
    }

    fn split_row(self: &Rc<Self>, split: &FaceSplitSuggestion) -> gtk::Box {
        let name = split_name(split);
        let title = gtk::Label::builder()
            .label(&name)
            .xalign(0.0)
            .css_classes(["heading"])
            .build();
        if split.person_name.is_some() {
            title.add_css_class("gold-accent");
        }
        let sentence = gtk::Label::builder()
            .label(split_sentence(split))
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label"])
            .build();
        let strips = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(16)
            .build();
        strips.append(&self.strip(&keep_text(split), &split.keep_face_ids, &name));
        strips.append(&self.strip(&moves_text(split), &split.split_face_ids, &name));
        let text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .hexpand(true)
            .build();
        text.append(&title);
        text.append(&sentence);
        text.append(&strips);
        let id = String::new();
        let buttons = self.buttons();
        for (title, resolution, label, gold) in [
            ("Dismiss", "dismiss", dismiss_split_label(split), false),
            ("Split into two", "split", split_label(split), true),
        ] {
            buttons.append(&self.action_button(
                title,
                &label,
                gold,
                split_payload(&id, split, resolution),
                split_done_text(resolution, split),
            ));
        }
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .css_classes(["glass-card"])
            .build();
        row.append(&text);
        row.append(&buttons);
        row.set_accessible_role(gtk::AccessibleRole::Group);
        row.update_property(&[gtk::accessible::Property::Label(&split_row_label(split))]);
        reveal(&row);
        row
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn merge(name: Option<&str>, other: Option<&str>) -> FaceMergeSuggestion {
        FaceMergeSuggestion {
            person_id: 1,
            person_name: name.map(str::to_string),
            representative_face_id: Some(10),
            face_count: 12,
            other_person_id: 2,
            other_name: other.map(str::to_string),
            other_representative_face_id: Some(20),
            other_face_count: 1,
            score: 0.824,
            created_at: 0.0,
        }
    }

    fn split(name: Option<&str>) -> FaceSplitSuggestion {
        FaceSplitSuggestion {
            person_id: 5,
            person_name: name.map(str::to_string),
            representative_face_id: Some(50),
            score: 0.31,
            keep_count: 8,
            split_count: 3,
            keep_face_ids: vec![1, 2],
            split_face_ids: vec![3],
            created_at: 0.0,
        }
    }

    fn status() -> FaceRefreshStatus {
        FaceRefreshStatus {
            model: "arcface".into(),
            processing: "rust-align112-v1".into(),
            outdated: 0,
            total: 0,
            done: 0,
            pending: 0,
            missing: 0,
            offline: 0,
            failed: 0,
            job_id: None,
            state: None,
            progress: None,
        }
    }

    fn job(state: &str, progress: f64) -> CatalogJob {
        CatalogJob {
            id: REFRESH_JOB.into(),
            kind: REFRESH_KIND.into(),
            file_ids: Vec::new(),
            state: state.into(),
            progress,
            error: None,
            created_at: 0.0,
            updated_at: 0.0,
        }
    }

    #[test]
    fn merge_labels_name_both_people_in_plain_words() {
        let m = merge(Some("Alex"), None);
        assert_eq!(merge_label(&m), "Merge Unnamed into Alex");
        assert_eq!(
            not_same_label(&m),
            "Alex and Unnamed are not the same person"
        );
        assert_eq!(
            dismiss_merge_label(&m),
            "Dismiss the suggestion to merge Unnamed into Alex"
        );
        assert_eq!(merge_sentence(&m), "Might be the same person (82% alike).");
        assert_eq!(
            merge_row_label(&m),
            "Alex (12 faces) and Unnamed (1 face) might be the same person, 82% alike."
        );
        assert_eq!(merge_done_text("merge", &m), "Merged Unnamed into Alex.");
        assert_eq!(
            merge_done_text("notSamePerson", &m),
            "Noted: Alex and Unnamed are different people."
        );
        assert_eq!(merge_done_text("dismiss", &m), "Dismissed this suggestion.");
        assert_eq!(short_name(Some("  ")), "Unnamed");
    }

    #[test]
    fn split_labels_describe_both_groups() {
        let s = split(Some("Kim"));
        assert_eq!(split_label(&s), "Split Kim into two people");
        assert_eq!(
            dismiss_split_label(&s),
            "Dismiss the suggestion to split Kim"
        );
        assert_eq!(keep_text(&s), "Stays (8)");
        assert_eq!(moves_text(&s), "Moves to a new person (3)");
        assert_eq!(
            split_row_label(&s),
            "Kim's faces look like two different people. 8 faces stay, 3 faces would move to a new person."
        );
        assert_eq!(split_done_text("split", &s), "Split Kim into two people.");
        assert_eq!(split_done_text("dismiss", &s), "Dismissed this suggestion.");
        assert_eq!(split_label(&split(None)), "Split Unnamed into two people");
    }

    #[test]
    fn queue_texts_and_tabs() {
        assert_eq!(tab_text(QUEUE_TITLES[1], 3), "Merges (3)");
        assert_eq!(
            Queue::Merges.header_text(2),
            "Possibly the same person, 2 waiting"
        );
        assert_eq!(
            Queue::Splits.header_text(0),
            "Possibly two people, 0 waiting"
        );
        assert_eq!(
            Queue::Merges.empty_text(),
            "No people look like the same person right now."
        );
        assert_eq!(
            Queue::Splits.empty_text(),
            "No one looks like two different people right now."
        );
        assert_eq!(face_count_text(1), "1 face");
        assert_eq!(face_count_text(0), "0 faces");
    }

    #[test]
    fn requests_carry_the_queue_and_both_people() {
        let list = list_payload("r1", Queue::Splits, 50);
        assert_eq!(list.queue.as_deref(), Some("splits"));
        assert_eq!(
            (list.offset, list.limit, list.person_id),
            (Some(50), Some(PAGE_SIZE), None)
        );
        let m = merge(Some("Alex"), Some("Sam"));
        let merge = merge_payload("r2", &m, "merge");
        assert_eq!(
            (
                merge.person_id,
                merge.other_person_id,
                merge.face_id,
                merge.resolution.as_str()
            ),
            (1, Some(2), None, "merge")
        );
        let s = split(None);
        let dismiss = split_payload("r3", &s, "dismiss");
        assert_eq!(
            (dismiss.person_id, dismiss.other_person_id, dismiss.face_id),
            (5, None, None)
        );
        let json = serde_json::to_value(&dismiss).unwrap();
        assert!(json.get("otherPersonID").is_none() && json.get("faceID").is_none());
        let refresh = refresh_list_payload("r4");
        assert_eq!(refresh.queue.as_deref(), Some("refresh"));
        assert_eq!((refresh.offset, refresh.limit), (None, None));
    }

    #[test]
    fn refresh_controls_target_the_face_refresh_job() {
        let enqueue = refresh_request("enqueueFaceRefresh");
        assert_eq!(enqueue.action, "enqueueFaceRefresh");
        assert_eq!((enqueue.job_id.as_deref(), enqueue.limit), (None, None));
        for action in ["pauseJob", "resumeJob", "cancelJob"] {
            let request = refresh_request(action);
            assert_eq!(request.action, action);
            assert_eq!(request.job_id.as_deref(), Some("face-refresh"));
        }
    }

    #[test]
    fn refresh_view_follows_the_backlog_and_job() {
        assert_eq!(refresh_view(&status()), None);
        let mut s = status();
        s.outdated = 120;
        assert_eq!(
            refresh_view(&s),
            Some(RefreshView {
                text: "Faces need updating (120)".into(),
                controls: vec![("enqueueFaceRefresh", "Update faces")],
            })
        );
        assert_eq!(
            control_label("enqueueFaceRefresh", &s),
            "Update 120 faces that need updating for the current face model"
        );
        apply_job(&mut s, &job("running", 0.426));
        s.missing = 2;
        s.failed = 1;
        assert_eq!(
            refresh_view(&s),
            Some(RefreshView {
                text: "Updating faces: 42% · 2 missing · 1 failed".into(),
                controls: vec![("pauseJob", "Pause"), ("cancelJob", "Cancel")],
            })
        );
        apply_job(&mut s, &job("paused", 0.5));
        assert_eq!(
            refresh_view(&s).map(|view| (view.text, view.controls)),
            Some((
                "Updating faces: 50% (paused) · 2 missing · 1 failed".into(),
                vec![("resumeJob", "Resume"), ("cancelJob", "Cancel")]
            ))
        );
        apply_job(&mut s, &job("completed", 1.0));
        s.outdated = 0;
        assert_eq!(
            refresh_view(&s).map(|view| view.text),
            Some("Faces updated · 2 missing · 1 failed".into())
        );
        s.missing = 0;
        s.failed = 0;
        assert_eq!(refresh_view(&s), None);
        let mut none = status();
        none.model.clear();
        none.outdated = 4;
        assert_eq!(refresh_view(&none), None);
        assert_eq!(control_label("pauseJob", &s), "Pause updating faces");
        assert_eq!(
            control_label("enqueueFaceRefresh", &s),
            "Try updating faces again"
        );
    }

    #[test]
    fn refresh_percent_prefers_job_progress_and_never_rounds_up() {
        let mut s = status();
        s.total = 3;
        s.done = 2;
        assert_eq!(refresh_percent(&s), 66);
        s.progress = Some(0.999);
        assert_eq!(refresh_percent(&s), 99);
        assert!(is_refresh_job(&job("queued", 0.0)));
    }

    #[test]
    fn appending_pages_skips_suggestions_already_listed() {
        let mut items = vec![merge(None, None)];
        let mut other = merge(None, None);
        other.other_person_id = 9;
        append_unique(&mut items, &[merge(None, None), other], |m| {
            (m.person_id, m.other_person_id)
        });
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn failed_control_shows_the_engine_message() {
        let response = CatalogResponse {
            request_id: "r".into(),
            status: "error".into(),
            message: Some(
                "Install the face recognition model in Settings, then update faces again.".into(),
            ),
            hits: Vec::new(),
            chapters: Vec::new(),
            jobs: Vec::new(),
            events: None,
            takes: None,
            recommendation: None,
            suggested_take_groups: None,
            take_groups: None,
            take_qualities: None,
            take_goal_ranking: None,
        };
        assert_eq!(
            control_failed_text(&response),
            "Install the face recognition model in Settings, then update faces again."
        );
    }
}
