//! CLIP embedding query handlers — text → vector, file → vector. Used by
//! the Library tab's semantic search box and the "similar files" similarity
//! seed. Both emit `clipTextEmbedding` so the existing consumer dispatches
//! uniformly regardless of source.

use crate::ipc::{
    self, sink::Sink, ClipTextEmbedding, EngineError, EventPayload, IpcEvent, Wrap,
};

/// Pull the stored CLIP image embedding for a file_id from `clip_embeddings`
/// and emit it as a `clipTextEmbedding` event so the app's existing CLIP-
/// search consumer can use it as a similarity seed.
pub(crate) async fn handle_embed_image_query(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    payload: ipc::EmbedImageQueryPayload,
) {
    let query_id = payload.query_id.clone();

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<Vec<f32>>> {
        let conn = db.lock();
        let blob: Option<Vec<u8>> = conn
            .query_row(
                "SELECT embedding FROM clip_embeddings WHERE file_id = ?1",
                rusqlite::params![payload.file_id],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .ok();
        // A row from another model or engine (wrong width, NaN) would make a
        // meaningless search seed; treat it as missing.
        Ok(blob.and_then(|b| crate::models::clip_space::decode(&b)))
    })
    .await;

    match result {
        Ok(Ok(Some(embedding))) => {
            sink.send(IpcEvent::now(EventPayload::ClipTextEmbedding(Wrap::new(
                ClipTextEmbedding {
                    query_id,
                    query: format!("file:{}", payload.file_id),
                    embedding,
                },
            ))))
            .await;
        }
        Ok(Ok(None)) => {
            // No embedding for this file yet — resolve the awaiting query with
            // an empty embedding so "find similar" falls back cleanly instead
            // of stalling 5s on a reply that never carries this query_id (#12).
            sink.send(IpcEvent::now(EventPayload::ClipTextEmbedding(Wrap::new(
                ClipTextEmbedding {
                    query_id,
                    query: format!("file:{}", payload.file_id),
                    embedding: Vec::new(),
                },
            ))))
            .await;
        }
        Ok(Err(err)) => {
            tracing::warn!(?err, "embed_image_query failed");
            sink.send(IpcEvent::now(EventPayload::ClipTextEmbedding(Wrap::new(
                ClipTextEmbedding {
                    query_id,
                    query: format!("file:{}", payload.file_id),
                    embedding: Vec::new(),
                },
            ))))
            .await;
        }
        Err(err) => {
            // JoinError = the embed task panicked. Resolve the awaiting query
            // with an empty embedding (same shape as the Ok(Err) arm) so "find
            // similar" falls back cleanly instead of stalling 5s on a reply that
            // never carries this query_id (#12).
            tracing::warn!(?err, "embed_image_query spawn failed");
            sink.send(IpcEvent::now(EventPayload::ClipTextEmbedding(Wrap::new(
                ClipTextEmbedding {
                    query_id,
                    query: format!("file:{}", payload.file_id),
                    embedding: Vec::new(),
                },
            ))))
            .await;
        }
    }
}

/// Tokenize + embed a free-text query through CLIP-text. The model stays
/// loaded between queries (avoids the 100–300 ms create cost on every
/// keystroke) and is parked after `models::idle::PARK_AFTER` of no use.
pub(crate) async fn handle_embed_text_query(sink: Sink, payload: ipc::EmbedTextQueryPayload) {
    let query = payload.query.clone();
    let query_id = payload.query_id.clone();

    // A previously reported GPU reset: emit an empty embedding so search
    // falls back to FTS5 without a timeout or another model submission.
    if crate::coordinator::process_gpu_device_removed() {
        sink.send(IpcEvent::now(EventPayload::ClipTextEmbedding(Wrap::new(
            ClipTextEmbedding {
                query_id,
                query,
                embedding: Vec::new(),
            },
        ))))
        .await;
        return;
    }

    let result = tokio::task::spawn_blocking(move || embed_text_blocking(&payload.query)).await;

    match result {
        Ok(Ok(embedding)) => {
            sink.send(IpcEvent::now(EventPayload::ClipTextEmbedding(Wrap::new(
                ClipTextEmbedding {
                    query_id,
                    query,
                    embedding,
                },
            ))))
            .await;
        }
        Ok(Err(err)) => {
            // Resolve the awaiting query with an empty embedding (the same shape
            // the CLIP-disabled fast path uses) so the search box drops to the
            // clean FTS fallback immediately instead of stalling 5s and tripping
            // the global red error pill on every keystroke (#12). A user-facing
            // "install CLIP" nudge, if wanted, belongs on the search-box-local
            // channel, not the global engine-status error.
            tracing::warn!(?err, "CLIP text embed failed");
            if crate::models::runtime::error_has_device_removed_marker(&err) {
                sink.send(EngineError::new(
                    "gpu_device_removed",
                    crate::coordinator::GPU_DEVICE_REMOVED_MESSAGE,
                )
                .with_model("mobileclip_text")
                .into_event())
                .await;
            }
            sink.send(IpcEvent::now(EventPayload::ClipTextEmbedding(Wrap::new(
                ClipTextEmbedding {
                    query_id,
                    query,
                    embedding: Vec::new(),
                },
            ))))
            .await;
        }
        Err(err) => {
            // JoinError = the embed task panicked. Resolve with an empty
            // embedding (same shape as the Ok(Err) arm) so the search box drops
            // to the FTS fallback immediately instead of stalling 5s (#12).
            tracing::warn!(?err, "CLIP embed spawn failed");
            sink.send(IpcEvent::now(EventPayload::ClipTextEmbedding(Wrap::new(
                ClipTextEmbedding {
                    query_id,
                    query,
                    embedding: Vec::new(),
                },
            ))))
            .await;
        }
    }
}

pub(crate) static CLIP_TEXT: crate::models::idle::IdleSlot<crate::models::clip_text::ClipText> =
    crate::models::idle::IdleSlot::new("CLIP text", crate::models::idle::PARK_AFTER);

/// CLIP text embedding of `query` on the calling thread, reusing the
/// process-wide session while it is loaded. Used by search and Chat.
pub(crate) fn embed_text_blocking(query: &str) -> anyhow::Result<Vec<f32>> {
    if crate::coordinator::process_gpu_device_removed() {
        anyhow::bail!(crate::models::runtime::GPU_DEVICE_REMOVED_MARKER);
    }
    CLIP_TEXT.with(load_clip_text, |model| {
        if crate::coordinator::process_gpu_device_removed() {
            anyhow::bail!(crate::models::runtime::GPU_DEVICE_REMOVED_MARKER);
        }
        model.embed(query)
    })?
}

fn load_clip_text() -> anyhow::Result<crate::models::clip_text::ClipText> {
    let weights = crate::models::clip_text::default_weights_path()?;
    let dir = weights
        .parent()
        .ok_or_else(|| anyhow::anyhow!("text weights have no parent dir"))?;
    let vocab_path = dir.join("vocab.json");
    let merges_path = dir.join("merges.txt");
    let vocab = std::fs::read_to_string(&vocab_path).map_err(|e| {
        anyhow::anyhow!("vocab.json missing at {}: {}", vocab_path.display(), e)
    })?;
    let merges = std::fs::read_to_string(&merges_path).map_err(|e| {
        anyhow::anyhow!("merges.txt missing at {}: {}", merges_path.display(), e)
    })?;
    let tokenizer = crate::models::ClipTokenizer::new(&vocab, &merges)?;
    // EP crash-safety: this is the one GPU-EP session bind that happens
    // OUTSIDE the scan's load_default arm window — the CLIP-text encoder
    // loads lazily on the first search query (and again after it was parked).
    // Without arming here, a hard native crash while binding a freshly-installed
    // CUDA/OpenVINO pack on the search path leaves no `.ep_attempt` breadcrumb,
    // so the engine crash-loops on every search instead of reverting to
    // DirectML. Mirror scan.rs: arm the override-aware EP, bind, then disarm (a
    // Rust error still disarms below; only a hard crash leaves the breadcrumb).
    let armed_ep = crate::models::runtime::armed_provider();
    crate::models::ep_guard::arm(armed_ep.as_str());
    let loaded = crate::models::clip_text::ClipText::load(weights, tokenizer);
    crate::models::ep_guard::disarm(armed_ep.as_str());
    loaded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::idle::{Park, PARK_AFTER};

    fn resident_mb() -> f64 {
        let Ok(mut system) = sysinfo::System::new() else { return 0.0 };
        let pid = sysinfo::Pid::from_u32(std::process::id());
        system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
        system.process(pid).map_or(0.0, |process| process.memory() as f64 / 1_048_576.0)
    }

    #[test]
    #[ignore = "needs installed CLIP text weights and ONNX Runtime: FILEID_MODELS_DIR=... ORT_DYLIB_PATH=... cargo test --lib clip_text_parks -- --ignored --nocapture"]
    fn clip_text_parks_and_frees_memory() {
        let pause = std::env::var_os("FILEID_PARK_PAUSE").is_some();
        let phase = |label: &str| {
            println!("PHASE {label} pid={} rss={:.0} MB", std::process::id(), resident_mb());
            if pause {
                std::thread::sleep(std::time::Duration::from_secs(4));
            }
        };
        phase("before");
        let first = embed_text_blocking("a dog on a beach").unwrap();
        for cycle in 0..3 {
            assert!(CLIP_TEXT.is_loaded());
            phase(&format!("loaded-{cycle}"));
            assert!(!CLIP_TEXT.park_idle(std::time::Instant::now()));
            assert!(CLIP_TEXT.park_idle(std::time::Instant::now() + PARK_AFTER));
            assert!(!CLIP_TEXT.is_loaded());
            phase(&format!("parked-{cycle}"));
            assert_eq!(first, embed_text_blocking("a dog on a beach").unwrap(), "a reloaded model answers the same");
        }
    }
}
