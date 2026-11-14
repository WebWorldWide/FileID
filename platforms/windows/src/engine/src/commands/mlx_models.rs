//! Model commands in the engine the Mac app hosts, for the MLX models the
//! Swift engine runs (`crate::apple::vlm`): prewarm and benchmark. Downloads
//! of MLX models are not served here yet (Stage 9); prewarm of a model that
//! isn't installed fails without touching the network.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::apple::vlm::{self, Event, Resident};
use crate::ipc::{sink::Sink, EngineError, EventPayload, IpcEvent, ModelBenchmark, ModelDownloadProgress, Wrap};

fn display_name(kind: &str) -> String {
    vlm::model_spec(kind).ok().and_then(|spec| spec["displayName"].as_str().map(str::to_owned)).unwrap_or_else(|| kind.to_owned())
}

fn progress_event(kind: &str, fraction: f64, message: String, bytes_done: Option<u64>, total_bytes: Option<u64>) -> IpcEvent {
    IpcEvent::now(EventPayload::ModelDownloadProgress(Wrap::new(ModelDownloadProgress {
        model_kind: kind.to_owned(),
        fraction,
        message,
        bytes_done,
        total_bytes,
    })))
}

/// `prewarmModel` for an MLX model: load it and report as the Swift engine
/// does (load progress, then "<name> ready.", or `prewarm_cancelled` /
/// `prewarm_failed`).
pub(crate) async fn prewarm(sink: Sink, kind: String, cancel: Arc<AtomicBool>) {
    let display = display_name(&kind);
    if crate::models::license::policy_for(&kind).is_some() && vlm::model_spec(&kind).is_ok_and(|spec| spec["licenseAccepted"] != true) {
        sink.send(
            EngineError::new("model_license_not_accepted", format!("Accept the {display} license terms in Settings before using it."))
                .with_model(kind.clone())
                .into_event(),
        )
        .await;
        return;
    }
    if !vlm::installed(&kind) {
        sink.send(EngineError::new("prewarm_failed", format!("Prewarm {display} failed: {display} isn't installed.")).into_event()).await;
        return;
    }
    let _sleep = crate::platform::SleepGuard::acquire();
    let progress_sink = sink.clone();
    let progress_kind = kind.clone();
    let result = Resident::shared()
        .acquire(&kind, &cancel, move |event| {
            if let Event::Progress(value) = event {
                let bytes = |key: &str| value[key].as_u64().filter(|bytes| *bytes > 0);
                let _ = progress_sink.try_send(progress_event(
                    &progress_kind,
                    value["fraction"].as_f64().unwrap_or(0.0),
                    value["message"].as_str().unwrap_or_default().to_owned(),
                    bytes("bytesDone"),
                    bytes("totalBytes"),
                ));
            }
        })
        .await;
    match result {
        Ok(lease) => {
            drop(lease);
            sink.send(progress_event(&kind, 1.0, format!("{display} ready."), None, None)).await;
        }
        Err(_) if cancel.load(Ordering::Relaxed) => {
            sink.send(EngineError::new("prewarm_cancelled", format!("Prewarm {display} cancelled.")).into_event()).await;
        }
        Err(error) => {
            sink.send(EngineError::new("prewarm_failed", format!("Prewarm {display} failed: {error:#}")).into_event()).await;
        }
    }
}

/// `benchmarkModel` for an MLX model: the Swift engine's benchmark (cold
/// load, fixed prompts, test card, peak footprint), stored like the Rust one.
pub(crate) async fn benchmark(kind: &str, cancel: Arc<AtomicBool>, mut progress: impl FnMut(&str, usize, usize)) -> anyhow::Result<ModelBenchmark> {
    if !vlm::installed(kind) {
        anyhow::bail!("{} isn't downloaded. Benchmarks only run on installed models and never download.", display_name(kind));
    }
    let mut lease = Resident::shared().acquire(kind, &cancel, |_| {}).await?;
    let value = lease
        .benchmark(&cancel, |event| {
            if let Event::Progress(value) = event {
                let count = |key: &str| value[key].as_u64().unwrap_or(0) as usize;
                progress(value["phase"].as_str().unwrap_or_default(), count("completed"), count("total"));
            }
        })
        .await?;
    let mut benchmark: ModelBenchmark = serde_json::from_value(value)?;
    let hardware = crate::models::benchmark::hardware();
    benchmark.hardware.clone_from(&hardware);
    crate::models::benchmark::save_in(&crate::models::benchmark::store_path()?, kind, benchmark.clone(), &hardware)?;
    Ok(benchmark)
}
