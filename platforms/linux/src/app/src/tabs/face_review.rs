// Face review queue on the People tab: suggestions the engine made for new
// faces instead of assigning them (IPC 1.23 listFaceReview / resolveFaceReview /
// undoFaceReview). Wording matches the macOS and WinUI apps.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use adw::prelude::*;
use gtk::glib;

use crate::engine_client::{texture_from_decoded, EngineClient, EngineEvent, ThumbReply};
use fileid_engine::ipc::{
    CommandPayload, FaceReviewItem, FaceReviewPage, ListFaceReviewPayload, ResolveFaceReviewPayload,
    UndoFaceReviewPayload,
};

pub(super) const PAGE_SIZE: i64 = 50;
const CROP_PX: i32 = 96;
const UNNAMED: &str = "an unnamed person";

static NEXT_REQUEST: AtomicU64 = AtomicU64::new(0);

pub(super) fn request_id() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    format!("review-{}-{millis}-{}", std::process::id(), NEXT_REQUEST.fetch_add(1, Ordering::Relaxed))
}

pub(super) fn person_name(name: Option<&str>) -> String {
    name.map(str::trim).filter(|name| !name.is_empty()).map_or_else(|| UNNAMED.to_string(), str::to_string)
}

pub(super) fn percent(score: f64) -> i64 {
    (score.clamp(0.0, 1.0) * 100.0).round() as i64
}

pub(super) fn reason_sentence(item: &FaceReviewItem) -> String {
    let name = person_name(item.person_name.as_deref());
    let pct = percent(item.score);
    match item.reason.as_str() {
        "suggested" => format!("Looks like {name} ({pct}% match)."),
        "uncertain" => format!("Might be {name}, but the match is weak ({pct}%)."),
        _ if item.runner_up_name.is_some() || item.runner_up_person_id.is_some() => {
            let runner = person_name(item.runner_up_name.as_deref());
            format!("Could be {name} or {runner}; both look about as close.")
        }
        _ => format!("Could be {name}, but it's a close call."),
    }
}

pub(super) fn file_label(path: &str) -> String {
    crate::v3::file_name(path)
}

pub(super) fn face_label(item: &FaceReviewItem) -> String {
    format!("Face in {}", file_label(&item.path))
}

pub(super) fn accept_label(item: &FaceReviewItem) -> String {
    format!("Accept: this is {}", person_name(item.person_name.as_deref()))
}

pub(super) fn reject_label(item: &FaceReviewItem) -> String {
    format!("Not {}", person_name(item.person_name.as_deref()))
}

pub(super) const IGNORE_LABEL: &str = "Ignore this face";
pub(super) const EMPTY_TEXT: &str = "No suggestions to review.";

pub(super) fn row_label(item: &FaceReviewItem) -> String {
    format!("{} In {}.", reason_sentence(item), file_label(&item.path))
}

pub(super) fn header_text(waiting: i64) -> String {
    format!("Review suggestions, {waiting} waiting")
}

pub(super) fn button_text(waiting: i64) -> String {
    format!("Review suggestions ({waiting})")
}

pub(super) fn resolved_text(resolution: &str, item: &FaceReviewItem) -> String {
    let name = person_name(item.person_name.as_deref());
    match resolution {
        "accept" => format!("Added this face to {name}."),
        "reject" => format!("Noted: this face is not {name}."),
        _ => "Ignored this face.".to_string(),
    }
}

pub(super) fn refusal_text(page: &FaceReviewPage) -> String {
    page.message.clone().unwrap_or_else(|| "FileID couldn't do that. Try again.".to_string())
}

#[derive(Debug, Default)]
pub(super) struct ReviewPaging {
    pub items: Vec<FaceReviewItem>,
    pub total: i64,
}

impl ReviewPaging {
    pub fn replace(&mut self, page: &FaceReviewPage) {
        self.items = page.items.clone();
        self.total = page.total;
    }

    pub fn append(&mut self, page: &FaceReviewPage) {
        for item in &page.items {
            if !self.items.iter().any(|known| known.face_id == item.face_id) {
                self.items.push(item.clone());
            }
        }
        self.total = page.total;
    }

    pub fn remove(&mut self, face_id: i64) -> Option<FaceReviewItem> {
        let index = self.items.iter().position(|item| item.face_id == face_id)?;
        self.total = (self.total - 1).max(0);
        Some(self.items.remove(index))
    }

    pub fn next_offset(&self) -> i64 {
        self.items.len() as i64
    }

    pub fn has_more(&self) -> bool {
        self.total > self.items.len() as i64
    }
}

#[derive(Clone)]
enum Pending {
    Reload,
    More,
    Resolve { item: FaceReviewItem, resolution: &'static str },
    Undo,
}

pub(super) struct ReviewPanel {
    engine: Rc<RefCell<EngineClient>>,
    person: Option<i64>,
    paging: RefCell<ReviewPaging>,
    pending: RefCell<HashMap<String, Pending>>,
    busy: Cell<bool>,
    on_total: RefCell<Option<Box<dyn Fn(i64)>>>,
    pub root: adw::ToastOverlay,
    header: gtk::Label,
    message: gtk::Label,
    list: gtk::Box,
    empty: gtk::Label,
    more: gtk::Button,
}

impl ReviewPanel {
    pub fn new(engine: Rc<RefCell<EngineClient>>, person: Option<i64>) -> Rc<Self> {
        let header = gtk::Label::builder().xalign(0.0).css_classes(["heading"]).build();
        let message = gtk::Label::builder().xalign(0.0).wrap(true).visible(false).css_classes(["error"]).build();
        let list = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).build();
        let empty = gtk::Label::builder().label(EMPTY_TEXT).xalign(0.0).visible(false).css_classes(["dim-label"]).build();
        let more = gtk::Button::builder().label("Show more").halign(gtk::Align::Start).visible(false).build();
        let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10).build();
        body.append(&header);
        body.append(&message);
        body.append(&list);
        body.append(&empty);
        body.append(&more);
        let root = adw::ToastOverlay::new();
        root.set_child(Some(&body));
        let panel = Rc::new(Self {
            engine,
            person,
            paging: RefCell::new(ReviewPaging::default()),
            pending: RefCell::new(HashMap::new()),
            busy: Cell::new(false),
            on_total: RefCell::new(None),
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

    pub fn set_on_total(&self, callback: impl Fn(i64) + 'static) {
        *self.on_total.borrow_mut() = Some(Box::new(callback));
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

    fn list_command(&self, id: &str, offset: i64) -> CommandPayload {
        CommandPayload::ListFaceReview(ListFaceReviewPayload {
            request_id: id.to_string(),
            person_id: self.person,
            offset: Some(offset),
            limit: Some(PAGE_SIZE),
        })
    }

    pub fn reload(&self) {
        let id = request_id();
        self.send(self.list_command(&id, 0), id, Pending::Reload);
    }

    fn load_more(&self) {
        let id = request_id();
        let offset = self.paging.borrow().next_offset();
        self.send(self.list_command(&id, offset), id, Pending::More);
    }

    fn resolve(&self, item: &FaceReviewItem, resolution: &'static str) {
        if self.busy.get() {
            return;
        }
        let id = request_id();
        let payload = CommandPayload::ResolveFaceReview(ResolveFaceReviewPayload {
            request_id: id.clone(),
            face_id: item.face_id,
            person_id: item.person_id,
            resolution: resolution.to_string(),
        });
        if self.send(payload, id, Pending::Resolve { item: item.clone(), resolution }) {
            self.set_busy(true);
        }
    }

    fn undo(&self, operation: String) {
        let id = request_id();
        let payload = CommandPayload::UndoFaceReview(UndoFaceReviewPayload { request_id: id.clone(), operation_id: operation });
        self.send(payload, id, Pending::Undo);
    }

    fn on_page(self: &Rc<Self>, page: FaceReviewPage) {
        let Some(pending) = self.pending.borrow_mut().remove(&page.request_id) else { return };
        let ok = page.status == "ok";
        if !ok {
            self.show_message(&refusal_text(&page));
        }
        match pending {
            Pending::Reload if ok => {
                self.message.set_visible(false);
                self.paging.borrow_mut().replace(&page);
            }
            Pending::More if ok => self.paging.borrow_mut().append(&page),
            Pending::Resolve { item, resolution } => {
                self.set_busy(false);
                if ok {
                    self.message.set_visible(false);
                    self.paging.borrow_mut().remove(item.face_id);
                    self.paging.borrow_mut().total = page.total;
                    self.toast(&resolved_text(resolution, &item), page.operation_id.clone());
                }
            }
            Pending::Undo if ok => {
                self.message.set_visible(false);
                self.reload();
            }
            _ => {}
        }
        self.render();
    }

    fn toast(self: &Rc<Self>, title: &str, operation: Option<String>) {
        let toast = adw::Toast::builder().title(title).use_markup(false).timeout(10).build();
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
        let (items, total, more) = {
            let paging = self.paging.borrow();
            (paging.items.clone(), paging.total, paging.has_more())
        };
        self.header.set_text(&header_text(total));
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        for item in &items {
            self.list.append(&self.build_row(item));
        }
        self.empty.set_visible(items.is_empty());
        self.more.set_visible(more);
        if let Some(callback) = self.on_total.borrow().as_ref() {
            callback(total);
        }
    }

    fn build_row(self: &Rc<Self>, item: &FaceReviewItem) -> gtk::Box {
        let face = crop_picture(&face_label(item));
        let candidate = crop_picture(&person_name(item.person_name.as_deref()));
        load_face(&self.engine, &face, Some(item.face_id), Some((item.path.clone(), Some(item.bbox.clone()))));
        load_face(&self.engine, &candidate, item.representative_face_id, None);
        let reason = gtk::Label::builder().label(reason_sentence(item)).xalign(0.0).wrap(true).build();
        let file = gtk::Label::builder()
            .label(file_label(&item.path))
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .css_classes(["tile-caption"])
            .build();
        let name = gtk::Label::builder()
            .label(person_name(item.person_name.as_deref()))
            .xalign(0.0)
            .css_classes(["heading"])
            .build();
        if item.person_name.is_some() {
            name.add_css_class("gold-accent");
        }
        let text = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).hexpand(true).build();
        text.append(&name);
        text.append(&reason);
        text.append(&file);
        let buttons = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(6).valign(gtk::Align::Center).build();
        for (title, resolution, label, gold) in [
            ("Ignore", "ignore", IGNORE_LABEL.to_string(), false),
            ("Not this person", "reject", reject_label(item), false),
            ("Accept", "accept", accept_label(item), true),
        ] {
            let button = gtk::Button::with_label(title);
            if gold {
                button.add_css_class("gold-button");
            }
            button.update_property(&[gtk::accessible::Property::Label(&label)]);
            button.set_tooltip_text(Some(&label));
            let weak = Rc::downgrade(self);
            let item = item.clone();
            button.connect_clicked(move |_| {
                if let Some(panel) = weak.upgrade() {
                    panel.resolve(&item, resolution);
                }
            });
            buttons.append(&button);
        }
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .css_classes(["glass-card"])
            .build();
        row.append(&face);
        row.append(&candidate);
        row.append(&text);
        row.append(&buttons);
        row.set_accessible_role(gtk::AccessibleRole::Group);
        row.update_property(&[gtk::accessible::Property::Label(&row_label(item))]);
        reveal(&row);
        row
    }
}

fn crop_picture(label: &str) -> gtk::Picture {
    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Cover)
        .width_request(64)
        .height_request(64)
        .css_classes(["tile-thumb"])
        .build();
    picture.update_property(&[gtk::accessible::Property::Label(label)]);
    picture
}

fn load_face(engine: &Rc<RefCell<EngineClient>>, picture: &gtk::Picture, face: Option<i64>, source: Option<(String, Option<String>)>) {
    let picture_weak = picture.downgrade();
    let engine = engine.clone();
    glib::MainContext::default().spawn_local(async move {
        let source = match source {
            Some(source) => Some(source),
            None => match face {
                Some(face) => crate::engine_client::read_db(move |conn| {
                    conn.query_row(
                        "SELECT f.path_text, fp.bbox FROM face_prints fp JOIN files f ON f.id=fp.file_id WHERE fp.id=?1",
                        [face],
                        |row| Ok(Some((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))),
                    )
                    .unwrap_or(None)
                })
                .recv()
                .await
                .ok()
                .flatten(),
                None => None,
            },
        };
        let (Some((path, bbox)), Some(picture)) = (source, picture_weak.upgrade()) else { return };
        let (path, bbox) = super::people::face_thumbnail_source(face, &path, bbox.as_deref());
        let rx = super::people::request_face_thumbnail(&engine, &picture, path, bbox, CROP_PX);
        drop(picture);
        if let Ok(ThumbReply::Image(decoded)) = rx.recv().await
            && let Some(picture) = picture_weak.upgrade()
        {
            picture.set_paintable(Some(&texture_from_decoded(decoded)));
        }
    });
}

fn reveal(row: &gtk::Box) {
    row.set_opacity(0.0);
    let weak = row.downgrade();
    crate::spring::animate_with(crate::spring::Spring::Standard, row, 0.0, 1.0, move |value| {
        if let Some(row) = weak.upgrade() {
            row.set_opacity(value.clamp(0.0, 1.0));
        }
    });
}

/// Asks for the number of waiting suggestions (every person's) and hands it to `done`.
pub(super) fn count_waiting(engine: &Rc<RefCell<EngineClient>>, done: impl Fn(i64) + 'static) {
    let id = request_id();
    let events = engine.borrow_mut().subscribe();
    let payload = CommandPayload::ListFaceReview(ListFaceReviewPayload {
        request_id: id.clone(),
        person_id: None,
        offset: Some(0),
        limit: Some(1),
    });
    if engine.borrow_mut().send(payload).is_err() {
        return;
    }
    glib::MainContext::default().spawn_local(async move {
        while let Ok(event) = events.recv().await {
            match event {
                EngineEvent::FaceReview(page) if page.request_id == id => {
                    if page.status == "ok" {
                        done(page.total);
                    }
                    return;
                }
                EngineEvent::Exited => return,
                _ => {}
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(reason: &str, name: Option<&str>, runner_id: Option<i64>, runner: Option<&str>) -> FaceReviewItem {
        FaceReviewItem {
            face_id: 7,
            file_id: 3,
            path: "/photos/Beach Day.jpg".into(),
            bbox: "b".into(),
            person_id: 1,
            person_name: name.map(str::to_string),
            representative_face_id: Some(2),
            score: 0.616,
            reason: reason.into(),
            runner_up_person_id: runner_id,
            runner_up_name: runner.map(str::to_string),
            runner_up_score: None,
            created_at: 0.0,
        }
    }

    fn page(items: Vec<FaceReviewItem>, total: i64) -> FaceReviewPage {
        FaceReviewPage {
            request_id: "r".into(),
            action: "list".into(),
            status: "ok".into(),
            message: None,
            operation_id: None,
            face_id: None,
            person_id: None,
            total,
            offset: 0,
            items,
        }
    }

    #[test]
    fn reasons_follow_the_shared_wording() {
        assert_eq!(reason_sentence(&item("suggested", Some("Alex"), None, None)), "Looks like Alex (62% match).");
        assert_eq!(
            reason_sentence(&item("uncertain", None, None, None)),
            "Might be an unnamed person, but the match is weak (62%)."
        );
        assert_eq!(
            reason_sentence(&item("ambiguous", Some("Alex"), Some(4), Some("Sam"))),
            "Could be Alex or Sam; both look about as close."
        );
        assert_eq!(
            reason_sentence(&item("ambiguous", Some("Alex"), Some(4), None)),
            "Could be Alex or an unnamed person; both look about as close."
        );
        assert_eq!(reason_sentence(&item("ambiguous", Some("Alex"), None, None)), "Could be Alex, but it's a close call.");
        assert_eq!(person_name(Some("  ")), "an unnamed person");
        assert_eq!(percent(0.554), 55);
    }

    #[test]
    fn labels_name_the_person_and_file() {
        let alex = item("suggested", Some("Alex"), None, None);
        assert_eq!(face_label(&alex), "Face in Beach Day.jpg");
        assert_eq!(accept_label(&alex), "Accept: this is Alex");
        assert_eq!(reject_label(&alex), "Not Alex");
        assert_eq!(row_label(&alex), "Looks like Alex (62% match). In Beach Day.jpg.");
        assert_eq!(header_text(3), "Review suggestions, 3 waiting");
        assert_eq!(button_text(3), "Review suggestions (3)");
    }

    #[test]
    fn paging_appends_without_duplicates_and_drops_resolved_rows() {
        let mut paging = ReviewPaging::default();
        let mut first = item("suggested", None, None, None);
        first.face_id = 1;
        let mut second = first.clone();
        second.face_id = 2;
        paging.replace(&page(vec![first.clone()], 2));
        assert!(paging.has_more());
        assert_eq!(paging.next_offset(), 1);
        paging.append(&page(vec![first.clone(), second], 2));
        assert_eq!(paging.items.len(), 2);
        assert!(!paging.has_more());
        assert_eq!(paging.remove(1).map(|item| item.face_id), Some(1));
        assert_eq!(paging.total, 1);
        assert!(paging.remove(9).is_none());
    }

    #[test]
    fn request_ids_are_unique_nonces() {
        let a = request_id();
        let b = request_id();
        assert_ne!(a, b);
        assert!(a.len() <= 128 && a.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'));
    }
}
