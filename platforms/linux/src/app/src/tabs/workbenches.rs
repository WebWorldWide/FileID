use super::workbench_state::*;
use crate::engine_client::{EngineClient, EngineState};
use adw::prelude::*;
use fileid_engine::ipc::*;
use std::{cell::RefCell, rc::Rc};

pub(super) type Engine = Rc<RefCell<EngineClient>>;
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
/// Video and audio by the engine's own classifier, so every extension the
/// scan treats as media (MTS, M2TS, 3GP, MPG, Opus, AIFF, ...) plays in place.
pub(super) fn is_playable_media(path: &str) -> bool {
    use fileid_engine::pipeline::discovery::FileKind;
    std::path::Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(FileKind::from_extension(ext), FileKind::Video | FileKind::Audio))
}
pub fn catalog(engine: Engine) -> gtk::Widget {
    super::moments::catalog(engine)
}

pub(super) fn timeline_request(file_ids: Vec<i64>, mode: &str) -> CatalogRequest {
    let mut r = catalog_request("enqueueTimeline");
    r.file_ids = Some(file_ids);
    r.timeline_mode = Some(mode.into());
    r
}

/// The engine resumes only paused timeline jobs; a failed or cancelled one is
/// retried as a fresh job over the same files, as on macOS.
pub(super) fn job_request(action: &str, job: &CatalogJob) -> CatalogRequest {
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

pub(super) fn job_controls(job: &CatalogJob) -> Vec<(&'static str, &'static str)> {
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
            channel: crate::engine_client::QueryChannel::Tools,
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

pub fn chat(engine: Engine) -> gtk::Widget {
    super::chat::chat(engine)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn camera_and_phone_media_play_in_place() {
        for path in ["/v/clip.MTS", "/v/a.m2ts", "/v/b.3gp", "/v/c.mpg", "/a/d.opus", "/a/e.aiff", "/v/f.mp4"] {
            assert!(is_playable_media(path), "{path}");
        }
        for path in ["/p/photo.jpg", "/d/notes.pdf", "/x/no_extension"] {
            assert!(!is_playable_media(path), "{path}");
        }
    }

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
