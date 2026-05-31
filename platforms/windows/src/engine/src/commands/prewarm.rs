//! `prewarmModel` IPC handler — downloads every file in the requested model
//! bundle, emits progress events as bytes flow, drops a `.installed`
//! sentinel when every file lands. The app's ModelInstallerService polls
//! the sentinel to flip its per-model status to `Installed`.
//!
//! In-flight dedupe prevents repeated "Install all" clicks from racing on
//! the .part file (first call's rename(.part → final) running while a
//! second call still streams bytes into the same .part).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;

use crate::downloader::{download_parallel, DownloadProgress, DownloadRequest};
use crate::ipc::{
    sink::Sink, EngineError, EventPayload, IpcEvent, ModelDownloadProgress, Wrap,
};
use crate::models::registry::{self, LookupResult, ModelFile};
use crate::platform;
use crate::util;

/// RAII guard that removes a model_kind from the in-flight set when the
/// prewarm function returns or unwinds.
struct ReleaseOnDrop {
    kind: String,
    set: &'static Mutex<HashSet<String>>,
}

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.set.lock().remove(&self.kind);
    }
}

pub(crate) async fn handle_prewarm_model(
    sink: Sink,
    model_kind: String,
    http_client: Arc<reqwest::Client>,
    cancel: Arc<AtomicBool>,
) {
    // Emit an immediate "Queued" progress event so the welcome sheet row
    // flips out of the captionless-spinner state the moment the engine
    // sees the prewarm command.
    sink.send(IpcEvent::now(EventPayload::ModelDownloadProgress(Wrap::new(
        ModelDownloadProgress {
            model_kind: model_kind.clone(),
            fraction: 0.0,
            message: "Queued — starting download…".to_string(),
            bytes_done: None,
            total_bytes: None,
        },
    ))))
    .await;

    tracing::info!(model_kind = %model_kind, "[PREWARM] entered handler");

    // Per-model-kind in-flight dedupe.
    static IN_FLIGHT: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let in_flight = IN_FLIGHT.get_or_init(|| Mutex::new(HashSet::new()));
    // Check + insert under a short-lived guard. parking_lot's MutexGuard is
    // !Send, so we MUST drop it before the first .await below.
    let already_in_flight = {
        let mut guard = in_flight.lock();
        if guard.contains(&model_kind) {
            true
        } else {
            guard.insert(model_kind.clone());
            false
        }
    };
    if already_in_flight {
        tracing::info!(model = %model_kind, "prewarm already in flight; ignoring duplicate request");
        sink.send(IpcEvent::now(EventPayload::ModelDownloadProgress(Wrap::new(
            ModelDownloadProgress {
                model_kind: model_kind.clone(),
                fraction: 0.0,
                message: "Already downloading...".to_string(),
                bytes_done: None,
                total_bytes: None,
            },
        ))))
        .await;
        tracing::info!(model_kind = %model_kind, outcome = "duplicate_in_flight", "[PREWARM] exiting");
        return;
    }
    // RAII guard removes us from the set on every exit path.
    let _release = ReleaseOnDrop {
        kind: model_kind.clone(),
        set: in_flight,
    };

    // (Re)installing a GPU EP pack is an explicit "try this EP again" — clear
    // any prior crash-disable (ep_guard) so the next launch re-attempts the bind.
    if matches!(model_kind.as_str(), "ort_cuda_x64" | "ort_openvino_x64") {
        crate::models::ep_guard::reenable();
    }

    // Distinguish "the engine can't resolve its models dir" (a storage/env
    // misconfig that makes `lookup_full` return Unknown for EVERY kind) from a
    // genuinely unregistered kind — otherwise the unknown-kind message below
    // would mislead the user into thinking the model itself is the problem.
    if let Err(err) = crate::paths::models_dir() {
        sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
            kind: "models_dir_unavailable".into(),
            message: format!(
                "FileID couldn't resolve its models folder, so '{model_kind}' \
                 can't be installed: {err}. Check that the %LOCALAPPDATA% (or \
                 %USERPROFILE%) environment variable is set."
            ),
            path: None,
            model_kind: Some(model_kind.clone()),
        }))))
        .await;
        tracing::warn!(model_kind = %model_kind, outcome = "models_dir_unavailable", "[PREWARM] exiting");
        return;
    }

    let model = match registry::lookup_full(&model_kind) {
        LookupResult::Found(m) => m,
        LookupResult::Unknown => {
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "unknown_model".into(),
                message: format!(
                    "This FileID engine doesn't recognize the model '{model_kind}'. \
                     The engine is most likely older than the app — reinstall or \
                     rebuild FileID so the app and engine match, then try again."
                ),
                path: None,
                model_kind: Some(model_kind.clone()),
            }))))
            .await;
            tracing::warn!(model_kind = %model_kind, outcome = "unknown_model", "[PREWARM] exiting");
            return;
        }
    };

    tracing::info!(model = %model.id, files = model.files.len(), "starting prewarm");

    if let Some(sentinel) = registry::sentinel_path(&model) {
        if sentinel.exists() {
            sink.send(IpcEvent::now(EventPayload::ModelDownloadProgress(Wrap::new(
                ModelDownloadProgress {
                    model_kind: model_kind.clone(),
                    fraction: 1.0,
                    message: format!("{} already installed", model.display_name),
                    bytes_done: None,
                    total_bytes: None,
                },
            ))))
            .await;
            tracing::info!(model_kind = %model_kind, outcome = "already_installed", "[PREWARM] exiting");
            return;
        }
    }

    let total_bytes_estimate: u64 = model.files.iter().map(|f| f.approx_bytes).sum();
    let total_estimate = total_bytes_estimate.max(1);
    let file_count = model.files.len();

    // Per-file bytes_done counters. All file progress callbacks update their
    // own slot so the aggregate fraction stays correct when multiple files
    // run concurrently.
    let per_file_done: Arc<Vec<AtomicU64>> =
        Arc::new((0..file_count).map(|_| AtomicU64::new(0)).collect());

    // Partition into regular vs zip files. Zip files run sequentially
    // because the post-download extract step mutates DLL search paths and
    // is order-sensitive (Performance Packs).
    let mut regular_indices: Vec<usize> = Vec::with_capacity(file_count);
    let mut zip_indices: Vec<usize> = Vec::with_capacity(file_count);
    for (idx, file) in model.files.iter().enumerate() {
        if file.dest.extension().and_then(|s| s.to_str()) == Some("zip") {
            zip_indices.push(idx);
        } else {
            regular_indices.push(idx);
        }
    }

    let make_progress_cb = |idx: usize, file: &ModelFile, label: String| {
        let model_kind_local = model_kind.clone();
        let display_name = model.display_name.to_string();
        let sink_for_progress = sink.clone();
        let per_file_done = per_file_done.clone();
        let file_bytes_mb = file.approx_bytes / (1024 * 1024);
        let file_label_for_msg = label;
        move |p: DownloadProgress| {
            per_file_done[idx].store(p.bytes_done, Ordering::Relaxed);
            let cur_total: u64 = per_file_done
                .iter()
                .map(|c| c.load(Ordering::Relaxed))
                .sum();
            let fraction = (cur_total as f64) / (total_estimate as f64);
            let msg = if file_count == 1 {
                format!("Downloading {display_name}…")
            } else {
                format!(
                    "Downloading {display_name} — {file_label_for_msg} ({of} of {total}, ~{mb} MB)",
                    of = idx + 1,
                    total = file_count,
                    mb = file_bytes_mb,
                )
            };
            let event = IpcEvent::now(EventPayload::ModelDownloadProgress(Wrap::new(
                ModelDownloadProgress {
                    model_kind: model_kind_local.clone(),
                    fraction: fraction.min(0.999),
                    message: msg,
                    bytes_done: Some(cur_total),
                    total_bytes: Some(total_estimate),
                },
            )));
            let s = sink_for_progress.clone();
            tokio::spawn(async move {
                s.send(event).await;
            });
        }
    };

    // Run regular (non-zip) files in parallel. The downloader's global HTTP
    // semaphore caps total concurrent requests so this doesn't worsen
    // cross-model rate-limit pressure.
    let mut regular_handles: Vec<
        tokio::task::JoinHandle<Result<(), (String, PathBuf, anyhow::Error)>>,
    > = Vec::with_capacity(regular_indices.len());
    for idx in regular_indices.iter().copied() {
        let file = model.files[idx].clone();
        let label = file
            .dest
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();
        let file_bytes_mb = file.approx_bytes / (1024 * 1024);
        tracing::info!(
            model_kind = %model_kind,
            file_idx = idx,
            file_total = file_count,
            file = %label,
            file_mb = file_bytes_mb,
            "[PREWARM] starting file (parallel)"
        );
        let http_client = http_client.clone();
        let cancel = cancel.clone();
        let progress_cb = make_progress_cb(idx, &file, label.clone());
        let req = DownloadRequest {
            url: file.url.clone(),
            destination: file.dest.clone(),
            expected_sha256: file.sha256.clone(),
        };
        let label_for_err = label.clone();
        let dest_for_err = file.dest.clone();
        regular_handles.push(tokio::spawn(async move {
            download_parallel(http_client, req, cancel, progress_cb)
                .await
                .map_err(|e| (label_for_err, dest_for_err, e))
        }));
    }

    let mut first_err: Option<(String, PathBuf, anyhow::Error)> = None;
    for h in regular_handles {
        match h.await {
            Ok(Ok(())) => {}
            Ok(Err(triple)) => {
                if first_err.is_none() {
                    first_err = Some(triple);
                }
            }
            Err(join_err) => {
                if first_err.is_none() {
                    first_err = Some(("(task)".into(), PathBuf::new(), anyhow::anyhow!(join_err)));
                }
            }
        }
    }
    if let Some((label, dest, err)) = first_err {
        tracing::warn!(?err, file = %label, "model download failed");
        sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
            kind: "model_download_failed".into(),
            message: format!(
                "Couldn't download {label}: {err}\n\n\
                 Large model downloads can take several minutes — \
                 check your connection and click Retry. Downloads \
                 resume from where they stopped, so no progress is lost."
            ),
            path: Some(dest.display().to_string()),
            model_kind: Some(model_kind.clone()),
        }))))
        .await;
        tracing::warn!(model_kind = %model_kind, outcome = "download_failed", file = %label, "[PREWARM] exiting");
        return;
    }
    // Lock in the final byte counts for regular files so zip progress
    // computes against the correct baseline.
    for idx in regular_indices.iter().copied() {
        per_file_done[idx].store(model.files[idx].approx_bytes, Ordering::Relaxed);
    }

    // Now run zip files sequentially, with post-download extract.
    for idx in zip_indices.iter().copied() {
        let file = &model.files[idx];
        let label = file
            .dest
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();
        let file_bytes_mb = file.approx_bytes / (1024 * 1024);
        tracing::info!(
            model_kind = %model_kind,
            file_idx = idx,
            file_total = file_count,
            file = %label,
            file_mb = file_bytes_mb,
            "[PREWARM] starting file (zip, sequential)"
        );
        let progress_cb = make_progress_cb(idx, file, label.clone());
        let req = DownloadRequest {
            url: file.url.clone(),
            destination: file.dest.clone(),
            expected_sha256: file.sha256.clone(),
        };
        if let Err(err) =
            download_parallel(http_client.clone(), req, cancel.clone(), progress_cb).await
        {
            tracing::warn!(?err, file = %label, "model download failed");
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "model_download_failed".into(),
                message: format!(
                    "Couldn't download {label}: {err}\n\n\
                     Large model downloads can take several minutes — \
                     check your connection and click Retry. Downloads \
                     resume from where they stopped, so no progress is lost."
                ),
                path: Some(file.dest.display().to_string()),
                model_kind: Some(model_kind.clone()),
            }))))
            .await;
            tracing::warn!(model_kind = %model_kind, outcome = "download_failed", file = %label, "[PREWARM] exiting");
            return;
        }
        let dest = file.dest.clone();
        let extract =
            tokio::task::spawn_blocking(move || util::zip::extract_into_parent(&dest)).await;
        if let Err(err) = extract.unwrap_or(Err(anyhow::anyhow!("zip extract panicked"))) {
            tracing::warn!(?err, "zip extract failed");
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "zip_extract_failed".into(),
                message: format!("Couldn't extract {label}: {err}"),
                path: Some(file.dest.display().to_string()),
                model_kind: Some(model_kind.clone()),
            }))))
            .await;
            tracing::warn!(model_kind = %model_kind, outcome = "zip_extract_failed", file = %label, "[PREWARM] exiting");
            return;
        }
        let _ = std::fs::remove_file(&file.dest);
        if let Some(parent) = file.dest.parent() {
            platform::register_dll_dirs_under(parent);
        }
        per_file_done[idx].store(file.approx_bytes, Ordering::Relaxed);
    }

    // Atomic write so a kill mid-write can never leave a half-written
    // sentinel that subsequent runs treat as "installed."
    if let Some(sentinel) = registry::sentinel_path(&model) {
        if let Some(parent) = sentinel.parent() {
            if let Err(err) = tokio::fs::create_dir_all(parent).await {
                tracing::error!(?err, dir = %parent.display(), "could not create .sentinels dir");
                sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                    kind: "sentinel_dir_create_failed".into(),
                    message: format!(
                        "Couldn't create the sentinel directory at {}. Model {} is downloaded but not registered as installed; try again.",
                        parent.display(),
                        model.display_name
                    ),
                    path: Some(parent.display().to_string()),
                    model_kind: Some(model_kind.clone()),
                }))))
                .await;
                return;
            }
        }
        let tmp = sentinel.with_extension("installed.tmp");
        if let Err(err) = tokio::fs::write(&tmp, model.id.as_bytes()).await {
            tracing::error!(?err, tmp = %tmp.display(), "sentinel tmp write failed");
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "sentinel_write_failed".into(),
                message: format!("Couldn't write the install marker for {}: {err}", model.display_name),
                path: Some(tmp.display().to_string()),
                model_kind: Some(model_kind.clone()),
            }))))
            .await;
            return;
        }
        if let Err(err) = tokio::fs::rename(&tmp, &sentinel).await {
            tracing::error!(?err, from = %tmp.display(), to = %sentinel.display(), "sentinel rename failed");
            let _ = tokio::fs::remove_file(&tmp).await;
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "sentinel_rename_failed".into(),
                message: format!(
                    "Couldn't finalize the install marker for {}: {err}",
                    model.display_name
                ),
                path: Some(sentinel.display().to_string()),
                model_kind: Some(model_kind.clone()),
            }))))
            .await;
            return;
        }
    }

    sink.send(IpcEvent::now(EventPayload::ModelDownloadProgress(Wrap::new(
        ModelDownloadProgress {
            model_kind: model_kind.clone(),
            fraction: 1.0,
            message: format!("{} installed", model.display_name),
            bytes_done: Some(total_bytes_estimate),
            total_bytes: Some(total_bytes_estimate),
        },
    ))))
    .await;
    tracing::info!(model_kind = %model_kind, outcome = "installed", "[PREWARM] exiting");
}
