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
fn page(body: &gtk::Box) -> gtk::Widget {
    body.set_margin_top(20);
    body.set_margin_bottom(20);
    body.set_margin_start(26);
    body.set_margin_end(26);
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
    if !std::path::Path::new(&hit.path).is_file() {
        status.set_label("File is unavailable offline or has moved.");
        return;
    }
    let is_media = hit.kind != "page"
        && matches!(
            std::path::Path::new(&hit.path)
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Some(
                "mp4"
                    | "mov"
                    | "m4v"
                    | "mkv"
                    | "avi"
                    | "webm"
                    | "mp3"
                    | "m4a"
                    | "wav"
                    | "flac"
                    | "ogg"
                    | "aac"
            )
        );
    if is_media {
        play_inline(&hit.path, hit.start_seconds, status);
        return;
    }
    let uri = gio::File::for_path(&hit.path).uri();
    match gio::AppInfo::launch_default_for_uri(&uri, gio::AppLaunchContext::NONE) {
        Ok(()) => status.set_label("Opened in the local default application."),
        Err(e) => status.set_label(&format!("Could not open file: {e}")),
    }
}

/// Plays a matching video or audio file inside FileID, starting at the
/// matched moment when there is one.
fn play_inline(path: &str, start_seconds: Option<f64>, status: &gtk::Label) {
    let media = gtk::MediaFile::for_filename(path);
    if let Some(seconds) = start_seconds.filter(|s| s.is_finite() && *s > 0.0) {
        let micros = (seconds * 1_000_000.0) as i64;
        media.connect_prepared_notify(move |stream| {
            if stream.is_prepared() && stream.is_seekable() {
                stream.seek(micros);
            }
        });
    }
    let video = gtk::Video::builder()
        .media_stream(&media)
        .hexpand(true)
        .vexpand(true)
        .build();
    let dialog = adw::Dialog::builder()
        .title(
            std::path::Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        )
        .content_width(900)
        .content_height(560)
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&video));
    dialog.set_child(Some(&toolbar));
    let on_close = media.clone();
    dialog.connect_closed(move |_| on_close.pause());
    dialog.present(Some(status));
    media.play();
    status.set_label(if start_seconds.is_some() {
        "Playing from the matched moment."
    } else {
        "Playing in FileID."
    });
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
    jobs: gtk::Box,
    title: gtk::Entry,
    summary: gtk::Entry,
    start: gtk::SpinButton,
    end: gtk::SpinButton,
    root: gtk::Box,
}
impl CatalogUi {
    fn send(&self, slot: &str, request: CatalogRequest) {
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
            self.requests.borrow_mut().clear();
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
    let status = label("Sparse sampled frames are unverified and can miss brief events.");
    body.append(&status);
    let timeline = row();
    let timeline_mode = gtk::DropDown::from_strings(&["Quick samples", "Significant moments"]);
    timeline.append(&timeline_mode);
    body.append(&timeline);
    body.append(&label("Timeline analysis needs a local visual model from Deep Analyze; nothing is downloaded automatically. Quick samples caption one moment per ten seconds; significant moments also examine overlapping four-second frame sequences. With a speech model installed from the model catalog (such as Qwen3-ASR), speech is transcribed on this device in 30-second searchable passages. Every draft chapter needs review."));
    let hits = column();
    body.append(&hits);
    let chapters = column();
    body.append(&label("Chapter markers (original files are unchanged)"));
    body.append(&chapters);
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
        button(&timeline, "Analyze selected video timeline", move || {
            if ui.requests.borrow().busy("timeline") {
                return;
            }
            let Some(hit) = ui.selected.borrow().clone() else {
                ui.status
                    .set_label("Select a video result (Chapters) first.");
                return;
            };
            let mode = if timeline_mode.selected() == 1 {
                "moments"
            } else {
                "sampled"
            };
            ui.send("timeline", timeline_request(vec![hit.file_id], mode));
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
                                ui.render_chapters(r.chapters);
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
                                        let job = job.clone();
                                        button(&controls, name, move || {
                                            u.send("job-control", job_request(action, &job));
                                        });
                                    }
                                    jr.append(&controls);
                                    ui.jobs.append(&jr);
                                }
                            }
                            "job-control" | "timeline" => ui.send("jobs", catalog_request("jobs")),
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
    (page(&body), ui)
}

fn timeline_request(file_ids: Vec<i64>, mode: &str) -> CatalogRequest {
    let mut r = catalog_request("enqueueTimeline");
    r.file_ids = Some(file_ids);
    r.timeline_mode = Some(mode.into());
    r
}

/// The engine resumes only paused timeline jobs; a failed or cancelled one is
/// retried as a fresh job over the same files, as on macOS.
fn job_request(action: &str, job: &CatalogJob) -> CatalogRequest {
    if action == "retryTimeline" {
        let mut ids = job.file_ids.clone();
        ids.sort_unstable();
        ids.dedup();
        let mut r = catalog_request("enqueueTimeline");
        r.file_ids = Some(ids);
        return r;
    }
    let mut r = catalog_request(action);
    r.job_id = Some(job.id.clone());
    r
}

fn job_controls(job: &CatalogJob) -> Vec<(&'static str, &'static str)> {
    let mut controls = vec![];
    if !matches!(job.kind.as_str(), "timelineSample" | "catalogIndex") {
        return controls;
    }
    if matches!(job.state.as_str(), "running" | "queued") {
        controls.push(("pauseJob", "Pause"));
    }
    match (job.kind.as_str(), job.state.as_str()) {
        (_, "paused") => controls.push(("resumeJob", "Resume")),
        ("catalogIndex", "failed" | "cancelled") => controls.push(("resumeJob", "Retry")),
        ("timelineSample", "failed" | "cancelled") => controls.push(("retryTimeline", "Retry")),
        _ => {}
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
}
impl ToolsUi {
    fn recipe(&self) -> ToolRecipe {
        ToolRecipe {
            kind: ["photo", "chapters"][self.kind.selected() as usize].into(),
            format: ["png", "jpeg", "tiff", "json", "vtt"][self.format.selected() as usize].into(),
            max_dimension: self.dimension.value() as u32,
            allow_upscale: None,
            video_aspect_ratio: None,
            video_framing: None,
            video_stabilization: None,
        }
    }
    fn update(&self) {
        let s = self.state.borrow();
        self.execute
            .set_sensitive(s.can_execute() && s.supports(&self.recipe()));
        self.undo.set_sensitive(
            LINUX_TOOL_UNDO_SAFETY_ACCEPTED && s.history.is_some() && !s.requests.busy("operation"),
        );
        self.preview
            .set_sensitive(s.supports(&self.recipe()) && !s.requests.busy("operation"));
    }
    fn send(&self, action: &str, operation: Option<String>) {
        if action == "undo" && !LINUX_TOOL_UNDO_SAFETY_ACCEPTED {
            self.status.set_label(TOOL_UNDO_UNAVAILABLE_REASON);
            return;
        }
        if action != "capabilities" && self.state.borrow().requests.busy("operation") {
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
            } else {
                state.requests.forget("history");
                state.operation_action = Some(action.to_owned());
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
            self.state.borrow_mut().requests.clear();
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
            offset: 0,
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
    let status=label("Capabilities are checked by the engine. Exports preserve originals. Video and export cancellation are unavailable.");
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
    let kind = gtk::DropDown::from_strings(&["Photo", "Chapter markers"]);
    let format = gtk::DropDown::from_strings(&[
        "PNG",
        "JPEG",
        "TIFF",
        "JSON",
        "WebVTT markers (not speech)",
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
                                state.operation_action.take().unwrap_or_default()
                            } else {
                                slot
                            }
                        };
                        let action = action.as_str();
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
    (page(&body), ui)
}

fn dispatch_chat(
    engine: &Engine,
    state: &Rc<RefCell<ChatState>>,
    status: &gtk::Label,
    request: ChatRequest,
) {
    if let Err(error) = engine
        .borrow_mut()
        .send(CommandPayload::ChatRequest(ChatRequestPayload { request }))
    {
        state.borrow_mut().interrupted();
        status.set_label(&format!("Chat transport failed: {error}. Persisted outcomes are unconfirmed; refresh history after reconnecting."));
    }
}

pub fn chat(engine: Engine) -> gtk::Widget {
    let body = column();
    let conversation = entry("Conversation ID");
    conversation.set_text("linux-default");
    body.append(&conversation);
    let status=label("Persistent keyword retrieval chat. Local model summary/streaming is unavailable; citations are indexed evidence, not verified inference.");
    body.append(&status);
    let transcript = column();
    body.append(&transcript);
    let input = entry("Ask about indexed files (maximum 2000 characters)");
    input.set_max_length(2000);
    body.append(&input);
    let actions = row();
    body.append(&actions);
    let state = Rc::new(RefCell::new(ChatState::default()));
    let send: Rc<dyn Fn(&str)> = {
        let engine = engine.clone();
        let conversation = conversation.clone();
        let input = input.clone();
        let state = state.clone();
        let status = status.clone();
        Rc::new(move |action| {
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
                model_kind: None,
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
    page(&body)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn job(kind: &str, state: &str) -> CatalogJob {
        CatalogJob {
            id: "job".into(),
            kind: kind.into(),
            file_ids: vec![42, 7, 42],
            state: state.into(),
            progress: 0.2,
            error: None,
            created_at: 0.0,
            updated_at: 0.0,
        }
    }
    fn actions(job: &CatalogJob) -> Vec<&'static str> {
        job_controls(job)
            .into_iter()
            .map(|(action, _)| action)
            .collect()
    }
    #[test]
    fn job_controls_follow_engine_transitions() {
        assert_eq!(
            actions(&job("timelineSample", "running")),
            ["pauseJob", "cancelJob"]
        );
        assert_eq!(
            actions(&job("timelineSample", "paused")),
            ["resumeJob", "cancelJob"]
        );
        assert_eq!(actions(&job("timelineSample", "failed")), ["retryTimeline"]);
        assert!(actions(&job("timelineSample", "completed")).is_empty());
        assert_eq!(actions(&job("catalogIndex", "failed")), ["resumeJob"]);
        assert_eq!(
            actions(&job("catalogIndex", "paused")),
            ["resumeJob", "cancelJob"]
        );
        assert!(actions(&job("unknown", "paused")).is_empty());
    }
    #[test]
    fn timeline_requests_carry_files_mode_and_retry_identity() {
        let request = timeline_request(vec![42], "moments");
        assert_eq!(request.action, "enqueueTimeline");
        assert_eq!(request.file_ids.as_deref(), Some(&[42][..]));
        assert_eq!(request.timeline_mode.as_deref(), Some("moments"));
        let retry = job_request("retryTimeline", &job("timelineSample", "failed"));
        assert_eq!(
            (
                retry.action.as_str(),
                retry.file_ids.as_deref(),
                retry.job_id.as_deref()
            ),
            ("enqueueTimeline", Some(&[7, 42][..]), None)
        );
        let pause = job_request("pauseJob", &job("timelineSample", "running"));
        assert_eq!(
            (pause.action.as_str(), pause.job_id.as_deref()),
            ("pauseJob", Some("job"))
        );
    }
}

#[cfg(test)]
#[path = "workbench_regression_tests.rs"]
mod regression_tests;
#[cfg(test)]
pub(super) fn run_review_regressions() {
    regression_tests::run();
}
