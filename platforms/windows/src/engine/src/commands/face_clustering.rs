//! `runFaceClustering` IPC handler: re-cluster every face in the DB and
//! refresh the People tab. The actual clustering algorithm lives in
//! `pipeline::face_clustering`; this handler loads embeddings, calls the
//! algorithm, and persists the resulting `persons` + `face_prints.person_id`
//! assignments in one transaction.

use std::collections::HashMap;
use std::time::Instant;

use crate::ipc::{
    sink::Sink, EngineError, EventPayload, FaceClusteringResult, IpcEvent, Wrap,
};
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
            && revision == current && blob.len() == 512;
        anyhow::ensure!(compatible, "Face caches need a compatible refresh before clustering; existing People were preserved.");
        let key = (model.unwrap_or_default(), processing.unwrap_or_default());
        anyhow::ensure!(space.as_ref().is_none_or(|s| *s == key), "Mixed face-model or processing spaces cannot be compared; existing People were preserved.");
        space = Some(key);
        let embedding: Vec<f32> = blob.as_chunks::<4>().0.iter().map(|v| f32::from_le_bytes(*v)).collect();
        let norm: f64 = embedding.iter().map(|v| f64::from(*v).powi(2)).sum();
        anyhow::ensure!(embedding.iter().all(|v| v.is_finite()) && (0.95..=1.05).contains(&norm), "Invalid face vector; existing People were preserved.");
        anyhow::ensure!(faces.len() < 200_000, "Face clustering exceeds the safe batch size; existing People were preserved.");
        faces.push(FaceRow { face_id: row.get(0)?, file_id: row.get(1)?, embedding, quality: row.get::<_,f64>(3)? as f32 });
    }
    Ok(faces)
}

pub(crate) async fn handle_run_face_clustering(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
) {
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<FaceClusteringResult> {
        let started = Instant::now();

        // PHASE 1 — hold the writer lock only for the DB reads. The multi-second
        // cluster() pass below runs lock-free so suggested-merges (a read-only
        // query) and other writes don't serialize behind it. consolidate() —
        // which is cheap relative to clustering — runs in phase 3 under the
        // persist lock, so its name-based auto-merge guard sees the same snapshot
        // the persist uses (audit C1-023). The engine-side single-flight guard
        // (main.rs) keeps two clustering runs from racing; a face inserted between
        // phase 1 and phase 3 is benign — it lands with person_id=NULL and is
        // picked up next run.
        struct PriorIdentity {
            name: Option<String>,
            title: Option<String>,
            first_name: Option<String>,
            middle_name: Option<String>,
            last_name: Option<String>,
            suffix: Option<String>,
            is_unknown: i64,
            created_at: f64,
        }

        let faces: Vec<FaceRow>;
        // (b) raw "different people" verdict pairs, loaded here so phase 2 can
        // build that part of the blocked set without touching the DB. The NAME
        // guard is NOT loaded here — it's re-derived in PHASE 3 from the
        // under-lock identity snapshot (see audit C1-023 below), so a rename
        // committed during the lock-free phase-2 window can't unblock a
        // wrong-cluster auto-merge off a stale phase-1 name snapshot.
        let verdict_pairs: Vec<(i64, i64)>;
        {
            let conn = db.lock();

            faces = load_compatible_faces(&conn)?;
            if faces.is_empty() {
                return Ok(FaceClusteringResult {
                    person_count: conn.query_row("SELECT COUNT(*) FROM persons", [], |r| r.get(0))?,
                    face_count: 0,
                    unmatched_faces: 0,
                    duration_seconds: started.elapsed().as_secs_f64(),
                });
            }

            // (b) Raw "different people" verdict pairs. Re-projected onto the faces'
            // CURRENT clusters in phase 2. R3-15: resolve each anchor by its
            // churn-stable (file_id, bbox) key — which a faces_evaluated re-scan
            // preserves — to the face id that CURRENTLY occupies that slot, instead
            // of the legacy face_a/face_b id that the re-scan's DELETE+INSERT churns.
            // Falls back to the legacy id for pre-v17 rows / unresolvable keys; if
            // neither resolves, the pair is dropped (guard (c) still backstops).
            verdict_pairs = {
                let mut vstmt = conn.prepare(
                    "SELECT face_a, face_b, file_a, bbox_a, file_b, bbox_b \
                     FROM face_verifications \
                     WHERE same_person = 0 \
                       AND ((face_a IS NOT NULL AND face_b IS NOT NULL) \
                            OR (file_a IS NOT NULL AND bbox_a IS NOT NULL \
                                AND file_b IS NOT NULL AND bbox_b IS NOT NULL))",
                )?;
                let raw = vstmt
                    .query_map([], |r| {
                        Ok((
                            r.get::<_, Option<i64>>(0)?,
                            r.get::<_, Option<i64>>(1)?,
                            r.get::<_, Option<i64>>(2)?,
                            r.get::<_, Option<String>>(3)?,
                            r.get::<_, Option<i64>>(4)?,
                            r.get::<_, Option<String>>(5)?,
                        ))
                    })?
                    .filter_map(|r| r.ok())
                    .collect::<Vec<_>>();
                let resolve = |legacy: Option<i64>, file: Option<i64>, bbox: Option<String>| -> Option<i64> {
                    if let (Some(f), Some(b)) = (file, bbox) {
                        if let Ok(id) = conn.query_row(
                            "SELECT id FROM face_prints WHERE file_id = ?1 AND bbox = ?2 LIMIT 1",
                            rusqlite::params![f, b],
                            |r| r.get::<_, i64>(0),
                        ) {
                            return Some(id);
                        }
                    }
                    match legacy {
                        Some(l)
                            if conn
                                .query_row("SELECT 1 FROM face_prints WHERE id = ?1", [l], |_| Ok(()))
                                .is_ok() =>
                        {
                            Some(l)
                        }
                        _ => None,
                    }
                };
                raw.into_iter()
                    .filter_map(|(fa, fb, file_a, bbox_a, file_b, bbox_b)| {
                        match (resolve(fa, file_a, bbox_a), resolve(fb, file_b, bbox_b)) {
                            (Some(a), Some(b)) => Some((a, b)),
                            _ => None,
                        }
                    })
                    .collect::<Vec<(i64, i64)>>()
            };

            // (c) The user-identity snapshot (prior names/title/is_unknown + the
            // face->person map) is read in PHASE 3 under the persist lock — NOT
            // here — so a People-tab edit (rename / merge / mark-unknown) that
            // commits during the lock-free phase 2 is carried forward instead of
            // being silently clobbered by a phase-1 snapshot that predates it.
            // (audit S0)
            drop(conn);
        }

        // PHASE 2 — no lock held, zero DB access. Pure in-memory clustering.
        let face_count = faces.len() as u64;
        // Raw clustering only. Auto-consolidation (which applies the name-based
        // auto-merge guard) is deferred to PHASE 3 so the name guard can be built
        // from the identity snapshot read UNDER the persist lock, not from a
        // phase-1 snapshot that a rename during the lock-free window would
        // invalidate. (audit C1-023)
        let (assignments, anchors) = cluster(&faces);

        // PHASE 3 — re-acquire the writer lock for the persist transaction.
        let conn = db.lock();
        let tx = conn.unchecked_transaction()?;
        let current = load_compatible_faces(&tx)?;
        anyhow::ensure!(current.len() == faces.len() && current.iter().zip(&faces).all(|(a,b)| a.face_id == b.face_id && a.file_id == b.file_id && a.embedding == b.embedding), "Face caches changed during clustering; existing People were preserved.");


        // Read the user-identity snapshot HERE — under the persist lock, inside
        // the transaction, BEFORE the DELETE below — rather than in phase 1.
        // Re-clustering drops + re-creates the persons table on EVERY run and is
        // auto-fired after every scan, so the names + "not this person" verdicts
        // the user entered must be carried forward. Reading it now (not from a
        // phase-1 snapshot) means a People-tab edit committed during the lock-free
        // phase 2 — which had to take this same writer lock — is reflected, instead
        // of being silently overwritten by a stale capture (data loss). We re-attach
        // each new cluster's identity from the prior person that owned the MAJORITY
        // of its member faces (ties broken toward the cluster's anchor face).
        // (audit S0)  [PriorIdentity is defined at the top of this closure.]
        let mut prior_by_person: HashMap<i64, PriorIdentity> = HashMap::new();
        let mut face_to_prior: HashMap<i64, i64> = HashMap::new();
        {
            let mut stmt = tx.prepare(
                "SELECT id, name, title, first_name, middle_name, last_name, suffix, \
                        COALESCE(is_unknown, 0), created_at \
                 FROM persons \
                 WHERE name IS NOT NULL OR COALESCE(is_unknown, 0) = 1",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    PriorIdentity {
                        name: r.get(1)?,
                        title: r.get(2)?,
                        first_name: r.get(3)?,
                        middle_name: r.get(4)?,
                        last_name: r.get(5)?,
                        suffix: r.get(6)?,
                        is_unknown: r.get(7)?,
                        created_at: r.get(8)?,
                    },
                ))
            })?;
            for row in rows {
                let (id, ident) = row?;
                prior_by_person.insert(id, ident);
            }
        }
        {
            let mut stmt =
                tx.prepare("SELECT id, person_id FROM face_prints WHERE person_id IS NOT NULL")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
            for row in rows {
                let (face_id, pid) = row?;
                if prior_by_person.contains_key(&pid) {
                    face_to_prior.insert(face_id, pid);
                }
            }
        }

        // Auto-consolidate near-certain duplicate clusters the over-split-safe
        // clusterer left fragmented (the "WAY too many similar faces" symptom),
        // RIGHT HERE under the persist lock — not in the lock-free phase 2 — so
        // the verification-aware blocked set is built from the same under-lock
        // snapshot the persist below uses. Two blocked-pair sources keep a
        // confirmed split from being silently re-merged:
        let (assignments, anchors) = {
            let threshold = crate::pipeline::face_clustering::automerge_threshold();
            let cluster_of: HashMap<i64, i32> =
                assignments.iter().map(|a| (a.face_id, a.cluster_id)).collect();
            let mut blocked: std::collections::HashSet<(i32, i32)> =
                std::collections::HashSet::new();

            // (a) Explicit "different people" verdicts, re-projected onto the
            // faces' CURRENT clusters. Precise, but the link rides face_prints.id,
            // which a faces_evaluated re-scan churns (DELETE+INSERT) — after which
            // a stored verdict's faces no longer resolve. Guard (b) backstops that.
            // Reads the pre-loaded `verdict_pairs` (phase 1), not the DB.
            for &(fa, fb) in &verdict_pairs {
                if let (Some(&ca), Some(&cb)) = (cluster_of.get(&fa), cluster_of.get(&fb)) {
                    if ca != cb {
                        blocked.insert(if ca < cb { (ca, cb) } else { (cb, ca) });
                    }
                }
            }

            // (b) Never auto-merge two clusters carrying DIFFERENT user-assigned
            // names. The face→name mapping is RE-DERIVED here from the under-lock
            // phase-3 identity snapshot (`face_to_prior` + `prior_by_person`),
            // exactly like the S0 identity carry-forward — NOT from a phase-1
            // `name_rows` capture. A rename committed during the lock-free phase-2
            // window (it had to take this same writer lock) is therefore reflected
            // in the guard, so it can never unblock a wrong-cluster auto-merge off
            // a stale name. Same-named fragments and named+unnamed pairs still
            // merge (the intended consolidation). (audit C1-023)
            let face_name: HashMap<i64, String> = face_to_prior
                .iter()
                .filter_map(|(&fid, &pid)| {
                    prior_by_person
                        .get(&pid)
                        .and_then(|p| p.name.clone())
                        .map(|name| (fid, name))
                })
                .collect();
            for pair in
                crate::pipeline::face_clustering::name_blocked_pairs(&face_name, &cluster_of)
            {
                blocked.insert(pair);
            }

            // (c) Never fold a user-marked UNKNOWN cluster into a different prior
            // person. mark-as-unknown nulls the name, so guard (b) is blind to it;
            // without this the unknown cluster consolidates into a named/other
            // person and the majority-vote persist (below) can overwrite a
            // user-assigned name with NULL. Keyed on is_unknown=1 (not name=NULL),
            // so untouched auto-clustered persons still consolidate normally. (R3-03)
            let unknown_persons: std::collections::HashSet<i64> = prior_by_person
                .iter()
                .filter_map(|(&pid, p)| (p.is_unknown == 1).then_some(pid))
                .collect();
            for pair in crate::pipeline::face_clustering::unknown_blocked_pairs(
                &face_to_prior,
                &unknown_persons,
                &cluster_of,
            ) {
                blocked.insert(pair);
            }

            let before = anchors.len();
            let (a, an) = crate::pipeline::face_clustering::consolidate(
                &faces, assignments, anchors, &blocked, threshold,
            );
            if an.len() != before {
                tracing::info!(
                    before,
                    after = an.len(),
                    merged = before - an.len(),
                    threshold,
                    "[CLUSTER] auto-consolidated near-duplicate clusters"
                );
            }
            (a, an)
        };

        // Persist clusters: clear existing person_id assignments + persons,
        // re-create one persons row per anchor, point face_prints at it.
        tx.execute("UPDATE face_prints SET person_id = NULL", [])?;
        tx.execute("DELETE FROM persons", [])?;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);

        // Per new cluster, tally which prior identity owned the most member faces.
        let mut cluster_votes: HashMap<i32, HashMap<i64, u32>> = HashMap::new();
        for a in &assignments {
            if let Some(&pid) = face_to_prior.get(&a.face_id) {
                *cluster_votes
                    .entry(a.cluster_id)
                    .or_default()
                    .entry(pid)
                    .or_insert(0) += 1;
            }
        }

        // Map cluster_id (1-based) → DB person row id.
        let mut cid_to_person: HashMap<i32, i64> = HashMap::new();
        for anchor in &anchors {
            // Winning prior person: most member faces; tie → owner of this
            // cluster's anchor face, else lowest prior person id (determinism).
            let mut best: Option<(i64, u32)> = None;
            if let Some(votes) = cluster_votes.get(&anchor.cluster_id) {
                let anchor_owner = face_to_prior.get(&anchor.anchor_face_id).copied();
                // Rank key (higher wins): most votes, then this cluster's anchor
                // owner, then lowest prior person id (Reverse) for determinism.
                let key = |pid: i64, count: u32| {
                    (count, Some(pid) == anchor_owner, std::cmp::Reverse(pid))
                };
                for (&pid, &count) in votes {
                    let better = match best {
                        None => true,
                        Some((bpid, bcount)) => key(pid, count) > key(bpid, bcount),
                    };
                    if better {
                        best = Some((pid, count));
                    }
                }
            }
            let inherited = best.and_then(|(pid, _)| prior_by_person.get(&pid));
            let created = inherited.map(|i| i.created_at).unwrap_or(now);

            tx.execute(
                "INSERT INTO persons \
                   (name, title, first_name, middle_name, last_name, suffix, is_unknown, \
                    representative_face_id, file_count, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                rusqlite::params![
                    inherited.and_then(|i| i.name.clone()),
                    inherited.and_then(|i| i.title.clone()),
                    inherited.and_then(|i| i.first_name.clone()),
                    inherited.and_then(|i| i.middle_name.clone()),
                    inherited.and_then(|i| i.last_name.clone()),
                    inherited.and_then(|i| i.suffix.clone()),
                    inherited.map(|i| i.is_unknown).unwrap_or(0),
                    anchor.anchor_face_id,
                    anchor.member_count as i64,
                    created,
                ],
            )?;
            let person_id = tx.last_insert_rowid();
            cid_to_person.insert(anchor.cluster_id, person_id);
        }

        let mut update = tx.prepare("UPDATE face_prints SET person_id = ?1 WHERE id = ?2")?;
        for a in &assignments {
            if let Some(&pid) = cid_to_person.get(&a.cluster_id) {
                update.execute(rusqlite::params![pid, a.face_id])?;
            }
        }
        drop(update);
        tx.commit()?;

        Ok(FaceClusteringResult {
            person_count: anchors.len() as u32,
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
        for fault in ["model", "processing", "legacy", "revision", "nan", "dimension"] {
            let conn = rusqlite::Connection::open_in_memory()?;
            conn.execute_batch("CREATE TABLE face_prints(id INTEGER,file_id INTEGER,arcface_embedding BLOB,face_quality REAL,excluded INTEGER,embedding_model TEXT,processing_version TEXT,source_revision TEXT); CREATE TABLE catalog_revisions(file_id INTEGER,revision TEXT);")?;
            let mut vector = vec![0.0f32;128]; vector[0]=1.0;
            let blob: Vec<u8> = vector.iter().flat_map(|v| v.to_le_bytes()).collect();
            for id in 1..=2 {
                conn.execute("INSERT INTO catalog_revisions VALUES(?1,'current')", [id])?;
                conn.execute("INSERT INTO face_prints VALUES(?1,?1,?2,1,0,'weights','aligned','current')", rusqlite::params![id,blob])?;
            }
            assert_eq!(load_compatible_faces(&conn)?.len(),2);
            match fault {
                "model" => {conn.execute("UPDATE face_prints SET embedding_model='other' WHERE id=2", [])?;}
                "processing" => {conn.execute("UPDATE face_prints SET processing_version='other' WHERE id=2", [])?;}
                "legacy" => {conn.execute("UPDATE face_prints SET embedding_model=NULL WHERE id=2", [])?;}
                "revision" => {conn.execute("UPDATE face_prints SET source_revision='old' WHERE id=2", [])?;}
                "nan" => {conn.execute("UPDATE face_prints SET arcface_embedding=?1 WHERE id=2", [vec![f32::NAN.to_le_bytes();128].concat()])?;}
                _ => {conn.execute("UPDATE face_prints SET arcface_embedding=X'00' WHERE id=2", [])?;}
            }
            assert!(load_compatible_faces(&conn).is_err(), "{fault}");
            assert_eq!(conn.query_row("SELECT count(*) FROM face_prints",[], |r|r.get::<_,i64>(0))?,2);
        }
        Ok(())
    }
    #[tokio::test]
    async fn handler_preserves_people_on_incompatible_or_empty_caches() -> anyhow::Result<()> {
        for empty in [false,true] {
            let conn = rusqlite::Connection::open_in_memory()?;
            conn.execute_batch("CREATE TABLE persons(id INTEGER,name TEXT); INSERT INTO persons VALUES(8,'Confirmed'); CREATE TABLE face_prints(id INTEGER,file_id INTEGER,person_id INTEGER,arcface_embedding BLOB,face_quality REAL,excluded INTEGER,embedding_model TEXT,processing_version TEXT,source_revision TEXT); CREATE TABLE catalog_revisions(file_id INTEGER,revision TEXT);")?;
            if !empty {
                let mut vector = vec![0.0f32;128]; vector[0]=1.0;
                let blob: Vec<u8> = vector.iter().flat_map(|v|v.to_le_bytes()).collect();
                for id in 1..=2 {
                    conn.execute("INSERT INTO catalog_revisions VALUES(?1,'current')", [id])?;
                    conn.execute("INSERT INTO face_prints VALUES(?1,?1,8,?2,1,0,?3,'aligned','current')", rusqlite::params![id,blob,format!("different-weights-{id}")])?;
                }
            }
            let db = std::sync::Arc::new(parking_lot::Mutex::new(conn));
            let (sink,mut rx) = Sink::channel_for_test(4);
            handle_run_face_clustering(sink,db.clone()).await;
            let event = rx.recv().await.ok_or_else(||anyhow::anyhow!("missing terminal event"))?;
            if empty { assert!(matches!(event.payload,EventPayload::FaceClusteringComplete(_))); }
            else { assert!(matches!(event.payload,EventPayload::Error(_))); }
            assert_eq!(db.lock().query_row("SELECT name FROM persons WHERE id=8",[],|r|r.get::<_,String>(0))?,"Confirmed");
            if !empty { assert_eq!(db.lock().query_row("SELECT COUNT(*) FROM face_prints WHERE person_id=8",[],|r|r.get::<_,i64>(0))?,2); }
        }
        Ok(())
    }

}
