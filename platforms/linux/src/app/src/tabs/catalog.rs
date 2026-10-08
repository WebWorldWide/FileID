use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use adw::prelude::*;
use fileid_engine::ipc::{
    CatalogChapter, CatalogHit, CatalogRequest, CatalogRequestPayload, CatalogResponse,
    CommandPayload,
};
use gtk::glib;

use crate::engine_client::{texture_from_decoded, EngineClient, EngineEvent};

struct Ui {
    engine: Rc<RefCell<EngineClient>>,
    pending: RefCell<Option<String>>,
    action: RefCell<String>,
    hits: RefCell<Vec<CatalogHit>>,
    chapters: RefCell<Vec<CatalogChapter>>,
    selected: RefCell<Option<CatalogHit>>,
    editing: RefCell<Option<String>>,
    preview_generation: Cell<u64>,
    query: gtk::SearchEntry,
    search: gtk::Button,
    files: gtk::ListBox,
    markers: gtk::ListBox,
    filename: gtk::Label,
    picture: gtk::Picture,
    media_hint: gtk::Label,
    title: gtk::Entry,
    summary: gtk::TextView,
    start: gtk::Entry,
    end: gtk::Entry,
    new: gtk::Button,
    refresh: gtk::Button,
    undo: gtk::Button,
    remove: gtk::Button,
    save: gtk::Button,
    status: gtk::Label,
}

pub fn present(engine: &Rc<RefCell<EngineClient>>, parent: &gtk::Button) {
    let query = gtk::SearchEntry::builder()
        .placeholder_text("Search files, descriptions, or chapters")
        .hexpand(true)
        .build();
    let search = gtk::Button::with_label("Search");
    let files = gtk::ListBox::new();
    files.set_selection_mode(gtk::SelectionMode::Single);
    let markers = gtk::ListBox::new();
    markers.set_selection_mode(gtk::SelectionMode::Single);
    let filename = label("");
    filename.add_css_class("title-3");
    let picture = gtk::Picture::builder()
        .height_request(140)
        .can_shrink(true)
        .build();
    let media_hint =
        label("Automatic chapter analysis and video playback are not available here yet.");
    let title = gtk::Entry::builder()
        .placeholder_text("Chapter title")
        .max_length(200)
        .build();
    let summary = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .build();
    let start = gtk::Entry::builder()
        .placeholder_text("Start seconds")
        .text("0")
        .width_chars(12)
        .hexpand(true)
        .build();
    let end = gtk::Entry::builder()
        .placeholder_text("End seconds")
        .text("0")
        .width_chars(12)
        .hexpand(true)
        .build();
    let new = gtk::Button::with_label("New chapter");
    let refresh = gtk::Button::with_label("Refresh");
    let undo = gtk::Button::with_label("Undo edit");
    let remove = gtk::Button::with_label("Remove selected");
    let save = gtk::Button::with_label("Add chapter");
    let status = label("Search to choose a file or timestamp. Markers are stored in the catalog; originals remain unchanged.");
    let ui = Rc::new(Ui {
        engine: engine.clone(),
        pending: RefCell::new(None),
        action: RefCell::new(String::new()),
        hits: RefCell::new(vec![]),
        chapters: RefCell::new(vec![]),
        selected: RefCell::new(None),
        editing: RefCell::new(None),
        preview_generation: Cell::new(0),
        query,
        search,
        files,
        markers,
        filename,
        picture,
        media_hint,
        title,
        summary,
        start,
        end,
        new,
        refresh,
        undo,
        remove,
        save,
        status,
    });
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_start(20);
    body.set_margin_end(20);
    body.set_margin_top(20);
    body.set_margin_bottom(20);
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let heading = label("Search & Moments");
    heading.add_css_class("title-2");
    heading.set_hexpand(true);
    let done = gtk::Button::with_label("Done");
    header.append(&heading);
    header.append(&done);
    body.append(&header);
    let search_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    search_row.append(&ui.query);
    search_row.append(&ui.search);
    body.append(&search_row);
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    content.append(
        &gtk::ScrolledWindow::builder()
            .width_request(250)
            .vexpand(true)
            .child(&ui.files)
            .build(),
    );
    let editor = gtk::Box::new(gtk::Orientation::Vertical, 10);
    editor.set_hexpand(true);
    editor.append(&ui.filename);
    editor.append(&ui.picture);
    editor.append(&ui.media_hint);
    editor.append(
        &gtk::ScrolledWindow::builder()
            .min_content_height(100)
            .child(&ui.markers)
            .build(),
    );
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    for button in [&ui.new, &ui.refresh, &ui.undo, &ui.remove] {
        actions.append(button);
    }
    editor.append(&actions);
    editor.append(&ui.title);
    editor.append(
        &gtk::ScrolledWindow::builder()
            .min_content_height(70)
            .child(&ui.summary)
            .build(),
    );
    let times = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    times.append(&ui.start);
    times.append(&ui.end);
    times.append(&ui.save);
    editor.append(&times);
    content.append(
        &gtk::ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .child(&editor)
            .build(),
    );
    body.append(&content);
    body.append(&ui.status);
    let dialog = adw::Dialog::builder()
        .title("Search & Moments")
        .content_width(960)
        .content_height(680)
        .child(&body)
        .build();
    let weak = dialog.downgrade();
    done.connect_clicked(move |_| {
        if let Some(dialog) = weak.upgrade() {
            let _ = dialog.close();
        }
    });
    wire(&ui);
    let rx = ui.engine.borrow_mut().subscribe();
    let weak = Rc::downgrade(&ui);
    let task = glib::MainContext::default().spawn_local(async move {
        while let Ok(event) = rx.recv().await {
            let Some(ui) = weak.upgrade() else { return; };
            match event {
                EngineEvent::CatalogResponse(response) => receive(&ui, response),
                EngineEvent::Spawning | EngineEvent::Exited | EngineEvent::Failed(_) => {
                    ui.pending.borrow_mut().take(); update(&ui);
                    ui.status.set_label("The engine stopped or restarted. Refresh chapters before retrying an edit.");
                }
                _ => {}
            }
        }
    });
    dialog.connect_closed(move |_| task.abort());
    let keep_alive = ui.clone();
    dialog.connect_closed(move |_| {
        keep_alive
            .preview_generation
            .set(keep_alive.preview_generation.get().wrapping_add(1));
    });
    update(&ui);
    dialog.present(Some(parent));
}

fn label(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .build()
}
fn clear(list: &gtk::ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
}
fn nonce() -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    format!(
        "gtk-moments-{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}
fn update(ui: &Ui) {
    let ready = ui.pending.borrow().is_none();
    let selected = ui.selected.borrow().is_some();
    ui.query.set_sensitive(ready);
    ui.search.set_sensitive(ready);
    ui.files.set_sensitive(ready);
    ui.markers.set_sensitive(ready);
    for button in [&ui.new, &ui.refresh, &ui.undo, &ui.save] {
        button.set_sensitive(ready && selected);
    }
    ui.remove
        .set_sensitive(ready && ui.markers.selected_row().is_some());
    ui.title.set_sensitive(ready && selected);
    ui.summary.set_sensitive(ready && selected);
    ui.start.set_sensitive(ready && selected);
    ui.end.set_sensitive(ready && selected);
}
fn reset(ui: &Ui) {
    ui.editing.borrow_mut().take();
    ui.title.set_text("");
    ui.summary.buffer().set_text("");
    ui.start.set_text("0");
    ui.end.set_text("0");
    ui.save.set_label("Add chapter");
    update(ui);
}
fn send(ui: &Ui, action: &str) {
    if ui.pending.borrow().is_some() {
        return;
    }
    let request_id = nonce();
    let mut request = CatalogRequest {
        request_id: request_id.clone(),
        action: action.into(),
        query: None,
        file_id: ui.selected.borrow().as_ref().map(|hit| hit.file_id),
        chapter: None,
        chapter_id: None,
        job_id: None,
        file_ids: None,
        search_mode: None,
        result_scope: None,
        query_vector: None,
        embedding_model: None,
        limit: None,
        timeline_mode: None,
        event: None,
        event_id: None,
        take_feedback: None,
    };
    if action == "search" {
        let query = ui.query.text().trim().to_owned();
        if query.is_empty() {
            return;
        }
        request.query = Some(query);
    } else if action == "saveChapter" {
        let Some(file_id) = request.file_id else {
            return;
        };
        let start = ui.start.text().parse::<f64>();
        let end = ui.end.text().parse::<f64>();
        let (Ok(start), Ok(end)) = (start, end) else {
            ui.status
                .set_label("Enter start and end times in seconds, using a decimal point.");
            return;
        };
        let title = ui.title.text().trim().to_owned();
        let buffer = ui.summary.buffer();
        let summary = buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .to_string();
        if !start.is_finite()
            || !end.is_finite()
            || start < 0.0
            || end < start
            || title.is_empty()
            || title.chars().count() > 200
            || summary.chars().count() > 4000
        {
            ui.status.set_label(
                "Use a title, a bounded description, and finite times with end at or after start.",
            );
            return;
        }
        let id = ui
            .editing
            .borrow()
            .clone()
            .unwrap_or_else(|| format!("chapter-{request_id}"));
        *ui.editing.borrow_mut() = Some(id.clone());
        ui.save.set_label("Save chapter");
        request.chapter = Some(CatalogChapter {
            id,
            file_id,
            start_seconds: start,
            end_seconds: end,
            title,
            summary,
            source_revision: String::new(),
            model_version: "user".into(),
            confidence: 1.0,
            user_edited: true,
            stale: false,
        });
    } else if action == "deleteChapter" {
        let Some(row) = ui.markers.selected_row() else {
            return;
        };
        request.chapter_id = ui
            .chapters
            .borrow()
            .get(row.index() as usize)
            .map(|chapter| chapter.id.clone());
    }
    *ui.pending.borrow_mut() = Some(request_id);
    *ui.action.borrow_mut() = action.into();
    update(ui);
    ui.status.set_label("Working…");
    if ui
        .engine
        .borrow_mut()
        .send(CommandPayload::CatalogRequest(Box::new(
            CatalogRequestPayload { request },
        )))
        .is_err()
    {
        ui.pending.borrow_mut().take();
        update(ui);
        ui.status.set_label(
            "The engine could not receive this request. Refresh before retrying an edit.",
        );
    }
}
fn receive(ui: &Ui, response: CatalogResponse) {
    if ui.pending.borrow().as_deref() != Some(&response.request_id) {
        return;
    }
    ui.pending.borrow_mut().take();
    if response.status == "ok" {
        if ui.action.borrow().as_str() == "search" {
            ui.selected.borrow_mut().take();
            clear(&ui.files);
            ui.picture.set_paintable(None::<&gtk::gdk::Paintable>);
            ui.filename.set_label("");
            clear(&ui.markers);
            reset(ui);
            *ui.hits.borrow_mut() = response.hits;
            for hit in ui.hits.borrow().iter() {
                let time = hit
                    .start_seconds
                    .map(|seconds| format!(" · {seconds:.3}s"))
                    .unwrap_or_default();
                ui.files.append(&label(&format!(
                    "{}{time}\n{}",
                    std::path::Path::new(&hit.path)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy(),
                    hit.text
                )));
            }
        } else {
            let editing = ui.editing.borrow().clone();
            clear(&ui.markers);
            *ui.chapters.borrow_mut() = response.chapters;
            for chapter in ui.chapters.borrow().iter() {
                ui.markers.append(&label(&format!(
                    "{:.3}s · {}{}",
                    chapter.start_seconds,
                    chapter.title,
                    if chapter.stale { " · stale" } else { "" }
                )));
            }
            let index = editing.as_ref().and_then(|id| {
                ui.chapters
                    .borrow()
                    .iter()
                    .position(|chapter| &chapter.id == id)
            });
            if let Some(row) = index.and_then(|index| ui.markers.row_at_index(index as i32)) {
                ui.markers.select_row(Some(&row));
            } else {
                reset(ui);
            }
        }
    }
    ui.status.set_label(
        response
            .message
            .as_deref()
            .unwrap_or(if response.status == "ok" {
                "Ready."
            } else {
                "Request failed."
            }),
    );
    update(ui);
}
fn wire(ui: &Rc<Ui>) {
    for (button, action) in [
        (&ui.search, "search"),
        (&ui.refresh, "detail"),
        (&ui.undo, "undoChapterEdit"),
        (&ui.remove, "deleteChapter"),
        (&ui.save, "saveChapter"),
    ] {
        let weak = Rc::downgrade(ui);
        button.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                send(&ui, action);
            }
        });
    }
    let weak = Rc::downgrade(ui);
    ui.query.connect_activate(move |_| {
        if let Some(ui) = weak.upgrade() {
            send(&ui, "search");
        }
    });
    let weak = Rc::downgrade(ui);
    ui.new.connect_clicked(move |_| {
        if let Some(ui) = weak.upgrade() {
            ui.markers.unselect_all();
            reset(&ui);
        }
    });
    let weak = Rc::downgrade(ui);
    ui.markers.connect_row_selected(move |_, row| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        let chapter = row.and_then(|row| ui.chapters.borrow().get(row.index() as usize).cloned());
        if let Some(chapter) = chapter {
            *ui.editing.borrow_mut() = Some(chapter.id);
            ui.title.set_text(&chapter.title);
            ui.summary.buffer().set_text(&chapter.summary);
            ui.start.set_text(&chapter.start_seconds.to_string());
            ui.end.set_text(&chapter.end_seconds.to_string());
            ui.save.set_label("Save chapter");
        }
        update(&ui);
    });
    let weak = Rc::downgrade(ui);
    ui.files.connect_row_selected(move |_, row| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if ui.pending.borrow().is_some() {
            return;
        }
        let hit = row.and_then(|row| ui.hits.borrow().get(row.index() as usize).cloned());
        *ui.selected.borrow_mut() = hit.clone();
        clear(&ui.markers);
        reset(&ui);
        ui.preview_generation
            .set(ui.preview_generation.get().wrapping_add(1));
        let generation = ui.preview_generation.get();
        ui.picture.set_paintable(None::<&gtk::gdk::Paintable>);
        if let Some(hit) = hit {
            if hit.kind == "chapter" {
                *ui.editing.borrow_mut() = hit.evidence_id.clone();
            }
            ui.filename.set_label(&hit.path);
            let seconds = hit.start_seconds.unwrap_or(0.0);
            ui.start.set_text(&seconds.to_string());
            ui.end.set_text(&seconds.to_string());
            let image_extension = std::path::Path::new(&hit.path)
                .extension()
                .and_then(|extension| extension.to_str())
                .map(|extension| extension.to_ascii_lowercase());
            if hit.kind == "image"
                || matches!(
                    image_extension.as_deref(),
                    Some("png" | "jpg" | "jpeg" | "tif" | "tiff" | "webp" | "bmp" | "gif")
                )
            {
                let rx = ui
                    .engine
                    .borrow()
                    .request_scaled_thumbnail(hit.path.clone(), 1280);
                let weak = Rc::downgrade(&ui);
                glib::MainContext::default().spawn_local(async move {
                    if let Ok(Some(decoded)) = rx.recv().await {
                        if let Some(ui) = weak.upgrade() {
                            if ui.preview_generation.get() == generation {
                                ui.picture
                                    .set_paintable(Some(&texture_from_decoded(decoded)));
                            }
                        }
                    }
                });
            }
            send(&ui, "detail");
        }
    });
}
