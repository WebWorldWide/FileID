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

/// Per-model-kind cancel flags. A prewarm download polls ITS kind's flag, so
/// cancelling one model (the per-row Cancel button) no longer aborts every other
/// concurrent download, and a fresh prewarm of one kind can't un-cancel a
/// different kind's pending cancel — both were bugs of the old single
/// process-global flag.
static PREWARM_CANCELS: OnceLock<Mutex<std::collections::HashMap<String, Arc<AtomicBool>>>> =
    OnceLock::new();

/// Get-or-create this kind's cancel flag WITHOUT changing its value. The
/// fresh-start reset is [`reset_prewarm_cancel`], called synchronously at
/// dispatch time, so this getter can't clobber a cancel that raced in after the
/// handler task was spawned.
fn prewarm_cancel_flag(model_kind: &str) -> Arc<AtomicBool> {
    let map = PREWARM_CANCELS.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    map.lock()
        .entry(model_kind.to_string())
        .or_insert_with(|| Arc::new(AtomicBool::new(false)))
        .clone()
}

/// Reset (creating if needed) this kind's cancel flag to un-cancelled. Called
/// from the PrewarmModel dispatch arm BEFORE the handler task is spawned, so the
/// flag always exists (and is false) before any subsequent CancelPrewarm for
/// this kind is processed by the serial stdio loop — closing the lazy-create
/// race where a cancel landing in the spawn→register window was silently
/// dropped. Resets ONLY this kind; a different kind's pending cancel is untouched.
pub fn reset_prewarm_cancel(model_kind: &str) {
    let map = PREWARM_CANCELS.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    map.lock()
        .entry(model_kind.to_string())
        .or_insert_with(|| Arc::new(AtomicBool::new(false)))
        .store(false, Ordering::Relaxed);
}

/// Signal cancellation for one model kind (create-or-set, so a cancel is never
/// lost even if processed before the handler first reads the flag), or ALL known
/// prewarm flags when `model_kind` is None (the "cancel everything" form).
/// In-flight `download_parallel` calls poll their flag after every chunk and bail.
pub fn cancel_prewarm(model_kind: Option<&str>) {
    let map = PREWARM_CANCELS.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    let mut guard = map.lock();
    match model_kind {
        Some(kind) => {
            guard
                .entry(kind.to_string())
                .or_insert_with(|| Arc::new(AtomicBool::new(false)))
                .store(true, Ordering::Relaxed);
            tracing::info!(model_kind = kind, "CancelPrewarm received (per-model)");
        }
        None => {
            for flag in guard.values() {
                flag.store(true, Ordering::Relaxed);
            }
            tracing::info!("CancelPrewarm received (all in-flight)");
        }
    }
}

pub(crate) async fn handle_prewarm_model(
    sink: Sink,
    model_kind: String,
    http_client: Arc<reqwest::Client>,
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

    // Per-model cancel flag (reset to un-cancelled for this fresh prewarm).
    // download_parallel below polls it after every chunk.
    let cancel = prewarm_cancel_flag(&model_kind);

    // (Re)installing a GPU EP pack is an explicit "try this EP again" — clear
    // any prior crash-disable (ep_guard) so the next launch re-attempts the bind.
    match model_kind.as_str() {
        "ort_cuda_x64" => crate::models::ep_guard::reenable_ep("cuda"),
        "ort_openvino_x64" => crate::models::ep_guard::reenable_ep("openvino"),
        _ => {}
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
            // Send inline (drop-on-full) rather than via a spawned task. Spawning
            // a task per callback gave no ordering guarantee, so a later,
            // higher-fraction event could reach the channel before an earlier,
            // lower one and bounce the progress bar backward. try_send preserves
            // emission order; progress is throttled upstream so drops are benign.
            let _ = sink_for_progress.try_send(event);
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
            expected_bytes: Some(file.approx_bytes),
        };
        let label_for_err = label.clone();
        let dest_for_err = file.dest.clone();
        regular_handles.push(tokio::spawn(async move {
            download_parallel(http_client, req, cancel, progress_cb)
                .await
                .map_err(|e| (label_for_err, dest_for_err, e))
        }));
    }

    // Collect EVERY failed file, not just the first — a multi-file or
    // multi-pack failure must not be masked by reporting only one. Each is
    // logged on its own, then surfaced as a single combined error.
    let mut errs: Vec<(String, PathBuf, anyhow::Error)> = Vec::new();
    for h in regular_handles {
        match h.await {
            Ok(Ok(())) => {}
            Ok(Err(triple)) => errs.push(triple),
            Err(join_err) => {
                errs.push(("(task)".into(), PathBuf::new(), anyhow::anyhow!(join_err)));
            }
        }
    }
    if !errs.is_empty() {
        // A user-initiated cancel is not a network failure: surface a benign
        // event the app suppresses (ModelInstallerService treats
        // "prewarm_cancelled" as a no-op) instead of a red "download failed —
        // check your connection / Retry" pill.
        if cancel.load(Ordering::Relaxed) {
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "prewarm_cancelled".into(),
                message: format!("Download of {model_kind} cancelled."),
                path: None,
                model_kind: Some(model_kind.clone()),
            }))))
            .await;
            tracing::info!(model_kind = %model_kind, outcome = "cancelled", "[PREWARM] exiting");
            return;
        }
        for (label, _dest, err) in &errs {
            tracing::warn!(?err, file = %label, "model download failed");
        }
        let detail = errs
            .iter()
            .map(|(label, _dest, err)| format!("{label}: {err}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (first_label, first_dest, _) = &errs[0];
        let summary = if errs.len() == 1 {
            format!("Couldn't download {first_label}")
        } else {
            format!("Couldn't download {} files", errs.len())
        };
        sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
            kind: "model_download_failed".into(),
            message: format!(
                "{summary}:\n{detail}\n\n\
                 Large model downloads can take several minutes — \
                 check your connection and click Retry. Downloads \
                 resume from where they stopped, so no progress is lost."
            ),
            path: Some(first_dest.display().to_string()),
            model_kind: Some(model_kind.clone()),
        }))))
        .await;
        tracing::warn!(model_kind = %model_kind, outcome = "download_failed", failed = errs.len(), "[PREWARM] exiting");
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
            expected_bytes: Some(file.approx_bytes),
        };
        if let Err(err) =
            download_parallel(http_client.clone(), req, cancel.clone(), progress_cb).await
        {
            if cancel.load(Ordering::Relaxed) {
                sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                    kind: "prewarm_cancelled".into(),
                    message: format!("Download of {model_kind} cancelled."),
                    path: None,
                    model_kind: Some(model_kind.clone()),
                }))))
                .await;
                tracing::info!(model_kind = %model_kind, outcome = "cancelled", "[PREWARM] exiting");
                return;
            }
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
            // Returns the dirs whose AddDllDirectory call failed; an empty Vec is
            // full success. Log failures (path-redacted) so a pack whose GPU DLLs
            // won't be on the search path at load is diagnosable.
            let failed = platform::register_dll_dirs_under(parent);
            for dir in &failed {
                tracing::warn!(
                    dir = %platform::redact_path_for_log(dir),
                    "[PREWARM] AddDllDirectory failed for pack dir; its DLLs may not load"
                );
            }
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
            // A failed write can leave a partial .tmp behind; remove it (mirroring
            // the rename path below) so a half-written marker isn't left on disk.
            let _ = tokio::fs::remove_file(&tmp).await;
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "sentinel_write_failed".into(),
                message: format!(
                    "Couldn't write the install marker for {}: {err}. The model is \
                     downloaded but not registered as installed; try again.",
                    model.display_name
                ),
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
                    "Couldn't finalize the install marker for {}: {err}. The model is \
                     downloaded but not registered as installed; try again.",
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

#[cfg(test)]
mod tests {
    use super::*;

    // Per-model cancel registry: cancelling one kind must not touch another, and
    // a fresh prewarm of a kind resets only its own flag. Unique kind names keep
    // the process-global static from cross-contaminating parallel tests.
    #[test]
    fn per_model_cancel_is_isolated() {
        let a = prewarm_cancel_flag("test_kind_alpha");
        let b = prewarm_cancel_flag("test_kind_beta");
        cancel_prewarm(Some("test_kind_alpha"));
        assert!(a.load(Ordering::Relaxed), "the cancelled kind's flag must be set");
        assert!(!b.load(Ordering::Relaxed), "a different kind's flag must NOT be set");
        // reset_prewarm_cancel (the dispatch-time fresh start) clears only its kind.
        reset_prewarm_cancel("test_kind_alpha");
        assert!(!a.load(Ordering::Relaxed), "reset clears its own kind's flag");
        assert!(!b.load(Ordering::Relaxed), "and still leaves the other kind untouched");
        // A cancel that arrives BEFORE the flag was ever fetched must still record
        // (create-or-set) — this is the lazy-create-race fix.
        cancel_prewarm(Some("test_kind_gamma"));
        assert!(
            prewarm_cancel_flag("test_kind_gamma").load(Ordering::Relaxed),
            "a cancel before first fetch must still be recorded"
        );
    }
}
