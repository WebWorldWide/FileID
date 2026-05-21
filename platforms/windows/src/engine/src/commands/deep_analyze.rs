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
        }
    }
}

pub(crate) async fn handle_deep_analyze_folder(
    sink: Sink,
    db: Arc<Mutex<rusqlite::Connection>>,
    payload: ipc::DeepAnalyzeFolderPayload,
    cancel: Arc<AtomicBool>,
) {
    let prefix = format!("{}%", payload.path_prefix);
    let ids = match collect_file_ids(
        &db,
        "WHERE path_text LIKE ?1 AND kind IN ('image','video')",
        &[&prefix],
    ) {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(?err, "deep_analyze_folder query");
            return;
        }
    };
    // Folder-scoped Deep Analyze is a manual action → full enrichment (Both).
    run_deep_analyze_batch(sink, db, &payload.model_kind, ids, cancel, true, false).await;
}

pub(crate) async fn handle_deep_analyze_all(
    sink: Sink,
    db: Arc<Mutex<rusqlite::Connection>>,
    payload: ipc::DeepAnalyzeAllPayload,
    cancel: Arc<AtomicBool>,
) {
    let ids = match collect_file_ids(&db, "WHERE kind IN ('image','video')", &[]) {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(?err, "deep_analyze_all query");
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
    )
    .await;
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
) {
    // TagsOnly = one VLM call/file (background auto-tag, ~3× faster); Both =
    // caption + tags + rename (the manual Deep Analyze pass).
    let mode = if tags_only {
        AnalyzeMode::TagsOnly
    } else {
        AnalyzeMode::Both
    };

    // Resolve both VLM backends up front so we can gate correctly BEFORE
    // sending DeepAnalyzeStarting. The persistent llama-server only needs
    // llama-server.exe; the per-file CLI needs llama-mtmd-cli.exe. find() is a
    // cheap (~one --version probe) check and find_weights is just file
    // existence — doing them first lets a server-capable runtime proceed even
    // when the CLI-binary check fails (the ordering trap), while still
    // surfacing a clean "runtime missing" error when NOTHING is available.
    let weights = crate::models::vlm::find_weights(model_kind);
    let runner = match crate::models::vlm::VlmRunner::find() {
        Ok(r) => Some(r),
        Err(err) => {
            // No usable CLI binary. Fine IF the persistent server can run
            // (weights present → it only needs llama-server.exe). But with no
            // weights either, nothing can analyze: surface the error and return
            // BEFORE Starting, because the client's Error handler does NOT clear
            // DeepAnalyze* state — sending Starting first would strand the UI on
            // a "Loading model…" banner forever.
            if weights.is_none() {
                sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                    kind: "llama_cpp_missing".into(),
                    message: format!("{err}"),
                    path: None,
                    model_kind: None,
                }))))
                .await;
                return;
            }
            tracing::warn!(?err, "[VLM] CLI binary unavailable; relying on the persistent server");
            None
        }
    };

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
                    tracing::info!(model_kind, "[VLM-SERVER] persistent server up; using it for the batch");
                    Some(s)
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
    let started_at = Instant::now();

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
                    // TagsOnly never writes vlm_description, so "already done"
                    // means the file already has ≥1 source='vlm' tag. Checking
                    // vlm_description here (as the Both path does) would never
                    // match → the auto-tag pass would re-tag the whole library
                    // on every scan instead of resuming on untagged files only.
                    conn.query_row(
                        "SELECT EXISTS(SELECT 1 FROM tags WHERE file_id=?1 AND source='vlm')",
                        rusqlite::params![file_id],
                        |r| r.get::<_, bool>(0),
                    )
                    .unwrap_or(false)
                } else {
                    conn.query_row(
                        "SELECT vlm_description FROM files WHERE id = ?1",
                        rusqlite::params![file_id],
                        |r| r.get::<_, Option<String>>(0),
                    )
                    .unwrap_or(None)
                    .is_some()
                }
            };
            if already {
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
        sink.send(IpcEvent::now(EventPayload::DeepAnalyzeProgress(Wrap::new(
            DeepAnalyzeProgress {
                processed: idx as u64,
                total,
                eta_seconds: None,
                current_path,
                model_kind: model_kind.to_string(),
                current_caption: None,
            },
        ))))
        .await;

        let sink_c = sink.clone();
        let model_kind_c = model_kind.to_string();
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
                    eta_seconds: None,
                    current_path: None,
                    model_kind: kind,
                    current_caption: Some(snapshot),
                }),
            )));
        };
        // Persistent server when up (model already resident); else per-file CLI.
        let outcome = if let Some(srv) = server.as_ref() {
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
}
