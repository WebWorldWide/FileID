//! Deep Analyze (VLM caption + rename) IPC handlers — single file, folder
//! prefix, or whole library. Streams per-token caption chunks to the UI at
//! 4 Hz so a 50-tok/sec VLM doesn't flood the sink.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::ipc::{
    self, sink::Sink, DeepAnalyzeComplete, DeepAnalyzeFileDone, DeepAnalyzeProgress,
    DeepAnalyzeStarting, DeepAnalyzeStartingPhase, EngineError, EventPayload, IpcEvent, Wrap,
};
use crate::pipeline::deep_analyze::{analyze_file, analyze_file_via_server, AnalyzeMode};

/// Append a per-token caption chunk from `llama-mtmd-cli` with normalized
/// single-space separators. The CLI emits one stdout line per `on_token`
/// call with variable whitespace; trim + join with one space produces
/// clean English-prose output regardless of the model's whitespace habit.
pub(crate) fn append_caption_chunk(buf: &Arc<Mutex<String>>, chunk: &str) {
    let trimmed = chunk.trim();
    if trimmed.is_empty() {
        return;
    }
    let mut b = buf.lock();
    if !b.is_empty() && !b.ends_with(' ') {
        b.push(' ');
    }
    b.push_str(trimmed);
}

/// Rolling-rate ETA for the Deep Analyze batch, mirroring the scan pipeline's
/// EMA approach (scan_session.rs `maybe_emit_progress`): seconds-remaining =
/// (total - completed) / rolling_fps, or None until there's a positive rate or
/// when nothing remains. Keeps the Deep Analyze progress UI's ETA consistent
/// with the scan sidebar (F-C2-008).
fn batch_eta_seconds(rolling_fps: f64, completed: u64, total: u64) -> Option<f64> {
    let remaining = total.saturating_sub(completed);
    if rolling_fps > 0.01 && remaining > 0 {
        Some(remaining as f64 / rolling_fps)
    } else {
        None
    }
}

pub(crate) async fn handle_deep_analyze_file(
    sink: Sink,
    db: Arc<Mutex<rusqlite::Connection>>,
    payload: ipc::DeepAnalyzeFilePayload,
    cancel: Arc<AtomicBool>,
) {
    sink.send(IpcEvent::now(EventPayload::DeepAnalyzeStarting(Wrap::new(
        DeepAnalyzeStarting {
            model_kind: payload.model_kind.clone(),
            phase: DeepAnalyzeStartingPhase::LoadingModel,
            message: format!("Captioning file #{}…", payload.file_id),
        },
    ))))
    .await;

    let runner = match crate::models::vlm::VlmRunner::find() {
        Ok(r) => r,
        Err(err) => {
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "llama_cpp_missing".into(),
                message: format!("{err}"),
                path: None,
                model_kind: None,
            }))))
            .await;
            // Always send a terminal Complete so the UI clears the
            // "Loading model…" card instead of stranding forever (#6).
            sink.send(IpcEvent::now(EventPayload::DeepAnalyzeComplete(Wrap::new(
                DeepAnalyzeComplete {
                    processed: 0,
                    failed: 1,
                    total_seconds: 0.0,
                    model_kind: payload.model_kind.clone(),
                    cancelled: true,
                },
            ))))
            .await;
            return;
        }
    };

    let sink_c = sink.clone();
    let model_kind = payload.model_kind.clone();
    let model_kind_for_progress = model_kind.clone();
    let file_id = payload.file_id;
    let started_at = Instant::now();
    // Accumulate per-token text so the UI can render the live caption
    // stream word-by-word. Throttle wire emission to 4 Hz so a
    // 50-tok/sec VLM doesn't flood the sink.
    let caption_buf = Arc::new(Mutex::new(String::new()));
    let last_emit = Arc::new(Mutex::new(Instant::now() - Duration::from_millis(500)));
    let caption_buf_cb = caption_buf.clone();
    let last_emit_cb = last_emit.clone();
    let outcome = analyze_file(
        db,
        &runner,
        file_id,
        &model_kind,
        AnalyzeMode::Both,
        cancel.clone(),
        move |chunk| {
            // Intentional try_send + drop-on-overflow. Per-token streaming
            // can fire 50+/sec and the original tokio::spawn(async {
            // send.await }) pattern would pile up unbounded tasks if the
            // sink filled. Drops are fine — UI gets the next chunk a few
            // ms later.
            append_caption_chunk(&caption_buf_cb, chunk);
            let now = Instant::now();
            let should_emit = {
                let mut last = last_emit_cb.lock();
                if now.duration_since(*last) >= Duration::from_millis(250) {
                    *last = now;
                    true
                } else {
                    false
                }
            };
            if !should_emit {
                return;
            }
            let snapshot = caption_buf_cb.lock().clone();
            let kind = model_kind_for_progress.clone();
            let _ = sink_c.try_send(IpcEvent::now(EventPayload::DeepAnalyzeProgress(Wrap::new(
                DeepAnalyzeProgress {
                    processed: 0,
                    total: 1,
                    eta_seconds: None,
                    current_path: None,
                    model_kind: kind,
                    current_caption: Some(snapshot),
                },
            ))));
        },
    )
    .await;

    match outcome {
        Ok(out) => {
            sink.send(IpcEvent::now(EventPayload::DeepAnalyzeFileDone(Wrap::new(
                DeepAnalyzeFileDone {
                    file_id: out.file_id,
                    description: out.description.clone().unwrap_or_default(),
                    proposed_name: out.proposed_name.clone(),
                    model_kind: model_kind.clone(),
                },
            ))))
            .await;
            sink.send(IpcEvent::now(EventPayload::DeepAnalyzeComplete(Wrap::new(
                DeepAnalyzeComplete {
                    processed: 1,
                    failed: 0,
                    total_seconds: started_at.elapsed().as_secs_f64(),
                    model_kind,
                    cancelled: false,
                },
            ))))
            .await;
        }
        Err(err) => {
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "deep_analyze_failed".into(),
                message: format!("{err}"),
                path: None,
                model_kind: None,
            }))))
            .await;
            // Terminal Complete on the analyze failure too, mirroring the batch
            // handler's convention so the card clears / Analyze-All re-enables (#6).
            // Derive `cancelled` from the cooperative cancel flag: a genuine
            // analyze failure (decode/VLM/persist) must report cancelled:false so
            // the app's "(1 failed)" warning fires; only a real user-cancel reports
            // cancelled:true. Hard-coding true mislabeled every failure as a cancel
            // and suppressed the warning toast.
            let was_cancelled = cancel.load(Ordering::Relaxed);
            sink.send(IpcEvent::now(EventPayload::DeepAnalyzeComplete(Wrap::new(
                DeepAnalyzeComplete {
                    processed: 0,
                    failed: 1,
                    total_seconds: started_at.elapsed().as_secs_f64(),
                    model_kind,
                    cancelled: was_cancelled,
                },
            ))))
            .await;
        }
    }
}

pub(crate) async fn handle_deep_analyze_folder(
    sink: Sink,
    db: Arc<Mutex<rusqlite::Connection>>,
    payload: ipc::DeepAnalyzeFolderPayload,
    cancel: Arc<AtomicBool>,
) {
    // P16: sargable range seek on the path_text index instead of a
    // non-sargable `LIKE 'prefix%'` full-table scan.
    let lo = payload.path_prefix.clone();
    let filter = deep_analyze_target_filter();
    let ids_result = match crate::scan_session::prefix_upper_bound(&lo) {
        Some(hi) => collect_file_ids(
            &db,
            &format!("WHERE path_text >= ?1 AND path_text < ?2 AND {filter}"),
            &[&lo, &hi],
        ),
        None => collect_file_ids(&db, &format!("WHERE {filter}"), &[]),
    };
    let ids = match ids_result {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(?err, "deep_analyze_folder query");
            // Terminal Complete on the query failure so the UI clears the
            // "Preparing…" card instead of stranding forever (#6).
            sink.send(IpcEvent::now(EventPayload::DeepAnalyzeComplete(Wrap::new(
                DeepAnalyzeComplete {
                    processed: 0,
                    failed: 1,
                    total_seconds: 0.0,
                    model_kind: payload.model_kind.clone(),
                    cancelled: true,
                },
            ))))
            .await;
            return;
        }
    };
    // Folder-scoped Deep Analyze is a manual action → full enrichment (Both).
    run_deep_analyze_batch(sink, db, &payload.model_kind, ids, cancel, true, false, true).await;
}

pub(crate) async fn handle_deep_analyze_all(
    sink: Sink,
    db: Arc<Mutex<rusqlite::Connection>>,
    payload: ipc::DeepAnalyzeAllPayload,
    cancel: Arc<AtomicBool>,
) {
    let ids = match collect_file_ids(
        &db,
        &format!("WHERE {}", deep_analyze_target_filter()),
        &[],
    ) {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(?err, "deep_analyze_all query");
            // Terminal Complete on the query failure so the UI clears the
            // "Preparing…" card instead of stranding forever (#6).
            sink.send(IpcEvent::now(EventPayload::DeepAnalyzeComplete(Wrap::new(
                DeepAnalyzeComplete {
                    processed: 0,
                    failed: 1,
                    total_seconds: 0.0,
                    model_kind: payload.model_kind.clone(),
                    cancelled: true,
                },
            ))))
            .await;
            return;
        }
    };
    run_deep_analyze_batch(
        sink,
        db,
        &payload.model_kind,
        ids,
        cancel,
        payload.skip_existing,
        payload.tags_only,
        payload.propose_renames,
    )
    .await;
}

/// The `kind IN (...) AND failed = 0` predicate every Deep Analyze target query
/// shares. `'pdf'` is included only when the `pdf-analyze` render path is
/// compiled in (default-on) — without it `rasterize_for_vlm` returns a
/// feature-gate error for every PDF, so queuing them would only manufacture
/// failures (F-C1-005). `failed = 0` excludes rows a prior GPU death marked
/// failed, parity with the macOS reference (F-C1-022).
pub(crate) fn deep_analyze_target_filter() -> &'static str {
    #[cfg(feature = "pdf-analyze")]
    {
        "kind IN ('image','video','pdf') AND failed = 0"
    }
    #[cfg(not(feature = "pdf-analyze"))]
    {
        "kind IN ('image','video') AND failed = 0"
    }
}

fn collect_file_ids(
    db: &Arc<Mutex<rusqlite::Connection>>,
    where_clause: &str,
    params: &[&dyn rusqlite::ToSql],
) -> rusqlite::Result<Vec<i64>> {
    let conn = db.lock();
    let sql = format!("SELECT id FROM files {} ORDER BY id", where_clause);
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params), |r| r.get::<_, i64>(0))?;
    rows.collect()
}

async fn run_deep_analyze_batch(
    sink: Sink,
    db: Arc<Mutex<rusqlite::Connection>>,
    model_kind: &str,
    file_ids: Vec<i64>,
    cancel: Arc<AtomicBool>,
    skip_existing: bool,
    tags_only: bool,
    propose_renames: bool,
) {
    // TagsOnly = one VLM call/file (background auto-tag, ~3× faster); Both =
    // caption + tags + rename (the manual Deep Analyze pass).
    let mode = if tags_only {
        AnalyzeMode::TagsOnly
    } else if propose_renames {
        AnalyzeMode::Both
    } else {
        AnalyzeMode::CaptionAndTags
    };

    // Resolve both VLM backends up front so we can gate correctly BEFORE
    // sending DeepAnalyzeStarting. The persistent llama-server only needs
    // llama-server.exe; the per-file CLI needs llama-mtmd-cli.exe. find() is a
    // cheap (~one --version probe) check and find_weights is just file
    // existence — doing them first lets a server-capable runtime proceed even
    // when the CLI-binary check fails (the ordering trap), while still
    // surfacing a clean "runtime missing" error when NOTHING is available.
    // Weights gate FIRST: without the model's gguf/mmproj on disk, neither the
    // persistent server nor the per-file CLI can analyze anything. Surface a
    // clear, actionable error BEFORE DeepAnalyzeStarting — the client's Error
    // handler doesn't clear DeepAnalyze* state, so erroring after Starting would
    // strand the UI on a "Loading model…" banner.
    let weights = crate::models::vlm::find_weights(model_kind);
    if weights.is_none() {
        sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
            kind: "vlm_model_missing".into(),
            message: format!(
                "The {model_kind} model isn't installed yet. Install it from the Deep Analyze tab, then try again."
            ),
            path: None,
            model_kind: Some(model_kind.to_string()),
        }))))
        .await;
        return;
    }
    // The CLI binary (llama-mtmd-cli.exe) is OPTIONAL: the persistent server only
    // needs llama-server.exe. None just means "server-only"; the no-backend gate
    // below surfaces a runtime error if the server also can't start.
    let runner = crate::models::vlm::VlmRunner::find().ok();
    if runner.is_none() {
        tracing::warn!("[VLM] llama-mtmd-cli unavailable; will rely on the persistent server");
    }

    sink.send(IpcEvent::now(EventPayload::DeepAnalyzeStarting(Wrap::new(
        DeepAnalyzeStarting {
            model_kind: model_kind.to_string(),
            phase: DeepAnalyzeStartingPhase::LoadingModel,
            message: format!("Analyzing {} file(s)…", file_ids.len()),
        },
    ))))
    .await;

    // Prefer the PERSISTENT llama-server (loads the model ONCE → ~1-3 s/file).
    // The per-file CLI `runner` reloads the multi-GB model on every call, which
    // is fine for a 1-file Deep Analyze but turns a whole-library pass into many
    // hours. Fall back to the CLI when weights are missing or the server can't
    // start. The server is dropped (and killed) when this function returns —
    // including the cancel-early path below.
    let server = match weights {
        Some((gguf, mmproj)) => {
            match crate::models::vlm_server::VlmServer::start(&gguf, &mmproj).await {
                Ok(s) => {
                    // A2: verify the server accepts our multimodal payload shape
                    // BEFORE committing the whole batch to it. If it rejects the
                    // request (e.g. 400 on the image_url data-URI — a format that
                    // was never hardware-verified), fall back to the per-file CLI
                    // instead of failing every file silently.
                    match crate::pipeline::deep_analyze::vlm_server_payload_ok(&s).await {
                        Ok(()) => {
                            tracing::info!(model_kind, "[VLM-SERVER] persistent server up; payload self-test OK; using it for the batch");
                            Some(s)
                        }
                        Err(probe_err) => {
                            tracing::warn!(?probe_err, "[VLM-SERVER] payload self-test failed; falling back to per-file CLI");
                            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                                kind: "vlm_server_payload_rejected".into(),
                                message: format!(
                                    "The VLM server rejected the image request format; using the slower per-file path instead. ({probe_err:#})"
                                ),
                                path: None,
                                model_kind: None,
                            }))))
                            .await;
                            None
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!(?err, "[VLM-SERVER] unavailable; falling back to per-file CLI");
                    None
                }
            }
        }
        None => None,
    };

    let total = file_ids.len() as u64;
    let mut processed = 0u64;
    let mut failed = 0u64;
    // Use the persistent server until it errors; if it dies mid-batch and a CLI
    // runner exists, fall back to per-file CLI for the remaining files instead of
    // failing every one. (audit E5)
    let mut use_server = server.is_some();
    let started_at = Instant::now();
    // Rolling files/sec over completed files → the ETA shown on the NEXT file's
    // progress frames. EMA-smoothed (0.7 old / 0.3 new), mirroring the scan
    // pipeline (scan_session.rs). (F-C2-008)
    let mut rolling_fps = 0.0f64;

    // No runtime can run the (present) weights: the persistent server didn't
    // start AND there's no CLI binary. Surface the runtime problem ONCE here
    // instead of failing every file in the loop, then clear the UI's
    // DeepAnalyze* state (Starting was already sent above).
    if server.is_none() && runner.is_none() {
        sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
            kind: "llama_cpp_missing".into(),
            message: "The llama.cpp runtime isn't usable for image analysis (no working \
                      llama-server.exe or llama-mtmd-cli.exe). Update it from \
                      Settings -> Performance -> 'Install llama.cpp runtime'."
                .into(),
            path: None,
            model_kind: None,
        }))))
        .await;
        sink.send(IpcEvent::now(EventPayload::DeepAnalyzeComplete(Wrap::new(
            DeepAnalyzeComplete {
                processed: 0,
                failed: 0,
                total_seconds: started_at.elapsed().as_secs_f64(),
                model_kind: model_kind.to_string(),
                cancelled: true,
            },
        ))))
        .await;
        return;
    }

    for (idx, file_id) in file_ids.iter().copied().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            sink.send(IpcEvent::now(EventPayload::DeepAnalyzeComplete(Wrap::new(
                DeepAnalyzeComplete {
                    processed,
                    failed,
                    total_seconds: started_at.elapsed().as_secs_f64(),
                    model_kind: model_kind.to_string(),
                    cancelled: true,
                },
            ))))
            .await;
            return;
        }

        if skip_existing {
            let already = {
                let conn = db.lock();
                if tags_only {
                    // ENG-40: TagsOnly never writes vlm_description, but it DOES
                    // write vlm_model (persist_vlm_results sets it on every
                    // successful pass). Keying "already done" on ≥1 source='vlm'
                    // tag row re-analyzed any file whose VLM tags were all
                    // stopword/empty-filtered (zero rows persisted) on every run.
                    // Mirror the macOS reference (DeepAnalyzeRunner.swift): a file
                    // is DONE when it was analyzed BY THIS MODEL (vlm_model match)
                    // — the processed marker is written even when no tag survives
                    // filtering, while genuinely-unprocessed files (vlm_model NULL
                    // or a different model) still run.
                    conn.query_row(
                        "SELECT EXISTS(SELECT 1 FROM files WHERE id=?1 AND vlm_model=?2)",
                        rusqlite::params![file_id, model_kind],
                        |r| r.get::<_, bool>(0),
                    )
                    .unwrap_or(false)
                } else {
                    // F-C1-020: the full pass is model-aware too. "Already done"
                    // must mean "captioned BY THIS MODEL" (vlm_model match), not
                    // "captioned by anything" — otherwise switching the VLM and
                    // re-running skips every file the OLD model captioned, so the
                    // new model never runs. Require both a non-null caption AND a
                    // matching vlm_model so a model switch re-analyzes.
                    conn.query_row(
                        "SELECT EXISTS(SELECT 1 FROM files \
                         WHERE id=?1 AND vlm_model=?2 AND vlm_description IS NOT NULL)",
                        rusqlite::params![file_id, model_kind],
                        |r| r.get::<_, bool>(0),
                    )
                    .unwrap_or(false)
                }
            };
            if already {
                let is_last = idx as u64 == total.saturating_sub(1);
                if idx % 100 == 0 || is_last {
                    sink.send(IpcEvent::now(EventPayload::DeepAnalyzeProgress(Wrap::new(
                        DeepAnalyzeProgress {
                            processed: idx as u64,
                            total,
                            eta_seconds: batch_eta_seconds(rolling_fps, idx as u64, total),
                            current_path: None,
                            model_kind: model_kind.to_string(),
                            current_caption: None,
                        },
                    ))))
                    .await;
                }
                continue;
            }
        }

        let current_path: Option<String> = {
            let conn = db.lock();
            conn.query_row(
                "SELECT path_text FROM files WHERE id = ?1",
                rusqlite::params![file_id],
                |r| r.get::<_, String>(0),
            )
            .ok()
        };
        // ETA from the rate of the files completed BEFORE this one. The IPC
        // currentPath carries the real path (not redacted) for parity with the
        // macOS reference; we never log it here. (F-C2-008)
        let eta_seconds = batch_eta_seconds(rolling_fps, idx as u64, total);
        sink.send(IpcEvent::now(EventPayload::DeepAnalyzeProgress(Wrap::new(
            DeepAnalyzeProgress {
                processed: idx as u64,
                total,
                eta_seconds,
                current_path: current_path.clone(),
                model_kind: model_kind.to_string(),
                current_caption: None,
            },
        ))))
        .await;

        let sink_c = sink.clone();
        let model_kind_c = model_kind.to_string();
        // Carry ETA + the current file path onto the streamed caption frames too,
        // so the Deep Analyze UI keeps showing both while a caption renders
        // token-by-token (the macOS schema usage). (F-C2-008)
        let current_path_cb = current_path.clone();
        let caption_buf = Arc::new(Mutex::new(String::new()));
        let last_emit = Arc::new(Mutex::new(Instant::now() - Duration::from_millis(500)));
        let caption_buf_cb = caption_buf.clone();
        let last_emit_cb = last_emit.clone();
        let on_token = move |chunk: &str| {
            append_caption_chunk(&caption_buf_cb, chunk);
            let now = Instant::now();
            let should_emit = {
                let mut last = last_emit_cb.lock();
                if now.duration_since(*last) >= Duration::from_millis(250) {
                    *last = now;
                    true
                } else {
                    false
                }
            };
            if !should_emit {
                return;
            }
            let snapshot = caption_buf_cb.lock().clone();
            let kind = model_kind_c.clone();
            let _ = sink_c.try_send(IpcEvent::now(EventPayload::DeepAnalyzeProgress(
                Wrap::new(DeepAnalyzeProgress {
                    processed: idx as u64,
                    total,
                    eta_seconds,
                    current_path: current_path_cb.clone(),
                    model_kind: kind,
                    current_caption: Some(snapshot),
                }),
            )));
        };
        // Persistent server while it's healthy (model already resident); else
        // per-file CLI. `use_server` flips off below if the server dies. (audit E5)
        let server_active = if use_server { server.as_ref() } else { None };
        let file_started = Instant::now();
        let outcome = if let Some(srv) = server_active {
            analyze_file_via_server(db.clone(), srv, file_id, model_kind, mode, cancel.clone(), on_token)
                .await
        } else if let Some(r) = runner.as_ref() {
            analyze_file(db.clone(), r, file_id, model_kind, mode, cancel.clone(), on_token).await
        } else {
            // Neither backend available (server failed to start AND no CLI
            // binary). Can't analyze this file — record a failure and move on.
            Err(anyhow::anyhow!(
                "no VLM backend available — server failed to start and the CLI binary is missing"
            ))
        };

        // Fold this file's wall time into the rolling rate driving the next
        // file's ETA (EMA, mirroring scan_session.rs). (F-C2-008)
        let dt = file_started.elapsed().as_secs_f64();
        if dt > 0.0 {
            let instant = 1.0 / dt;
            rolling_fps = if rolling_fps <= 0.0 {
                instant
            } else {
                0.7 * rolling_fps + 0.3 * instant
            };
        }

        match outcome {
            Ok(out) => {
                processed += 1;
                sink.send(IpcEvent::now(EventPayload::DeepAnalyzeFileDone(Wrap::new(
                    DeepAnalyzeFileDone {
                        file_id: out.file_id,
                        description: out.description.clone().unwrap_or_default(),
                        proposed_name: out.proposed_name.clone(),
                        model_kind: model_kind.to_string(),
                    },
                ))))
                .await;
            }
            Err(err) => {
                failed += 1;
                tracing::warn!(?err, file_id, "deep analyze file failed");
                // F-C1-021: a per-file error (unreadable image, decode failure,
                // one rejected request) must NOT tear down a HEALTHY persistent
                // server and downgrade the rest of the batch to the many-times
                // slower per-file CLI. Only genuine server DEATH justifies the
                // fallback. Re-probe the server with the same one-shot payload
                // self-test used at startup; abandon it for the remaining files
                // ONLY if that probe also fails (the server is actually gone).
                if use_server && runner.is_some() {
                    let server_dead = match server.as_ref() {
                        Some(srv) => crate::pipeline::deep_analyze::vlm_server_payload_ok(srv)
                            .await
                            .is_err(),
                        None => true,
                    };
                    if server_dead {
                        tracing::warn!(
                            "[DEEP-ANALYZE] persistent server is unresponsive; falling back to per-file CLI for the rest of the batch"
                        );
                        use_server = false;
                    } else {
                        tracing::debug!(
                            file_id,
                            "[DEEP-ANALYZE] per-file error but server still healthy; keeping the persistent server"
                        );
                    }
                }
            }
        }
    }

    sink.send(IpcEvent::now(EventPayload::DeepAnalyzeComplete(Wrap::new(
        DeepAnalyzeComplete {
            processed,
            failed,
            total_seconds: started_at.elapsed().as_secs_f64(),
            model_kind: model_kind.to_string(),
            cancelled: false,
        },
    ))))
    .await;
}

#[cfg(test)]
mod tests {
    use super::append_caption_chunk;
    use parking_lot::Mutex;
    use std::sync::Arc;

    fn run_caption_chunks(chunks: &[&str]) -> String {
        let buf = Arc::new(Mutex::new(String::new()));
        for c in chunks {
            append_caption_chunk(&buf, c);
        }
        let result = buf.lock().clone();
        result
    }

    #[test]
    fn batch_eta_seconds_mirrors_scan_eta_semantics() {
        // No rate yet (first file) → None, just like the scan ramp-up.
        assert_eq!(super::batch_eta_seconds(0.0, 0, 100), None);
        // 2 files/sec, 10 of 100 done → 90 remaining → 45 s.
        assert_eq!(super::batch_eta_seconds(2.0, 10, 100), Some(45.0));
        // Nothing remaining → None (no negative/zero ETA).
        assert_eq!(super::batch_eta_seconds(2.0, 100, 100), None);
        // A vanishingly small rate is treated as "no rate" (matches the scan
        // pipeline's > 0.01 fps gate) so we don't emit an absurd ETA.
        assert_eq!(super::batch_eta_seconds(0.001, 1, 100), None);
    }

    #[test]
    fn caption_chunks_join_with_single_space() {
        let out = run_caption_chunks(&["A", "dog", "sits", "on", "a", "couch"]);
        assert_eq!(out, "A dog sits on a couch");
    }

    #[test]
    fn caption_chunks_trim_trailing_whitespace() {
        // CLI emits trailing space / padding on some lines — must not
        // produce double-spaces.
        let out = run_caption_chunks(&["A", "dog ", " sits  ", "on", "  a couch"]);
        assert_eq!(out, "A dog sits on a couch");
    }

    #[test]
    fn caption_chunks_drop_blank_lines() {
        // CLI emits blank lines between tokens occasionally — must be ignored.
        let out = run_caption_chunks(&["A", "", "dog", "   ", "sits"]);
        assert_eq!(out, "A dog sits");
    }

    #[test]
    fn caption_chunks_handle_multi_word_lines() {
        // Some prompts produce whole sentences per line — keep internal
        // spacing intact, single-space at line boundary.
        let out = run_caption_chunks(&["A dog sits", "on a couch"]);
        assert_eq!(out, "A dog sits on a couch");
    }

    fn in_memory_db() -> Arc<Mutex<rusqlite::Connection>> {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).expect("migrations apply");
        Arc::new(Mutex::new(conn))
    }

    /// Insert a minimal `files` row, returning its id. `vlm_model` /
    /// `vlm_description` are set only when provided so the skip predicate
    /// tests can model "captioned by a specific model" vs "never analyzed".
    fn insert_file(
        db: &Arc<Mutex<rusqlite::Connection>>,
        path: &str,
        kind: &str,
        failed: i64,
        vlm_model: Option<&str>,
        vlm_description: Option<&str>,
    ) -> i64 {
        let conn = db.lock();
        conn.execute(
            "INSERT INTO files \
             (path_text, path_hash, size_bytes, scanned_at, kind, extension, failed, vlm_model, vlm_description) \
             VALUES (?1, 0, 1, 0.0, ?2, '', ?3, ?4, ?5)",
            rusqlite::params![path, kind, failed, vlm_model, vlm_description],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    /// F-C1-005 + F-C1-022: the shared target filter selects renderable PDFs
    /// (when the pdf-analyze render path is compiled in) and excludes rows a
    /// prior GPU death marked failed=1 — parity with the macOS reference.
    #[test]
    fn target_filter_includes_pdfs_and_excludes_failed() {
        let db = in_memory_db();
        let img = insert_file(&db, r"C:\lib\a.jpg", "image", 0, None, None);
        let vid = insert_file(&db, r"C:\lib\b.mp4", "video", 0, None, None);
        let pdf = insert_file(&db, r"C:\lib\c.pdf", "pdf", 0, None, None);
        // failed=1 image (GPU-death-marked) must NOT be a target.
        let dead = insert_file(&db, r"C:\lib\d.jpg", "image", 1, None, None);
        // A non-renderable kind is never a Deep Analyze target.
        let _doc = insert_file(&db, r"C:\lib\e.docx", "doc", 0, None, None);

        let ids = super::collect_file_ids(
            &db,
            &format!("WHERE {}", super::deep_analyze_target_filter()),
            &[],
        )
        .unwrap();

        assert!(ids.contains(&img), "image must be a target");
        assert!(ids.contains(&vid), "video must be a target");
        #[cfg(feature = "pdf-analyze")]
        assert!(ids.contains(&pdf), "pdf must be a target when render ships");
        #[cfg(not(feature = "pdf-analyze"))]
        assert!(!ids.contains(&pdf), "pdf excluded without the render feature");
        assert!(!ids.contains(&dead), "failed=1 row must be excluded");
    }

    /// F-C1-020: the full-pass skip predicate keys on (file, vlm_model). A file
    /// captioned by an OLD model is NOT "already done" for a NEW model, so a
    /// VLM switch re-analyzes instead of skipping every prior file.
    #[test]
    fn full_pass_skip_is_model_aware() {
        let db = in_memory_db();
        let fid = insert_file(
            &db,
            r"C:\lib\a.jpg",
            "image",
            0,
            Some("gemma-3-4b"),
            Some("a dog on a couch"),
        );

        // This is the exact predicate the non-tags-only skip branch runs.
        let skip_for = |model: &str| -> bool {
            let conn = db.lock();
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM files \
                 WHERE id=?1 AND vlm_model=?2 AND vlm_description IS NOT NULL)",
                rusqlite::params![fid, model],
                |r| r.get::<_, bool>(0),
            )
            .unwrap_or(false)
        };

        // Same model that captioned it → skip (already done by this model).
        assert!(skip_for("gemma-3-4b"), "same-model row is already done");
        // Different model → NOT skipped, so the new model re-analyzes the file.
        assert!(
            !skip_for("qwen2.5-vl-7b"),
            "a model switch must re-analyze, not skip the old model's caption"
        );
    }
}
