// Library Selected pane (macOS `LibrarySelectedPane`): a 300 px side pane
// that follows the grid's single-click selection with a 16:10 preview, the
// file's facts, people, tags, caption and Open preview / Show in Files / Open.
// A file whose location is offline shows the OFFLINE pill and can't be opened.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use super::offline_roots::OfflineRoots;
use super::util::{decode_px, fmt_date, icon_for_kind, icon_paintable};
use fileid_engine::util::display::file_size_i64;
use crate::engine_client::{texture_from_decoded, EngineClient, FileRow, ThumbReply};

const PANE_PREVIEW_PX: i32 = 1024;
const TAG_LIMIT: i64 = 24;
pub(super) const OFFLINE_NOTE: &str = "Its drive or folder isn't connected. Tags and descriptions are kept.";

/// Tooltips for Show in Files and Open: why they're off while offline.
pub(super) fn action_tips(offline: bool) -> (&'static str, &'static str) {
    if offline {
        (
            "Reconnect the drive to show this file in Files",
            "Reconnect the drive to open this file",
        )
    } else {
        ("Show in Files", "Open in the default app")
    }
}

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
    pub revealer: crate::motion::SpringRevealer,
    engine: Rc<RefCell<EngineClient>>,
    picture: gtk::Picture,
    frame: gtk::AspectFrame,
    actions: gtk::Box,
    reveal: gtk::Button,
    open: gtk::Button,
    offline_note: gtk::Box,
    offline: Rc<RefCell<OfflineRoots>>,
    shown_offline: Cell<bool>,
    name: gtk::Label,
    folder: gtk::Label,
    facts: gtk::Box,
    current: RefCell<Option<FileRow>>,
    generation: Rc<Cell<u64>>,
    ask: gtk::Button,
    picked: Rc<RefCell<Vec<(i64, String)>>>,
}

impl SelectedPane {
    pub fn new(
        engine: Rc<RefCell<EngineClient>>,
        offline: Rc<RefCell<OfflineRoots>>,
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
        close.update_property(&[gtk::accessible::Property::Label("Close")]);
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
        let offline_note = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        offline_note.append(&crate::v3::pill("OFFLINE", crate::shell::status::Tone::Warning));
        let note = crate::v3::wrapped(OFFLINE_NOTE, &["meta"]);
        note.set_hexpand(true);
        offline_note.append(&note);
        offline_note.set_visible(false);
        offline_note.update_property(&[gtk::accessible::Property::Label(&format!("Offline. {OFFLINE_NOTE}"))]);
        body.append(&offline_note);
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
        reveal.update_property(&[gtk::accessible::Property::Label("Show in Files")]);
        let open = gtk::Button::builder()
            .icon_name("document-open-symbolic")
            .css_classes(["secondary"])
            .tooltip_text("Open in the default app")
            .build();
        open.update_property(&[gtk::accessible::Property::Label("Open in the default app")]);
        actions.append(&reveal);
        actions.append(&open);
        body.append(&actions);
        let ask = crate::v3::secondary("Ask about this file", false);
        ask.set_halign(gtk::Align::Start);
        ask.set_tooltip_text(Some("Open Chat with only the selected files attached (Ctrl+click adds files, Shift+click a range)"));
        body.append(&ask);
        let picked: Rc<RefCell<Vec<(i64, String)>>> = Rc::new(RefCell::new(Vec::new()));
        let chosen = picked.clone();
        ask.connect_clicked(move |button| {
            let picked = chosen.borrow();
            let ids: Vec<i64> = picked.iter().map(|(id, _)| *id).collect();
            let kinds: Vec<&str> = picked.iter().map(|(_, kind)| kind.as_str()).collect();
            super::chat_handoff::ask_about(button, &ids, &kinds);
        });

        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .width_request(300)
            .child(&body)
            .build();
        let revealer = crate::motion::SpringRevealer::with_child(crate::motion::Slide::Left, &scroller);

        let pane = Rc::new(Self {
            revealer,
            engine,
            picture,
            frame,
            actions,
            reveal: reveal.clone(),
            open: open.clone(),
            offline_note,
            offline,
            shown_offline: Cell::new(false),
            name,
            folder,
            facts,
            current: RefCell::new(None),
            generation: Rc::new(Cell::new(0)),
            ask,
            picked,
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

    /// The selected files the Ask button attaches to the next chat question.
    pub fn set_picked(&self, picked: Vec<(i64, String)>) {
        let count = picked.len();
        self.ask.set_label(&if count == 1 { "Ask about this file".to_owned() } else { format!("Ask about {count} files") });
        self.ask.update_property(&[gtk::accessible::Property::Label(&format!("Ask Chat about the {count} selected files"))]);
        self.picked.replace(picked);
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
        self.render(row, true);
        self.revealer.set_reveal_child(true);
    }

    /// Redraws the pane when the file on it changed under a reload (a scan
    /// added a caption, smart name or faces). `details` also re-reads people
    /// and tags, which live outside the row; the picture reloads only when
    /// the file itself changed, so a refresh never flickers.
    pub fn refresh(&self, row: FileRow, details: bool) {
        let Some(current) = self.current.borrow().clone() else {
            return;
        };
        if current.id != row.id {
            return;
        }
        let change = row_change(&current, &row);
        if change.facts || details {
            self.render(row, change.picture);
        }
    }

    /// More than one file: the count and their combined size.
    pub fn show_many(&self, count: usize, bytes: i64) {
        self.generation.set(self.generation.get().wrapping_add(1));
        self.current.replace(None);
        self.frame.set_visible(false);
        self.actions.set_visible(false);
        self.set_offline(false);
        self.name.set_text(&super::library_selection::many_title(count));
        let detail = super::library_selection::many_detail(bytes);
        self.folder.set_text(&detail);
        self.folder.set_tooltip_text(None);
        while let Some(child) = self.facts.first_child() {
            self.facts.remove(&child);
        }
        self.revealer.update_property(&[gtk::accessible::Property::Label(&format!(
            "{} selected, {detail}",
            super::library_selection::many_title(count)
        ))]);
        self.revealer.set_reveal_child(true);
    }

    fn render(&self, row: FileRow, picture: bool) {
        self.frame.set_visible(true);
        self.actions.set_visible(true);
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
            .append(&kv_row("Size", &file_size_i64(row.size_bytes)));
        if let Some(name) = row.proposed_name.as_ref().filter(|name| !name.is_empty()) {
            self.facts
                .append(&kv_row("Suggested name", &format!("{name}.{}", row.extension)));
        }
        if let Some(caption) = row
            .description
            .as_ref()
            .filter(|caption| !caption.is_empty())
        {
            self.facts.append(&kv_row("Description", caption));
            if row.caption_inherited {
                self.facts.append(&super::library::inherited_caption_note());
            }
        }
        append_details(&self.facts, row.id);
        if picture {
            self.load_picture(&row);
        }
        self.current.replace(Some(row));
        self.apply_offline();
    }

    /// Re-reads whether the file on the pane is offline (after the library's
    /// roots refresh, or when a new file lands on the pane).
    pub fn apply_offline(&self) {
        let offline = self
            .current
            .borrow()
            .as_ref()
            .is_some_and(|row| self.offline.borrow().is_offline(&row.path));
        self.set_offline(offline);
    }

    fn set_offline(&self, offline: bool) {
        let (reveal_tip, open_tip) = action_tips(offline);
        self.reveal.set_sensitive(!offline);
        self.reveal.set_tooltip_text(Some(reveal_tip));
        self.open.set_sensitive(!offline);
        self.open.set_tooltip_text(Some(open_tip));
        if self.shown_offline.replace(offline) == offline {
            return;
        }
        self.offline_note.set_visible(offline);
        if offline {
            let note = self.offline_note.downgrade();
            let _ = crate::spring::animate(&self.offline_note, 0.0, 1.0, move |value| {
                if let Some(note) = note.upgrade() {
                    note.set_opacity(value.clamp(0.0, 1.0));
                }
            });
        }
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
            decode_px(&self.picture, PANE_PREVIEW_PX),
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

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct RowChange {
    pub facts: bool,
    pub picture: bool,
}

pub(super) fn row_change(old: &FileRow, new: &FileRow) -> RowChange {
    let picture = old.path != new.path
        || old.kind != new.kind
        || old.content_hash != new.content_hash
        || old.modified_at != new.modified_at
        || old.size_bytes != new.size_bytes;
    let facts = picture
        || old.name != new.name
        || old.created_at != new.created_at
        || old.proposed_name != new.proposed_name
        || old.description != new.description
        || old.caption_inherited != new.caption_inherited
        || old.has_faces != new.has_faces
        || old.has_text != new.has_text;
    RowChange { facts, picture }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> FileRow {
        FileRow {
            id: 7,
            path: "/photos/beach.jpg".into(),
            name: "beach.jpg".into(),
            kind: "image".into(),
            extension: "jpg".into(),
            size_bytes: 1_000,
            created_at: Some(1.0),
            modified_at: Some(2.0),
            has_faces: false,
            has_text: false,
            file_ref: None,
            content_hash: Some(vec![1, 2]),
            description: None,
            proposed_name: None,
            caption_inherited: false,
        }
    }

    #[test]
    fn a_scan_that_adds_a_caption_redraws_the_facts_but_keeps_the_picture() {
        let old = row();
        assert_eq!(row_change(&old, &old.clone()), RowChange::default());
        let mut captioned = old.clone();
        captioned.description = Some("Two people on a beach".into());
        captioned.has_faces = true;
        assert_eq!(
            row_change(&old, &captioned),
            RowChange {
                facts: true,
                picture: false
            }
        );
        let mut edited = old.clone();
        edited.content_hash = Some(vec![3]);
        assert_eq!(
            row_change(&old, &edited),
            RowChange {
                facts: true,
                picture: true
            }
        );
    }

    #[test]
    fn offline_files_explain_why_files_and_open_are_off() {
        assert_eq!(action_tips(false), ("Show in Files", "Open in the default app"));
        let (reveal, open) = action_tips(true);
        assert!(reveal.starts_with("Reconnect the drive"));
        assert!(open.starts_with("Reconnect the drive"));
        assert!(OFFLINE_NOTE.contains("Tags and descriptions are kept"));
    }

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
