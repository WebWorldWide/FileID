use super::*;
use crate::ipc::sink::Sink;

fn database() -> Connection {
    let conn = crate::db::test_support::memory_db();
    crate::db::test_support::insert_file(&conn, 1, "/internal/party.mov", "video");
    crate::db::test_support::insert_file(&conn, 2, "/internal/cake.jpg", "image");
    conn
}

fn request(value: serde_json::Value) -> CatalogRequest {
    serde_json::from_value(value).unwrap()
}

#[test]
fn enqueue_records_portable_recipe_and_rejects_unusable_requests() {
    let mut conn = database();
    let ok = request(
        serde_json::json!({"requestID":"r","action":"enqueueTimeline","fileIDs":[1,1],"timelineMode":"moments"}),
    );
    let (id, response) = enqueue(&mut conn, &ok, Some("qwen3_vl_4b"), true).unwrap();
    assert_eq!(response.status, "ok");
    assert!(response
        .message
        .unwrap()
        .contains("Install a speech model from the model catalog"));
    assert_eq!(response.jobs.len(), 1);
    let (kind, ids, recipe, state): (String, String, String, String) = conn
        .query_row(
            "SELECT kind,file_ids_json,recipe_json,state FROM catalog_jobs WHERE id=?1",
            [&id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        (kind.as_str(), ids.as_str(), state.as_str()),
        (JOB_KIND, "[1]", "queued")
    );
    let recipe: Recipe = serde_json::from_str(&recipe).unwrap();
    assert_eq!(recipe.model_kind, "qwen3_vl_4b");
    assert_eq!(recipe.timeline_mode.as_deref(), Some("moments"));

    let photo = request(serde_json::json!({"requestID":"r","action":"enqueueTimeline","fileIDs":[2]}));
    assert!(enqueue(&mut conn, &photo, Some("qwen3_vl_4b"), true).is_err());
    let empty = request(serde_json::json!({"requestID":"r","action":"enqueueTimeline","fileIDs":[]}));
    assert!(enqueue(&mut conn, &empty, Some("qwen3_vl_4b"), true).is_err());
    let mode =
        request(serde_json::json!({"requestID":"r","action":"enqueueTimeline","fileIDs":[1],"timelineMode":"dense"}));
    assert!(enqueue(&mut conn, &mode, Some("qwen3_vl_4b"), true).is_err());
    let no_model = enqueue(&mut conn, &ok, None, true).unwrap_err().to_string();
    assert!(no_model.contains("No model will be downloaded"));
    let no_ffmpeg = enqueue(&mut conn, &ok, Some("qwen3_vl_4b"), false)
        .unwrap_err()
        .to_string();
    assert!(no_ffmpeg.contains("FFmpeg"));
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM catalog_jobs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn recovery_pauses_interrupted_timeline_jobs_only() {
    let conn = database();
    for (id, kind, state) in [
        ("a", JOB_KIND, "running"),
        ("b", JOB_KIND, "queued"),
        ("c", JOB_KIND, "completed"),
        ("d", "catalogIndex", "running"),
    ] {
        conn.execute("INSERT INTO catalog_jobs(id,kind,file_ids_json,recipe_json,state,created_at,updated_at) VALUES(?1,?2,'[1]','{}',?3,0,0)", params![id, kind, state]).unwrap();
    }
    recover(&conn).unwrap();
    let states: Vec<String> = ["a", "b", "c", "d"]
        .iter()
        .map(|id| {
            conn.query_row("SELECT state FROM catalog_jobs WHERE id=?1", [id], |r| r.get(0))
                .unwrap()
        })
        .collect();
    assert_eq!(states, ["paused", "paused", "completed", "running"]);
    assert!(is_timeline_job(&conn, "a").unwrap());
    assert!(!is_timeline_job(&conn, "d").unwrap());
    assert!(!is_timeline_job(&conn, "missing").unwrap());
}

#[test]
fn start_claims_only_queued_jobs_and_resumes_from_checkpoint() {
    let mut conn = database();
    let (id, _) = enqueue(
        &mut conn,
        &request(serde_json::json!({"requestID":"r","action":"enqueueTimeline","fileIDs":[1]})),
        Some("qwen3_vl_4b"),
        true,
    )
    .unwrap();
    conn.execute("UPDATE catalog_jobs SET checkpoint_json='1' WHERE id=?1", [&id])
        .unwrap();
    let job = start(&conn, &id).unwrap().unwrap();
    assert_eq!((job.ids.as_slice(), job.index), (&[1][..], 1));
    assert!(require_running(&conn, &id).is_ok());
    assert!(start(&conn, &id).unwrap().is_none());
    conn.execute("UPDATE catalog_jobs SET state='paused' WHERE id=?1", [&id])
        .unwrap();
    assert!(require_running(&conn, &id).unwrap_err().is::<Interrupted>());
}

#[test]
fn resume_is_allowed_for_timeline_jobs_through_catalog_actions() {
    let mut conn = database();
    let (id, _) = enqueue(
        &mut conn,
        &request(serde_json::json!({"requestID":"r","action":"enqueueTimeline","fileIDs":[1]})),
        Some("qwen3_vl_4b"),
        true,
    )
    .unwrap();
    let pause = request(serde_json::json!({"requestID":"p","action":"pauseJob","jobID":id}));
    assert_eq!(
        super::super::catalog::execute(&mut conn, &pause).unwrap().jobs[0].state,
        "paused"
    );
    let resume = request(serde_json::json!({"requestID":"p","action":"resumeJob","jobID":id}));
    assert_eq!(
        super::super::catalog::execute(&mut conn, &resume).unwrap().jobs[0].state,
        "queued"
    );
    conn.execute("INSERT INTO catalog_jobs(id,kind,file_ids_json,recipe_json,state,created_at,updated_at) VALUES('other','unknownKind','[]','{}','paused',0,0)", []).unwrap();
    let foreign = request(serde_json::json!({"requestID":"p","action":"resumeJob","jobID":"other"}));
    assert!(super::super::catalog::execute(&mut conn, &foreign).is_err());
}

#[test]
fn chapter_suggestions_replace_drafts_but_keep_user_edits() {
    let conn = database();
    let source = Source {
        path: PathBuf::from("/nonexistent"),
        size: 100,
        modified: Some(10.0),
        revision: revision(&conn, 1).unwrap(),
    };
    let texts = [
        "Birthday cake with candles on table",
        "Birthday cake candles burning on table",
        "Children playing soccer in park",
        "Children kicking soccer ball in park",
    ];
    for (index, text) in texts.iter().enumerate() {
        conn.execute(
            "INSERT INTO catalog_passages(id,file_id,start_seconds,end_seconds,text,source_revision,model_version,confidence) VALUES(?1,1,?2,?2,?3,?4,'frame-v',0)",
            params![format!("p{index}"), index as f64 * 10.0, text, source.revision],
        )
        .unwrap();
    }
    let stale = CatalogChapter {
        id: "old-draft".into(),
        file_id: 1,
        start_seconds: 0.0,
        end_seconds: 1.0,
        title: "Old".into(),
        summary: String::new(),
        source_revision: source.revision.clone(),
        model_version: format!("{}frame-v", analysis::CHAPTER_SUGGESTION_PREFIX),
        confidence: 0.2,
        user_edited: false,
        stale: false,
    };
    upsert_chapter(&conn, &stale).unwrap();
    let edited_id = format!("timeline-suggestion:1:20000:{}", source.revision);
    let edited = CatalogChapter {
        id: edited_id.clone(),
        title: "Soccer (renamed)".into(),
        user_edited: true,
        model_version: "user".into(),
        ..stale.clone()
    };
    upsert_chapter(&conn, &edited).unwrap();

    // The source file is absent, so persistence must refuse rather than write.
    assert!(persist_chapter_suggestions(&conn, 1, &source, "frame-v", 40.0).is_err());

    let titles = |conn: &Connection| -> Vec<String> {
        let mut statement = conn
            .prepare("SELECT title FROM catalog_chapters WHERE file_id=1 ORDER BY start_seconds,id")
            .unwrap();
        statement
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    assert_eq!(titles(&conn), ["Old", "Soccer (renamed)"]);
}

#[test]
fn chapter_suggestions_persist_when_source_is_unchanged() {
    let directory = std::env::temp_dir().join(format!("fileid-timeline-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("party.mov");
    std::fs::write(&path, vec![0u8; 100]).unwrap();
    let modified = std::fs::metadata(&path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let conn = database();
    conn.execute(
        "UPDATE files SET path_text=?1,modified_at=?2 WHERE id=1",
        params![path.to_str().unwrap(), modified],
    )
    .unwrap();
    let source = Source::load(&conn, 1).unwrap();
    source.verify(&conn, 1).unwrap();
    let texts = [
        "Birthday cake with candles on table",
        "Birthday cake candles burning on table",
        "Children playing soccer in park",
        "Children kicking soccer ball in park",
    ];
    for (index, text) in texts.iter().enumerate() {
        conn.execute(
            "INSERT INTO catalog_passages(id,file_id,start_seconds,end_seconds,text,source_revision,model_version,confidence) VALUES(?1,1,?2,?2,?3,?4,'frame-v',0)",
            params![format!("p{index}"), index as f64 * 10.0, text, source.revision],
        )
        .unwrap();
    }
    persist_chapter_suggestions(&conn, 1, &source, "frame-v", 40.0).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM catalog_chapters WHERE file_id=1 AND user_edited=0",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);
    let hits: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM catalog_evidence_fts WHERE catalog_evidence_fts MATCH 'soccer' AND kind='chapter'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(hits, 1);
    std::fs::write(&path, vec![1u8; 101]).unwrap();
    assert!(source.verify(&conn, 1).is_err());
    std::fs::remove_dir_all(directory).unwrap();
}

/// Runs only where the vetted FFmpeg runtime is staged (FILEID_FFMPEG_DIR).
#[test]
fn vetted_runtime_decodes_signals_and_frames_for_analysis() {
    if !video_export::available() {
        eprintln!("vetted FFmpeg runtime not staged; skipping");
        return;
    }
    let directory = std::env::temp_dir().join(format!("fileid-timeline-media-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("fixture.mp4");
    video_export::generate_test_video(&source);
    let cancel = AtomicBool::new(false);
    let probe = video_export::analysis_probe(&source, &cancel).unwrap();
    assert!((probe.duration - 2.0).abs() < 0.2);
    assert_eq!((probe.width, probe.height), (180, 320));
    let signals = video_export::signal_frames(&source, 0.0, probe.duration, &cancel).unwrap();
    assert!((2..=3).contains(&signals.len()));
    assert!(signals
        .iter()
        .all(|(_, pixels)| pixels.len() == analysis::SIGNAL_WIDTH * analysis::SIGNAL_HEIGHT));
    let size = video_export::fitted_size(probe.width, probe.height, 128);
    assert_eq!(size, (72, 128));
    let rgb = video_export::frame_rgb(&source, 1.0, size.0, size.1, &cancel).unwrap();
    assert_eq!(rgb.len(), 72 * 128 * 3);
    let jpeg = directory.join("frame.jpg");
    write_jpeg(&jpeg, rgb, size.0, size.1).unwrap();
    assert_eq!(image::image_dimensions(&jpeg).unwrap(), size);
    cancel.store(true, Ordering::Release);
    assert!(video_export::signal_frames(&source, 0.0, probe.duration, &cancel).is_err());
    std::fs::remove_dir_all(directory).unwrap();
}

/// Scripted stand-in for the resident VLM: deterministic captions, a fixed
/// moment answer, and an optional pause request after N caption calls.
struct ScriptedModel {
    captions: Vec<&'static str>,
    calls: std::sync::atomic::AtomicUsize,
    sequences: std::sync::atomic::AtomicUsize,
    speech_calls: std::sync::atomic::AtomicUsize,
    pause_after: Option<(usize, Arc<Mutex<Connection>>, String)>,
}

impl FrameModel for ScriptedModel {
    fn caption(
        &self,
        image: &Path,
        _: &str,
        _: u32,
        _: Arc<AtomicBool>,
    ) -> impl Future<Output = Result<String>> + Send {
        if image.extension().is_some_and(|e| e == "wav") {
            let bytes = std::fs::read(image).unwrap_or_default();
            let valid = bytes.len() > 44
                && &bytes[0..4] == b"RIFF"
                && &bytes[8..12] == b"WAVE"
                && u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize == bytes.len() - 8;
            self.speech_calls.fetch_add(1, Ordering::SeqCst);
            return std::future::ready(if valid {
                Ok("language English<asr_text>Happy birthday Sam".to_owned())
            } else {
                Err(anyhow!("malformed WAV"))
            });
        }
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let readable = image::image_dimensions(image).is_ok();
        if let Some((after, database, id)) = &self.pause_after {
            if call + 1 == *after {
                database
                    .lock()
                    .execute("UPDATE catalog_jobs SET state='paused' WHERE id=?1", [id])
                    .unwrap();
            }
        }
        let text = self.captions[call % self.captions.len()].to_owned();
        std::future::ready(if readable {
            Ok(text)
        } else {
            Err(anyhow!("frame was not a decodable image"))
        })
    }
    fn sequence(
        &self,
        images: &[&Path],
        _: &str,
        _: u32,
        _: Arc<AtomicBool>,
    ) -> impl Future<Output = Result<String>> + Send {
        self.sequences.fetch_add(1, Ordering::SeqCst);
        let readable = images.len() >= 2 && images.iter().all(|path| image::image_dimensions(path).is_ok());
        async move {
            if !readable {
                bail!("sequence frames were not decodable images")
            }
            Ok("```json\n{\"moments\":[{\"title\":\"Ball thrown\",\"summary\":\"A ball leaves a hand.\",\"firstFrame\":1,\"lastFrame\":2}]}\n```".to_owned())
        }
    }
}

fn scripted(
    captions: Vec<&'static str>,
    pause_after: Option<(usize, Arc<Mutex<Connection>>, String)>,
) -> ScriptedModel {
    ScriptedModel {
        captions,
        calls: 0.into(),
        sequences: 0.into(),
        speech_calls: 0.into(),
        pause_after,
    }
}

fn media_fixture(lavfi: &str) -> Option<(PathBuf, Arc<Mutex<Connection>>)> {
    if !video_export::available() {
        eprintln!("vetted FFmpeg runtime not staged; skipping");
        return None;
    }
    let directory = std::env::temp_dir().join(format!("fileid-timeline-e2e-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("clip.mp4");
    video_export::generate_silent_test_video(&path, lavfi);
    let metadata = std::fs::metadata(&path).unwrap();
    let modified = metadata
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let conn = crate::db::test_support::memory_db();
    conn.execute(
        "INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension) VALUES(1,?1,1,?2,?3,0,'video','mp4')",
        params![path.to_str().unwrap(), metadata.len() as i64, modified],
    )
    .unwrap();
    Some((directory, Arc::new(Mutex::new(conn))))
}

fn count(database: &Arc<Mutex<Connection>>, sql: &str) -> i64 {
    database.lock().query_row(sql, [], |r| r.get(0)).unwrap()
}

#[tokio::test]
async fn worker_samples_captions_drafts_chapters_and_resumes_after_pause() {
    // A still picture has no change peaks, so samples land on exact interval
    // boundaries: 0, 10, 20 and 30 seconds.
    let Some((directory, database)) = media_fixture("color=c=gray:size=64x36:rate=5:duration=35") else {
        return;
    };
    let (id, _) = enqueue(
        &mut database.lock(),
        &request(serde_json::json!({"requestID":"r","action":"enqueueTimeline","fileIDs":[1]})),
        Some("qwen3_vl_4b"),
        true,
    )
    .unwrap();
    let (sink, _events) = Sink::channel_for_test(1_024);
    let frames = directory.join("frames");
    let captions = vec![
        "Birthday cake with candles on table",
        "Birthday cake candles burning on table",
        "Children playing soccer in park",
        "Children kicking soccer ball in park",
    ];

    let job = start(&database.lock(), &id).unwrap().unwrap();
    let pausing = scripted(captions.clone(), Some((2, database.clone(), id.clone())));
    let cancel = Arc::new(AtomicBool::new(false));
    let first = process(&id, &job, &pausing, &frames, &cancel, &sink, &database, 1.0).await;
    assert!(
        first.unwrap_err().is::<Interrupted>(),
        "pausing must interrupt, not fail"
    );
    assert_eq!(
        count(&database, "SELECT COUNT(*) FROM catalog_passages WHERE confidence=0"),
        1,
        "only the frame finished before the pause is kept"
    );

    database
        .lock()
        .execute("UPDATE catalog_jobs SET state='queued' WHERE id=?1", [&id])
        .unwrap();
    let job = start(&database.lock(), &id).unwrap().unwrap();
    let resumed = scripted(captions[1..].to_vec(), None);
    process(&id, &job, &resumed, &frames, &cancel, &sink, &database, 1.0)
        .await
        .unwrap();
    assert_eq!(
        resumed.calls.load(Ordering::SeqCst),
        3,
        "evidence captured before the pause is reused"
    );
    assert_eq!(
        resumed.sequences.load(Ordering::SeqCst),
        0,
        "sampled mode never runs moment windows"
    );

    let samples: Vec<f64> = {
        let conn = database.lock();
        let mut statement = conn
            .prepare("SELECT start_seconds FROM catalog_passages WHERE file_id=1 ORDER BY start_seconds")
            .unwrap();
        statement
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    assert_eq!(samples.len(), 4);
    assert!(samples
        .iter()
        .zip([0.0, 10.0, 20.0, 30.0])
        .all(|(actual, expected)| (actual - expected).abs() < 1e-9));
    assert_eq!(count(&database, "SELECT COUNT(*) FROM catalog_chapters WHERE file_id=1 AND model_version LIKE 'timeline-chapter-suggestion-v1/%'"), 2);
    assert_eq!(count(&database, "SELECT COUNT(*) FROM catalog_coverage WHERE file_id=1 AND model_version='visual-change-signal-v1' AND status='sampled'"), 1);
    assert!(
        count(
            &database,
            "SELECT COUNT(*) FROM catalog_evidence_fts WHERE catalog_evidence_fts MATCH 'soccer'"
        ) >= 2
    );
    assert!(
        std::fs::read_dir(&frames).unwrap().next().is_none(),
        "temporary frames are removed"
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn worker_moment_windows_keep_user_edits_and_refuse_changed_sources() {
    let Some((directory, database)) = media_fixture("testsrc2=size=64x36:rate=5:duration=6") else {
        return;
    };
    let (id, _) = enqueue(
        &mut database.lock(),
        &request(
            serde_json::json!({"requestID":"r","action":"enqueueTimeline","fileIDs":[1],"timelineMode":"moments"}),
        ),
        Some("qwen3_vl_4b"),
        true,
    )
    .unwrap();
    let (sink, _events) = Sink::channel_for_test(1_024);
    let frames = directory.join("frames");
    let revision = revision(&database.lock(), 1).unwrap();
    let edited_id = format!("{}0", analysis::moment_prefix(1, &revision, 3.0));
    let edited = CatalogChapter {
        id: edited_id.clone(),
        file_id: 1,
        start_seconds: 3.0,
        end_seconds: 4.0,
        title: "Grandpa's throw".into(),
        summary: String::new(),
        source_revision: revision.clone(),
        model_version: "user".into(),
        confidence: 1.0,
        user_edited: true,
        stale: false,
    };
    upsert_chapter(&database.lock(), &edited).unwrap();

    let job = start(&database.lock(), &id).unwrap().unwrap();
    let model = scripted(vec!["A ball in a yard"], None);
    process(
        &id,
        &job,
        &model,
        &frames,
        &Arc::new(AtomicBool::new(false)),
        &sink,
        &database,
        1.0,
    )
    .await
    .unwrap();
    assert_eq!(
        model.sequences.load(Ordering::SeqCst),
        2,
        "a six-second clip has two overlapping windows"
    );
    let moments = count(&database, "SELECT COUNT(*) FROM catalog_chapters WHERE file_id=1 AND model_version LIKE 'timeline-moment-sequence-v1/%' AND user_edited=0");
    assert_eq!(
        moments, 1,
        "the edited window keeps the user's chapter instead of a draft"
    );
    let title: String = database
        .lock()
        .query_row("SELECT title FROM catalog_chapters WHERE id=?1", [&edited_id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(title, "Grandpa's throw");
    assert_eq!(
        count(
            &database,
            "SELECT COUNT(*) FROM catalog_coverage WHERE file_id=1 AND id LIKE 'moment-window:%' AND status='sampled'"
        ),
        2
    );

    let path: String = database
        .lock()
        .query_row("SELECT path_text FROM files WHERE id=1", [], |r| r.get(0))
        .unwrap();
    std::fs::write(&path, b"replaced").unwrap();
    database
        .lock()
        .execute(
            "UPDATE catalog_jobs SET state='queued',checkpoint_json='0' WHERE id=?1",
            [&id],
        )
        .unwrap();
    let job = start(&database.lock(), &id).unwrap().unwrap();
    let error = process(
        &id,
        &job,
        &model,
        &frames,
        &Arc::new(AtomicBool::new(false)),
        &sink,
        &database,
        1.0,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("source changed"));
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn speech_pass_transcribes_audio_chunks_and_resumes_by_chunk() {
    if !video_export::available() {
        eprintln!("vetted FFmpeg runtime not staged; skipping");
        return;
    }
    let directory = std::env::temp_dir().join(format!("fileid-timeline-speech-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("talk.mp4");
    video_export::generate_test_video(&path);
    let metadata = std::fs::metadata(&path).unwrap();
    let modified = metadata
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let conn = crate::db::test_support::memory_db();
    conn.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension) VALUES(1,?1,1,?2,?3,0,'video','mp4')",
        params![path.to_str().unwrap(), metadata.len() as i64, modified]).unwrap();
    let database = Arc::new(Mutex::new(conn));
    let (id, _) = enqueue(
        &mut database.lock(),
        &request(serde_json::json!({"requestID":"r","action":"enqueueTimeline","fileIDs":[1]})),
        Some("qwen3_vl_4b"),
        true,
    )
    .unwrap();
    let job = start(&database.lock(), &id).unwrap().unwrap();
    let (sink, _events) = Sink::channel_for_test(1_024);
    let frames = directory.join("frames");
    let model = scripted(vec!["unused"], None);
    transcribe(
        &id,
        &job,
        "qwen3_asr_0_6b_gguf_q8_0",
        &model,
        &frames,
        &Arc::new(AtomicBool::new(false)),
        &sink,
        &database,
        0.8,
    )
    .await
    .unwrap();
    assert_eq!(
        model.speech_calls.load(Ordering::SeqCst),
        1,
        "a two-second clip is one chunk"
    );
    let (text, start, end, confidence): (String, f64, f64, f64) = database
        .lock()
        .query_row(
            "SELECT text,start_seconds,end_seconds,confidence FROM catalog_passages WHERE file_id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(text, "Happy birthday Sam");
    assert!(start.abs() < 1e-9 && end > 1.5 && (confidence - 0.5).abs() < 1e-9);
    assert_eq!(
        count(
            &database,
            "SELECT COUNT(*) FROM catalog_coverage WHERE id LIKE 'speech-coverage:%' AND status='verified'"
        ),
        2,
        "the chunk and the whole file are verified"
    );
    assert!(
        count(
            &database,
            "SELECT COUNT(*) FROM catalog_evidence_fts WHERE catalog_evidence_fts MATCH 'birthday'"
        ) >= 1
    );

    transcribe(
        &id,
        &job,
        "qwen3_asr_0_6b_gguf_q8_0",
        &model,
        &frames,
        &Arc::new(AtomicBool::new(false)),
        &sink,
        &database,
        0.8,
    )
    .await
    .unwrap();
    assert_eq!(
        model.speech_calls.load(Ordering::SeqCst),
        1,
        "verified chunks are not transcribed again"
    );
    let progress: f64 = database
        .lock()
        .query_row("SELECT progress FROM catalog_jobs WHERE id=?1", [&id], |r| r.get(0))
        .unwrap();
    assert!((progress - 1.0).abs() < 1e-9);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn wav_header_sizes_are_rewritten_for_streamed_audio() {
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&u32::MAX.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&[0; 16]);
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&u32::MAX.to_le_bytes());
    wav.extend_from_slice(&[1; 10]);
    video_export::fix_wav_sizes(&mut wav).unwrap();
    assert_eq!(
        u32::from_le_bytes(wav[4..8].try_into().unwrap()) as usize,
        wav.len() - 8
    );
    assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 10);
    assert!(video_export::fix_wav_sizes(&mut b"RIFF\0\0\0\0WAVEjunk".to_vec()).is_err());
}

struct OrderedModel {
    log: Arc<Mutex<Vec<String>>>,
}

impl FrameModel for OrderedModel {
    fn caption(&self, _: &Path, _: &str, _: u32, _: Arc<AtomicBool>) -> impl Future<Output = Result<String>> + Send {
        std::future::ready(Err(anyhow!("captions are not used in moments mode")))
    }
    fn sequence(
        &self,
        images: &[&Path],
        _: &str,
        _: u32,
        _: Arc<AtomicBool>,
    ) -> impl Future<Output = Result<String>> + Send {
        let log = self.log.clone();
        let first = images[0].file_stem().unwrap().to_string_lossy().into_owned();
        async move {
            log.lock().push(format!("infer-start {first}"));
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            log.lock().push(format!("infer-end {first}"));
            Ok("{\"moments\":[{\"title\":\"Ball thrown\",\"summary\":\"\",\"firstFrame\":1,\"lastFrame\":2}]}".to_owned())
        }
    }
}

#[tokio::test]
async fn next_moment_window_decodes_while_the_model_runs() {
    let directory = std::env::temp_dir().join(format!("fileid-timeline-prefetch-{}", uuid::Uuid::new_v4()));
    let frames = directory.join("frames");
    std::fs::create_dir_all(&frames).unwrap();
    let path = directory.join("clip.mp4");
    std::fs::write(&path, b"fixture video bytes").unwrap();
    let modified = std::fs::metadata(&path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let mut conn = crate::db::test_support::memory_db();
    conn.execute(
        "INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension) VALUES(1,?1,1,19,?2,0,'video','mp4')",
        params![path.to_str().unwrap(), modified],
    )
    .unwrap();
    let (id, _) = enqueue(
        &mut conn,
        &request(serde_json::json!({"requestID":"r","action":"enqueueTimeline","fileIDs":[1],"timelineMode":"moments"})),
        Some("qwen3_vl_4b"),
        true,
    )
    .unwrap();
    start(&conn, &id).unwrap().unwrap();
    let database = Arc::new(Mutex::new(conn));
    let source = Source::load(&database.lock(), 1).unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let decode_log = log.clone();
    let decoder = move |_: &Path, _: f64, size: (u32, u32), output: &Path, _: &AtomicBool| -> Result<()> {
        decode_log.lock().push(format!("decode {}", output.file_stem().unwrap().to_string_lossy()));
        std::thread::sleep(std::time::Duration::from_millis(10));
        write_jpeg(output, vec![128; (size.0 * size.1 * 3) as usize], size.0, size.1)
    };
    let (sink, _events) = Sink::channel_for_test(1_024);
    let model = OrderedModel { log: log.clone() };
    let duration = 6.0;
    let windows = analysis::windows(duration, &[]);
    assert_eq!(windows.len(), 2);
    analyze_moments(
        &id,
        1,
        &source,
        duration,
        &[],
        (4, 4),
        "fixture",
        (0, 1, 1.0),
        &model,
        &frames,
        &Arc::new(AtomicBool::new(false)),
        &sink,
        &database,
        decoder,
    )
    .await
    .unwrap();

    let log = log.lock().clone();
    let second = format!("decode moment-{}-", analysis::millis(windows[1].start));
    let first_second_decode = log.iter().position(|line| line.starts_with(&second)).expect("second window decoded");
    let first_inference_end = log.iter().position(|line| line.starts_with("infer-end")).unwrap();
    assert!(first_second_decode < first_inference_end, "{log:?}");
    assert_eq!(log.iter().filter(|line| line.starts_with("infer-start")).count(), 2);
    assert_eq!(
        count(
            &database,
            "SELECT COUNT(*) FROM catalog_coverage WHERE file_id=1 AND id LIKE 'moment-window:%' AND status='sampled'"
        ),
        2
    );
    assert!(std::fs::read_dir(&frames).unwrap().next().is_none(), "temporary frames are removed");
    std::fs::remove_dir_all(directory).unwrap();
}
