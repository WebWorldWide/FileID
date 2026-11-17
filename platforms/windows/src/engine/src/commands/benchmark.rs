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
    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    let known = crate::models::mlx::repo(&payload.model_kind).is_some();
    #[cfg(not(all(target_os = "macos", feature = "apple-adapters")))]
    let known = true;
    if !payload.is_valid() || !known {
        return finish(&sink, &payload, "error", Some("benchmarkModel needs a valid request ID and model kind.".into()), None).await;
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let claimed = {
        let mut slot = running().lock();
        let free = slot.is_none();
        if free {
            *slot = Some(cancel.clone());
        }
        free
    };
    if !claimed {
        return finish(&sink, &payload, "error", Some("Another benchmark is already running.".into()), None).await;
    }
    let progress_sink = sink.clone();
    let (request_id, model_kind) = (payload.request_id.clone(), payload.model_kind.clone());
    let progress = move |phase: &str, completed: usize, total: usize| {
        let event = BenchmarkProgress { request_id: request_id.clone(), model_kind: model_kind.clone(), phase: phase.into(), completed: completed as u32, total: total as u32 };
        let _ = progress_sink.try_send(IpcEvent::now(EventPayload::BenchmarkProgress(Wrap::new(event))));
    };
    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    let outcome = if crate::apple::vlm::model_spec(&payload.model_kind).is_ok() {
        super::mlx_models::benchmark(&payload.model_kind, cancel.clone(), progress).await
    } else {
        crate::models::benchmark::run(&payload.model_kind, payload.tune, cancel.clone(), progress).await
    };
    #[cfg(not(all(target_os = "macos", feature = "apple-adapters")))]
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

#[cfg(all(test, not(all(target_os = "macos", feature = "apple-adapters"))))]
mod tests {
    use super::*;

    // A benchmark of a model that is not on disk refuses with the reason and
    // writes nothing under the models folder: it never starts a download.
    #[test]
    fn a_model_that_is_not_installed_is_refused_without_a_download() {
        let _env = crate::paths::TEST_ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let saved = std::env::var_os("FILEID_MODELS_DIR");
        let models = std::env::temp_dir().join(format!("fileid-bench-empty-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&models).unwrap();
        unsafe { std::env::set_var("FILEID_MODELS_DIR", &models) };
        let (sink, mut events) = Sink::channel_for_test(64);
        let payload = BenchmarkModelPayload { request_id: "bench-1".into(), model_kind: "qwen2_5_vl_7b".into(), tune: false };
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(handle(sink, payload));
        let written: Vec<_> = walkdir::WalkDir::new(&models).min_depth(1).into_iter().filter_map(Result::ok).map(|e| e.into_path()).collect();
        std::fs::remove_dir_all(&models).ok();
        match saved {
            Some(value) => unsafe { std::env::set_var("FILEID_MODELS_DIR", value) },
            None => unsafe { std::env::remove_var("FILEID_MODELS_DIR") },
        }
        let mut results = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let EventPayload::BenchmarkResult(result) = event.payload {
                results.push(result.inner);
            }
        }
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, "error");
        let message = results[0].message.clone().unwrap_or_default();
        assert!(message.contains("not installed") && message.contains("no download was started"), "{message}");
        assert!(written.is_empty(), "nothing is written under the models folder: {written:?}");
    }
}
