// Only the two model bundles required by the Linux scan preflight are exposed.
// The shared engine owns downloads, SHA-256 verification, and keyed sentinels.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::thread;

use adw::prelude::*;
use gtk::glib;

use crate::engine_client::{EngineClient, EngineState};
use fileid_engine::models::registry::{self, LookupResult, Model};

const SCAN_MODELS: [(&str, &str, &str); 2] = [
    (
        "mobileclip_s2",
        "CLIP image encoder",
        "Indexes image content for local search. The engine downloads its pinned ONNX bundle only when you select Install.",
    ),
    (
        "arcface",
        "Face detection + recognition",
        "YuNet and SFace models needed by the scan pipeline. Face browsing is not yet available in the Linux UI.",
    ),
];

enum Transfer {
    Checking { after_download: bool },
    Missing,
    Downloading { fraction: f64, message: String },
    Cancelling,
    Installed,
    Cancelled,
    Failed(String),
}

struct ModelRow {
    kind: &'static str,
    model: Option<Model>,
    engine: Weak<RefCell<EngineClient>>,
    engine_ready: Cell<bool>,
    revision: Cell<u64>,
    transfer: RefCell<Transfer>,
    status: gtk::Label,
    progress: gtk::ProgressBar,
    install: gtk::Button,
    cancel: gtk::Button,
}

impl ModelRow {
    fn render(&self) {
        let transfer = self.transfer.borrow();
        let (status, action, can_install, show_progress, fraction, show_cancel) = match &*transfer {
            Transfer::Checking { after_download } => (
                if *after_download { "Verifying SHA-256 and pinned revision…" } else { "Checking installed files…" }.to_owned(),
                "Install", false, *after_download, 1.0, false,
            ),
            Transfer::Missing => ("Not installed".to_owned(), "Install", self.engine_ready.get(), false, 0.0, false),
            Transfer::Downloading { fraction, message } => (
                message.clone(), "Install", false, true, *fraction, true,
            ),
            Transfer::Cancelling => ("Cancelling download…".to_owned(), "Install", false, true, self.progress.fraction(), true),
            Transfer::Installed => (
                "Installed · SHA-256 verified against pinned files".to_owned(), "Verify again", true, false, 1.0, false,
            ),
            Transfer::Cancelled => ("Download cancelled; partial files can be resumed.".to_owned(), "Retry", self.engine_ready.get(), false, 0.0, false),
            Transfer::Failed(message) => (message.clone(), "Retry", self.engine_ready.get() && self.model.is_some(), false, 0.0, false),
        };
        self.status.set_label(&status);
        self.status.set_tooltip_text(Some(&status));
        match &*transfer {
            Transfer::Downloading { fraction, .. } if *fraction > 0.0 =>
                self.progress.set_text(Some(&format!("{:.0}%", fraction * 100.0))),
            Transfer::Downloading { .. } => self.progress.set_text(Some("Queued")),
            Transfer::Cancelling => self.progress.set_text(Some("Cancelling")),
            Transfer::Checking { after_download: true } =>
                self.progress.set_text(Some("Verifying")),
            _ => self.progress.set_text(None),
        }
        self.progress.set_show_text(show_progress);
        self.progress.set_visible(show_progress);
        self.progress.set_fraction(fraction);
        self.install.set_label(action);
        self.install.set_visible(!show_progress);
        self.install.set_sensitive(can_install);
        self.cancel.set_visible(show_cancel);
        self.cancel.set_sensitive(!matches!(&*transfer, Transfer::Cancelling));
    }

    fn check_installed(self: &Rc<Self>, after_download: bool) {
        let Some(model) = self.model.clone() else {
            *self.transfer.borrow_mut() = Transfer::Failed("Model registry is unavailable on this device.".to_owned());
            self.render();
            return;
        };
        let revision = self.revision.get().wrapping_add(1);
        self.revision.set(revision);
        *self.transfer.borrow_mut() = Transfer::Checking { after_download };
        self.render();
        let (tx, rx) = async_channel::bounded(1);
        thread::spawn(move || {
            let _ = tx.send_blocking(registry::installation_complete(&model));
        });
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let Ok(installed) = rx.recv().await else { return; };
            let Some(row) = weak.upgrade() else { return; };
            if row.revision.get() != revision { return; }
            *row.transfer.borrow_mut() = if installed {
                Transfer::Installed
            } else if after_download {
                Transfer::Failed("Engine reported completion, but the pinned model files did not pass SHA-256 verification. Retry to repair the bundle.".to_owned())
            } else {
                Transfer::Missing
            };
            row.render();
        });
    }

    fn install(self: &Rc<Self>) {
        if !self.engine_ready.get() || self.model.is_none() { return; }
        if matches!(*self.transfer.borrow(), Transfer::Installed) {
            self.check_installed(false);
            return;
        }
        if !matches!(*self.transfer.borrow(), Transfer::Missing | Transfer::Cancelled | Transfer::Failed(_)) { return; }
        let result = self.engine.upgrade()
            .ok_or_else(|| "Engine is no longer available.".to_owned())
            .and_then(|engine| engine.borrow_mut().prewarm_model(self.kind).map_err(|err| err.to_string()));
        self.revision.set(self.revision.get().wrapping_add(1));
        *self.transfer.borrow_mut() = match result {
            Ok(()) => Transfer::Downloading { fraction: 0.0, message: "Queued — waiting for engine…".to_owned() },
            Err(err) => Transfer::Failed(format!("Could not start download: {err}")),
        };
        self.render();
    }

    fn cancel(&self) {
        if !matches!(*self.transfer.borrow(), Transfer::Downloading { .. }) { return; }
        let result = self.engine.upgrade()
            .ok_or_else(|| "Engine is no longer available.".to_owned())
            .and_then(|engine| engine.borrow_mut().cancel_prewarm(self.kind).map_err(|err| err.to_string()));
        *self.transfer.borrow_mut() = match result {
            Ok(()) => Transfer::Cancelling,
            Err(err) => Transfer::Failed(format!("Could not cancel download: {err}")),
        };
        self.render();
    }

    fn on_progress(self: &Rc<Self>, progress: &fileid_engine::ipc::ModelDownloadProgress) {
        if progress.model_kind != self.kind { return; }
        if progress.fraction >= 1.0 {
            self.check_installed(true);
            return;
        }
        if matches!(*self.transfer.borrow(), Transfer::Cancelling) { return; }
        self.revision.set(self.revision.get().wrapping_add(1));
        let fraction = if progress.fraction.is_finite() { progress.fraction.clamp(0.0, 0.999) } else { 0.0 };
        *self.transfer.borrow_mut() = Transfer::Downloading { fraction, message: progress.message.clone() };
        self.render();
    }

    fn on_error(&self, kind: &str, message: &str) {
        self.revision.set(self.revision.get().wrapping_add(1));
        *self.transfer.borrow_mut() = if kind == "prewarm_cancelled" {
            Transfer::Cancelled
        } else if kind == "model_download_disk_full" {
            Transfer::Failed("Not enough disk space for the model and temporary download files. Free space in the Linux models directory, then Retry.".to_owned())
        } else {
            Transfer::Failed(message.to_owned())
        };
        self.render();
    }
}

fn build_model_row(
    kind: &'static str,
    title: &str,
    description: &str,
    engine: &Rc<RefCell<EngineClient>>,
) -> (gtk::Widget, Rc<ModelRow>) {
    let model = match registry::lookup_full(kind) {
        LookupResult::Found(model) => Some(model),
        LookupResult::Unknown => None,
    };
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .margin_top(16)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .css_classes(["fileid-glass"])
        .build();
    let heading = gtk::Label::builder().label(title).xalign(0.0).css_classes(["heading"]).build();
    card.append(&heading);
    card.append(&gtk::Label::builder().label(description).xalign(0.0).wrap(true).css_classes(["dim-label"]).build());
    let detail = match &model {
        Some(model) => {
            let bytes: u64 = model.files.iter().map(|file| file.approx_bytes).sum();
            format!("{} · {} files · about {} MB · pinned downloads from huggingface.co", kind, model.files.len(), bytes / 1_048_576)
        }
        None => format!("{kind} · model directory unavailable"),
    };
    card.append(&gtk::Label::builder().label(&detail).xalign(0.0).wrap(true).css_classes(["dim-label"]).build());
    let status = gtk::Label::builder().xalign(0.0).wrap(true).build();
    card.append(&status);
    let progress = gtk::ProgressBar::new();
    progress.set_visible(false);
    card.append(&progress);
    let actions = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(8).build();
    let install = gtk::Button::with_label("Install");
    install.add_css_class("suggested-action");
    actions.append(&install);
    let cancel = gtk::Button::with_label("Cancel");
    cancel.set_visible(false);
    actions.append(&cancel);
    card.append(&actions);
    let row = Rc::new(ModelRow {
        kind,
        model,
        engine: Rc::downgrade(engine),
        engine_ready: Cell::new(false),
        revision: Cell::new(0),
        transfer: RefCell::new(Transfer::Checking { after_download: false }),
        status,
        progress,
        install: install.clone(),
        cancel: cancel.clone(),
    });
    let weak = Rc::downgrade(&row);
    install.connect_clicked(move |_| {
        if let Some(row) = weak.upgrade() { row.install(); }
    });
    let weak = Rc::downgrade(&row);
    cancel.connect_clicked(move |_| {
        if let Some(row) = weak.upgrade() { row.cancel(); }
    });
    row.render();
    row.check_installed(false);
    (card.upcast(), row)
}

pub fn build(engine: Rc<RefCell<EngineClient>>) -> gtk::Widget {
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .css_classes(["fileid-tab"])
        .build();
    root.append(&gtk::Label::builder().label("Scan models").xalign(0.0).css_classes(["title-1"]).build());
    root.append(&gtk::Label::builder()
        .label("The Library scan requires these two local model bundles. Nothing is downloaded unless you press Install. The engine verifies each file against its pinned SHA-256 before marking the bundle installed. No Deep Analyze models are offered on Linux.")
        .xalign(0.0).wrap(true).css_classes(["dim-label"]).build());
    let connection = gtk::Label::builder().label("Engine: starting…").xalign(0.0).css_classes(["dim-label"]).build();
    root.append(&connection);
    let rows: Vec<Rc<ModelRow>> = SCAN_MODELS.iter().map(|(kind, title, description)| {
        let (card, row) = build_model_row(kind, title, description, &engine);
        root.append(&card);
        row
    }).collect();
    let rx = engine.borrow_mut().subscribe();
    glib::MainContext::default().spawn_local(async move {
        while let Ok(event) = rx.recv().await {
            match &event {
                EngineState::Ready => {
                    connection.set_label("Engine: ready");
                    for row in &rows { row.engine_ready.set(true); row.render(); }
                }
                EngineState::ModelDownloadProgress(progress) => {
                    for row in &rows { row.on_progress(progress); }
                }
                EngineState::Error { kind, message, model_kind: Some(model_kind) } => {
                    for row in &rows {
                        if row.kind == model_kind { row.on_error(kind, message); }
                    }
                }
                EngineState::Failed(message) => {
                    connection.set_label(&format!("Engine unavailable: {message}"));
                    for row in &rows {
                        row.engine_ready.set(false);
                        if matches!(*row.transfer.borrow(), Transfer::Downloading { .. } | Transfer::Cancelling) {
                            row.on_error("engine_disconnected", "Engine disconnected during the download. Restart FileID and Retry.");
                        } else {
                            row.render();
                        }
                    }
                }
                _ => {}
            }
        }
    });
    let scroll = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).build();
    scroll.set_child(Some(&root));
    scroll.upcast()
}
