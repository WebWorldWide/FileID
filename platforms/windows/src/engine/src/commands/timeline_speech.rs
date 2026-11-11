//! Speech pass with an on-device recogniser that needs no model install
//! (Apple Speech in the Mac app's engine). Rows match the Swift engine's
//! `TimelineAnalysis.transcribeSpeech` byte for byte: one coverage row per
//! file and per overlapping chunk, so a job resumed by either engine skips the
//! chunks the other verified, and an unreadable chunk stays incomplete.

use super::{
    blocking, interrupted_or, publish, running_source, set_progress, timeline_media, upsert_coverage, while_running, with_db,
    Interrupted, Job,
};
use crate::ipc::sink::Sink;
use crate::pipeline::speech_passages::{self, Chunk, Passage, Segment};
use anyhow::Result;
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub(super) trait Recognizer: Send + Sync + 'static {
    /// Whether recognition can run now; may show the system permission prompt.
    fn available(&self) -> bool;
    fn model_version(&self) -> String;
    /// Words of `[start, end)`, timestamps relative to `start`.
    fn transcribe(&self, source: &Path, start: f64, end: f64, cancel: &AtomicBool) -> Result<Vec<Segment>>;
}

/// Apple Speech runs in the Mac app's engine unless FILEID_TIMELINE_SPEECH=0,
/// the Swift engine's switch.
pub(super) fn enabled() -> bool {
    cfg!(all(target_os = "macos", feature = "apple-adapters")) && std::env::var("FILEID_TIMELINE_SPEECH").as_deref() != Ok("0")
}

#[cfg(all(target_os = "macos", feature = "apple-adapters"))]
struct AppleSpeech;

#[cfg(all(target_os = "macos", feature = "apple-adapters"))]
impl Recognizer for AppleSpeech {
    fn available(&self) -> bool {
        crate::apple::media::speech_status(true) == crate::apple::media::SpeechStatus::Ready
    }
    fn model_version(&self) -> String {
        crate::apple::media::speech_model_version()
    }
    fn transcribe(&self, source: &Path, start: f64, end: f64, cancel: &AtomicBool) -> Result<Vec<Segment>> {
        crate::apple::media::transcribe(source, start, end, cancel)
    }
}

pub(super) async fn transcribe(
    id: &str,
    job: &Job,
    cancel: &Arc<AtomicBool>,
    sink: &Sink,
    database: &Arc<Mutex<Connection>>,
    visual_share: f64,
) -> Result<()> {
    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    return transcribe_with(Arc::new(AppleSpeech), timeline_media::probe, id, job, cancel, sink, database, visual_share).await;
    #[cfg(not(all(target_os = "macos", feature = "apple-adapters")))]
    {
        let _ = (id, job, cancel, sink, database, visual_share);
        Ok(())
    }
}

/// What the speech pass needs to know about a video: the decoder's probe.
pub(super) type Probe = fn(&Path, &AtomicBool) -> Result<super::video_export::AnalysisProbe>;

#[allow(clippy::too_many_arguments)]
pub(super) async fn transcribe_with<R: Recognizer>(
    recognizer: Arc<R>,
    probe: Probe,
    id: &str,
    job: &Job,
    cancel: &Arc<AtomicBool>,
    sink: &Sink,
    database: &Arc<Mutex<Connection>>,
    visual_share: f64,
) -> Result<()> {
    let total = job.ids.len().max(1);
    for (index, &file_id) in job.ids.iter().enumerate() {
        let source = running_source(database, id, file_id).await?;
        let probe = {
            let (path, flag) = (source.path.clone(), cancel.clone());
            blocking(move || probe(&path, &flag)).await.map_err(|error| interrupted_or(cancel, error))?
        };
        let chunks = speech_passages::chunks(probe.duration);
        if !probe.has_audio || chunks.is_empty() {
            continue;
        }
        let version = {
            let recognizer = recognizer.clone();
            blocking(move || Ok(recognizer.model_version())).await?
        };
        let coverage = format!("speech-coverage:{file_id}:{}:{version}", source.revision);
        {
            let (pending, checked, model_version, duration) = (coverage.clone(), source.clone(), version.clone(), probe.duration);
            with_db(database, move |conn| {
                checked.verify(conn, file_id)?;
                conn.execute(
                    "INSERT OR IGNORE INTO catalog_coverage(id,file_id,start_seconds,end_seconds,status,source_revision,model_version) VALUES(?1,?2,0,?3,'incomplete',?4,?5)",
                    params![pending, file_id, duration, checked.revision, model_version],
                )?;
                Ok(())
            })
            .await?;
        }
        let available = {
            let recognizer = recognizer.clone();
            blocking(move || Ok(recognizer.available())).await?
        };
        if !available {
            continue;
        }
        let mut complete = true;
        for (chunk_index, chunk) in chunks.iter().copied().enumerate() {
            let chunk_id = format!("{coverage}:chunk:{chunk_index}");
            let (lookup, revision, model_version) = (chunk_id.clone(), source.revision.clone(), version.clone());
            let verified: bool = while_running(database, id, move |conn| {
                Ok(conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM catalog_coverage WHERE id=?1 AND status='verified' AND source_revision=?2 AND model_version=?3)",
                    params![lookup, revision, model_version],
                    |r| r.get(0),
                )?)
            })
            .await?;
            if !verified {
                let (recognizer, path, flag) = (recognizer.clone(), source.path.clone(), cancel.clone());
                let heard = blocking(move || recognizer.transcribe(&path, chunk.extract_start, chunk.extract_end, &flag)).await;
                if cancel.load(Ordering::Acquire) {
                    return Err(Interrupted.into());
                }
                let passages = heard.ok().map(|segments| speech_passages::passages(&segments, &chunk, probe.duration));
                complete &= passages.is_some();
                let (checked, model_version, chunk_id) = (source.clone(), version.clone(), chunk_id.clone());
                while_running(database, id, move |conn| {
                    checked.verify(conn, file_id)?;
                    persist_chunk(conn, file_id, &checked.revision, &model_version, &chunk_id, &chunk, passages.as_deref())
                })
                .await?;
            }
            let progress = visual_share
                + (1.0 - visual_share) * (index as f64 + (chunk_index + 1) as f64 / chunks.len() as f64) / total as f64;
            let job_id = id.to_owned();
            with_db(database, move |conn| set_progress(conn, &job_id, progress)).await?;
            publish(id, sink, database).await;
        }
        let checked = source.clone();
        with_db(database, move |conn| {
            checked.verify(conn, file_id)?;
            conn.execute(
                "UPDATE catalog_coverage SET status=?1 WHERE id=?2",
                params![if complete { "verified" } else { "incomplete" }, coverage],
            )?;
            Ok(())
        })
        .await?;
    }
    Ok(())
}

/// The Swift engine's `persistSpeechChunk`.
fn persist_chunk(
    conn: &Connection,
    file_id: i64,
    revision: &str,
    version: &str,
    chunk_id: &str,
    chunk: &Chunk,
    passages: Option<&[Passage]>,
) -> Result<()> {
    let status = if passages.is_some() { "verified" } else { "incomplete" };
    upsert_coverage(conn, chunk_id, file_id, chunk.extract_start, chunk.extract_end, status, revision, version)?;
    for (index, passage) in passages.unwrap_or_default().iter().enumerate() {
        let start = (passage.start_seconds * 1000.0).round() as i64;
        conn.execute(
            "INSERT OR REPLACE INTO catalog_passages(id,file_id,start_seconds,end_seconds,text,source_revision,model_version,confidence) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                format!("speech:{file_id}:{revision}:{version}:{start}:{index}"),
                file_id,
                passage.start_seconds,
                passage.end_seconds,
                passage.text,
                revision,
                version,
                passage.confidence
            ],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{enqueue, start, CatalogRequest};
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// Scripted words per chunk; `None` makes that chunk unreadable.
    struct Scripted {
        available: bool,
        chunks: Mutex<Vec<Option<Vec<Segment>>>>,
        calls: AtomicUsize,
        pause: Option<(usize, Arc<Mutex<Connection>>, String)>,
    }

    impl Recognizer for Scripted {
        fn available(&self) -> bool {
            self.available
        }
        fn model_version(&self) -> String {
            "apple-speech-ondevice-v1/en_US/macOS-26.0.0".into()
        }
        fn transcribe(&self, _: &Path, start: f64, _: f64, _: &AtomicBool) -> Result<Vec<Segment>> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some((at, database, id)) = &self.pause
                && call == *at
            {
                database.lock().execute("UPDATE catalog_jobs SET state='paused' WHERE id=?1", [id])?;
            }
            let index = speech_passages::chunks(100.0).iter().position(|c| c.extract_start == start).unwrap();
            self.chunks.lock()[index].clone().ok_or_else(|| anyhow::anyhow!("no speech"))
        }
    }

    fn probe_100s(_: &Path, _: &AtomicBool) -> Result<super::super::video_export::AnalysisProbe> {
        Ok(super::super::video_export::AnalysisProbe { duration: 100.0, width: 64, height: 36, has_audio: true })
    }

    fn words(text: &str, at: f64) -> Vec<Segment> {
        text.split(' ')
            .enumerate()
            .map(|(i, word)| Segment { text: word.into(), timestamp: at + i as f64 * 0.4, duration: 0.3, confidence: 0.8 })
            .collect()
    }

    fn fixture() -> (std::path::PathBuf, Arc<Mutex<Connection>>, String) {
        let directory = std::env::temp_dir().join(format!("fileid-speech-resume-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("talk.mov");
        std::fs::write(&path, b"not decoded: the probe is scripted").unwrap();
        let metadata = std::fs::metadata(&path).unwrap();
        let modified = metadata.modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64();
        let mut conn = crate::db::test_support::memory_db();
        conn.execute(
            "INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension) VALUES(1,?1,1,?2,?3,0,'video','mov')",
            params![path.to_str().unwrap(), metadata.len() as i64, modified],
        )
        .unwrap();
        let request: CatalogRequest = serde_json::from_value(serde_json::json!({"requestID":"r","action":"enqueueTimeline","fileIDs":[1]})).unwrap();
        let (id, _) = enqueue(&mut conn, &request, Some("qwen3_vl_4b"), true).unwrap();
        (directory, Arc::new(Mutex::new(conn)), id)
    }

    fn rows(database: &Arc<Mutex<Connection>>, sql: &str) -> Vec<(String, String)> {
        let conn = database.lock();
        let mut statement = conn.prepare(sql).unwrap();
        statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap()
    }

    #[tokio::test]
    async fn chunks_resume_after_pause_and_retry_only_unreadable_ones() {
        let (directory, database, id) = fixture();
        let job = start(&database.lock(), &id).unwrap().unwrap();
        let revision: String = super::super::revision(&database.lock(), 1).unwrap();
        let (sink, _events) = Sink::channel_for_test(1_024);
        let cancel = Arc::new(AtomicBool::new(false));
        let version = "apple-speech-ondevice-v1/en_US/macOS-26.0.0";
        let script = vec![Some(words("Happy birthday Sam", 1.0)), None, Some(words("Make a wish", 2.0))];
        let recognizer = Arc::new(Scripted {
            available: true,
            chunks: Mutex::new(script),
            calls: 0.into(),
            pause: Some((1, database.clone(), id.clone())),
        });

        let paused = transcribe_with(recognizer.clone(), probe_100s, &id, &job, &cancel, &sink, &database, 0.8).await;
        assert!(paused.unwrap_err().is::<Interrupted>(), "a pause during a chunk stops the pass");
        assert_eq!(recognizer.calls.load(Ordering::SeqCst), 2);
        let chunks = rows(&database, "SELECT id,status FROM catalog_coverage WHERE id LIKE 'speech-coverage:%:chunk:%' ORDER BY id");
        assert_eq!(chunks, [(format!("speech-coverage:1:{revision}:{version}:chunk:0"), "verified".to_owned())]);
        let passage = rows(&database, "SELECT id,text FROM catalog_passages WHERE file_id=1");
        assert_eq!(passage, [(format!("speech:1:{revision}:{version}:1000:0"), "Happy birthday Sam".to_owned())]);

        database.lock().execute("UPDATE catalog_jobs SET state='running' WHERE id=?1", [&id]).unwrap();
        let resumed = Arc::new(Scripted { available: true, chunks: Mutex::new(recognizer.chunks.lock().clone()), calls: 0.into(), pause: None });
        transcribe_with(resumed.clone(), probe_100s, &id, &job, &cancel, &sink, &database, 0.8).await.unwrap();
        assert_eq!(resumed.calls.load(Ordering::SeqCst), 2, "the verified first chunk is skipped");
        let statuses = rows(&database, "SELECT id,status FROM catalog_coverage WHERE id LIKE 'speech-coverage:%' ORDER BY id");
        assert_eq!(statuses.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>(), ["incomplete", "verified", "incomplete", "verified"]);
        let extract: (f64, f64) = database.lock().query_row(
            "SELECT start_seconds,end_seconds FROM catalog_coverage WHERE id LIKE '%:chunk:1'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(extract, (43.5, 91.5), "chunk coverage spans the overlapping extract, as in the Swift engine");

        resumed.chunks.lock()[1] = Some(words("Blow out the candles", 0.5));
        transcribe_with(resumed.clone(), probe_100s, &id, &job, &cancel, &sink, &database, 0.8).await.unwrap();
        assert_eq!(resumed.calls.load(Ordering::SeqCst), 3, "only the unreadable chunk is retried");
        let file: String = database.lock().query_row(
            "SELECT status FROM catalog_coverage WHERE id=?1", [format!("speech-coverage:1:{revision}:{version}")], |r| r.get(0)).unwrap();
        assert_eq!(file, "verified");
        let texts: Vec<String> = rows(&database, "SELECT id,text FROM catalog_passages WHERE file_id=1 ORDER BY start_seconds").into_iter().map(|r| r.1).collect();
        assert_eq!(texts, ["Happy birthday Sam", "Blow out the candles", "Make a wish"]);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn unavailable_recognition_leaves_the_file_incomplete_without_chunks() {
        let (directory, database, id) = fixture();
        let job = start(&database.lock(), &id).unwrap().unwrap();
        let (sink, _events) = Sink::channel_for_test(1_024);
        let recognizer = Arc::new(Scripted { available: false, chunks: Mutex::new(vec![None; 3]), calls: 0.into(), pause: None });
        transcribe_with(recognizer.clone(), probe_100s, &id, &job, &Arc::new(AtomicBool::new(false)), &sink, &database, 0.8).await.unwrap();
        assert_eq!(recognizer.calls.load(Ordering::SeqCst), 0);
        let coverage = rows(&database, "SELECT id,status FROM catalog_coverage WHERE id LIKE 'speech-coverage:%'");
        assert_eq!(coverage.len(), 1);
        assert_eq!(coverage[0].1, "incomplete");
        std::fs::remove_dir_all(directory).unwrap();
    }
}
