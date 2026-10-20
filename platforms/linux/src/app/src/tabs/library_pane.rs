// Library Selected pane (macOS `LibrarySelectedPane`): a 300 px side pane
// that follows the grid's single-click selection with a 16:10 preview, the
// file's facts, people, tags, caption and Open preview / Show in Files / Open.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use super::util::{fmt_date, format_bytes, icon_for_kind, icon_paintable};
use crate::engine_client::{texture_from_decoded, EngineClient, FileRow, ThumbReply};

const PANE_PREVIEW_PX: i32 = 1024;
const TAG_LIMIT: i64 = 24;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct FileDetails {
    pub people: Vec<String>,
    pub tags: Vec<String>,
}

/// People and tags for one file, read off the main loop on a short-lived
/// read connection (one per selection, never per tile).
pub(super) fn read_details(file_id: i64) -> async_channel::Receiver<FileDetails> {
    crate::engine_client::read_db(move |conn| details_from(conn, file_id))
}

fn details_from(conn: &rusqlite::Connection, file_id: i64) -> FileDetails {
    let strings = |sql: &str, limit: i64| -> Vec<String> {
        conn.prepare(sql)
            .and_then(|mut stmt| {
                stmt.query_map(rusqlite::params![file_id, limit], |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap_or_default()
    };
    FileDetails {
        people: strings(
            "SELECT DISTINCT p.name FROM face_prints f JOIN persons p ON p.id = f.person_id \
             WHERE f.file_id = ?1 AND TRIM(COALESCE(p.name, '')) <> '' ORDER BY p.name LIMIT ?2",
            TAG_LIMIT,
        ),
        tags: strings(
            "SELECT tag FROM tags WHERE file_id = ?1 GROUP BY tag \
             ORDER BY MAX(COALESCE(score, 0)) DESC, tag LIMIT ?2",
            TAG_LIMIT,
        ),
    }
}

pub(super) fn kv_row(key: &str, value: &str) -> gtk::Box {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .css_classes(["kv-row"])
        .build();
    row.append(
        &gtk::Label::builder()
            .label(key.to_uppercase())
            .xalign(0.0)
            .width_request(80)
            .valign(gtk::Align::Start)
            .css_classes(["kv-key"])
            .build(),
    );
    row.append(
        &gtk::Label::builder()
            .label(value)
            .xalign(0.0)
            .wrap(true)
            .selectable(true)
            .hexpand(true)
            .css_classes(["kv-value"])
            .build(),
    );
    row
}

pub(super) fn chips(values: &[String]) -> gtk::FlowBox {
    let flow = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .max_children_per_line(12)
        .row_spacing(4)
        .column_spacing(4)
        .build();
    for value in values {
        flow.append(
            &gtk::Label::builder()
                .label(value)
                .css_classes(["chip"])
                .build(),
        );
    }
    flow
}

/// Appends People and Tags rows to `target` once the off-main read lands.
pub(super) fn append_details(target: &gtk::Box, file_id: i64) {
    let rx = read_details(file_id);
    let weak = target.downgrade();
    glib::MainContext::default().spawn_local(async move {
        let Ok(details) = rx.recv().await else { return };
        let Some(target) = weak.upgrade() else { return };
        if !details.people.is_empty() {
            target.append(&kv_row("People", &details.people.join(", ")));
        }
        if !details.tags.is_empty() {
            target.append(
                &gtk::Label::builder()
                    .label("TAGS")
                    .xalign(0.0)
                    .css_classes(["kv-key"])
                    .build(),
            );
            target.append(&chips(&details.tags));
        }
    });
}

pub(super) struct SelectedPane {
    pub revealer: gtk::Revealer,
    engine: Rc<RefCell<EngineClient>>,
    picture: gtk::Picture,
    name: gtk::Label,
    folder: gtk::Label,
    facts: gtk::Box,
    current: RefCell<Option<FileRow>>,
    generation: Rc<Cell<u64>>,
}

impl SelectedPane {
    pub fn new(
        engine: Rc<RefCell<EngineClient>>,
        on_preview: Rc<dyn Fn()>,
        on_close: Rc<dyn Fn()>,
    ) -> Rc<Self> {
        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .css_classes(["selected-pane"])
            .build();
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        head.append(
            &gtk::Label::builder()
                .label("SELECTED")
                .xalign(0.0)
                .hexpand(true)
                .css_classes(["section-label"])
                .build(),
        );
        let close = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .css_classes(["tb-plain"])
            .tooltip_text("Close (Esc)")
            .build();
        close.connect_clicked(move |_| on_close());
        head.append(&close);
        body.append(&head);

        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Cover)
            .css_classes(["tile-thumb"])
            .build();
        let frame = gtk::AspectFrame::builder()
            .ratio(1.6)
            .obey_child(false)
            .child(&picture)
            .build();
        body.append(&frame);
        let name = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["title-3"])
            .build();
        body.append(&name);
        let folder = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .css_classes(["meta"])
            .build();
        body.append(&folder);
        let facts = gtk::Box::new(gtk::Orientation::Vertical, 6);
        body.append(&facts);

        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let preview = gtk::Button::builder()
            .label("Open preview")
            .css_classes(["gold-button", "compact"])
            .build();
        preview.connect_clicked(move |_| on_preview());
        actions.append(&preview);
        let reveal = gtk::Button::builder()
            .icon_name("folder-open-symbolic")
            .css_classes(["secondary"])
            .tooltip_text("Show in Files")
            .build();
        let open = gtk::Button::builder()
            .icon_name("document-open-symbolic")
            .css_classes(["secondary"])
            .tooltip_text("Open in the default app")
            .build();
        actions.append(&reveal);
        actions.append(&open);
        body.append(&actions);

        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .width_request(300)
            .child(&body)
            .build();
        let revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideLeft)
            .transition_duration(220)
            .reveal_child(false)
            .child(&scroller)
            .build();

        let pane = Rc::new(Self {
            revealer,
            engine,
            picture,
            name,
            folder,
            facts,
            current: RefCell::new(None),
            generation: Rc::new(Cell::new(0)),
        });
        let weak = Rc::downgrade(&pane);
        reveal.connect_clicked(move |button| {
            if let Some(row) = weak
                .upgrade()
                .and_then(|pane| pane.current.borrow().clone())
            {
                crate::v3::show_in_files(button, &row.path);
            }
        });
        let weak = Rc::downgrade(&pane);
        open.connect_clicked(move |button| {
            if let Some(row) = weak
                .upgrade()
                .and_then(|pane| pane.current.borrow().clone())
            {
                crate::v3::open_file(button, &row.path);
            }
        });
        pane
    }

    pub fn show(&self, row: FileRow) {
        if self
            .current
            .borrow()
            .as_ref()
            .is_some_and(|current| current.id == row.id)
        {
            self.revealer.set_reveal_child(true);
            return;
        }
        self.name.set_text(&row.name);
        let folder = crate::v3::parent_name(&row.path);
        self.folder.set_text(&folder);
        self.folder.set_tooltip_text(Some(&folder));
        while let Some(child) = self.facts.first_child() {
            self.facts.remove(&child);
        }
        self.facts.append(&kv_row("Kind", &row.kind));
        if let Some(date) = fmt_date(row.created_at.or(row.modified_at)) {
            self.facts.append(&kv_row("Captured", &date));
        }
        self.facts
            .append(&kv_row("Size", &format_bytes(row.size_bytes)));
        if let Some(name) = row.proposed_name.as_ref().filter(|name| !name.is_empty()) {
            self.facts
                .append(&kv_row("Smart name", &format!("{name}.{}", row.extension)));
        }
        if let Some(caption) = row
            .description
            .as_ref()
            .filter(|caption| !caption.is_empty())
        {
            self.facts.append(&kv_row("Caption", caption));
        }
        append_details(&self.facts, row.id);
        self.load_picture(&row);
        self.current.replace(Some(row));
        self.revealer.set_reveal_child(true);
    }

    pub fn hide(&self) {
        self.revealer.set_reveal_child(false);
        self.current.replace(None);
    }

    pub fn current_id(&self) -> Option<i64> {
        self.current.borrow().as_ref().map(|row| row.id)
    }

    fn load_picture(&self, row: &FileRow) {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        let fallback = icon_paintable(icon_for_kind(&row.kind), 96);
        self.picture.set_paintable(fallback.as_ref());
        if row.kind != "image" && row.kind != "video" {
            return;
        }
        let rx = self.engine.borrow().request_preview(
            row.path.clone(),
            PANE_PREVIEW_PX,
            row.kind == "video",
        );
        let picture = self.picture.downgrade();
        let pane_generation = self.generation.clone();
        glib::MainContext::default().spawn_local(async move {
            let Ok(ThumbReply::Image(decoded)) = rx.recv().await else {
                return;
            };
            if pane_generation.get() != generation {
                return;
            }
            if let Some(picture) = picture.upgrade() {
                picture.set_paintable(Some(&texture_from_decoded(decoded)));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn details_list_named_people_and_top_tags_once() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE tags (file_id INTEGER, tag TEXT, source TEXT, score DOUBLE);
             CREATE TABLE persons (id INTEGER PRIMARY KEY, name TEXT);
             CREATE TABLE face_prints (id INTEGER PRIMARY KEY, file_id INTEGER, person_id INTEGER);
             INSERT INTO tags VALUES (1, 'beach', 'clip', 0.9), (1, 'beach', 'ram', 0.4), (1, 'dog', 'ram', 0.95), (2, 'car', 'ram', 1.0);
             INSERT INTO persons VALUES (1, 'Ada'), (2, NULL), (3, '  ');
             INSERT INTO face_prints VALUES (1, 1, 1), (2, 1, 1), (3, 1, 2), (4, 1, 3);",
        )
        .unwrap();
        assert_eq!(
            details_from(&conn, 1),
            FileDetails {
                people: vec!["Ada".into()],
                tags: vec!["dog".into(), "beach".into()]
            }
        );
        assert_eq!(details_from(&conn, 9), FileDetails::default());
    }
}
