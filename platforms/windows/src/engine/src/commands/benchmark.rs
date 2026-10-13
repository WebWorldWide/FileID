//! `benchmarkModel` / `cancelBenchmark`: run the on-device benchmark for one
//! installed model and report progress and the result over IPC.

use crate::ipc::sink::Sink;
use crate::ipc::{BenchmarkModelPayload, BenchmarkProgress, BenchmarkResult, EventPayload, IpcEvent, Wrap};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

fn running() -> &'static Mutex<Option<Arc<AtomicBool>>> {
    static RUNNING: OnceLock<Mutex<Option<Arc<AtomicBool>>>> = OnceLock::new();
    RUNNING.get_or_init(|| Mutex::new(None))
}

async fn finish(sink: &Sink, payload: &BenchmarkModelPayload, status: &str, message: Option<String>, benchmark: Option<crate::ipc::ModelBenchmark>) {
    let result = BenchmarkResult { request_id: payload.request_id.clone(), model_kind: payload.model_kind.clone(), status: status.into(), message, benchmark };
    sink.send(IpcEvent::now(EventPayload::BenchmarkResult(Wrap::new(result)))).await;
}

pub async fn handle(sink: Sink, payload: BenchmarkModelPayload) {
    if !payload.is_valid() {
        return finish(&sink, &payload, "error", Some("benchmarkModel needs a valid request ID and model kind.".into()), None).await;
    }
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut slot = running().lock();
        if slot.is_some() {
            drop(slot);
            return finish(&sink, &payload, "error", Some("Another benchmark is already running.".into()), None).await;
        }
        *slot = Some(cancel.clone());
    }
    let progress_sink = sink.clone();
    let (request_id, model_kind) = (payload.request_id.clone(), payload.model_kind.clone());
    let progress = move |phase: &str, completed: usize, total: usize| {
        let event = BenchmarkProgress { request_id: request_id.clone(), model_kind: model_kind.clone(), phase: phase.into(), completed: completed as u32, total: total as u32 };
        let _ = progress_sink.try_send(IpcEvent::now(EventPayload::BenchmarkProgress(Wrap::new(event))));
    };
    let outcome = crate::models::benchmark::run(&payload.model_kind, payload.tune, cancel.clone(), progress).await;
    *running().lock() = None;
    match outcome {
        Ok(benchmark) => finish(&sink, &payload, "ok", None, Some(benchmark)).await,
        Err(_) if cancel.load(Ordering::Relaxed) => finish(&sink, &payload, "cancelled", Some("Benchmark cancelled; no result was stored.".into()), None).await,
        Err(error) => finish(&sink, &payload, "error", Some(format!("{error:#}").chars().take(2_000).collect()), None).await,
    }
}

pub fn cancel() {
    if let Some(flag) = running().lock().as_ref() {
        flag.store(true, Ordering::Relaxed);
    }
}
