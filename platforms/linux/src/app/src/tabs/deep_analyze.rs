use crate::{
    engine_client::{EngineClient, EngineState, QuerySpec},
    native_runtime::{self, VLM_KIND},
};
use adw::prelude::*;
use fileid_engine::{
    ipc::*,
    models::registry::{self, LookupResult},
};
use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    rc::Rc,
};

#[path = "deep_status.rs"]
mod deep_status;
use deep_status::DeepTransportStatus;

#[derive(Default)]
struct DeepRun {
    active: bool,
    cancelling: bool,
    failure: Option<String>,
    terminal: bool,
    transport_status: DeepTransportStatus,
}
impl DeepRun {
    fn begin(&mut self) {
        *self = Self {
            active: true,
            ..Self::default()
        };
    }
    fn accepts(&self, model: &str) -> bool {
        self.active && model == VLM_KIND
    }
    fn record_error(&mut self, kind: &str, model: Option<&str>, message: String) -> bool {
        let scoped = matches!(kind, "llama_cpp_missing" | "deep_analyze_failed")
            || (model == Some(VLM_KIND)
                && matches!(kind, "vlm_model_missing" | "deep_analyze_busy"));
        if !self.active || !scoped || model.is_some_and(|model| model != VLM_KIND) {
            return false;
        }
        self.failure.get_or_insert(message);
        true
    }
    fn finish(&mut self) {
        self.active = false;
        self.cancelling = false;
        self.terminal = true;
    }
    fn may_show_idle_availability(&self, verifying: bool) -> bool {
        !self.active && !verifying && self.failure.is_none() && !self.terminal
    }
}

fn exclusions_at(path: &Path) -> Result<Vec<PathBuf>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(format!("Cannot validate Deep Analyze exclusions: {e}")),
    };
    let map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(&bytes).map_err(|_| {
            "Cannot validate Deep Analyze exclusions: settings are malformed.".to_owned()
        })?;
    let Some(value) = map.get("deepAnalyzeExcludedFolders") else {
        return Ok(vec![]);
    };
    let folders = value
        .as_array()
        .filter(|v| v.len() <= 256)
        .ok_or("Cannot validate Deep Analyze exclusions: expected up to 256 absolute folders.")?;
    folders.iter().map(|v| {
        let raw = v.as_str().ok_or("Invalid excluded folder.")?;
        let p = Path::new(raw);
        if !p.is_absolute() { return Err("Excluded folders must be absolute; repair Settings before analyzing.".into()); }
        p.canonicalize().map_err(|_| "Cannot resolve an excluded folder; restore it or repair Settings before analyzing.".into())
    }).collect()
}

fn check_target(payload: &CommandPayload, exclusions: &[PathBuf]) -> Result<(), String> {
    if matches!(payload, CommandPayload::DeepAnalyzeAll(_)) {
        return if exclusions.is_empty() {
            Ok(())
        } else {
            Err("Whole-library analysis is disabled: this IPC contract cannot enforce saved exclusions. Use a verified non-excluded file/folder, or wait for exclusion support.".into())
        };
    }
    let db = fileid_engine::paths::db_path().map_err(|e| e.to_string())?;
    check_target_at(payload, exclusions, &db)
}

fn check_target_at(
    payload: &CommandPayload,
    exclusions: &[PathBuf],
    db: &Path,
) -> Result<(), String> {
    let conn = fileid_engine::db::open_read(db).map_err(|e| e.to_string())?;
    let paths: Vec<String> = match payload {
        CommandPayload::DeepAnalyzeFile(p) => vec![conn
            .query_row(
                "SELECT path_text FROM files WHERE id=?1",
                [p.file_id],
                |r| r.get(0),
            )
            .map_err(|_| "Choose an existing indexed file ID.".to_owned())?],
        CommandPayload::DeepAnalyzeFolder(p) => {
            let folder = Path::new(&p.path_prefix)
                .canonicalize()
                .map_err(|_| "Choose an existing absolute folder.".to_owned())?;
            if exclusions
                .iter()
                .any(|e| folder.starts_with(e) || e.starts_with(&folder))
            {
                return Err("This folder overlaps a saved Deep Analyze exclusion. Choose a non-excluded target.".into());
            }
            let mut stmt = conn
                .prepare("SELECT path_text FROM files WHERE substr(path_text,1,length(?1))=?1")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([&p.path_prefix], |r| r.get(0))
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?
        }
        _ => return Err("Invalid Deep Analyze target.".into()),
    };
    for raw in paths {
        let path = Path::new(&raw)
            .canonicalize()
            .map_err(|_| "An indexed target is offline; restore it before analyzing.".to_owned())?;
        if exclusions.iter().any(|e| path.starts_with(e)) {
            return Err("The target is inside a saved Deep Analyze exclusion (including resolved symlinks).".into());
        }
    }
    Ok(())
}

struct DeepUi {
    engine: Rc<RefCell<EngineClient>>,
    run: RefCell<DeepRun>,
    verifying: Cell<bool>,
    status: gtk::Label,
    progress: gtk::ProgressBar,
    results: gtk::Box,
    start: gtk::Button,
    cancel: gtk::Button,
    scope: gtk::DropDown,
    target: gtk::Entry,
    skip: gtk::CheckButton,
    renames: gtk::CheckButton,
    review_revision: Cell<u64>,
}
impl DeepUi {
    fn update(&self) {
        let run = self.run.borrow();
        let busy = run.active || self.verifying.get();
        let exclusions = fileid_engine::paths::app_settings_path()
            .map_err(|e| e.to_string())
            .and_then(|p| exclusions_at(&p));
        let blocked = match exclusions {
            Err(e) => Some(e),
            Ok(paths) if self.scope.selected() == 0 && !paths.is_empty() => Some("Whole-library analysis disabled: saved exclusions cannot be enforced by this IPC contract. Select a non-excluded indexed file/folder.".to_owned()),
            _ => None,
        };
        self.start.set_sensitive(
            native_runtime::require_vlm().is_ok()
                && self.engine.borrow().is_ready()
                && !busy
                && blocked.is_none(),
        );
        if run.may_show_idle_availability(self.verifying.get()) {
            if let Some(reason) = native_runtime::require_vlm().err().or(blocked) {
                self.status.set_label(&reason);
            }
        }
        self.cancel.set_sensitive(run.active && !run.cancelling);
        self.scope.set_sensitive(!busy);
        self.target
            .set_sensitive(!busy && self.scope.selected() != 0);
        self.skip.set_sensitive(!busy);
        self.renames.set_sensitive(!busy);
    }
    fn payload(&self) -> Result<CommandPayload, String> {
        let model_kind = VLM_KIND.to_owned();
        Ok(match self.scope.selected() {
            0 => CommandPayload::DeepAnalyzeAll(DeepAnalyzeAllPayload {
                model_kind,
                skip_existing: self.skip.is_active(),
                tags_only: false,
                propose_renames: self.renames.is_active(),
                file_ids: None,
                excluded_folders: None,
            }),
            1 => {
                let path = Path::new(self.target.text().as_str()).to_path_buf();
                if !path.is_absolute() || !path.is_dir() {
                    return Err("Enter an existing absolute indexed folder.".into());
                }
                CommandPayload::DeepAnalyzeFolder(DeepAnalyzeFolderPayload {
                    model_kind,
                    path_prefix: format!("{}/", path.to_string_lossy().trim_end_matches('/')),
                })
            }
            _ => CommandPayload::DeepAnalyzeFile(DeepAnalyzeFilePayload {
                model_kind,
                file_id: self
                    .target
                    .text()
                    .parse::<i64>()
                    .ok()
                    .filter(|i| *i > 0)
                    .ok_or("Enter a positive indexed file ID.")?,
            }),
        })
    }
    fn start(self: &Rc<Self>) {
        if let Err(reason) = native_runtime::require_vlm() {
            self.status.set_label(&reason);
            return;
        }
        if !self.engine.borrow().is_ready() || self.run.borrow().active || self.verifying.get() {
            return;
        }
        let payload = match self.payload() {
            Ok(p) => p,
            Err(e) => {
                self.status.set_label(&e);
                return;
            }
        };
        self.verifying.set(true);
        self.status
            .set_label("Verifying pinned Qwen2.5-VL weights, native runtime and exclusions…");
        self.update();
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let result: Result<CommandPayload, String> = (|| {
                native_runtime::require_vlm()?;
                let settings =
                    fileid_engine::paths::app_settings_path().map_err(|e| e.to_string())?;
                check_target(&payload, &exclusions_at(&settings)?)?;
                let LookupResult::Found(model) = registry::lookup_full(VLM_KIND) else {
                    return Err("Pinned Qwen model registry unavailable.".into());
                };
                if !registry::installation_complete(&model) {
                    return Err("Install Qwen2.5-VL-7B in Settings first; the pinned files have not passed SHA-256 verification. No download started.".into());
                }
                Ok(payload)
            })();
            let _ = tx.send_blocking(result);
        });
        let ui = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let result = rx.recv().await;
            ui.verifying.set(false);
            match result {
                Ok(Ok(payload)) if ui.engine.borrow().is_ready() => {
                    if let Err(reason) = native_runtime::require_vlm() {
                        ui.status.set_label(&reason);
                        ui.update();
                        return;
                    }
                    match ui.engine.borrow_mut().send(payload) {
                        Ok(()) => {
                            ui.run.borrow_mut().begin();
                            ui.review_revision.set(ui.review_revision.get().wrapping_add(1));
                            while let Some(c) = ui.results.first_child() { ui.results.remove(&c); }
                            ui.progress.set_fraction(0.0);
                            ui.status.set_label("Queued for engine analysis; native model execution is not yet confirmed.");
                        }
                        Err(e) => ui.status.set_label(&format!("Could not start analysis: {e}")),
                    }
                }
                Ok(Ok(_)) => ui.status.set_label("Engine disconnected during verification. Retry after restarting."),
                Ok(Err(e)) => ui.status.set_label(&e),
                Err(e) => ui.status.set_label(&format!("Verification interrupted: {e}")),
            }
            ui.update();
        });
    }
    fn cancellation_requested(&self) {
        let mut run = self.run.borrow_mut();
        run.cancelling = true;
        self.status
            .set_label(&DeepTransportStatus::cancellation_requested(
                run.failure.as_deref(),
            ));
    }
    fn event(&self, event: EngineState) {
        match event {
            EngineState::DeepAnalyzeStarting(s)
                if self.run.borrow().accepts(&s.model_kind)
                    && self.run.borrow().failure.is_none() =>
            {
                self.status.set_label(&s.message)
            }
            EngineState::DeepAnalyzeProgress(p)
                if self.run.borrow().accepts(&p.model_kind)
                    && self.run.borrow().failure.is_none() =>
            {
                self.progress.set_fraction(if p.total == 0 {
                    0.0
                } else {
                    (p.processed as f64 / p.total as f64).clamp(0.0, 1.0)
                });
                self.status.set_label(&format!(
                    "{} / {}{}\n{}\n{}",
                    p.processed,
                    p.total,
                    p.eta_seconds
                        .filter(|s| s.is_finite() && *s >= 0.0)
                        .map(|s| format!(" · ETA {s:.0}s"))
                        .unwrap_or_default(),
                    p.current_path.unwrap_or_default(),
                    p.current_caption.unwrap_or_default()
                ));
            }
            EngineState::DeepAnalyzeFileDone(f) if self.run.borrow().accepts(&f.model_kind) => {
                self.results.append(
                    &gtk::Label::builder()
                        .xalign(0.0)
                        .wrap(true)
                        .selectable(true)
                        .label(format!(
                            "#{}\n{}\nProposed name (not applied): {}",
                            f.file_id,
                            f.description,
                            f.proposed_name.as_deref().unwrap_or("None")
                        ))
                        .build(),
                )
            }
            EngineState::DeepAnalyzeComplete(c) if self.run.borrow().accepts(&c.model_kind) => {
                self.run.borrow_mut().finish();
                let run = self.run.borrow();
                let outcome = if run.failure.is_some() {
                    "Failed / unavailable"
                } else if c.cancelled {
                    "Cancelled"
                } else if c.failed > 0 {
                    "Failed"
                } else {
                    "Completed"
                };
                self.status.set_label(&format!(
                    "{outcome} · {} processed · {} failed · {:.1}s{}{}",
                    c.processed,
                    c.failed,
                    c.total_seconds,
                    if c.cancelled && run.failure.is_some() {
                        " · cancellation reported"
                    } else {
                        ""
                    },
                    run.failure
                        .as_ref()
                        .map(|message| format!("\n{message}"))
                        .unwrap_or_default()
                ));
            }
            EngineState::Error {
                kind,
                message,
                model_kind,
            } => {
                let mut run = self.run.borrow_mut();
                if run.record_error(&kind, model_kind.as_deref(), message) {
                    self.status.set_label(&format!(
                        "Failed / unavailable: {}\nWaiting for engine completion…",
                        run.failure.as_deref().unwrap_or_default()
                    ));
                }
            }
            EngineState::Failed(e) => {
                let mut run = self.run.borrow_mut();
                run.finish();
                let failure = run.failure.clone();
                self.status
                    .set_label(&run.transport_status.failed(failure.as_deref(), &e));
            }
            EngineState::Exited => {
                let mut run = self.run.borrow_mut();
                run.finish();
                self.status
                    .set_label(&run.transport_status.exited(run.failure.as_deref()));
            }
            _ => {}
        }
        self.update();
    }
}

pub fn build_deep_analyze_tab(engine: Rc<RefCell<EngineClient>>) -> gtk::Widget {
    build_deep_ui(engine).0
}

fn build_deep_ui(engine: Rc<RefCell<EngineClient>>) -> (gtk::Widget, Rc<DeepUi>) {
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.append(
        &gtk::Label::builder()
            .label("Deep Analyze")
            .xalign(0.0)
            .css_classes(["title-1"])
            .build(),
    );
    body.append(&gtk::Label::builder().label("Native Deep AI analysis is unavailable with this backend. Review stored captions and proposed names below; they are suggestions and originals are not renamed. Installing Qwen weights or configuring executables does not enable analysis.").xalign(0.0).wrap(true).build());
    let scope =
        gtk::DropDown::from_strings(&["Whole library", "Indexed folder", "Indexed file ID"]);
    let target = gtk::Entry::builder()
        .placeholder_text("Absolute folder or indexed file ID")
        .build();
    let skip = gtk::CheckButton::with_label("Skip existing captions (whole library)");
    skip.set_active(true);
    let renames = gtk::CheckButton::with_label("Propose smart names (whole library)");
    renames.set_active(true);
    let start = gtk::Button::with_label("Analyze with Qwen2.5-VL-7B");
    start.add_css_class("suggested-action");
    let cancel = gtk::Button::with_label("Cancel analysis");
    let status = gtk::Label::builder()
        .label(native_runtime::require_vlm().unwrap_err())
        .xalign(0.0)
        .wrap(true)
        .selectable(true)
        .build();
    let progress = gtk::ProgressBar::new();
    let results = gtk::Box::new(gtk::Orientation::Vertical, 8);
    for widget in [
        scope.clone().upcast::<gtk::Widget>(),
        target.clone().upcast(),
        skip.clone().upcast(),
        renames.clone().upcast(),
        start.clone().upcast(),
        cancel.clone().upcast(),
        status.clone().upcast(),
        progress.clone().upcast(),
    ] {
        body.append(&widget);
    }
    let ui = Rc::new(DeepUi {
        engine: engine.clone(),
        run: RefCell::new(DeepRun::default()),
        verifying: Cell::new(false),
        status,
        progress,
        results: results.clone(),
        start: start.clone(),
        cancel: cancel.clone(),
        scope: scope.clone(),
        target,
        skip,
        renames,
        review_revision: Cell::new(0),
    });
    {
        let ui = ui.clone();
        start.connect_clicked(move |_| ui.start());
    }
    {
        let ui = ui.clone();
        scope.connect_selected_notify(move |_| ui.update());
    }
    {
        let ui = ui.clone();
        cancel.connect_clicked(move |_| {
            if !ui.run.borrow().active || ui.run.borrow().cancelling {
                return;
            }
            match ui
                .engine
                .borrow_mut()
                .send(CommandPayload::DeepAnalyzeCancel(Empty {}))
            {
                Ok(()) => ui.cancellation_requested(),
                Err(e) => ui
                    .status
                    .set_label(&DeepTransportStatus::cancellation_failed(
                        ui.run.borrow().failure.as_deref(),
                        &e.to_string(),
                    )),
            }
            ui.update();
        });
    }
    let refresh = gtk::Button::with_label("Review indexed captions");
    {
        let ui = ui.clone();
        refresh.connect_clicked(move |_| {
            if ui.run.borrow().active || ui.verifying.get() {
                return;
            }
            let revision = ui.review_revision.get().wrapping_add(1);
            ui.review_revision.set(revision);
            let rx = ui.engine.borrow().query_files(QuerySpec {
                search: String::new(),
                kind: None,
                limit: 1000,
            });
            let ui = ui.clone();
            glib::MainContext::default().spawn_local(async move {
                let result = rx.recv().await;
                if ui.review_revision.get() != revision || ui.run.borrow().active {
                    return;
                }
                while let Some(child) = ui.results.first_child() {
                    ui.results.remove(&child);
                }
                match result {
                    Ok(Ok((files, _))) => {
                        for file in files
                            .into_iter()
                            .filter(|f| f.description.is_some() || f.proposed_name.is_some())
                        {
                            ui.results.append(
                                &gtk::Label::builder()
                                    .xalign(0.0)
                                    .wrap(true)
                                    .selectable(true)
                                    .label(format!(
                                        "Stored result · {}\n{}\nIndexed proposed name: {}",
                                        file.path,
                                        file.description.as_deref().unwrap_or("No indexed caption"),
                                        file.proposed_name.as_deref().unwrap_or("None")
                                    ))
                                    .build(),
                            );
                        }
                        if ui.results.first_child().is_none() {
                            ui.results.append(&gtk::Label::new(Some(
                                "No captions in the latest 1000 indexed files.",
                            )));
                        }
                    }
                    Ok(Err(e)) => ui.status.set_label(&format!("Library unavailable: {e}")),
                    Err(e) => ui.status.set_label(&format!("Review interrupted: {e}")),
                }
            });
        });
    }
    body.append(&refresh);
    body.append(&results);
    let rx = engine.borrow_mut().subscribe();
    {
        let ui = ui.clone();
        glib::MainContext::default().spawn_local(async move {
            while let Ok(event) = rx.recv().await {
                ui.event(event);
            }
        });
    }
    ui.update();
    (
        gtk::ScrolledWindow::builder()
            .child(&body)
            .hexpand(true)
            .vexpand(true)
            .build()
            .upcast(),
        ui,
    )
}

#[cfg(test)]
pub(super) fn run_review_regressions(engine: Rc<RefCell<EngineClient>>) {
    let (_widget, ui) = build_deep_ui(engine);
    ui.run.borrow_mut().active = true;
    ui.event(EngineState::DeepAnalyzeStarting(DeepAnalyzeStarting {
        model_kind: VLM_KIND.into(),
        phase: DeepAnalyzeStartingPhase::LoadingModel,
        message: "Fixture: loading model, not real inference".into(),
    }));
    assert!(ui.status.text().starts_with("Fixture:"));
    ui.event(EngineState::DeepAnalyzeProgress(DeepAnalyzeProgress {
        model_kind: VLM_KIND.into(),
        processed: 1,
        total: 2,
        eta_seconds: Some(3.0),
        current_path: Some("/generated/photo.png".into()),
        current_caption: Some("Fixture streamed caption".into()),
    }));
    assert_eq!(ui.progress.fraction(), 0.5);
    assert!(ui.status.text().contains("Fixture streamed caption"));
    ui.event(EngineState::DeepAnalyzeFileDone(DeepAnalyzeFileDone {
        model_kind: "other".into(),
        file_id: 1,
        description: "Wrong model".into(),
        proposed_name: None,
    }));
    assert!(ui.results.first_child().is_none());
    ui.event(EngineState::DeepAnalyzeFileDone(DeepAnalyzeFileDone {
        model_kind: VLM_KIND.into(),
        file_id: 1,
        description: "Fixture caption".into(),
        proposed_name: Some("fixture.png".into()),
    }));
    assert!(ui.results.first_child().is_some());
    ui.cancel.emit_clicked();
    assert!(ui.run.borrow().cancelling);
    assert!(ui.run.borrow().active);
    assert!(!ui.start.is_sensitive());
    ui.event(EngineState::DeepAnalyzeComplete(DeepAnalyzeComplete {
        model_kind: VLM_KIND.into(),
        processed: 1,
        failed: 0,
        total_seconds: 0.1,
        cancelled: true,
    }));
    assert!(!ui.run.borrow().active);
    assert!(ui.status.text().starts_with("Cancelled"));
    ui.event(EngineState::DeepAnalyzeProgress(DeepAnalyzeProgress {
        model_kind: VLM_KIND.into(),
        processed: 2,
        total: 2,
        eta_seconds: None,
        current_path: None,
        current_caption: Some("Late wrong caption".into()),
    }));
    assert!(ui.status.text().starts_with("Cancelled"));
    ui.run.borrow_mut().active = true;
    ui.event(EngineState::Error {
        kind: "llama_cpp_missing".into(),
        model_kind: Some(VLM_KIND.into()),
        message: "Fixture: incompatible native runtime".into(),
    });
    assert!(ui.run.borrow().active);
    assert!(!ui.start.is_sensitive());
    ui.event(EngineState::DeepAnalyzeComplete(DeepAnalyzeComplete {
        model_kind: VLM_KIND.into(),
        processed: 0,
        failed: 0,
        total_seconds: 0.0,
        cancelled: false,
    }));
    assert!(!ui.run.borrow().active);
    assert!(ui.status.text().starts_with("Failed / unavailable"));
    assert!(ui.status.text().contains("incompatible native runtime"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_dispatch_checks_unavailable_capability_before_work_and_before_send() {
        let source = include_str!("deep_analyze.rs");
        let start = source
            .split("fn start(self: &Rc<Self>)")
            .nth(1)
            .unwrap()
            .split("fn cancellation_requested")
            .next()
            .unwrap();
        assert!(
            start.find("native_runtime::require_vlm()").unwrap()
                < start.find("self.payload()").unwrap()
        );
        let pre_send = start.split("Ok(Ok(payload)) if").nth(1).unwrap();
        assert!(
            pre_send.find("native_runtime::require_vlm()").unwrap()
                < pre_send
                    .find("ui.engine.borrow_mut().send(payload)")
                    .unwrap()
        );
        let update = source
            .split("fn update(&self)")
            .nth(1)
            .unwrap()
            .split("fn payload(&self)")
            .next()
            .unwrap();
        assert!(update.contains("native_runtime::require_vlm().is_ok()"));
        assert!(
            update.find("self.start.set_sensitive").unwrap()
                < update.find("native_runtime::require_vlm().is_ok()").unwrap()
        );
    }

    #[test]
    fn deep_default_literal_matches_actual_original_six_fields_without_scope_claims() {
        let payload = DeepAnalyzeAllPayload {
            model_kind: VLM_KIND.into(),
            skip_existing: true,
            tags_only: false,
            propose_renames: true,
            file_ids: None,
            excluded_folders: None,
        };
        assert!(payload.file_ids.is_none());
        assert!(payload.excluded_folders.is_none());
        let wire = serde_json::to_value(payload).unwrap();
        assert!(wire.get("fileIDs").is_none());
        assert!(wire.get("excludedFolders").is_none());
        assert!(native_runtime::require_vlm().is_err());
    }

    #[test]
    fn deep_terminal_status_is_not_replaced_by_idle_capability_messages() {
        let mut run = DeepRun::default();
        assert!(run.may_show_idle_availability(false));
        assert!(!run.may_show_idle_availability(true));
        run.begin();
        assert!(!run.may_show_idle_availability(false));
        run.finish();
        assert!(!run.may_show_idle_availability(false));
        run.begin();
        assert!(!run.terminal);
        run.active = false;
        assert!(run.may_show_idle_availability(false));
        run.failure = Some("Generated owned failure".into());
        assert!(!run.may_show_idle_availability(false));
    }

    #[test]
    fn deep_run_ignores_other_models_and_post_terminal_events() {
        let mut run = DeepRun {
            active: true,
            cancelling: true,
            ..DeepRun::default()
        };
        assert!(run.accepts(VLM_KIND));
        assert!(!run.accepts("gemma_3_4b"));
        run.finish();
        assert!(!run.accepts(VLM_KIND));
        assert!(!run.cancelling);
    }
    #[test]
    fn deep_failure_latch_is_narrow_owned_and_reset_only_for_a_new_run() {
        let mut run = DeepRun::default();
        assert!(!run.record_error("llama_cpp_missing", None, "stale error".into()));
        run.begin();
        for (kind, model) in [
            ("scan_failed", None),
            ("scan_failed", Some(VLM_KIND)),
            ("vlm_server_payload_rejected", None),
            ("vlm_model_missing", None),
            ("llama_cpp_missing", Some("gemma_3_4b")),
        ] {
            assert!(!run.record_error(kind, model, "unrelated error".into()));
        }
        assert!(run.failure.is_none());
        assert!(run.record_error("llama_cpp_missing", None, "runtime unavailable".into()));
        assert!(run.active);
        assert!(run.record_error("deep_analyze_failed", None, "later failure".into()));
        assert_eq!(run.failure.as_deref(), Some("runtime unavailable"));
        run.finish();
        assert!(!run.active);
        assert_eq!(run.failure.as_deref(), Some("runtime unavailable"));
        assert!(!run.record_error("deep_analyze_failed", None, "late error".into()));
        run.begin();
        assert!(run.failure.is_none());
        assert!(run.record_error("deep_analyze_failed", None, "analysis failed".into()));
        assert!(run.active);
    }

    #[test]
    fn exclusions_fail_closed_and_whole_library_never_drops_them() {
        let dir = std::env::temp_dir().join(super::super::workbench_state::request_id());
        std::fs::create_dir_all(&dir).unwrap();
        let settings = dir.join("settings.json");
        assert!(exclusions_at(&settings).unwrap().is_empty());
        for raw in [
            "broken",
            "null",
            "{\"deepAnalyzeExcludedFolders\":null}",
            "{\"deepAnalyzeExcludedFolders\":[\"relative\"]}",
        ] {
            std::fs::write(&settings, raw).unwrap();
            assert!(exclusions_at(&settings).is_err());
        }
        std::fs::write(
            &settings,
            serde_json::to_vec(&serde_json::json!({"deepAnalyzeExcludedFolders":[dir]})).unwrap(),
        )
        .unwrap();
        let exclusions = exclusions_at(&settings).unwrap();
        let payload = CommandPayload::DeepAnalyzeAll(DeepAnalyzeAllPayload {
            model_kind: VLM_KIND.into(),
            skip_existing: true,
            tags_only: false,
            propose_renames: true,
            file_ids: None,
            excluded_folders: None,
        });
        assert!(check_target(&payload, &exclusions)
            .unwrap_err()
            .contains("disabled"));
        assert!(check_target(&payload, &[]).is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    #[ignore = "requires Xvfb/display; run alone with disposable XDG state"]
    fn deep_none_model_errors_survive_zero_count_complete_and_keep_terminal_ownership() {
        require_empty_deep_status_fixture();
        adw::init().unwrap();
        let engine = Rc::new(RefCell::new(EngineClient::new()));
        engine.borrow().publish_test_event(EngineState::Ready);
        let (_widget, ui) = build_deep_ui(engine.clone());
        let error = serde_json::json!({
            "t": "2026-10-10T00:00:00Z",
            "payload": {"error": {"_0": {
                "kind": "llama_cpp_missing",
                "message": "The llama.cpp runtime isn't usable for image analysis (no working llama-server.exe or llama-mtmd-cli.exe). Update it from Settings -> Performance -> 'Install llama.cpp runtime'."
            }}}
        });
        let complete = serde_json::json!({
            "t": "2026-10-10T00:00:00Z",
            "payload": {"deepAnalyzeComplete": {"_0": {
                "processed": 0, "failed": 0, "totalSeconds": 0.0,
                "modelKind": VLM_KIND, "cancelled": false
            }}}
        });
        *ui.run.borrow_mut() = DeepRun {
            active: true,
            ..DeepRun::default()
        };
        deliver_deep_wire(&engine, error.clone());
        assert!(ui.status.text().contains("runtime isn't usable"));
        assert!(ui.run.borrow().active);
        assert!(!ui.start.is_sensitive());
        ui.event(EngineState::DeepAnalyzeProgress(DeepAnalyzeProgress {
            model_kind: VLM_KIND.into(),
            processed: 0,
            total: 1,
            eta_seconds: None,
            current_path: None,
            current_caption: Some("Late progress must not hide the failure".into()),
        }));
        assert!(ui.status.text().contains("runtime isn't usable"));
        deliver_deep_wire(&engine, complete.clone());
        assert!(!ui.run.borrow().active);
        assert!(ui.status.text().contains("runtime isn't usable"));
        assert!(ui.status.text().starts_with("Failed / unavailable"));
        assert!(!ui.status.text().starts_with("Completed"));

        let mut failure = error.clone();
        failure["payload"]["error"]["_0"]["kind"] = "deep_analyze_failed".into();
        failure["payload"]["error"]["_0"]["message"] = "Generated fixture analysis failed".into();
        *ui.run.borrow_mut() = DeepRun {
            active: true,
            ..DeepRun::default()
        };
        deliver_deep_wire(&engine, failure.clone());
        assert!(ui.run.borrow().active);
        deliver_deep_wire(&engine, complete.clone());
        assert!(ui.status.text().starts_with("Failed / unavailable"));
        assert!(ui
            .status
            .text()
            .contains("Generated fixture analysis failed"));

        let terminal_status = ui.status.text();
        deliver_deep_wire(&engine, error.clone());
        assert_eq!(ui.status.text(), terminal_status);
        *ui.run.borrow_mut() = DeepRun {
            active: true,
            ..DeepRun::default()
        };
        ui.status.set_label("Owned run before unrelated errors");
        let mut unrelated = error.clone();
        unrelated["payload"]["error"]["_0"]["kind"] = "scan_failed".into();
        unrelated["payload"]["error"]["_0"]["modelKind"] = VLM_KIND.into();
        deliver_deep_wire(&engine, unrelated);
        let mut other_model = error.clone();
        other_model["payload"]["error"]["_0"]["modelKind"] = "gemma_3_4b".into();
        deliver_deep_wire(&engine, other_model);
        let mut fallback = error.clone();
        fallback["payload"]["error"]["_0"]["kind"] = "vlm_server_payload_rejected".into();
        deliver_deep_wire(&engine, fallback);
        assert_eq!(ui.status.text(), "Owned run before unrelated errors");
        let mut other_complete = complete.clone();
        other_complete["payload"]["deepAnalyzeComplete"]["_0"]["modelKind"] = "gemma_3_4b".into();
        deliver_deep_wire(&engine, other_complete);
        assert!(ui.run.borrow().active);
        let mut success = complete.clone();
        success["payload"]["deepAnalyzeComplete"]["_0"]["processed"] = 1.into();
        deliver_deep_wire(&engine, success);
        assert!(ui.status.text().starts_with("Completed · 1 processed"));
        assert!(!ui.run.borrow().active);

        *ui.run.borrow_mut() = DeepRun {
            active: true,
            ..DeepRun::default()
        };
        let mut cancelled = complete.clone();
        cancelled["payload"]["deepAnalyzeComplete"]["_0"]["cancelled"] = true.into();
        deliver_deep_wire(&engine, cancelled.clone());
        assert!(ui.status.text().starts_with("Cancelled"));
        *ui.run.borrow_mut() = DeepRun {
            active: true,
            ..DeepRun::default()
        };
        deliver_deep_wire(&engine, error);
        deliver_deep_wire(&engine, cancelled);
        assert!(ui.status.text().contains("runtime isn't usable"));
        assert!(!ui.status.text().starts_with("Completed"));
        assert!(!ui.run.borrow().active);

        *ui.run.borrow_mut() = DeepRun {
            active: true,
            ..DeepRun::default()
        };
        let mut counted_failure = complete;
        counted_failure["payload"]["deepAnalyzeComplete"]["_0"]["failed"] = 1.into();
        deliver_deep_wire(&engine, counted_failure);
        assert!(ui.status.text().starts_with("Failed"));
        assert!(!ui.status.text().starts_with("Completed"));
    }

    #[test]
    #[ignore = "requires GTK/display; authored only in bounded hardening repair"]
    fn deep_owned_failure_survives_cancel_request_and_transport_terminals() {
        require_empty_deep_status_fixture();
        adw::init().unwrap();
        let engine = Rc::new(RefCell::new(EngineClient::new()));
        engine.borrow().publish_test_event(EngineState::Ready);
        let (_widget, ui) = build_deep_ui(engine.clone());
        let error = serde_json::json!({
            "t": "2026-10-10T00:00:00Z",
            "payload": {"error": {"_0": {
                "kind": "llama_cpp_missing",
                "message": "Generated fixture: runtime unavailable"
            }}}
        });
        ui.run.borrow_mut().begin();
        deliver_deep_wire(&engine, error.clone());
        assert!(ui.run.borrow().active);
        ui.event(EngineState::Failed(
            "Generated fixture: engine output read error".into(),
        ));
        assert!(!ui.run.borrow().active);
        assert!(ui.status.text().contains("runtime unavailable"));
        assert!(ui.status.text().contains("engine output read error"));
        ui.event(EngineState::Exited);
        assert!(ui.status.text().contains("runtime unavailable"));
        assert!(ui.status.text().contains("engine output read error"));
        assert!(ui
            .status
            .text()
            .contains("Engine exited; analysis interrupted"));
        assert!(!ui.status.text().contains("Completed"));

        ui.run.borrow_mut().begin();
        assert!(ui.run.borrow().failure.is_none());
        ui.event(EngineState::Failed(
            "Generated fixture: new transport error".into(),
        ));
        ui.event(EngineState::Exited);
        assert!(ui.status.text().contains("new transport error"));
        assert!(!ui.status.text().contains("runtime unavailable"));
        assert!(!ui.status.text().contains("engine output read error"));
        assert!(!ui.status.text().contains("Cancelled"));

        ui.run.borrow_mut().begin();
        let mut analysis_error = error.clone();
        analysis_error["payload"]["error"]["_0"]["kind"] = "deep_analyze_failed".into();
        analysis_error["payload"]["error"]["_0"]["message"] =
            "Generated fixture: first analysis failure".into();
        deliver_deep_wire(&engine, analysis_error);
        deliver_deep_wire(&engine, error);
        assert!(ui.status.text().contains("first analysis failure"));
        assert!(!ui.status.text().contains("runtime unavailable"));
        ui.cancellation_requested();
        ui.update();
        assert!(ui.run.borrow().active);
        assert!(ui.run.borrow().cancelling);
        assert!(!ui.start.is_sensitive());
        assert!(ui.status.text().contains("first analysis failure"));
        assert!(ui.status.text().contains("waiting for engine completion"));
        assert!(!ui.status.text().contains("Cancelled"));
        ui.event(EngineState::Failed(
            "Generated fixture: pipe interrupted".into(),
        ));
        assert!(!ui.run.borrow().active);
        assert!(!ui.run.borrow().cancelling);
        ui.event(EngineState::Exited);
        assert!(ui.status.text().contains("first analysis failure"));
        assert!(ui.status.text().contains("pipe interrupted"));
        assert!(!ui.status.text().contains("Cancelled"));

        ui.run.borrow_mut().begin();
        ui.event(EngineState::DeepAnalyzeComplete(DeepAnalyzeComplete {
            model_kind: VLM_KIND.into(),
            processed: 2,
            failed: 0,
            total_seconds: 1.0,
            cancelled: false,
        }));
        assert!(ui
            .status
            .text()
            .starts_with("Completed · 2 processed · 0 failed"));
        assert!(!ui.status.text().contains("first analysis failure"));
        assert!(!ui.status.text().contains("pipe interrupted"));
    }

    fn require_empty_deep_status_fixture() {
        const INSTRUCTION: &str = "Deep status fixture requires exclusive generated Linux XDG state with empty settings. Run platforms/linux/build/test-deep-status-fixture.sh /absolute/path/to/test-binary standalone; do not set environment inside a running test suite.";
        assert!(cfg!(target_os = "linux"), "{INSTRUCTION}");
        let root = std::env::var_os("FILEID_DEEP_STATUS_FIXTURE_ROOT")
            .map(PathBuf::from)
            .expect(INSTRUCTION);
        let xdg = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .expect(INSTRUCTION);
        let suffix = root
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix("fileid-deep-status-fixture."))
            .expect(INSTRUCTION);
        assert!(
            root.parent() == Some(Path::new("/tmp"))
                && suffix.len() == 8
                && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric()),
            "{INSTRUCTION}"
        );
        assert!(xdg == root.join("xdg"), "{INSTRUCTION}");
        for directory in [root.clone(), xdg.clone(), xdg.join("FileID")] {
            let metadata = std::fs::symlink_metadata(directory).expect(INSTRUCTION);
            assert!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "{INSTRUCTION}"
            );
        }
        let marker = root.join(".fileid-deep-status-fixture");
        let settings = xdg.join("FileID/app-settings.json");
        for (path, expected) in [
            (marker, b"fileid-deep-status-fixture-v1\n".as_slice()),
            (settings, b"{}\n".as_slice()),
        ] {
            let metadata = std::fs::symlink_metadata(&path).expect(INSTRUCTION);
            assert!(
                metadata.is_file()
                    && !metadata.file_type().is_symlink()
                    && metadata.len() == expected.len() as u64,
                "{INSTRUCTION}"
            );
            assert_eq!(
                std::fs::read(path).expect(INSTRUCTION),
                expected,
                "{INSTRUCTION}"
            );
        }
    }

    fn deliver_deep_wire(engine: &Rc<RefCell<EngineClient>>, wire: serde_json::Value) {
        let event: IpcEvent = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            serde_json::to_value(&event).unwrap()["payload"],
            wire["payload"]
        );
        let event = match event.payload {
            EventPayload::Error(error) => EngineState::Error {
                kind: error.inner.kind,
                message: error.inner.message,
                model_kind: error.inner.model_kind,
            },
            EventPayload::DeepAnalyzeComplete(c) => EngineState::DeepAnalyzeComplete(c.inner),
            _ => panic!("Unexpected Deep fixture wire event"),
        };
        engine.borrow().publish_test_event(event);
        let context = glib::MainContext::default();
        while context.pending() {
            context.iteration(false);
        }
    }

    #[cfg(unix)]
    #[test]
    fn explicit_targets_reject_excluded_symlink_and_parent_folder() {
        let dir = std::env::temp_dir().join(super::super::workbench_state::request_id());
        let private = dir.join("private");
        let allowed = dir.join("allowed");
        std::fs::create_dir_all(&private).unwrap();
        std::fs::create_dir_all(&allowed).unwrap();
        std::fs::write(private.join("photo.png"), b"generated fixture").unwrap();
        std::fs::write(allowed.join("photo.png"), b"generated fixture").unwrap();
        std::os::unix::fs::symlink(private.join("photo.png"), allowed.join("alias.png")).unwrap();
        let db = dir.join("fixture.sqlite");
        let conn = fileid_engine::db::open_writer(&db).unwrap();
        for (id, file) in [
            (1, private.join("photo.png")),
            (2, allowed.join("photo.png")),
            (3, allowed.join("alias.png")),
        ] {
            conn.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(?1,?2,?1,1,0,'image','png')", rusqlite::params![id, file.to_str().unwrap()]).unwrap();
        }
        let exclusions = vec![private.canonicalize().unwrap()];
        for id in [1, 3] {
            let p = CommandPayload::DeepAnalyzeFile(DeepAnalyzeFilePayload {
                model_kind: VLM_KIND.into(),
                file_id: id,
            });
            assert!(check_target_at(&p, &exclusions, &db).is_err());
        }
        let p = CommandPayload::DeepAnalyzeFile(DeepAnalyzeFilePayload {
            model_kind: VLM_KIND.into(),
            file_id: 2,
        });
        assert!(check_target_at(&p, &exclusions, &db).is_ok());
        for folder in [&dir, &allowed, &private] {
            let p = CommandPayload::DeepAnalyzeFolder(DeepAnalyzeFolderPayload {
                model_kind: VLM_KIND.into(),
                path_prefix: format!("{}/", folder.display()),
            });
            assert!(check_target_at(&p, &exclusions, &db).is_err());
        }
        drop(conn);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
