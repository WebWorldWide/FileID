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

        let mut faces: Vec<FaceRow> = Vec::new();
        // (b) raw "different people" verdict pairs, loaded here so phase 2 can
        // build that part of the blocked set without touching the DB. The NAME
        // guard is NOT loaded here — it's re-derived in PHASE 3 from the
        // under-lock identity snapshot (see audit C1-023 below), so a rename
        // committed during the lock-free phase-2 window can't unblock a
        // wrong-cluster auto-merge off a stale phase-1 name snapshot.
        let verdict_pairs: Vec<(i64, i64)>;
        {
            let conn = db.lock();

            // (a) Load every face that has an ArcFace embedding.
            //
            // Pre-clustering quality gate (FILEID_FACE_CLUSTER_MIN_QUALITY): faces
            // below this quality produce non-discriminative "noise" embeddings —
            // ground-truth labels showed same-person cosine on such faces is ~0.14
            // (indistinguishable from different-person ~0.16), and they chain
            // through hub faces into low-cohesion mega-cones. Faces at/above it
            // separate cleanly (same ~0.59 vs diff max ~0.47). Gated faces are left
            // UNCLUSTERED (person_id NULL) — still searchable, never in a bad
            // cluster. `face_quality` = YuNet det.score × landmark geometry, so this
            // naturally keeps well-detected frontal faces on any corpus. 0 disables.
            //
            // TRADE-OFF: 0.35 is PRECISION-tuned. On this scanned/old corpus the
            // recurring-face quality median is ~0.37, so 0.35 drops the lower tail —
            // but sub-0.35 faces embed as noise (same-person cosine ~0.14 ==
            // different-person) and can't be correctly clustered anyway, so little
            // ACHIEVABLE recall is lost (it took the owner's labelled F1 0.89→1.0).
            // Full-84k-corpus recall at 0.35 is still owed (NEXT.md); a modern
            // high-quality library sits well above 0.35 so most faces pass. Lower
            // it for a very-low-quality corpus where the People tab comes up sparse.
            let min_cluster_quality: f32 = std::env::var("FILEID_FACE_CLUSTER_MIN_QUALITY")
                .ok()
                .and_then(|s| s.trim().parse::<f32>().ok())
                .filter(|v| v.is_finite())
                .map(|v| v.clamp(0.0, 1.0))
                .unwrap_or(0.35);
            {
                let mut stmt = conn.prepare(
                    "SELECT id, arcface_embedding, COALESCE(face_quality, 0.0) \
                     FROM face_prints \
                     WHERE arcface_embedding IS NOT NULL AND COALESCE(excluded, 0) = 0",
                )?;
                let rows = stmt.query_map([], |r| {
                    let id: i64 = r.get(0)?;
                    let blob: Vec<u8> = r.get(1)?;
                    let quality: f64 = r.get(2)?;
                    Ok((id, blob, quality))
                })?;
                for row in rows {
                    let (id, blob, quality) = row?;
                    if (quality as f32) < min_cluster_quality {
                        continue;
                    }
                    if blob.len() % 4 != 0 || blob.is_empty() {
                        continue;
                    }
                    let mut embedding = Vec::with_capacity(blob.len() / 4);
                    for chunk in blob.chunks_exact(4) {
                        embedding
                            .push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
                    }
                    faces.push(FaceRow {
                        face_id: id,
                        embedding,
                        quality: quality as f32,
                    });
                }
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
        // All face embeddings must share one dimensionality (SFace = 128). A
        // mixed/corrupt set — e.g. legacy 512-d ArcFace rows left over from
        // before the commercial-clean swap, or a truncated blob — would make
        // the clusterer index out of bounds and panic, aborting the whole run
        // (and, pre-B7, the engine). Keep only the dominant dimension so one
        // stray row can neither crash clustering nor hijack the dim by loading
        // first.
        {
            let mut dim_counts: HashMap<usize, usize> = HashMap::new();
            for f in &faces {
                *dim_counts.entry(f.embedding.len()).or_insert(0) += 1;
            }
            if let Some((&modal_dim, _)) = dim_counts.iter().max_by_key(|&(_, c)| *c) {
                let before = faces.len();
                faces.retain(|f| f.embedding.len() == modal_dim);
                let dropped = before - faces.len();
                if dropped > 0 {
                    tracing::warn!(
                        modal_dim,
                        dropped,
                        "[CLUSTER] dropped faces with off-dimension embeddings"
                    );
                }
            }
        }

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
            // Junk-cluster suppression — drop 1–2 face clusters built only from
            // low-quality faces so they don't become spurious singleton persons
            // (the over-split the People tab shows). Pure removal: never merges
            // identities; suppressed faces fall through to person_id = NULL and
            // stay candidates. Mirrors FaceClustering.swift. (face-quality gate)
            let min_size = crate::pipeline::face_clustering::min_cluster_size();
            let q_floor = crate::pipeline::face_clustering::solo_quality_floor();
            let before_supp = an.len();
            let (a, an) = crate::pipeline::face_clustering::suppress_low_quality_micro_clusters(
                &faces, a, an, min_size, q_floor,
            );
            if an.len() != before_supp {
                tracing::info!(
                    before = before_supp,
                    after = an.len(),
                    suppressed = before_supp - an.len(),
                    min_size,
                    q_floor,
                    "[CLUSTER] suppressed low-quality micro-clusters"
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
