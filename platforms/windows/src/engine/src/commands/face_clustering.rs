//! `runFaceClustering` IPC handler: re-cluster every face in the DB and
//! refresh the People tab. The actual clustering algorithm lives in
//! `pipeline::face_clustering`; this handler loads embeddings, calls the
//! algorithm, and persists the resulting `persons` + `face_prints.person_id`
//! assignments in one transaction.

use std::time::Instant;

use crate::ipc::{sink::Sink, EngineError, EventPayload, FaceClusteringResult, IpcEvent, Wrap};
use crate::pipeline::face_clustering::{cluster, FaceRow};

#[derive(Debug, PartialEq)]
struct ParticipantSnapshot {
    input: crate::pipeline::face_analysis_cache::Input,
    person_id: Option<i64>,
}

struct CompatiblePartition {
    faces: Vec<FaceRow>,
    snapshots: Vec<ParticipantSnapshot>,
    deferred_faces: u64,
}

fn load_current_partition(conn: &rusqlite::Connection, selected_model: &str) -> anyhow::Result<CompatiblePartition> {
    use crate::pipeline::face_analysis_cache::{self as cache, Input};
    anyhow::ensure!(!selected_model.is_empty() && selected_model.len() <= 150, "Invalid selected face model");
    let mut stmt = conn.prepare(
        "SELECT fp.id,fp.file_id,CASE WHEN length(fp.arcface_embedding)=512 THEN fp.arcface_embedding ELSE X'' END,COALESCE(fp.face_quality,0), \
         fp.embedding_model,fp.processing_version,fp.source_revision,r.revision,fp.bbox,fp.person_id, \
         f.path_text,f.size_bytes,f.modified_at,f.file_ref,f.failed \
         FROM face_prints fp LEFT JOIN files f ON f.id=fp.file_id \
         LEFT JOIN catalog_revisions r ON r.file_id=fp.file_id WHERE COALESCE(fp.excluded,0)=0 ORDER BY fp.id")?;
    let mut rows = stmt.query([])?;
    let mut partition = CompatiblePartition { faces: Vec::new(), snapshots: Vec::new(), deferred_faces: 0 };
    while let Some(row) = rows.next()? {
        let blob: Vec<u8> = row.get(2)?;
        let quality: f64 = row.get(3)?;
        let model: Option<String> = row.get(4)?;
        let processing: Option<String> = row.get(5)?;
        let source: Option<String> = row.get(6)?;
        let current: Option<String> = row.get(7)?;
        let path: Option<String> = row.get(10)?;
        let size: Option<i64> = row.get(11)?;
        let modified: Option<f64> = row.get(12)?;
        let failed: Option<i64> = row.get(14)?;
        let input = Input { id: row.get(0)?, file_id: row.get(1)?, bbox: row.get(8)?,
            path: path.clone().unwrap_or_default(), size: size.unwrap_or(-1), modified, file_ref: row.get(13)? };
        let compatible = failed == Some(0) && path.is_some() && size.is_some_and(|value| value >= 0)
            && modified.is_some_and(|value| value.is_finite())
            && model.as_deref() == Some(selected_model) && processing.as_deref() == Some(cache::PROCESSING_VERSION)
            && source.as_deref() == Some(input.revision().as_str()) && source == current
            && cache::normalized(&blob) && quality.is_finite() && (quality as f32).is_finite();
        if !compatible { partition.deferred_faces += 1; continue; }
        anyhow::ensure!(partition.faces.len() < 200_000, "Face clustering exceeds safe batch size; existing People preserved.");
        let embedding = blob.chunks_exact(4).map(|bytes| f32::from_le_bytes(bytes.try_into().expect("four-byte chunk"))).collect();
        partition.faces.push(FaceRow { face_id: input.id, file_id: input.file_id, embedding, quality: quality as f32 });
        partition.snapshots.push(ParticipantSnapshot { input, person_id: row.get(9)? });
    }
    Ok(partition)
}

pub(super) fn load_current_face_members(conn: &rusqlite::Connection, selected_model: &str) -> anyhow::Result<Vec<(FaceRow,Option<i64>)>> {
    let partition=load_current_partition(conn,selected_model)?;
    Ok(partition.faces.into_iter().zip(partition.snapshots).map(|(face,snapshot)| (face,snapshot.person_id)).collect())
}

fn require_unchanged_partition(conn: &rusqlite::Connection, selected_model: &str, original: &CompatiblePartition) -> anyhow::Result<u64> {
    let current = load_current_partition(conn, selected_model)?;
    anyhow::ensure!(current.snapshots == original.snapshots && current.faces.len() == original.faces.len()
        && current.faces.iter().zip(&original.faces).all(|(a,b)| a.face_id == b.face_id && a.file_id == b.file_id
            && a.embedding == b.embedding && a.quality.to_bits() == b.quality.to_bits()),
        "Face caches or assignments changed during clustering; existing People preserved.");
    Ok(current.deferred_faces)
}

pub(crate) async fn handle_run_face_clustering(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<FaceClusteringResult> {
        let started = Instant::now();
        let expected = crate::pipeline::face_refresh::run(&db, &cancel)?;
        anyhow::ensure!(!cancel.load(std::sync::atomic::Ordering::Relaxed), "Face processing cancelled; existing People preserved");

        let Some(selected_model) = expected else {
            return Ok(FaceClusteringResult { person_count: db.lock().query_row("SELECT COUNT(*) FROM persons",[],|row| row.get(0))?,
                face_count: 0, unmatched_faces: 0, deferred_faces: Some(0), duration_seconds: started.elapsed().as_secs_f64() });
        };
        let partition = load_current_partition(&db.lock(), &selected_model)?;
        tracing::info!(eligible_faces=partition.faces.len(), deferred_faces=partition.deferred_faces, "Face cache grouping coverage");
        if partition.faces.is_empty() {
            return Ok(FaceClusteringResult { person_count: db.lock().query_row("SELECT COUNT(*) FROM persons",[],|row| row.get(0))?,
                face_count: 0, unmatched_faces: 0, deferred_faces: Some(partition.deferred_faces), duration_seconds: started.elapsed().as_secs_f64() });
        }
        let faces = &partition.faces;
        let face_count = faces.len() as u64;
        let (assignments, anchors) = cluster(&faces);
        anyhow::ensure!(!cancel.load(std::sync::atomic::Ordering::Relaxed), "Face processing cancelled; existing People preserved");
        let conn = db.lock();
        let tx = conn.unchecked_transaction()?;
        let deferred_faces = require_unchanged_partition(&tx, &selected_model, &partition)?;
        let person_count =
            super::person_cluster_persistence::persist(&tx, &faces, assignments, anchors)?;
        tx.commit()?;
        Ok(FaceClusteringResult {
            person_count,
            face_count,
            unmatched_faces: 0,
            deferred_faces: Some(deferred_faces),
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
mod compatible_partition_tests {
    use super::*;
    use crate::pipeline::face_analysis_cache::{Input, PROCESSING_VERSION};
    use rusqlite::{params, Connection};

    fn fixture() -> anyhow::Result<Connection> {
        let conn = Connection::open_in_memory()?;
        crate::db::migrations::apply(&conn)?;
        conn.execute_batch("INSERT INTO persons(id,first_name,is_unknown,created_at) VALUES(7,'Confirmed',0,123),(8,NULL,0,123),(9,NULL,1,123); CREATE TABLE fixture_refs(person_id INTEGER REFERENCES persons(id)); INSERT INTO fixture_refs VALUES(7),(8),(9);")?;
        let mut blob = vec![0_u8;512]; blob[..4].copy_from_slice(&1_f32.to_le_bytes());
        for id in 1_i64..=13 {
            let path = format!("/tmp/fileid-partition-fixture-{id}.png");
            conn.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension,file_ref) VALUES(?1,?2,?1,4,42,1,'image','png',?1)",params![id,path])?;
            let revision = Input{id,file_id:id,bbox:"0,0,1,1".into(),path,size:4,modified:Some(42.0),file_ref:Some(id)}.revision();
            conn.execute("INSERT INTO catalog_revisions(file_id,revision,processing_version,updated_at) VALUES(?1,?2,'file-stat-v1',1)",params![id,revision])?;
            let person = match id { 1|11 => Some(7), 2..=9|12 => Some(8), 13 => Some(9), _ => None };
            conn.execute("INSERT INTO face_prints(id,file_id,person_id,print_data,bbox,arcface_embedding,face_quality,embedding_model,processing_version,source_revision) VALUES(?1,?1,?2,X'00','0,0,1,1',?3,1,'selected',?4,?5)",params![id,person,blob,PROCESSING_VERSION,revision])?;
        }
        conn.execute_batch("UPDATE face_prints SET processing_version='rust-align112-v1' WHERE id=2; UPDATE face_prints SET embedding_model='other-model' WHERE id=3; UPDATE face_prints SET source_revision='old' WHERE id=4; UPDATE files SET failed=1 WHERE id=5; UPDATE face_prints SET arcface_embedding=NULL WHERE id=6; UPDATE face_prints SET arcface_embedding=X'00' WHERE id=7; UPDATE face_prints SET excluded=1 WHERE id=11;")?;
        conn.execute("UPDATE face_prints SET arcface_embedding=?1 WHERE id=8",[vec![f32::NAN.to_le_bytes();128].concat()])?;
        let old: String = conn.query_row("SELECT revision FROM catalog_revisions WHERE file_id=9",[],|row| row.get(0))?;
        conn.execute("UPDATE files SET modified_at=84 WHERE id=9",[])?;
        conn.execute("INSERT INTO catalog_revisions(file_id,revision,processing_version,updated_at) VALUES(9,?1,'file-stat-v1',1)",[old])?;
        conn.execute_batch("INSERT INTO catalog_observations(id,file_id,person_id,region_json,source_revision,model_version,confidence,user_edited,stale) VALUES('manual',2,8,'manual-correction','old','manual',1,1,0); INSERT INTO face_verifications(person_a,person_b,face_a,face_b,same_person,confidence,vlm_model,verified_at) VALUES(7,8,1,2,0,1,'manual',1),(8,8,2,12,1,1,'manual',1);")?;
        Ok(conn)
    }

    fn persist_partition(conn: &Connection, partition: &CompatiblePartition) -> anyhow::Result<u32> {
        let (assignments,anchors) = cluster(&partition.faces);
        let tx = conn.unchecked_transaction()?;
        require_unchanged_partition(&tx,"selected",partition)?;
        let count = super::super::person_cluster_persistence::persist(&tx,&partition.faces,assignments,anchors)?;
        tx.commit()?;
        Ok(count)
    }

    #[test]
    fn actual_selected_partition_defers_each_invalid_cache_without_comparing_spaces() -> anyhow::Result<()> {
        for fault in ["model","processing","legacy","revision","nan","dimension","zero","quality","catalog"] {
            let conn=fixture()?;
            match fault {
                "model" => { conn.execute("UPDATE face_prints SET embedding_model='foreign' WHERE id=1",[])?; }
                "processing" => { conn.execute("UPDATE face_prints SET processing_version='rust-align112-v1' WHERE id=1",[])?; }
                "legacy" => { conn.execute("UPDATE face_prints SET embedding_model=NULL WHERE id=1",[])?; }
                "revision" => { conn.execute("UPDATE face_prints SET source_revision='old' WHERE id=1",[])?; }
                "nan" => { conn.execute("UPDATE face_prints SET arcface_embedding=?1 WHERE id=1",[vec![f32::NAN.to_le_bytes();128].concat()])?; }
                "dimension" => { conn.execute("UPDATE face_prints SET arcface_embedding=X'00' WHERE id=1",[])?; }
                "zero" => { conn.execute("UPDATE face_prints SET arcface_embedding=?1 WHERE id=1",[vec![0_u8;512]])?; }
                "quality" => { conn.execute("UPDATE face_prints SET face_quality=?1 WHERE id=1",[f64::INFINITY])?; }
                _ => { conn.execute("UPDATE catalog_revisions SET revision='old' WHERE file_id=1",[])?; }
            }
            let partition=load_current_partition(&conn,"selected")?;
            assert_eq!(partition.faces.iter().map(|face| face.face_id).collect::<Vec<_>>(),vec![10,12,13],"{fault}");
            assert_eq!(partition.deferred_faces,9,"{fault}");
            assert_eq!(conn.query_row("SELECT COUNT(*) FROM face_prints",[],|row| row.get::<_,i64>(0))?,13);
            assert_eq!(conn.query_row("SELECT first_name FROM persons WHERE id=7",[],|row| row.get::<_,String>(0))?,"Confirmed");
        }
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_grouping_emits_no_success_or_identity_changes() -> anyhow::Result<()> {
        let db=std::sync::Arc::new(parking_lot::Mutex::new(fixture()?));
        let (sink,mut receiver)=Sink::channel_for_test(4);
        handle_run_face_clustering(sink,db.clone(),std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true))).await;
        let event=receiver.recv().await.expect("terminal cancellation event");
        assert!(matches!(event.payload,EventPayload::Error(_)));
        assert_eq!(db.lock().query_row("SELECT first_name FROM persons WHERE id=7",[],|row| row.get::<_,String>(0))?,"Confirmed");
        assert_eq!(db.lock().query_row("SELECT COUNT(*) FROM persons",[],|row| row.get::<_,i64>(0))?,3);
        Ok(())
    }

    #[test]
    fn compatible_faces_group_while_unrecoverable_and_ignored_history_survives() -> anyhow::Result<()> {
        let conn = fixture()?;
        let partition = load_current_partition(&conn,"selected")?;
        assert_eq!(partition.faces.iter().map(|face| face.face_id).collect::<Vec<_>>(),vec![1,10,12,13]);
        assert_eq!(partition.deferred_faces,8);
        assert_eq!(load_current_partition(&conn,"other-model")?.faces.iter().map(|face| face.face_id).collect::<Vec<_>>(),vec![3]);
        let old: Vec<(i64,Option<i64>,Option<Vec<u8>>,Option<String>,Option<String>,Option<String>)> = {
            let mut query = conn.prepare("SELECT id,person_id,arcface_embedding,embedding_model,processing_version,source_revision FROM face_prints WHERE id BETWEEN 2 AND 9 OR id=11 ORDER BY id")?;
            let rows = query.query_map([],|row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)))?.collect::<rusqlite::Result<Vec<_>>>()?; rows
        };
        assert_eq!(persist_partition(&conn,&partition)?,4);
        assert_eq!(persist_partition(&conn,&load_current_partition(&conn,"selected")?)?,4);
        for (id,person,blob,model,processing,revision) in old {
            assert_eq!(conn.query_row("SELECT person_id,arcface_embedding,embedding_model,processing_version,source_revision FROM face_prints WHERE id=?1",[id],|row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)))?,(person,blob,model,processing,revision));
        }
        assert_eq!(conn.query_row("SELECT first_name,created_at FROM persons WHERE id=7",[],|row| Ok((row.get::<_,String>(0)?,row.get::<_,f64>(1)?)))?,("Confirmed".into(),123.0));
        for (face,person) in [(1,7),(12,8),(13,9)] { assert_eq!(conn.query_row("SELECT person_id FROM face_prints WHERE id=?1",[face],|row| row.get::<_,i64>(0))?,person); }
        assert_ne!(conn.query_row("SELECT person_id FROM face_prints WHERE id=10",[],|row| row.get::<_,i64>(0))?,8);
        assert_eq!(conn.query_row("SELECT person_id,region_json,user_edited FROM catalog_observations WHERE id='manual'",[],|row| Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?,row.get::<_,i64>(2)?)))?,(8,"manual-correction".into(),1));
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM face_verifications",[],|row| row.get::<_,i64>(0))?,2);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM fixture_refs JOIN persons ON persons.id=fixture_refs.person_id",[],|row| row.get::<_,i64>(0))?,3);
        Ok(())
    }

    #[test]
    fn deferred_coverage_is_current_at_commit_without_discarding_compatible_work() -> anyhow::Result<()> {
        let conn=fixture()?;
        let partition=load_current_partition(&conn,"selected")?;
        assert_eq!(partition.deferred_faces,8);
        conn.execute("UPDATE face_prints SET excluded=1 WHERE id=2",[])?;
        assert_eq!(require_unchanged_partition(&conn,"selected",&partition)?,7);
        assert_eq!(persist_partition(&conn,&partition)?,4);
        assert_eq!(conn.query_row("SELECT person_id FROM face_prints WHERE id=2",[],|row| row.get::<_,i64>(0))?,8);
        Ok(())
    }

    #[test]
    fn ambiguous_deferred_corrections_do_not_block_unrelated_faces_but_guard_participants() -> anyhow::Result<()> {
        for relevant in [false,true] {
            let conn=fixture()?;
            conn.execute("UPDATE face_prints SET file_id=2 WHERE id=3",[])?;
            if relevant { conn.execute("UPDATE face_prints SET file_id=2 WHERE id=12",[])?; }
            conn.execute("INSERT INTO face_verifications(person_a,person_b,face_a,face_b,file_a,bbox_a,file_b,bbox_b,same_person,confidence,vlm_model,verified_at) VALUES(9,7,2,3,2,'0,0,1,1',3,'0,0,1,1',0,1,'manual',2)",[])?;
            let partition=load_current_partition(&conn,"selected")?;
            let result=persist_partition(&conn,&partition);
            if relevant { assert!(result.is_err()); } else { assert_eq!(result?,4); }
            assert_eq!(conn.query_row("SELECT person_id FROM face_prints WHERE id=2",[],|row| row.get::<_,i64>(0))?,8);
            assert_eq!(conn.query_row("SELECT COUNT(*) FROM face_verifications",[],|row| row.get::<_,i64>(0))?,3);
        }
        Ok(())
    }

    #[test]
    fn conflicting_positive_links_to_deferred_identity_preserve_existing_assignments() -> anyhow::Result<()> {
        let conn=fixture()?;
        conn.execute("UPDATE face_prints SET person_id=8 WHERE id=10",[])?;
        conn.execute_batch("INSERT INTO face_verifications(person_a,person_b,face_a,face_b,same_person,confidence,vlm_model,verified_at) VALUES(7,9,2,10,1,1,'manual',2),(9,7,10,12,0,1,'manual',2)")?;
        let partition=load_current_partition(&conn,"selected")?;
        assert!(persist_partition(&conn,&partition).is_err());
        for face in [2,10,12] { assert_eq!(conn.query_row("SELECT person_id FROM face_prints WHERE id=?1",[face],|row| row.get::<_,i64>(0))?,8); }
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM persons",[],|row| row.get::<_,i64>(0))?,3);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM face_verifications",[],|row| row.get::<_,i64>(0))?,4);
        Ok(())
    }

    #[test]
    fn changed_participant_cache_source_geometry_or_owner_cannot_publish() -> anyhow::Result<()> {
        for sql in ["UPDATE files SET path_text='/tmp/replaced.png' WHERE id=1", "UPDATE files SET file_ref=99 WHERE id=1",
            "UPDATE files SET modified_at=84 WHERE id=1", "UPDATE face_prints SET bbox='new geometry' WHERE id=1",
            "UPDATE face_prints SET person_id=8 WHERE id=1", "UPDATE face_prints SET excluded=1 WHERE id=1",
            "UPDATE face_prints SET embedding_model='other' WHERE id=1", "UPDATE face_prints SET face_quality=0.5 WHERE id=1"] {
            let conn=fixture()?; let partition=load_current_partition(&conn,"selected")?;
            conn.execute_batch(sql)?;
            assert!(persist_partition(&conn,&partition).is_err(),"{sql}");
            assert!(conn.query_row("SELECT person_id FROM face_prints WHERE id=10",[],|row| row.get::<_,Option<i64>>(0))?.is_none());
            assert_eq!(conn.query_row("SELECT COUNT(*) FROM persons",[],|row| row.get::<_,i64>(0))?,3);
        }
        Ok(())
    }

    #[test]
    fn late_name_and_negative_verdict_are_rechecked_inside_persistence() -> anyhow::Result<()> {
        let conn=fixture()?;
        conn.execute_batch("UPDATE face_prints SET person_id=8 WHERE id=10")?;
        let partition=load_current_partition(&conn,"selected")?;
        let (assignments,anchors)=cluster(&partition.faces);
        conn.execute_batch("UPDATE persons SET first_name='Newly confirmed' WHERE id=8; INSERT INTO face_verifications(person_a,person_b,face_a,face_b,same_person,confidence,vlm_model,verified_at) VALUES(7,9,10,12,0,1,'manual',2)")?;
        let tx=conn.unchecked_transaction()?;
        require_unchanged_partition(&tx,"selected",&partition)?;
        super::super::person_cluster_persistence::persist(&tx,&partition.faces,assignments,anchors)?;
        tx.commit()?;
        assert_eq!(conn.query_row("SELECT first_name FROM persons WHERE id=8",[],|row| row.get::<_,String>(0))?,"Newly confirmed");
        let owners:(i64,i64)=conn.query_row("SELECT (SELECT person_id FROM face_prints WHERE id=10),(SELECT person_id FROM face_prints WHERE id=12)",[],|row| Ok((row.get(0)?,row.get(1)?)))?;
        assert_ne!(owners.0,owners.1);
        assert_eq!(owners.1,8);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM face_verifications",[],|row| row.get::<_,i64>(0))?,3);
        Ok(())
    }
}
