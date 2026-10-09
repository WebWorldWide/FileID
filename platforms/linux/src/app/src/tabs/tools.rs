use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use adw::prelude::*;
use fileid_engine::ipc::{
    CatalogHit, CatalogRequest, CatalogRequestPayload, CatalogResponse, CommandPayload,
    ToolCapability, ToolRecipe, ToolRequest, ToolRequestPayload, ToolResponse,
};
use gtk::glib;

use crate::engine_client::{EngineClient, EngineEvent};

struct Ui {
    engine: Rc<RefCell<EngineClient>>,
    pending: RefCell<Option<(String, String)>>,
    destination: RefCell<Option<String>>,
    operation: RefCell<Option<String>>,
    executed: Cell<bool>,
    has_undo: Cell<bool>,
    hits: RefCell<Vec<CatalogHit>>,
    capabilities: RefCell<Vec<ToolCapability>>,
    query: gtk::SearchEntry,
    search: gtk::Button,
    refresh: gtk::Button,
    files: gtk::ListBox,
    kind: gtk::ComboBoxText,
    format: gtk::ComboBoxText,
    dimension: gtk::SpinButton,
    enlarge: gtk::CheckButton,
    detail: gtk::Label,
    choose: gtk::Button,
    folder: gtk::Label,
    preview: gtk::Button,
    export: gtk::Button,
    undo: gtk::Button,
    history: gtk::Button,
    outputs: gtk::ListBox,
    status: gtk::Label,
}

pub fn present(engine: &Rc<RefCell<EngineClient>>, parent: &gtk::Button) {
    let query = gtk::SearchEntry::builder()
        .placeholder_text("Find files by name or description")
        .hexpand(true)
        .build();
    let search = gtk::Button::with_label("Find");
    let refresh = gtk::Button::with_label("Refresh tools");
    let files = gtk::ListBox::new();
    files.set_selection_mode(gtk::SelectionMode::Multiple);
    let kind = gtk::ComboBoxText::new();
    let format = gtk::ComboBoxText::new();
    let dimension = gtk::SpinButton::with_range(1.0, 8192.0, 1.0);
    dimension.set_value(4096.0);
    dimension.set_tooltip_text(Some("Maximum pixels on the longest side"));
    let enlarge = gtk::CheckButton::with_label("Enlarge smaller photos to the maximum size");
    let detail = label("");
    let choose = gtk::Button::with_label("Choose output folder");
    let folder = label("No output folder selected");
    folder.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    let preview = gtk::Button::with_label("Preview export");
    let export = gtk::Button::with_label("Export new versions");
    let undo = gtk::Button::with_label("Undo export");
    let history = gtk::Button::with_label("Last export");
    let outputs = gtk::ListBox::new();
    outputs.set_selection_mode(gtk::SelectionMode::None);
    let status = label("Exports create new versions and preserve originals. Conventional enlargement does not recover missing detail.");
    let ui = Rc::new(Ui {
        engine: engine.clone(),
        pending: RefCell::new(None),
        destination: RefCell::new(None),
        operation: RefCell::new(None),
        executed: Cell::new(false),
        has_undo: Cell::new(false),
        hits: RefCell::new(vec![]),
        capabilities: RefCell::new(vec![]),
        query,
        search,
        refresh,
        files,
        kind,
        format,
        dimension,
        enlarge,
        detail,
        choose,
        folder,
        preview,
        export,
        undo,
        history,
        outputs,
        status,
    });
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_start(20);
    body.set_margin_end(20);
    body.set_margin_top(20);
    body.set_margin_bottom(20);
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let title = label("File Tools");
    title.add_css_class("title-2");
    title.set_hexpand(true);
    let done = gtk::Button::with_label("Done");
    header.append(&title);
    header.append(&done);
    body.append(&header);
    let find = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    find.append(&ui.query);
    find.append(&ui.search);
    find.append(&ui.refresh);
    body.append(&find);
    body.append(
        &gtk::ScrolledWindow::builder()
            .min_content_height(170)
            .vexpand(true)
            .child(&ui.files)
            .build(),
    );
    let recipe = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    recipe.append(&ui.kind);
    recipe.append(&ui.format);
    recipe.append(&ui.dimension);
    body.append(&recipe);
    body.append(&ui.enlarge);
    body.append(&ui.detail);
    let folder_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    folder_row.append(&ui.choose);
    folder_row.append(&ui.folder);
    body.append(&folder_row);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    for button in [&ui.history, &ui.preview, &ui.export, &ui.undo] {
        actions.append(button);
    }
    body.append(&actions);
    body.append(
        &gtk::ScrolledWindow::builder()
            .min_content_height(140)
            .vexpand(true)
            .child(&ui.outputs)
            .build(),
    );
    body.append(&ui.status);
    let dialog = adw::Dialog::builder()
        .title("File Tools")
        .content_width(800)
        .content_height(660)
        .child(&body)
        .build();
    let weak_dialog = dialog.downgrade();
    done.connect_clicked(move |_| {
        if let Some(dialog) = weak_dialog.upgrade() {
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
                EngineEvent::CatalogResponse(response) => receive_search(&ui, response),
                EngineEvent::ToolResponse(response) => receive_tools(&ui, response),
                EngineEvent::Spawning | EngineEvent::Exited | EngineEvent::Failed(_) => {
                    ui.pending.borrow_mut().take(); invalidate(&ui);
                    ui.status.set_label("The engine stopped or restarted. Refresh tools; check Last export for interrupted work.");
                }
                _ => {}
            }
        }
    });
    dialog.connect_closed(move |_| {
        task.abort();
    });
    let keep_alive = ui.clone();
    dialog.connect_closed(move |_| {
        keep_alive.pending.borrow_mut().take();
    });
    update(&ui);
    dialog.present(Some(parent));
    send(&ui, "capabilities");
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

fn recipe(ui: &Ui) -> Option<ToolRecipe> {
    let kind = ui.kind.active_id()?.to_string();
    let format = ui.format.active_text()?.to_string();
    let value = ui.dimension.value();
    if !value.is_finite() || value.fract() != 0.0 || !(1.0..=8192.0).contains(&value) {
        return None;
    }
    Some(ToolRecipe {
        allow_upscale: (kind == "photo").then_some(ui.enlarge.is_active()),
        video_aspect_ratio: None,
        kind,
        format,
        max_dimension: value as u32,
    })
}

fn update(ui: &Ui) {
    let ready = ui.pending.borrow().is_none();
    for widget in [
        ui.query.clone().upcast::<gtk::Widget>(),
        ui.search.clone().upcast(),
        ui.refresh.clone().upcast(),
        ui.files.clone().upcast(),
        ui.kind.clone().upcast(),
        ui.format.clone().upcast(),
        ui.choose.clone().upcast(),
        ui.history.clone().upcast(),
    ] {
        widget.set_sensitive(ready);
    }
    ui.dimension.set_sensitive(ready);
    ui.enlarge.set_sensitive(ready);
    ui.preview.set_sensitive(
        ready
            && !ui.files.selected_rows().is_empty()
            && ui.destination.borrow().is_some()
            && recipe(ui).is_some(),
    );
    ui.export
        .set_sensitive(ready && ui.operation.borrow().is_some() && !ui.executed.get());
    ui.undo
        .set_sensitive(ready && ui.operation.borrow().is_some() && ui.has_undo.get());
}

fn invalidate(ui: &Ui) {
    if ui.pending.borrow().is_some() {
        return;
    }
    ui.operation.borrow_mut().take();
    ui.executed.set(false);
    ui.has_undo.set(false);
    clear(&ui.outputs);
    update(ui);
}

fn send(ui: &Ui, action: &str) {
    if ui.pending.borrow().is_some() {
        return;
    }
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let request_id = format!(
        "gtk-tools-{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let command = if action == "search" {
        let query = ui.query.text().trim().to_owned();
        if query.is_empty() {
            return;
        }
        CommandPayload::CatalogRequest(Box::new(CatalogRequestPayload {
            request: CatalogRequest {
                request_id: request_id.clone(),
                action: action.into(),
                query: Some(query),
                file_id: None,
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
            },
        }))
    } else {
        let ids = (action == "preview").then(|| {
            ui.files
                .selected_rows()
                .iter()
                .filter_map(|row| {
                    ui.hits
                        .borrow()
                        .get(row.index() as usize)
                        .map(|hit| hit.file_id)
                })
                .collect()
        });
        CommandPayload::ToolRequest(ToolRequestPayload {
            request: ToolRequest {
                request_id: request_id.clone(),
                action: action.into(),
                file_ids: ids,
                destination: ui.destination.borrow().clone(),
                recipe: if action == "preview" {
                    recipe(ui)
                } else {
                    None
                },
                operation_id: if matches!(action, "execute" | "undo") {
                    ui.operation.borrow().clone()
                } else {
                    None
                },
                destination_bookmark: None,
            },
        })
    };
    *ui.pending.borrow_mut() = Some((request_id, action.into()));
    update(ui);
    ui.status.set_label("Working…");
    if ui.engine.borrow_mut().send(command).is_err() {
        ui.pending.borrow_mut().take();
        update(ui);
        ui.status
            .set_label("The engine could not receive this request. Refresh tools before retrying.");
    }
}

fn accept(ui: &Ui, id: &str) -> Option<String> {
    let mut pending = ui.pending.borrow_mut();
    if pending.as_ref().is_some_and(|(request, _)| request == id) {
        pending.take().map(|(_, action)| action)
    } else {
        None
    }
}

fn receive_search(ui: &Ui, response: CatalogResponse) {
    if accept(ui, &response.request_id).as_deref() != Some("search") {
        return;
    }
    clear(&ui.files);
    let mut seen = HashSet::new();
    *ui.hits.borrow_mut() = if response.status == "ok" {
        response
            .hits
            .into_iter()
            .filter(|hit| seen.insert(hit.file_id))
            .collect()
    } else {
        vec![]
    };
    for hit in ui.hits.borrow().iter() {
        ui.files.append(&label(&hit.path));
    }
    ui.status.set_label(
        response
            .message
            .as_deref()
            .unwrap_or("Select files for export."),
    );
    invalidate(ui);
}

fn receive_tools(ui: &Ui, response: ToolResponse) {
    let Some(action) = accept(ui, &response.request_id) else {
        return;
    };
    ui.status.set_label(if response.message.is_empty() {
        "Choose files and an output folder, then preview the export."
    } else {
        &response.message
    });
    if action == "capabilities" && response.status == "ok" {
        *ui.capabilities.borrow_mut() = response
            .capabilities
            .into_iter()
            .filter(|capability| capability.available)
            .collect();
        ui.kind.remove_all();
        for capability in ui.capabilities.borrow().iter() {
            let name = match capability.id.as_str() {
                "photo" => "Photo conversion",
                "chapters" => "Chapter export",
                "video" => "Video conversion",
                _ => &capability.id,
            };
            ui.kind.append(Some(&capability.id), name);
        }
        ui.kind
            .set_active((!ui.capabilities.borrow().is_empty()).then_some(0));
    } else {
        if action == "preview" && response.status == "ok" {
            *ui.operation.borrow_mut() = response.operation_id;
            ui.executed.set(false);
        } else if action == "history" && response.status == "ok" {
            *ui.operation.borrow_mut() = response.operation_id;
            ui.executed.set(ui.operation.borrow().is_some());
        } else if action == "execute" {
            ui.executed.set(true);
        } else if action == "undo" && response.status == "ok" {
            ui.operation.borrow_mut().take();
            ui.executed.set(false);
        }
        ui.has_undo.set(
            response
                .outputs
                .iter()
                .any(|output| output.state == "completed"),
        );
        clear(&ui.outputs);
        for output in response.outputs {
            ui.outputs.append(&label(&format!(
                "{}\n{} · {}",
                output.output_path, output.state, output.message
            )));
        }
    }
    update(ui);
}

fn wire(ui: &Rc<Ui>) {
    for (button, action) in [
        (&ui.search, "search"),
        (&ui.refresh, "capabilities"),
        (&ui.history, "history"),
        (&ui.preview, "preview"),
        (&ui.export, "execute"),
        (&ui.undo, "undo"),
    ] {
        let weak = Rc::downgrade(ui);
        button.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                send(&ui, action);
            }
        });
    }
    let weak = Rc::downgrade(ui);
    ui.files.connect_selected_rows_changed(move |_| {
        if let Some(ui) = weak.upgrade() {
            invalidate(&ui);
        }
    });
    let weak = Rc::downgrade(ui);
    ui.format.connect_changed(move |_| {
        if let Some(ui) = weak.upgrade() {
            invalidate(&ui);
        }
    });
    let weak = Rc::downgrade(ui);
    ui.dimension.connect_value_changed(move |_| {
        if let Some(ui) = weak.upgrade() {
            invalidate(&ui);
        }
    });
    let weak = Rc::downgrade(ui);
    ui.enlarge.connect_toggled(move |_| {
        if let Some(ui) = weak.upgrade() {
            invalidate(&ui);
        }
    });
    let weak = Rc::downgrade(ui);
    ui.kind.connect_changed(move |_| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        ui.format.remove_all();
        if let Some(id) = ui.kind.active_id() {
            let capability = ui
                .capabilities
                .borrow()
                .iter()
                .find(|capability| capability.id == id.as_str())
                .cloned();
            if let Some(capability) = capability {
                for format in capability.output_formats {
                    ui.format.append_text(&format);
                }
                ui.format.set_active(Some(0));
                ui.detail.set_label(&capability.detail);
                ui.dimension.set_visible(capability.id == "photo");
                ui.enlarge.set_visible(capability.id == "photo");
            }
        }
        invalidate(&ui);
    });
    let weak = Rc::downgrade(ui);
    ui.choose.connect_clicked(move |button| {
        let parent = button.root().and_downcast::<gtk::Window>();
        let weak = weak.clone();
        gtk::FileDialog::builder()
            .title("Choose an output folder")
            .modal(true)
            .build()
            .select_folder(
                parent.as_ref(),
                gtk::gio::Cancellable::NONE,
                move |result| {
                    let Some(ui) = weak.upgrade() else {
                        return;
                    };
                    if let Ok(folder) = result {
                        if let Some(path) = folder.path() {
                            let path = path.to_string_lossy().into_owned();
                            ui.folder.set_label(&path);
                            *ui.destination.borrow_mut() = Some(path);
                            invalidate(&ui);
                        } else {
                            ui.status
                                .set_label("The selected folder has no usable filesystem path.");
                        }
                    }
                },
            );
    });
}
