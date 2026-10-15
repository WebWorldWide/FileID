// Extended model catalog: browse curated and catalog models for this machine,
// install a pinned model on request, and benchmark it locally.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use fileid_engine::ipc::{ModelBenchmark, ModelCatalogItem};
use gtk::glib;

use crate::engine_client::{EngineClient, EngineState};

const ROLES: [(&str, &str); 4] = [
    ("Vision", "vision"),
    ("Text", "text"),
    ("Embedding", "embedding"),
    ("Speech", "speech"),
];
const MAX_ROWS: usize = 80;

/// Models to show: optionally only those that fit, measured models first
/// (fastest decode first), then curated, then smallest resident estimate.
pub(crate) fn visible(models: &[ModelCatalogItem], only_fit: bool) -> Vec<&ModelCatalogItem> {
    let mut shown: Vec<&ModelCatalogItem> = models.iter().filter(|m| !only_fit || m.fits).collect();
    let speed = |m: &ModelCatalogItem| {
        m.benchmark
            .as_ref()
            .and_then(|b| b.generated_tokens_per_second)
            .unwrap_or(-1.0)
    };
    shown.sort_by(|a, b| {
        speed(b)
            .total_cmp(&speed(a))
            .then((b.status == "curated").cmp(&(a.status == "curated")))
            .then(a.estimated_resident_gb.total_cmp(&b.estimated_resident_gb))
            .then(a.id.cmp(&b.id))
    });
    shown.truncate(MAX_ROWS);
    shown
}

pub(crate) fn summary(model: &ModelCatalogItem) -> String {
    let terms = if model.terms_required {
        " · terms shown before install"
    } else {
        ""
    };
    let state = if model.installed {
        "installed"
    } else if model.runnable {
        "available"
    } else {
        "not yet runnable here"
    };
    format!(
        "{} · {} · {:.1} GB resident · {:.1} GB download · {} · {} · {}{}",
        model.name,
        model.tier,
        model.estimated_resident_gb,
        model.download_bytes as f64 / 1e9,
        model.license,
        model.status,
        state,
        terms
    )
}

pub(crate) fn benchmark_summary(benchmark: &ModelBenchmark) -> String {
    let mut parts = vec![format!("loads in {:.1} s", benchmark.load_seconds)];
    if let Some(rate) = benchmark.generated_tokens_per_second {
        parts.push(format!("{rate:.0} tok/s"));
    }
    if let Some(seconds) = benchmark.seconds_per_image {
        parts.push(format!("{seconds:.1} s/image"));
    }
    if let Some(accuracy) = benchmark.probe_accuracy {
        parts.push(format!("{:.0}% probes", accuracy * 100.0));
    }
    if let Some(rate) = benchmark.json_valid_rate {
        parts.push(format!("{:.0}% valid JSON", rate * 100.0));
    }
    if let Some(peak) = benchmark.peak_resident_gb {
        parts.push(format!("{peak:.1} GB peak"));
    }
    if benchmark
        .tuned_arguments
        .as_ref()
        .is_some_and(|args| !args.is_empty())
    {
        parts.push("tuned".into());
    }
    format!("Measured here: {}", parts.join(" · "))
}

struct Catalog {
    engine: Rc<RefCell<EngineClient>>,
    list: gtk::Box,
    status: gtk::Label,
    hardware: gtk::Label,
    role: gtk::DropDown,
    only_fit: gtk::CheckButton,
    models: RefCell<Vec<ModelCatalogItem>>,
    request: RefCell<String>,
}

impl Catalog {
    fn role(&self) -> &'static str {
        ROLES
            .get(self.role.selected() as usize)
            .map_or("vision", |(_, role)| role)
    }

    fn refresh(&self) {
        let id = format!("models-{}", glib::monotonic_time());
        *self.request.borrow_mut() = id.clone();
        if let Err(error) = self.engine.borrow_mut().list_models(&id, Some(self.role())) {
            self.status
                .set_label(&format!("Engine unavailable: {error}"));
        }
    }

    fn render(self: &Rc<Self>) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let models = self.models.borrow();
        let shown = visible(&models, self.only_fit.is_active());
        if shown.is_empty() {
            self.list.append(
                &gtk::Label::builder()
                    .label("No catalog models match these filters.")
                    .xalign(0.0)
                    .css_classes(["dim-label"])
                    .build(),
            );
        }
        for model in shown {
            let row = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(4)
                .css_classes(["fileid-glass"])
                .build();
            row.append(
                &gtk::Label::builder()
                    .label(summary(model))
                    .xalign(0.0)
                    .wrap(true)
                    .build(),
            );
            row.append(
                &gtk::Label::builder()
                    .label(format!("{} · {}", model.upstream, model.id))
                    .xalign(0.0)
                    .wrap(true)
                    .css_classes(["dim-label"])
                    .build(),
            );
            if let Some(benchmark) = &model.benchmark {
                row.append(
                    &gtk::Label::builder()
                        .label(benchmark_summary(benchmark))
                        .xalign(0.0)
                        .wrap(true)
                        .build(),
                );
            }
            let actions = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(8)
                .build();
            if model.runnable && !model.installed {
                let install = gtk::Button::with_label("Install");
                let (catalog, id) = (Rc::downgrade(self), model.id.clone());
                install.connect_clicked(move |_| {
                    if let Some(catalog) = catalog.upgrade() {
                        let sent = catalog.engine.borrow_mut().prewarm_model(&id);
                        catalog.status.set_label(&match sent {
                            Ok(()) => {
                                format!("Downloading {id} from its pinned Hugging Face revision…")
                            }
                            Err(error) => format!("Could not start the download: {error}"),
                        });
                    }
                });
                actions.append(&install);
            }
            if model.installed && model.format == "gguf" {
                for (label, tune) in [("Benchmark", false), ("Benchmark + tune", true)] {
                    let button = gtk::Button::with_label(label);
                    let (catalog, id) = (Rc::downgrade(self), model.id.clone());
                    button.connect_clicked(move |_| {
                        if let Some(catalog) = catalog.upgrade() {
                            let request = format!("bench-{}", glib::monotonic_time());
                            let sent = catalog
                                .engine
                                .borrow_mut()
                                .benchmark_model(&request, &id, tune);
                            catalog.status.set_label(&match sent {
                                Ok(()) => format!("Benchmarking {id}…"),
                                Err(error) => format!("Could not start the benchmark: {error}"),
                            });
                        }
                    });
                    actions.append(&button);
                }
            }
            row.append(&actions);
            self.list.append(&row);
        }
    }
}

pub fn build(engine: Rc<RefCell<EngineClient>>) -> gtk::Widget {
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .build();
    root.append(
        &gtk::Label::builder()
            .label("Model catalog")
            .xalign(0.0)
            .css_classes(["title-2"])
            .build(),
    );
    root.append(&gtk::Label::builder()
        .label("Permissively licensed local models pinned to exact Hugging Face revisions. Candidates have not been measured on this machine yet; Benchmark runs locally rendered probes (colour, counting, text) and records speed, memory and answer accuracy here only. Tuning keeps the fastest llama.cpp settings that give identical answers. Nothing downloads until you press Install.")
        .xalign(0.0).wrap(true).css_classes(["dim-label"]).build());
    let controls = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    let role = gtk::DropDown::from_strings(&ROLES.map(|(label, _)| label));
    let only_fit = gtk::CheckButton::with_label("Only models that fit this machine");
    only_fit.set_active(true);
    let refresh = gtk::Button::with_label("Refresh catalog");
    let cancel = gtk::Button::with_label("Stop benchmark");
    controls.append(&role);
    controls.append(&only_fit);
    controls.append(&refresh);
    controls.append(&cancel);
    root.append(&controls);
    let hardware = gtk::Label::builder()
        .xalign(0.0)
        .css_classes(["dim-label"])
        .build();
    root.append(&hardware);
    let status = gtk::Label::builder().xalign(0.0).wrap(true).build();
    root.append(&status);
    let list = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .build();
    root.append(&list);
    let catalog = Rc::new(Catalog {
        engine: engine.clone(),
        list,
        status,
        hardware,
        role: role.clone(),
        only_fit: only_fit.clone(),
        models: RefCell::new(Vec::new()),
        request: RefCell::new(String::new()),
    });
    {
        let catalog = catalog.clone();
        refresh.connect_clicked(move |_| catalog.refresh());
    }
    {
        let catalog = catalog.clone();
        role.connect_selected_notify(move |_| catalog.refresh());
    }
    {
        let catalog = catalog.clone();
        only_fit.connect_toggled(move |_| catalog.render());
    }
    {
        let catalog = catalog.clone();
        cancel.connect_clicked(move |_| {
            if let Err(error) = catalog.engine.borrow_mut().cancel_benchmark() {
                catalog
                    .status
                    .set_label(&format!("Engine unavailable: {error}"));
            }
        });
    }
    if engine.borrow().is_ready() {
        catalog.refresh();
    }
    let rx = engine.borrow_mut().subscribe();
    glib::MainContext::default().spawn_local(async move {
        while let Ok(event) = rx.recv().await {
            match event {
                EngineState::Ready => catalog.refresh(),
                EngineState::ModelCatalog(result) if result.request_id == *catalog.request.borrow() => {
                    catalog.hardware.set_label(&format!(
                        "This machine: {:.0} GB memory · {} MB dedicated GPU memory · {} models listed",
                        result.physical_memory_gb,
                        result.vram_mb,
                        result.models.len()
                    ));
                    *catalog.models.borrow_mut() = result.models;
                    catalog.render();
                }
                EngineState::BenchmarkProgress(progress) => catalog.status.set_label(&format!(
                    "Benchmarking {} · {} {}/{}",
                    progress.model_kind, progress.phase, progress.completed, progress.total
                )),
                EngineState::BenchmarkResult(result) => {
                    catalog.status.set_label(&match (&result.benchmark, result.message.as_deref()) {
                        (Some(benchmark), _) => format!("{}: {}", result.model_kind, benchmark_summary(benchmark)),
                        (None, Some(message)) => format!("{}: {message}", result.model_kind),
                        (None, None) => format!("{}: {}", result.model_kind, result.status),
                    });
                    catalog.refresh();
                }
                EngineState::ModelDownloadProgress(progress) if catalog.models.borrow().iter().any(|m| m.id == progress.model_kind) => {
                    catalog.status.set_label(&format!("Downloading {} · {:.0}%", progress.model_kind, progress.fraction * 100.0));
                    if progress.fraction >= 1.0 {
                        catalog.refresh();
                    }
                }
                _ => {}
            }
        }
    });
    root.upcast()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(
        id: &str,
        fits: bool,
        status: &str,
        resident: f64,
        rate: Option<f64>,
    ) -> ModelCatalogItem {
        ModelCatalogItem {
            id: id.into(),
            name: id.into(),
            family: "f".into(),
            upstream: format!("org/{id}"),
            role: "vision".into(),
            format: "gguf".into(),
            quantization: "Q4_K_M".into(),
            license: "Apache-2.0".into(),
            terms_required: false,
            parameters_billion: None,
            download_bytes: 2_000_000_000,
            estimated_resident_gb: resident,
            tier: "small".into(),
            status: status.into(),
            runnable: true,
            installed: rate.is_some(),
            fits,
            benchmark: rate.map(|r| ModelBenchmark {
                measured_at: "t".into(),
                hardware: "h".into(),
                load_seconds: 2.0,
                prompt_tokens_per_second: None,
                generated_tokens_per_second: Some(r),
                seconds_per_image: Some(1.5),
                peak_resident_gb: Some(2.4),
                json_valid_rate: Some(0.5),
                probe_accuracy: Some(0.8),
                tuned_arguments: Some(vec!["-t".into(), "4".into()]),
            }),
        }
    }

    #[test]
    fn measured_models_lead_then_curated_then_smallest() {
        let models = vec![
            model("big", true, "candidate", 9.0, None),
            model("curated", true, "curated", 7.0, None),
            model("slow", true, "verified", 3.0, Some(10.0)),
            model("fast", true, "verified", 5.0, Some(40.0)),
            model("tiny", true, "candidate", 1.0, None),
            model("huge", false, "candidate", 60.0, None),
        ];
        let ids: Vec<&str> = visible(&models, true)
            .iter()
            .map(|m| m.id.as_str())
            .collect();
        assert_eq!(ids, ["fast", "slow", "curated", "tiny", "big"]);
        assert_eq!(visible(&models, false).len(), 6);
    }

    #[test]
    fn summaries_state_license_status_and_measurements() {
        let measured = model("fast", true, "verified", 5.0, Some(40.0));
        assert!(summary(&measured).contains("Apache-2.0 · verified · installed"));
        let text = benchmark_summary(measured.benchmark.as_ref().unwrap());
        assert_eq!(text, "Measured here: loads in 2.0 s · 40 tok/s · 1.5 s/image · 80% probes · 50% valid JSON · 2.4 GB peak · tuned");
        let gemma = ModelCatalogItem {
            license: "Gemma".into(),
            terms_required: true,
            ..model("g", true, "candidate", 4.0, None)
        };
        assert!(summary(&gemma).ends_with("terms shown before install"));
    }
}
