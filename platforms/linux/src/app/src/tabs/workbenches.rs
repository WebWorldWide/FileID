use super::workbench_state::*;
use crate::engine_client::{EngineClient, EngineState};
use adw::prelude::*;
use fileid_engine::ipc::*;
use std::{cell::RefCell, rc::Rc, time::Duration};

type Engine = Rc<RefCell<EngineClient>>;
fn column() -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, 8)
}
fn row() -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Horizontal, 8)
}
fn label(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .selectable(true)
        .build()
}
fn entry(hint: &str) -> gtk::Entry {
    gtk::Entry::builder()
        .placeholder_text(hint)
        .hexpand(true)
        .build()
}
fn button(parent: &gtk::Box, text: &str, action: impl Fn() + 'static) -> gtk::Button {
    let b = gtk::Button::with_label(text);
    b.connect_clicked(move |_| action());
    parent.append(&b);
    b
}
fn clear(b: &gtk::Box) {
    while let Some(c) = b.first_child() {
        b.remove(&c);
    }
}
fn page(title: &str, body: &gtk::Box) -> gtk::Widget {
    body.set_margin_top(16);
    body.set_margin_bottom(16);
    body.set_margin_start(16);
    body.set_margin_end(16);
    body.prepend(
        &gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .css_classes(["title-1"])
            .build(),
    );
    gtk::ScrolledWindow::builder()
        .child(body)
        .hexpand(true)
        .vexpand(true)
        .build()
        .upcast()
}
fn evidence(hit: &CatalogHit) -> String {
    format!(
        "#{} · {} · {}{}{}\n{}",
        hit.file_id,
        hit.path,
        if hit.kind == "sampledFrame" {
            "sampled frame, unverified"
        } else {
            &hit.kind
        },
        hit.start_seconds
            .map(|s| format!(" · {s:.1}s"))
            .unwrap_or_default(),
        hit.page.map(|p| format!(" · page {p}")).unwrap_or_default(),
        hit.text
    )
}
fn open(hit: &CatalogHit, status: &gtk::Label) {
    let path = std::path::Path::new(&hit.path);
    if !path.is_absolute() || !path.is_file() {
        status.set_label("File is unavailable offline or has moved.");
        return;
    }
    let file = gio::File::for_path(&hit.path);
    let info = match file.query_info(
        "standard::content-type,access::can-execute",
        gio::FileQueryInfoFlags::NONE,
        gio::Cancellable::NONE,
    ) {
        Ok(info) => info,
        Err(_) => {
            status.set_label("File identity/type could not be checked; opening refused.");
            return;
        }
    };
    let content_type = info.content_type().unwrap_or_default();
    if info.boolean("access::can-execute")
        || gio::content_type_can_be_executable(&content_type)
        || content_type.contains("executable")
        || content_type.contains("desktop")
        || content_type.contains("shellscript")
    {
        status.set_label("Opening executable/script evidence is not allowed. Reveal it using your file manager instead.");
        return;
    }
    let uri = file.uri();
    match gio::AppInfo::launch_default_for_uri(&uri, gio::AppLaunchContext::NONE) {
        Ok(()) => status.set_label(if hit.start_seconds.is_some() {
            "Opened file. Timestamp seeking/playback is unavailable in this Linux workbench."
        } else {
            "Opened in the local default application."
        }),
        Err(e) => status.set_label(&format!("Could not open file: {e}")),
    }
}

struct CatalogUi {
    engine: Engine,
    requests: RefCell<Requests>,
    selected: RefCell<Option<CatalogHit>>,
    editing: RefCell<Option<String>>,
    mutation_file_id: RefCell<Option<i64>>,
    hybrid: RefCell<HybridSearch>,
    search_notice: gtk::Label,
    status: gtk::Label,
    hits: gtk::Box,
    chapters: gtk::Box,
    detail_hits: gtk::Box,
    jobs: gtk::Box,
    title: gtk::Entry,
    summary: gtk::Entry,
    start: gtk::SpinButton,
    end: gtk::SpinButton,
    root: gtk::Box,
}
impl CatalogUi {
    fn send(&self, slot: &str, request: CatalogRequest) {
        if request.action == "enqueueTimeline" {
            self.status.set_label(&crate::native_runtime::require_vlm().unwrap_err());
            return;
        }
        if slot == "mutation" {
            *self.mutation_file_id.borrow_mut() = request
                .file_id
                .or_else(|| request.chapter.as_ref().map(|c| c.file_id));
        }
        self.requests.borrow_mut().begin(slot, &request.request_id);
        if let Err(e) = self
            .engine
            .borrow_mut()
            .send(CommandPayload::CatalogRequest(Box::new(
                CatalogRequestPayload { request },
            )))
        {
            self.requests.borrow_mut().forget(slot);
            self.status.set_label(&e.to_string());
        }
    }
    fn detail(&self) {
        if let Some(hit) = self.selected.borrow().as_ref() {
            let mut r = catalog_request("detail");
            r.file_id = Some(hit.file_id);
            self.send("detail", r);
        }
    }
    fn is_selected(&self, file_id: i64) -> bool {
        self.selected
            .borrow()
            .as_ref()
            .is_some_and(|h| h.file_id == file_id)
    }
    fn select(&self, hit: CatalogHit) {
        *self.selected.borrow_mut() = Some(hit.clone());
        self.requests.borrow_mut().forget("detail");
        clear(&self.chapters);
        clear(&self.detail_hits);
        self.reset_editor();
        self.start.set_value(hit.start_seconds.unwrap_or(0.0));
        self.end.set_value(hit.start_seconds.unwrap_or(0.0));
        self.detail();
    }
    fn render_chapters(self: &Rc<Self>, chapters: Vec<CatalogChapter>) {
        for chapter in chapters {
            if !self.is_selected(chapter.file_id) {
                continue;
            }
            let cr = row();
            cr.append(&label(&format!(
                "{:.1}–{:.1}s · {}{}{}\n{}",
                chapter.start_seconds,
                chapter.end_seconds,
                chapter.title,
                if chapter.stale { " · STALE" } else { "" },
                if chapter
                    .model_version
                    .starts_with("timeline-chapter-suggestion-v1/")
                    && !chapter.user_edited
                {
                    " · Draft: sparse samples can miss events"
                } else {
                    ""
                },
                chapter.summary
            )));
            let u = self.clone();
            let c = chapter.clone();
            button(&cr, "Edit", move || {
                if !u.is_selected(c.file_id) {
                    return;
                }
                *u.editing.borrow_mut() = Some(c.id.clone());
                u.title.set_text(&c.title);
                u.summary.set_text(&c.summary);
                u.start.set_value(c.start_seconds);
                u.end.set_value(c.end_seconds);
            });
            let u = self.clone();
            button(&cr, "Delete", move || {
                if !u.is_selected(chapter.file_id) || u.requests.borrow().busy("mutation") {
                    return;
                }
                let mut r = catalog_request("deleteChapter");
                r.file_id = Some(chapter.file_id);
                r.chapter_id = Some(chapter.id.clone());
                u.send("mutation", r);
            });
            self.chapters.append(&cr);
        }
    }
    fn reset_editor(&self) {
        *self.editing.borrow_mut() = None;
        self.title.set_text("");
        self.summary.set_text("");
    }
}

pub fn catalog(engine: Engine) -> gtk::Widget {
    build_catalog(engine).0
}
fn build_catalog(engine: Engine) -> (gtk::Widget, Rc<CatalogUi>) {
    let body = column();
    let actions = row();
    let query = entry("Search names, text, chapters and sampled frames");
    actions.append(&query);
    body.append(&actions);
    let mode = gtk::DropDown::from_strings(&["Keyword", "Hybrid (real CLIP query)"]);
    actions.append(&mode);
    let search_notice = label("Keyword search selected. Hybrid uses existing local CLIP weights only; no download starts here.");
    body.append(&search_notice);
    let status = label("Sparse visual samples can miss fast events. Speech depends on installed Whisper and configured FFmpeg. Timestamp seeking is unavailable; job completion is not proof of exhaustive coverage.");
    body.append(&status);
    let hits = column();
    body.append(&hits);
    let chapters = column();
    body.append(&label("Chapter markers (original files are unchanged)"));
    body.append(&chapters);
    let detail_hits = column();
    body.append(&label("Timeline evidence · sampled frames are unverified; missing evidence never implies complete coverage"));
    body.append(&detail_hits);
    let title = entry("Chapter title");
    let summary = entry("Summary");
    let times = row();
    let start = gtk::SpinButton::with_range(0.0, 864000.0, 0.1);
    start.set_digits(1);
    let end = gtk::SpinButton::with_range(0.0, 864000.0, 0.1);
    end.set_digits(1);
    times.append(&label("Start seconds"));
    times.append(&start);
    times.append(&label("End seconds"));
    times.append(&end);
    body.append(&title);
    body.append(&summary);
    body.append(&times);
    let edits = row();
    body.append(&edits);
    let jobs = column();
    body.append(&label("Durable catalog jobs"));
    body.append(&jobs);
    let ui = Rc::new(CatalogUi {
        engine: engine.clone(),
        requests: RefCell::new(Requests::default()),
        selected: RefCell::new(None),
        editing: RefCell::new(None),
        mutation_file_id: RefCell::new(None),
        hybrid: RefCell::new(HybridSearch::default()),
        search_notice,
        status,
        hits,
        chapters,
        detail_hits,
        jobs,
        title,
        summary,
        start,
        end,
        root: body.clone(),
    });
    {
        let ui = ui.clone();
        let query = query.clone();
        button(&actions, "Search", move || {
            if query.text().trim().is_empty() {
                return;
            }
            *ui.selected.borrow_mut() = None;
            ui.requests.borrow_mut().forget("detail");
            ui.reset_editor();
            clear(&ui.chapters);
            clear(&ui.detail_hits);
            clear(&ui.hits);
            ui.requests.borrow_mut().forget("search");
            ui.hybrid.borrow_mut().cancel();
            if mode.selected() == 0 {
                ui.search_notice.set_label("Keyword search selected.");
                let mut r = catalog_request("search");
                r.query = Some(query.text().into());
                ui.send("search", r);
            } else {
                let request = ui.hybrid.borrow_mut().begin(query.text().trim());
                let id = request.query_id.clone();
                ui.search_notice
                    .set_label("Waiting for a real local CLIP query embedding…");
                let sent = ui
                    .engine
                    .borrow_mut()
                    .send(CommandPayload::EmbedTextQuery(request));
                if let Err(e) = sent {
                    let fallback = ui.hybrid.borrow_mut().timeout(&id);
                    if let Some(r) = fallback {
                        ui.search_notice
                            .set_label(&format!("Hybrid unavailable: {e}. Using keyword search."));
                        ui.send("search", r);
                    }
                } else {
                    let ui = ui.clone();
                    glib::timeout_add_local_once(Duration::from_secs(15), move || {
                        let fallback = ui.hybrid.borrow_mut().timeout(&id);
                        if let Some(r) = fallback {
                            ui.search_notice.set_label("Hybrid embedding timed out. Using keyword search; no inference result is fabricated.");
                            ui.send("search", r);
                        }
                    });
                }
            }
        });
    }
    {
        let ui = ui.clone();
        button(&actions, "Refresh jobs", move || {
            ui.send("jobs", catalog_request("jobs"))
        });
    }
    {
        let ui = ui.clone();
        button(&edits, "Add / save chapter", move || {
            if ui.requests.borrow().busy("mutation") {
                return;
            }
            let Some(hit) = ui.selected.borrow().clone() else {
                ui.status.set_label("Select a search result first.");
                return;
            };
            if ui.title.text().trim().is_empty() || ui.end.value() < ui.start.value() {
                ui.status
                    .set_label("Enter a title and an end time at or after start.");
                return;
            }
            let mut r = catalog_request("saveChapter");
            r.chapter = Some(CatalogChapter {
                id: ui.editing.borrow().clone().unwrap_or_else(request_id),
                file_id: hit.file_id,
                start_seconds: ui.start.value(),
                end_seconds: ui.end.value(),
                title: ui.title.text().into(),
                summary: ui.summary.text().into(),
                source_revision: String::new(),
                model_version: "user".into(),
                confidence: 1.0,
                user_edited: true,
                stale: false,
            });
            ui.send("mutation", r);
        });
    }
    {
        let ui = ui.clone();
        button(&edits, "New chapter", move || ui.reset_editor());
    }
    {
        let ui = ui.clone();
        button(&edits, "Undo chapter edit", move || {
            if ui.requests.borrow().busy("mutation") {
                return;
            }
            if let Some(hit) = ui.selected.borrow().as_ref() {
                let mut r = catalog_request("undoChapterEdit");
                r.file_id = Some(hit.file_id);
                ui.send("mutation", r);
            }
        });
    }
    let timeline_ids =
        entry("Video file IDs for timeline, comma separated; blank uses selected file");
    timeline_ids.set_sensitive(false);
    body.append(&timeline_ids);
    body.append(&label("Timeline generation is unavailable with this backend; stored evidence remains reviewable."));
    {
        let ui = ui.clone();
        let analyze = button(&edits, "Analyze video timeline · unavailable", move || {
            if let Err(reason) = crate::native_runtime::require_vlm() {
                ui.status.set_label(&reason);
                return;
            }
            if !ui.engine.borrow().is_ready() || ui.requests.borrow().busy("enqueue") {
                return;
            }
            let ids = if timeline_ids.text().trim().is_empty() {
                ui.selected
                    .borrow()
                    .as_ref()
                    .filter(|h| {
                        h.kind == "video" || h.kind == "sampledFrame" || h.start_seconds.is_some()
                    })
                    .map(|h| vec![h.file_id])
                    .ok_or("Choose an indexed video or enter video IDs.")
            } else {
                parse_file_ids(timeline_ids.text().as_str(), 1000)
            };
            let ids = match ids {
                Ok(ids) => ids,
                Err(e) => {
                    ui.status.set_label(e);
                    return;
                }
            };
            if let Err(e) = crate::native_runtime::require_vlm()
                .and_then(|()| crate::native_runtime::configured("FILEID_FFMPEG"))
                .and_then(|()| crate::native_runtime::configured("FILEID_FFPROBE"))
            {
                ui.status.set_label(&e);
                return;
            }
            let mut request = catalog_request("enqueueTimeline");
            request.file_ids = Some(ids);
            ui.status.set_label("Requesting local timeline sampling. No coverage or model execution is confirmed yet.");
            ui.send("enqueue", request);
        });
        analyze.set_sensitive(false);
    }
    let rx = engine.borrow_mut().subscribe();
    {
        let ui = ui.clone();
        glib::MainContext::default().spawn_local(async move {
            while let Ok(event) = rx.recv().await {
                match event {
                    EngineState::CatalogResponse(r) => {
                        let slot = ui.requests.borrow_mut().accept(&r.request_id, true);
                        let Some(slot) = slot else {
                            continue;
                        };
                        if slot != "jobs" || r.status != "ok" {
                            ui.status
                                .set_label(r.message.as_deref().unwrap_or(&r.status));
                        }
                        if r.status != "ok" {
                            continue;
                        }
                        match slot.as_str() {
                            "search" => {
                                clear(&ui.hits);
                                if r.hits.is_empty() {
                                    ui.hits.append(&label("No matches."));
                                }
                                for hit in r.hits {
                                    let hrow = row();
                                    hrow.append(&label(&evidence(&hit)));
                                    let u = ui.clone();
                                    let h = hit.clone();
                                    button(&hrow, "Chapters", move || {
                                        u.select(h.clone());
                                    });
                                    let status = ui.status.clone();
                                    button(&hrow, "Open file", move || open(&hit, &status));
                                    ui.hits.append(&hrow);
                                }
                            }
                            "detail" => {
                                clear(&ui.chapters);
                                clear(&ui.detail_hits);
                                ui.render_chapters(r.chapters);
                                for hit in r.hits {
                                    if !ui.is_selected(hit.file_id) { continue; }
                                    let hr = row();
                                    hr.append(&label(&evidence(&hit)));
                                    let status = ui.status.clone();
                                    button(&hr, "Open evidence", move || open(&hit, &status));
                                    ui.detail_hits.append(&hr);
                                }
                                if ui.detail_hits.first_child().is_none() { ui.detail_hits.append(&label("No timeline evidence returned. Coverage is unknown, not complete.")); }
                            }
                            "mutation" => {
                                let owner = ui.mutation_file_id.borrow_mut().take();
                                if ui.selected.borrow().as_ref().map(|h| h.file_id) == owner {
                                    ui.reset_editor();
                                    ui.detail();
                                }
                            }
                            "jobs" => {
                                clear(&ui.jobs);
                                for job in r.jobs {
                                    let jr = column();
                                    jr.append(&label(&format!(
                                        "{} · {} · {:.0}%{}",
                                        job.kind,
                                        job.state,
                                        job.progress * 100.0,
                                        job.error
                                            .as_ref()
                                            .map(|e| format!(" · {e}"))
                                            .unwrap_or_default()
                                    )));
                                    let controls = row();
                                    for (action, name) in job_controls(&job) {
                                        let u = ui.clone();
                                        let id = job.id.clone();
                                        let controlled_job = job.clone();
                                        button(&controls, name, move || {
                                            if !job_controls(&controlled_job).iter().any(|(allowed, _)| *allowed == action) {
                                                u.status.set_label("Timeline job controls are unavailable with this backend.");
                                                return;
                                            }
                                            if !u.engine.borrow().is_ready() || u.requests.borrow().busy("job-control") { return; }
                                            let mut r = catalog_request(action);
                                            r.job_id = Some(id.clone());
                                            u.send("job-control", r);
                                        });
                                    }
                                    jr.append(&controls);
                                    ui.jobs.append(&jr);
                                }
                            }
                            "job-control" | "enqueue" => { ui.send("jobs", catalog_request("jobs")); ui.detail(); },
                            _ => {}
                        }
                    }
                    EngineState::ClipTextEmbedding(response) => {
                        let result = ui.hybrid.borrow_mut().finish(&response);
                        if let Some((request,reason)) = result {
                            ui.search_notice.set_label(reason.as_deref().unwrap_or("Hybrid search uses a validated local CLIP embedding and the pinned CLIP model identity."));
                            ui.send("search",request);
                        }
                    }
                    EngineState::Ready => ui.send("jobs", catalog_request("jobs")),
                    EngineState::Failed(e) => {
                        ui.requests.borrow_mut().clear();
                        ui.status.set_label(&e);
                    }
                    EngineState::Exited => {
                        ui.requests.borrow_mut().clear();
                        ui.status
                            .set_label("Engine exited; pending requests interrupted.");
                    }
                    _ => {}
                }
            }
        });
    }
    let weak = Rc::downgrade(&ui);
    glib::timeout_add_local(Duration::from_secs(2), move || {
        let Some(ui) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if ui.root.is_mapped()
            && ui.engine.borrow().is_ready()
            && !ui.requests.borrow().busy("jobs")
        {
            ui.send("jobs", catalog_request("jobs"));
        }
        glib::ControlFlow::Continue
    });
    (page("Search & Moments", &body), ui)
}

fn parse_file_ids(raw: &str, max: usize) -> Result<Vec<i64>, &'static str> {
    let ids = raw
        .split(',')
        .map(|s| s.trim().parse::<i64>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "Enter comma-separated positive indexed video IDs.")?;
    if ids.is_empty() || ids.len() > max || ids.iter().any(|i| *i <= 0) {
        return Err("Invalid video selection size or file ID.");
    }
    let mut ids = ids;
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

fn job_controls(job: &CatalogJob) -> Vec<(&'static str, &'static str)> {
    let mut controls = vec![];
    if job.kind == "timelineSample" {
        return controls;
    }
    if matches!(job.state.as_str(), "running" | "queued") {
        controls.push(("pauseJob", "Pause"));
    }
    if job.kind == "catalogIndex"
        && matches!(job.state.as_str(), "paused" | "failed" | "cancelled")
    {
        controls.push(("resumeJob", "Resume / retry"));
    }
    if matches!(job.state.as_str(), "running" | "queued" | "paused") {
        controls.push(("cancelJob", "Cancel"));
    }
    controls
}

struct ToolsUi {
    engine: Engine,
    state: RefCell<ExportState>,
    status: gtk::Label,
    outputs: gtk::Box,
    capabilities: gtk::Box,
    ids: gtk::Entry,
    destination: gtk::Entry,
    kind: gtk::DropDown,
    format: gtk::DropDown,
    dimension: gtk::SpinButton,
    execute: gtk::Button,
    undo: gtk::Button,
    preview: gtk::Button,
    cancel: gtk::Button,
}
impl ToolsUi {
    fn recipe(&self) -> ToolRecipe {
        ToolRecipe {
            kind: ["photo", "chapters", "video"][self.kind.selected() as usize].into(),
            format: ["png", "jpeg", "tiff", "json", "vtt", "mp4"][self.format.selected() as usize]
                .into(),
            max_dimension: self.dimension.value() as u32,
        }
    }
    fn update(&self) {
        let s = self.state.borrow();
        let ready = self.engine.borrow().is_ready();
        self.execute
            .set_sensitive(ready && s.can_execute() && s.supports(&self.recipe()));
        self.undo.set_sensitive(
            ready
                && LINUX_TOOL_UNDO_SAFETY_ACCEPTED
                && s.history.is_some()
                && !s.requests.busy("operation"),
        );
        self.preview
            .set_sensitive(ready && s.supports(&self.recipe()) && !s.requests.busy("operation"));
        self.cancel.set_sensitive(ready && s.can_cancel());
    }
    fn send(&self, action: &str, operation: Option<String>) {
        if !self.engine.borrow().is_ready() {
            self.status
                .set_label("Engine is not ready; restart FileID before exporting.");
            return;
        }
        if action == "undo" && !LINUX_TOOL_UNDO_SAFETY_ACCEPTED {
            self.status.set_label(TOOL_UNDO_UNAVAILABLE_REASON);
            return;
        }
        if action == "cancel" {
            if !self.state.borrow().can_cancel() {
                return;
            }
        } else if action != "capabilities" && self.state.borrow().requests.busy("operation") {
            return;
        }
        if action == "preview" && !self.state.borrow().supports(&self.recipe()) {
            self.status
                .set_label("This recipe/format is unavailable; check engine capabilities.");
            return;
        }
        let mut r = ToolRequest {
            request_id: request_id(),
            action: action.into(),
            file_ids: None,
            destination: None,
            recipe: None,
            operation_id: operation,
            destination_bookmark: None,
        };
        if action == "preview" {
            let ids = self
                .ids
                .text()
                .split(',')
                .map(|s| s.trim().parse::<i64>())
                .collect::<Result<Vec<_>, _>>();
            let Ok(ids) = ids else {
                self.status
                    .set_label("Choose comma-separated catalog file IDs.");
                return;
            };
            r.file_ids = Some(ids);
            r.destination = Some(self.destination.text().into());
            r.recipe = Some(self.recipe());
            let mut s = self.state.borrow_mut();
            s.preview = None;
            s.preview_revision = s.revision;
        }
        if matches!(action, "execute" | "undo" | "preview")
            && self.state.borrow().requests.busy("operation")
        {
            return;
        }
        {
            let mut state = self.state.borrow_mut();
            let slot = if matches!(action, "capabilities" | "history") {
                action
            } else if action == "cancel" {
                state.cancelling = true;
                "cancel"
            } else {
                state.requests.forget("history");
                state.operation_action = Some(action.to_owned());
                state.active_operation = r.operation_id.clone();
                state.cancelling = false;
                "operation"
            };
            state.requests.begin(slot, &r.request_id);
        }
        if let Err(e) =
            self.engine
                .borrow_mut()
                .send(CommandPayload::ToolRequest(ToolRequestPayload {
                    request: r,
                }))
        {
            if action == "cancel" {
                let mut state = self.state.borrow_mut();
                state.requests.forget("cancel");
                state.cancelling = false;
            } else {
                self.state.borrow_mut().requests.clear();
            }
            self.status.set_label(&e.to_string());
        } else {
            self.status
                .set_label(&format!("Waiting for engine: {action}…"));
        }
        self.update();
    }
}

fn choose_indexed_files(ui: &Rc<ToolsUi>) {
    let dialog = adw::Dialog::builder()
        .title("Choose indexed files")
        .content_width(700)
        .content_height(500)
        .build();
    let body = column();
    body.append(&label("Latest 1000 indexed files; select up to 100. Search & Moments exposes IDs for files beyond this bounded list."));
    let list = column();
    let selected = Rc::new(RefCell::new(Vec::<i64>::new()));
    let scroller = gtk::ScrolledWindow::builder()
        .child(&list)
        .vexpand(true)
        .build();
    body.append(&scroller);
    let actions = row();
    body.append(&actions);
    let u = ui.clone();
    let selection = selected.clone();
    let d = dialog.clone();
    button(&actions, "Use selected files", move || {
        let ids = selection.borrow();
        if ids.is_empty() || ids.len() > 100 {
            u.status.set_label("Choose between 1 and 100 files.");
            return;
        }
        u.ids
            .set_text(&ids.iter().map(i64::to_string).collect::<Vec<_>>().join(","));
        d.close();
    });
    dialog.set_child(Some(&body));
    dialog.present(Some(&ui.ids));
    let rx = ui
        .engine
        .borrow()
        .query_files(crate::engine_client::QuerySpec {
            search: String::new(),
            kind: None,
            limit: 1000,
        });
    glib::MainContext::default().spawn_local(async move {
        match rx.recv().await {
            Ok(Ok((files, total))) => {
                list.append(&label(&format!("{} of {total} indexed files", files.len())));
                for file in files {
                    let check = gtk::CheckButton::with_label(&format!(
                        "#{} · {} · {}",
                        file.id, file.kind, file.path
                    ));
                    let selected = selected.clone();
                    check.connect_toggled(move |c| {
                        let mut ids = selected.borrow_mut();
                        ids.retain(|id| *id != file.id);
                        if c.is_active() {
                            ids.push(file.id);
                        }
                    });
                    list.append(&check);
                }
            }
            Ok(Err(e)) => list.append(&label(&format!("Library unavailable: {e}"))),
            Err(e) => list.append(&label(&format!("Selection interrupted: {e}"))),
        }
    });
}

// Parent may enable only after accepting the shared Tools Undo atomic no-clobber/cross-volume fix.
pub(super) const LINUX_TOOL_UNDO_SAFETY_ACCEPTED: bool = true;
const TOOL_UNDO_UNAVAILABLE_REASON: &str = "Export Undo unavailable: the Linux recovery runtime has not passed atomic no-clobber and cross-volume safety acceptance. Export receipts remain visible; no outputs are moved by this disabled action.";

pub fn tools(engine: Engine) -> gtk::Widget {
    build_tools(engine).0
}
fn build_tools(engine: Engine) -> (gtk::Widget, Rc<ToolsUi>) {
    let body = column();
    let status=label("Capabilities are checked by the engine. Exports preserve originals. Video requires explicitly configured native FFmpeg; cancellation is enabled only when the engine advertises interruptible exports.");
    body.append(&status);
    if !LINUX_TOOL_UNDO_SAFETY_ACCEPTED {
        body.append(&label(TOOL_UNDO_UNAVAILABLE_REASON));
    }
    let capabilities = column();
    body.append(&capabilities);
    let ids = entry("Catalog file IDs, comma separated (up to 100)");
    let destination = entry("Existing absolute output folder");
    body.append(&ids);
    body.append(&destination);
    let pickrow = row();
    body.append(&pickrow);
    let kind = gtk::DropDown::from_strings(&["Photo", "Chapter markers", "Video"]);
    let format = gtk::DropDown::from_strings(&[
        "PNG",
        "JPEG",
        "TIFF",
        "JSON",
        "WebVTT markers (not speech)",
        "MP4",
    ]);
    let dimension = gtk::SpinButton::with_range(1.0, 8192.0, 1.0);
    dimension.set_value(2048.0);
    pickrow.append(&kind);
    pickrow.append(&format);
    pickrow.append(&label("Max dimension"));
    pickrow.append(&dimension);
    let actions = row();
    body.append(&actions);
    let preview = gtk::Button::with_label("Preview");
    let execute = gtk::Button::with_label("Execute preview");
    execute.add_css_class("suggested-action");
    let undo = gtk::Button::with_label("Undo last export");
    if !LINUX_TOOL_UNDO_SAFETY_ACCEPTED {
        undo.set_tooltip_text(Some(TOOL_UNDO_UNAVAILABLE_REASON));
    }
    actions.append(&preview);
    actions.append(&execute);
    actions.append(&undo);
    let cancel = gtk::Button::with_label("Cancel export");
    actions.append(&cancel);
    let outputs = column();
    body.append(&outputs);
    let ui = Rc::new(ToolsUi {
        engine: engine.clone(),
        state: RefCell::new(ExportState::default()),
        status,
        outputs,
        capabilities,
        ids,
        destination,
        kind,
        format,
        dimension,
        execute,
        undo,
        preview,
        cancel,
    });
    {
        let u = ui.clone();
        ui.preview.connect_clicked(move |_| u.send("preview", None));
    }
    {
        let u = ui.clone();
        ui.execute.connect_clicked(move |_| {
            if !u.state.borrow().can_execute() || !u.state.borrow().supports(&u.recipe()) {
                return;
            }
            let id = u.state.borrow_mut().preview.take();
            u.send("execute", id);
        });
    }
    {
        let u = ui.clone();
        ui.cancel.connect_clicked(move |_| {
            let id = u.state.borrow().active_operation.clone();
            u.send("cancel", id);
        });
    }
    {
        let u = ui.clone();
        ui.undo.connect_clicked(move |_| {
            let id = u.state.borrow().history.clone();
            u.send("undo", id);
        });
    }
    {
        let u = ui.clone();
        button(&actions, "Last export", move || u.send("history", None));
    }
    {
        let u = ui.clone();
        button(&actions, "Capabilities", move || {
            u.send("capabilities", None)
        });
    }
    {
        let u = ui.clone();
        let body = body.clone();
        button(&actions, "Pick output folder", move || {
            let dialog = gtk::FileDialog::builder()
                .title("Existing output folder")
                .build();
            let win = body.root().and_downcast::<gtk::Window>();
            let u = u.clone();
            dialog.select_folder(win.as_ref(), gio::Cancellable::NONE, move |r| {
                if let Ok(file) = r {
                    if let Some(p) = file.path() {
                        u.destination.set_text(&p.to_string_lossy());
                    }
                }
            });
        });
    }
    for e in [&ui.ids, &ui.destination] {
        let u = ui.clone();
        e.connect_changed(move |_| {
            u.state.borrow_mut().invalidate();
            u.update();
        });
    }
    for e in [&ui.kind, &ui.format] {
        let u = ui.clone();
        e.connect_selected_notify(move |_| {
            u.state.borrow_mut().invalidate();
            u.update();
        });
    }
    {
        let u = ui.clone();
        ui.dimension.connect_value_changed(move |_| {
            u.state.borrow_mut().invalidate();
            u.update();
        });
    }
    {
        let u = ui.clone();
        button(&actions, "Choose indexed files", move || {
            choose_indexed_files(&u)
        });
    }
    let rx = engine.borrow_mut().subscribe();
    {
        let u = ui.clone();
        glib::MainContext::default().spawn_local(async move {
            while let Ok(ev) = rx.recv().await {
                match ev {
                    EngineState::Ready => {
                        u.send("capabilities", None);
                        u.send("history", None);
                    }
                    EngineState::ToolResponse(r) => {
                        let action = {
                            let mut state = u.state.borrow_mut();
                            let Some(slot) = state.requests.accept(&r.request_id, true) else {
                                continue;
                            };
                            if slot == "operation" {
                                state.active_operation = None;
                                state.cancelling = false;
                                state.requests.forget("cancel");
                                state.operation_action.take().unwrap_or_default()
                            } else {
                                slot
                            }
                        };
                        let action = action.as_str();
                        if action == "cancel" {
                            if r.status == "error" { u.state.borrow_mut().cancelling = false; }
                            u.status.set_label(&format!("{} · {}. Waiting for the original export result; no cancellation completion is confirmed.", r.status, r.message));
                            u.update();
                            continue;
                        }
                        u.status.set_label(&format!("{} · {}", r.status, r.message));
                        {
                            let mut s = u.state.borrow_mut();
                            if action == "capabilities" && r.status == "ok" {
                                s.capabilities = r.capabilities.clone();
                            }
                            if action == "preview"
                                && r.status == "ok"
                                && s.preview_revision == s.revision
                            {
                                s.preview = r.operation_id.clone();
                            }
                            if action == "history" {
                                s.invalidate();
                            }
                            if matches!(action, "execute" | "history")
                                && (r.status == "ok" || !r.outputs.is_empty())
                            {
                                s.history = if r.outputs.iter().any(|o| o.state == "completed") {
                                    r.operation_id.clone()
                                } else {
                                    None
                                };
                            }
                            if action == "undo" && r.status == "ok" {
                                s.history = None;
                            }
                        }
                        let replace_receipts =
                            action != "capabilities" && (r.status == "ok" || !r.outputs.is_empty());
                        if action == "capabilities" {
                            clear(&u.capabilities);
                        } else if replace_receipts {
                            clear(&u.outputs);
                        }
                        for c in r.capabilities {
                            u.capabilities.append(&label(&format!(
                                "{} · {} · {}",
                                c.id,
                                if c.available {
                                    "available"
                                } else {
                                    "unavailable"
                                },
                                c.detail
                            )));
                        }
                        if replace_receipts {
                            u.outputs.append(&label(&format!(
                                "{action} operation: {}",
                                r.operation_id.as_deref().unwrap_or("None")
                            )));
                        }
                        for o in r.outputs {
                            u.outputs.append(&label(&format!(
                                "#{} · {} → {} · {}\n{}",
                                o.file_id, o.source_path, o.output_path, o.state, o.message
                            )));
                        }
                        u.update();
                    }
                    EngineState::Failed(e) => {
                        u.state.borrow_mut().requests.clear();
                        u.state.borrow_mut().preview = None;
                        u.status.set_label(&e);
                        u.update();
                    }
                    EngineState::Exited => {
                        u.state.borrow_mut().requests.clear();
                        u.state.borrow_mut().preview = None;
                        u.status.set_label(
                            "Engine exited. Refresh history before retrying interrupted exports.",
                        );
                        u.update();
                    }
                    _ => {}
                }
            }
        });
    }
    ui.update();
    (page("File Tools", &body), ui)
}

fn retrieval_only(mut request: ChatRequest) -> ChatRequest {
    request.use_model = Some(false);
    request
}

fn dispatch_chat(
    engine: &Engine,
    state: &Rc<RefCell<ChatState>>,
    status: &gtk::Label,
    request: ChatRequest,
) {
    if let Err(error) = engine
        .borrow_mut()
        .send(CommandPayload::ChatRequest(ChatRequestPayload { request: retrieval_only(request) }))
    {
        state.borrow_mut().interrupted();
        status.set_label(&format!("Chat transport failed: {error}. Persisted outcomes are unconfirmed; refresh history after reconnecting."));
    }
}

pub fn chat(engine: Engine) -> gtk::Widget {
    build_chat(engine).0
}
fn build_chat(engine: Engine) -> (gtk::Widget, Rc<RefCell<ChatState>>) {
    let body = column();
    let conversation = entry("Conversation ID");
    conversation.set_text("linux-default");
    body.append(&conversation);
    let status=label("Persistent local evidence Chat uses keyword retrieval only. Model summaries are unavailable with this backend regardless of installed weights or executable configuration. Citations and sparse samples are suggestions, not exhaustive verified coverage.");
    let use_model =
        gtk::CheckButton::with_label("Model summaries · unavailable with this backend");
    use_model.set_active(false);
    use_model.set_sensitive(false);
    body.append(&use_model);
    body.append(&status);
    let transcript = column();
    body.append(&transcript);
    let input = entry("Ask about indexed files (maximum 2000 characters)");
    input.set_max_length(2000);
    body.append(&input);
    let actions = row();
    body.append(&actions);
    let state = Rc::new(RefCell::new(ChatState::default()));
    let result_state = state.clone();
    let send: Rc<dyn Fn(&str)> = {
        let engine = engine.clone();
        let conversation = conversation.clone();
        let input = input.clone();
        let state = state.clone();
        let status = status.clone();
        Rc::new(move |action| {
            if !engine.borrow().is_ready() {
                status
                    .set_label("Engine is not ready. Restart before sending or controlling Chat.");
                return;
            }
            let id = conversation.text().trim().to_owned();
            if id.is_empty() {
                status.set_label("Enter a conversation ID.");
                return;
            }
            if action == "send" && input.text().trim().is_empty() {
                return;
            }
            let r = ChatRequest {
                request_id: request_id(),
                conversation_id: id,
                action: action.into(),
                text: if action == "send" {
                    Some(input.text().into())
                } else {
                    None
                },
                use_model: Some(false),
            };
            let dispatch = state.borrow_mut().begin(&r);
            match dispatch {
                Ok(true) => { status.set_label("Waiting for engine retrieval…"); dispatch_chat(&engine,&state,&status,r); }
                Ok(false) => status.set_label("Clear deferred until the outstanding persisted write finishes. No deletion is confirmed yet."),
                Err(reason) => status.set_label(reason),
            }
        })
    };
    for (text, action) in [
        ("Send", "send"),
        ("History", "history"),
        ("Clear conversation", "clear"),
        ("Stop", "cancel"),
    ] {
        let send = send.clone();
        button(&actions, text, move || send(action));
    }
    {
        let state = state.clone();
        let transcript = transcript.clone();
        conversation.connect_changed(move |_| {
            state.borrow_mut().display.clear();
            clear(&transcript);
        });
    }
    let rx = engine.borrow_mut().subscribe();
    glib::MainContext::default().spawn_local(async move {
        while let Ok(ev) = rx.recv().await {
            match ev {
                EngineState::Ready => send("history"),
                EngineState::ChatResponse(r) => {
                    let deferred = state.borrow_mut().finish(&r);
                    if let Some(request) = deferred {
                        dispatch_chat(&engine, &state, &status, request);
                    }
                    if !chat_matches(
                        &mut state.borrow_mut().display,
                        &r.request_id,
                        &r.conversation_id,
                        conversation.text().trim(),
                        &r.status,
                    ) {
                        continue;
                    }
                    status.set_label(&format!("{} · {}", r.status, r.message));
                    clear(&transcript);
                    for m in r.messages {
                        transcript.append(&label(&format!("{}: {}", m.role, m.text)));
                    }
                    for hit in r.hits {
                        let hr = row();
                        hr.append(&label(&evidence(&hit)));
                        let s = status.clone();
                        button(&hr, "Open citation", move || open(&hit, &s));
                        transcript.append(&hr);
                    }
                }
                EngineState::Failed(e) => {
                    state.borrow_mut().interrupted();
                    status.set_label(&e);
                }
                EngineState::Exited => {
                    state.borrow_mut().interrupted();
                    status.set_label("Engine exited; request interrupted.");
                }
                _ => {}
            }
        }
    });
    (page("Chat", &body), result_state)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn job_controls_block_timeline_actions_and_preserve_catalog_index_retry() {
        let mut j = CatalogJob {
            id: "job".into(),
            kind: "timelineSample".into(),
            file_ids: vec![],
            state: "paused".into(),
            progress: 0.2,
            error: None,
            created_at: 0.0,
            updated_at: 0.0,
        };
        for state in ["queued", "running", "paused", "failed", "cancelled", "completed"] {
            j.state = state.into();
            assert!(job_controls(&j).is_empty());
        }
        j.state = "paused".into();
        j.kind = "catalogIndex".into();
        assert_eq!(job_controls(&j).len(), 2);
        j.state = "completed".into();
        assert!(job_controls(&j).is_empty());
    }

    #[test]
    fn timeline_enqueue_is_guarded_before_request_ownership_or_send() {
        let source = include_str!("workbenches.rs");
        let send = source.split("fn send(&self, slot: &str, request: CatalogRequest)")
            .nth(1).unwrap().split("fn detail(&self)").next().unwrap();
        assert!(send.find("request.action == \"enqueueTimeline\"").unwrap()
            < send.find("self.requests.borrow_mut().begin").unwrap());
        let enqueue = source.split("let analyze = button(").nth(1).unwrap()
            .split("let rx = engine.borrow_mut().subscribe()").next().unwrap();
        assert!(enqueue.find("require_vlm()").unwrap()
            < enqueue.find("ui.send(\"enqueue\", request)").unwrap());
        assert!(enqueue.contains("analyze.set_sensitive(false)"));
    }

    #[test]
    fn chat_retrieval_adapter_never_requests_model_summaries() {
        for action in ["send", "history", "clear", "cancel"] {
            for use_model in [None, Some(false), Some(true)] {
                let request = ChatRequest {
                    request_id: "fixture-request".into(),
                    conversation_id: "fixture-conversation".into(),
                    action: action.into(),
                    text: Some("fixture evidence query".into()),
                    use_model,
                };
                let safe = retrieval_only(request);
                assert_eq!(safe.use_model, Some(false));
                assert_eq!(safe.action, action);
                assert_eq!(safe.request_id, "fixture-request");
                assert_eq!(safe.conversation_id, "fixture-conversation");
                assert_eq!(safe.text.as_deref(), Some("fixture evidence query"));
            }
        }
    }
}

#[cfg(test)]
#[path = "workbench_regression_tests.rs"]
mod regression_tests;
#[cfg(test)]
pub(super) fn run_review_regressions() {
    regression_tests::run();
}
