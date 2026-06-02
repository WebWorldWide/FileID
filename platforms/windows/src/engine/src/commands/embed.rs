//! CLIP embedding query handlers — text → vector, file → vector. Used by
//! the Library tab's semantic search box and the "similar files" similarity
//! seed. Both emit `clipTextEmbedding` so the existing consumer dispatches
//! uniformly regardless of source.

use crate::ipc::{
    self, sink::Sink, ClipTextEmbedding, EventPayload, IpcEvent, Wrap,
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

    // CLIP is disabled (scene_vocab::ENABLE_CLIP) — emit an empty embedding so
    // the app's "find similar" path falls back cleanly instead of awaiting a
    // reply that never comes. No DB read, no model load.
    if !crate::models::scene_vocab::ENABLE_CLIP {
        sink.send(IpcEvent::now(EventPayload::ClipTextEmbedding(Wrap::new(
            ClipTextEmbedding {
                query_id,
                query: format!("file:{}", payload.file_id),
                embedding: Vec::new(),
            },
        ))))
        .await;
        return;
    }

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<Vec<f32>>> {
        let conn = db.lock();
        let blob: Option<Vec<u8>> = conn
            .query_row(
                "SELECT embedding FROM clip_embeddings WHERE file_id = ?1",
                rusqlite::params![payload.file_id],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .ok();
        Ok(blob.and_then(|b| {
            if b.is_empty() || b.len() % 4 != 0 {
                return None;
            }
            Some(
                b.chunks_exact(4)
                    .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                    .collect(),
            )
        }))
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

/// Tokenize + embed a free-text query through CLIP-text. The model is
/// process-static (lives in a `OnceLock<Mutex<Option<ClipText>>>`) so
/// back-to-back queries reuse the loaded ORT session — avoids the
/// 100–300 ms create cost on every keystroke.
pub(crate) async fn handle_embed_text_query(sink: Sink, payload: ipc::EmbedTextQueryPayload) {
    let query = payload.query.clone();
    let query_id = payload.query_id.clone();

    // CLIP is disabled (scene_vocab::ENABLE_CLIP) — emit an empty embedding so
    // the search box falls back to FTS5 (over VLM tags + filenames + OCR)
    // without a 5 s timeout. No model load.
    if !crate::models::scene_vocab::ENABLE_CLIP {
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

    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<f32>> {
        use std::sync::OnceLock;
        static TEXT_MODEL: OnceLock<
            parking_lot::Mutex<Option<crate::models::clip_text::ClipText>>,
        > = OnceLock::new();
        let cell = TEXT_MODEL.get_or_init(|| parking_lot::Mutex::new(None));
        let mut guard = cell.lock();
        if guard.is_none() {
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
            let model = crate::models::clip_text::ClipText::load(weights, tokenizer)?;
            *guard = Some(model);
        }
        let model = guard.as_mut().expect("just set");
        model.embed(&payload.query)
    })
    .await;

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
