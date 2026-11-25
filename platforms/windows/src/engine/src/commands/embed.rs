//! CLIP embedding queries (text → vector, file → vector) for the Library semantic search box and "similar files"; both
//! emit `clipTextEmbedding`.

use crate::ipc::{
    self, sink::Sink, ClipTextEmbedding, EngineError, EventPayload, IpcEvent, Wrap,
};

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
        // A row from another model or engine (wrong width, NaN) would be a meaningless seed: treat it as missing.
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
            // Resolve with an empty embedding so "find similar" falls back cleanly instead of stalling 5 s on a reply
            // that never carries this query_id.
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
            // The embed task panicked: resolve with an empty embedding (as the Ok(Err) arm) instead of stalling 5 s.
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

/// The model stays loaded between queries (avoids a 100–300 ms create per keystroke) and parks after
/// `models::idle::PARK_AFTER`.
pub(crate) async fn handle_embed_text_query(sink: Sink, payload: ipc::EmbedTextQueryPayload) {
    let query = payload.query.clone();
    let query_id = payload.query_id.clone();

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

    let prompt = (crate::models::search_strength::applies() && !query.trim().is_empty()).then(|| crate::models::search_strength::prompt(query.trim()));
    let result = tokio::task::spawn_blocking(move || crate::util::qos::interactive(|| embed_text_blocking(&payload.query))).await;

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
            if let Some(prompt) = prompt {
                let _ = tokio::task::spawn_blocking(move || crate::util::qos::interactive(|| embed_text_blocking(&prompt))).await;
            }
        }
        Ok(Err(err)) => {
            // Resolve with an empty embedding so search drops to the FTS fallback instead of stalling 5 s and tripping
            // the global error pill on every keystroke; any "install CLIP" nudge belongs on a search-box-local channel.
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
            // The embed task panicked: resolve with an empty embedding instead of stalling 5 s.
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

/// The most recent text vectors, so a repeated query, and the judged search that follows an embed, do not run the text
/// model again. A failed encode is never kept.
pub(crate) struct TextVectors {
    entries: std::collections::VecDeque<(String, Vec<f32>)>,
}

impl TextVectors {
    const CAPACITY: usize = 64;

    pub(crate) const fn new() -> Self {
        Self { entries: std::collections::VecDeque::new() }
    }

    pub(crate) fn get_or_encode(&mut self, text: &str, encode: impl FnOnce() -> anyhow::Result<Vec<f32>>) -> anyhow::Result<Vec<f32>> {
        if let Some(position) = self.entries.iter().position(|(key, _)| key == text) {
            let hit = self.entries.remove(position).expect("position came from this queue");
            let vector = hit.1.clone();
            self.entries.push_back(hit);
            return Ok(vector);
        }
        let vector = encode()?;
        if self.entries.len() == Self::CAPACITY {
            self.entries.pop_front();
        }
        self.entries.push_back((text.to_owned(), vector.clone()));
        Ok(vector)
    }
}

static TEXT_VECTORS: parking_lot::Mutex<TextVectors> = parking_lot::Mutex::new(TextVectors::new());

pub(crate) fn embed_text_blocking(query: &str) -> anyhow::Result<Vec<f32>> {
    if crate::coordinator::process_gpu_device_removed() {
        anyhow::bail!(crate::models::runtime::GPU_DEVICE_REMOVED_MARKER);
    }
    TEXT_VECTORS.lock().get_or_encode(query, || {
        CLIP_TEXT.with(load_clip_text, |model| {
            if crate::coordinator::process_gpu_device_removed() {
                anyhow::bail!(crate::models::runtime::GPU_DEVICE_REMOVED_MARKER);
            }
            model.embed(query)
        })?
    })
}

/// A catalog with picture vectors can be searched by meaning, and the text model is installed, on a machine with room to
/// hold it before anyone searches.
fn clip_text_prewarm_wanted(conn: &rusqlite::Connection, installed: bool, tier: crate::platform::MemoryTier) -> bool {
    installed
        && tier != crate::platform::MemoryTier::Low
        && conn.query_row("SELECT EXISTS(SELECT 1 FROM clip_embeddings)", [], |row| row.get(0)).unwrap_or(false)
}

/// Loads the CLIP text model so the first search after launch or a scan does not wait for it (479 ms quiet, over a second
/// on a busy Mac); it parks again after `PARK_AFTER` of no searches. Returns whether the model is loaded.
pub(crate) fn prewarm_clip_text(db: &parking_lot::Mutex<rusqlite::Connection>) -> bool {
    let installed = crate::models::clip_text::default_weights_path().is_ok_and(|path| path.is_file());
    if !clip_text_prewarm_wanted(&db.lock(), installed, crate::platform::memory_tier()) {
        return false;
    }
    match CLIP_TEXT.with(load_clip_text, |_| ()) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(?error, "CLIP text prewarm failed");
            false
        }
    }
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
    crate::models::clip_text::ClipText::load(weights, tokenizer)
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
    fn the_text_model_is_prewarmed_only_for_a_searchable_catalog_with_room_and_weights() {
        use crate::platform::MemoryTier::{Balanced, High, Low};
        let conn = crate::db::test_support::memory_db();
        assert!(!clip_text_prewarm_wanted(&conn, true, Balanced), "no picture vectors yet");
        conn.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension) VALUES(1,'/a.jpg','h',1,1,0,'image','jpg')", []).unwrap();
        conn.execute("INSERT INTO clip_embeddings(file_id,embedding,model) VALUES(1,X'00','m')", []).unwrap();
        assert!(clip_text_prewarm_wanted(&conn, true, Balanced));
        assert!(clip_text_prewarm_wanted(&conn, true, High));
        assert!(!clip_text_prewarm_wanted(&conn, true, Low), "a small Mac keeps its memory");
        assert!(!clip_text_prewarm_wanted(&conn, false, Balanced), "no weights installed");
    }

    #[test]
    fn a_repeated_text_is_encoded_once_and_a_failed_encode_is_not_kept() {
        let mut vectors = TextVectors::new();
        let mut calls = 0;
        let mut ask = |vectors: &mut TextVectors, text: &str| {
            vectors.get_or_encode(text, || {
                calls += 1;
                Ok(vec![text.len() as f32])
            })
        };
        assert_eq!(ask(&mut vectors, "a dog").unwrap(), vec![5.0]);
        assert_eq!(ask(&mut vectors, "a dog").unwrap(), vec![5.0]);
        assert_eq!(calls, 1, "the second ask came from the kept vector");
        assert!(vectors.get_or_encode("broken", || anyhow::bail!("no model")).is_err());
        assert_eq!(vectors.get_or_encode("broken", || Ok(vec![1.0])).unwrap(), vec![1.0], "the failure was not kept");
    }

    #[test]
    fn the_oldest_text_vector_is_dropped_first_and_a_hit_renews_its_place() {
        let mut vectors = TextVectors::new();
        let mut encoded = Vec::new();
        for index in 0..TextVectors::CAPACITY {
            vectors.get_or_encode(&format!("query {index}"), || Ok(vec![index as f32])).unwrap();
        }
        vectors.get_or_encode("query 0", || panic!("kept")).unwrap();
        vectors.get_or_encode("one more", || Ok(vec![-1.0])).unwrap();
        for text in ["query 0", "one more"] {
            vectors.get_or_encode(text, || panic!("{text} kept")).unwrap();
        }
        vectors.get_or_encode("query 1", || { encoded.push("query 1"); Ok(vec![1.0]) }).unwrap();
        assert_eq!(encoded, ["query 1"], "the least recently used entry was the one dropped");
    }

    #[test]
    fn prewarm_without_weights_loads_nothing() {
        let conn = parking_lot::Mutex::new(crate::db::test_support::memory_db());
        assert!(!prewarm_clip_text(&conn));
    }

    #[test]
    #[ignore = "needs installed CLIP text weights: FILEID_MODELS_DIR=... cargo test --lib clip_text_parks -- --ignored --nocapture"]
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
