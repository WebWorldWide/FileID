//! `runFaceClustering` IPC handler: re-cluster every face in the DB and
//! refresh the People tab. The actual clustering algorithm lives in
//! `pipeline::face_clustering`; this handler loads embeddings, calls the
//! algorithm, and persists the resulting `persons` + `face_prints.person_id`
//! assignments in one transaction.

use std::time::Instant;

use crate::ipc::{sink::Sink, EngineError, EventPayload, FaceClusteringResult, IpcEvent, Wrap};
use crate::pipeline::face_clustering::{cluster, FaceRow};

fn load_compatible_faces(conn: &rusqlite::Connection) -> anyhow::Result<Vec<FaceRow>> {
    let mut stmt = conn.prepare(
        "SELECT fp.id,fp.file_id,fp.arcface_embedding,COALESCE(fp.face_quality,0.0), \
         fp.embedding_model,fp.processing_version,fp.source_revision,r.revision \
         FROM face_prints fp LEFT JOIN catalog_revisions r ON r.file_id=fp.file_id \
         WHERE fp.arcface_embedding IS NOT NULL AND COALESCE(fp.excluded,0)=0 ORDER BY fp.id",
    )?;
    let mut cursor = stmt.query([])?;
    let mut space: Option<(String, String)> = None;
    let mut faces = Vec::new();
    while let Some(row) = cursor.next()? {
        let model: Option<String> = row.get(4)?;
        let processing: Option<String> = row.get(5)?;
        let revision: Option<String> = row.get(6)?;
        let current: Option<String> = row.get(7)?;
        let blob: Vec<u8> = row.get(2)?;
        let compatible = model.as_ref().is_some_and(|v| !v.is_empty())
            && processing.as_ref().is_some_and(|v| !v.is_empty())
            && revision.as_ref().is_some_and(|v| !v.is_empty())
            && revision == current
            && blob.len() == 512;
        anyhow::ensure!(compatible, "Face caches need a compatible refresh before clustering; existing People were preserved.");
        let key = (model.unwrap_or_default(), processing.unwrap_or_default());
        anyhow::ensure!(space.as_ref().is_none_or(|s| *s == key), "Mixed face-model or processing spaces cannot be compared; existing People were preserved.");
        space = Some(key);
        let embedding: Vec<f32> = blob
            .as_chunks::<4>()
            .0
            .iter()
            .map(|v| f32::from_le_bytes(*v))
            .collect();
        let norm: f64 = embedding.iter().map(|v| f64::from(*v).powi(2)).sum();
        anyhow::ensure!(
            embedding.iter().all(|v| v.is_finite()) && (0.95..=1.05).contains(&norm),
            "Invalid face vector; existing People were preserved."
        );
        anyhow::ensure!(
            faces.len() < 200_000,
            "Face clustering exceeds the safe batch size; existing People were preserved."
        );
        let quality = row.get::<_, f64>(3)? as f32;
        anyhow::ensure!(
            quality.is_finite(),
            "Invalid face quality; existing People were preserved."
        );
        faces.push(FaceRow {
            face_id: row.get(0)?,
            file_id: row.get(1)?,
            embedding,
            quality: row.get::<_, f64>(3)? as f32,
        });
    }
    Ok(faces)
}

pub(crate) async fn handle_run_face_clustering(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
) {
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<FaceClusteringResult> {
        let started = Instant::now();

        let faces: Vec<FaceRow>;
        {
            let conn = db.lock();

            faces = load_compatible_faces(&conn)?;
            if faces.is_empty() {
                return Ok(FaceClusteringResult {
                    person_count: conn
                        .query_row("SELECT COUNT(*) FROM persons", [], |r| r.get(0))?,
                    face_count: 0,
                    unmatched_faces: 0,
                    duration_seconds: started.elapsed().as_secs_f64(),
                });
            }
        }
        let face_count = faces.len() as u64;
        let (assignments, anchors) = cluster(&faces);
        let conn = db.lock();
        let tx = conn.unchecked_transaction()?;
        let current = load_compatible_faces(&tx)?;
        anyhow::ensure!(
            current.len() == faces.len()
                && current
                    .iter()
                    .zip(&faces)
                    .all(|(a, b)| a.face_id == b.face_id
                        && a.file_id == b.file_id
                        && a.embedding == b.embedding
                        && a.quality.to_bits() == b.quality.to_bits()),
            "Face caches changed during clustering; existing People were preserved."
        );
        let person_count =
            super::person_cluster_persistence::persist(&tx, &faces, assignments, anchors)?;
        tx.commit()?;
        Ok(FaceClusteringResult {
            person_count,
            face_count,
            unmatched_faces: 0,
            duration_seconds: started.elapsed().as_secs_f64(),
        })
    })
    .await;

    match result {
        Ok(Ok(r)) => {
            sink.send(IpcEvent::now(EventPayload::FaceClusteringComplete(
                Wrap::new(r),
            )))
            .await;
        }
        Ok(Err(err)) => {
            tracing::warn!(?err, "face clustering failed");
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "face_clustering_failed".into(),
                message: format!("Face clustering failed: {err}"),
                path: None,
                model_kind: None,
            }))))
            .await;
        }
        Err(err) => {
            tracing::warn!(?err, "face clustering spawn failed");
            // PAR-111: emit a face_clustering error so the app-side auto-trigger
            // gate (_faceClusterAutoInFlight) is released even when the
            // clustering closure panics — a JoinError otherwise fires no
            // completion/error event, leaving auto-clustering stuck for the
            // session.
            sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
                kind: "face_clustering_failed".into(),
                message: format!("Face clustering task did not complete: {err}"),
                path: None,
                model_kind: None,
            }))))
            .await;
        }
    }
}

#[cfg(test)]
mod cache_space_tests {
    use super::*;

    #[test]
    fn incompatible_spaces_are_rejected_before_persistence() -> anyhow::Result<()> {
        for fault in [
            "model",
            "processing",
            "legacy",
            "revision",
            "nan",
            "dimension",
        ] {
            let conn = rusqlite::Connection::open_in_memory()?;
            conn.execute_batch("CREATE TABLE face_prints(id INTEGER,file_id INTEGER,arcface_embedding BLOB,face_quality REAL,excluded INTEGER,embedding_model TEXT,processing_version TEXT,source_revision TEXT); CREATE TABLE catalog_revisions(file_id INTEGER,revision TEXT);")?;
            let mut vector = vec![0.0f32; 128];
            vector[0] = 1.0;
            let blob: Vec<u8> = vector.iter().flat_map(|v| v.to_le_bytes()).collect();
            for id in 1..=2 {
                conn.execute("INSERT INTO catalog_revisions VALUES(?1,'current')", [id])?;
                conn.execute(
                    "INSERT INTO face_prints VALUES(?1,?1,?2,1,0,'weights','aligned','current')",
                    rusqlite::params![id, blob],
                )?;
            }
            assert_eq!(load_compatible_faces(&conn)?.len(), 2);
            match fault {
                "model" => {
                    conn.execute(
                        "UPDATE face_prints SET embedding_model='other' WHERE id=2",
                        [],
                    )?;
                }
                "processing" => {
                    conn.execute(
                        "UPDATE face_prints SET processing_version='other' WHERE id=2",
                        [],
                    )?;
                }
                "legacy" => {
                    conn.execute("UPDATE face_prints SET embedding_model=NULL WHERE id=2", [])?;
                }
                "revision" => {
                    conn.execute(
                        "UPDATE face_prints SET source_revision='old' WHERE id=2",
                        [],
                    )?;
                }
                "nan" => {
                    conn.execute(
                        "UPDATE face_prints SET arcface_embedding=?1 WHERE id=2",
                        [vec![f32::NAN.to_le_bytes(); 128].concat()],
                    )?;
                }
                _ => {
                    conn.execute(
                        "UPDATE face_prints SET arcface_embedding=X'00' WHERE id=2",
                        [],
                    )?;
                }
            }
            assert!(load_compatible_faces(&conn).is_err(), "{fault}");
            assert_eq!(
                conn.query_row("SELECT count(*) FROM face_prints", [], |r| r
                    .get::<_, i64>(0))?,
                2
            );
        }
        Ok(())
    }
    #[tokio::test]
    async fn handler_preserves_people_on_incompatible_or_empty_caches() -> anyhow::Result<()> {
        for empty in [false, true] {
            let conn = rusqlite::Connection::open_in_memory()?;
            conn.execute_batch("CREATE TABLE persons(id INTEGER,name TEXT); INSERT INTO persons VALUES(8,'Confirmed'); CREATE TABLE face_prints(id INTEGER,file_id INTEGER,person_id INTEGER,arcface_embedding BLOB,face_quality REAL,excluded INTEGER,embedding_model TEXT,processing_version TEXT,source_revision TEXT); CREATE TABLE catalog_revisions(file_id INTEGER,revision TEXT);")?;
            if !empty {
                let mut vector = vec![0.0f32; 128];
                vector[0] = 1.0;
                let blob: Vec<u8> = vector.iter().flat_map(|v| v.to_le_bytes()).collect();
                for id in 1..=2 {
                    conn.execute("INSERT INTO catalog_revisions VALUES(?1,'current')", [id])?;
                    conn.execute(
                        "INSERT INTO face_prints VALUES(?1,?1,8,?2,1,0,?3,'aligned','current')",
                        rusqlite::params![id, blob, format!("different-weights-{id}")],
                    )?;
                }
            }
            let db = std::sync::Arc::new(parking_lot::Mutex::new(conn));
            let (sink, mut rx) = Sink::channel_for_test(4);
            handle_run_face_clustering(sink, db.clone()).await;
            let event = rx
                .recv()
                .await
                .ok_or_else(|| anyhow::anyhow!("missing terminal event"))?;
            if empty {
                assert!(matches!(
                    event.payload,
                    EventPayload::FaceClusteringComplete(_)
                ));
            } else {
                assert!(matches!(event.payload, EventPayload::Error(_)));
            }
            assert_eq!(
                db.lock()
                    .query_row("SELECT name FROM persons WHERE id=8", [], |r| r
                        .get::<_, String>(0))?,
                "Confirmed"
            );
            if !empty {
                assert_eq!(
                    db.lock().query_row(
                        "SELECT COUNT(*) FROM face_prints WHERE person_id=8",
                        [],
                        |r| r.get::<_, i64>(0)
                    )?,
                    2
                );
            }
        }
        Ok(())
    }
}
