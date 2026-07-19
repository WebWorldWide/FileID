//! `runFaceClustering` IPC handler: re-cluster every face in the DB and
//! refresh the People tab. The actual clustering algorithm lives in
//! `pipeline::face_clustering`; this handler loads embeddings, calls the
//! algorithm, and persists the resulting `persons` + `face_prints.person_id`
//! assignments in one transaction.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crate::ipc::{
    sink::Sink, EngineError, EventPayload, FaceClusteringResult, IpcEvent, Wrap,
};
use crate::pipeline::face_clustering::{cluster, ClusterAnchor, ClusterAssignment, FaceRow};
use rusqlite::OptionalExtension;

fn resolve_verdict_face(
    conn: &rusqlite::Connection,
    legacy: Option<i64>,
    file_id: Option<i64>,
    bbox: Option<String>,
) -> anyhow::Result<Option<i64>> {
    if let (Some(file_id), Some(bbox)) = (file_id, bbox) {
        let mut statement = conn.prepare(
            "SELECT id FROM face_prints WHERE file_id = ?1 AND bbox = ?2 ORDER BY id LIMIT 2",
        )?;
        let ids = statement
            .query_map(rusqlite::params![file_id, bbox], |row| row.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        match ids.as_slice() {
            [id] => return Ok(Some(*id)),
            [_, _, ..] => anyhow::bail!("different-people verdict anchor is ambiguous"),
            [] => {}
        }
    }
    let Some(legacy) = legacy else {
        return Ok(None);
    };
    conn.query_row(
        "SELECT id FROM face_prints WHERE id = ?1",
        [legacy],
        |row| row.get::<_, i64>(0),
    )
    .optional()
    .map_err(Into::into)
}

const MAX_DIFFERENT_VERDICTS: usize = 100_000;

fn load_different_verdict_pairs(
    conn: &rusqlite::Connection,
) -> anyhow::Result<HashSet<(i64, i64)>> {
    let mut statement = conn.prepare(
        "SELECT face_a, face_b, file_a, bbox_a, file_b, bbox_b \
         FROM face_verifications \
         WHERE same_person = 0 \
           AND ((face_a IS NOT NULL AND face_b IS NOT NULL) \
                OR (file_a IS NOT NULL AND bbox_a IS NOT NULL \
                    AND file_b IS NOT NULL AND bbox_b IS NOT NULL)) \
         ORDER BY person_a ASC, person_b ASC \
         LIMIT 100001",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, Option<i64>>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.len() > MAX_DIFFERENT_VERDICTS {
        anyhow::bail!("different-people verdict count exceeds the protected clustering limit");
    }
    let mut pairs = BTreeSet::new();
    for (face_a, face_b, file_a, bbox_a, file_b, bbox_b) in rows {
        let (Some(a), Some(b)) = (
            resolve_verdict_face(conn, face_a, file_a, bbox_a)?,
            resolve_verdict_face(conn, face_b, file_b, bbox_b)?,
        ) else {
            continue;
        };
        if a == b {
            anyhow::bail!("different-people verdict resolves both anchors to one face");
        }
        pairs.insert(if a < b { (a, b) } else { (b, a) });
    }
    Ok(pairs.into_iter().collect())
}

fn validate_persist_plan(
    conn: &rusqlite::Connection,
    assignments: &[ClusterAssignment],
    anchors: &[ClusterAnchor],
) -> anyhow::Result<()> {
    let mut anchor_by_cluster = HashMap::new();
    let mut anchor_cluster_by_face = HashMap::new();
    for anchor in anchors {
        if anchor_by_cluster.insert(anchor.cluster_id, anchor).is_some()
            || anchor_cluster_by_face
                .insert(anchor.anchor_face_id, anchor.cluster_id)
                .is_some()
        {
            anyhow::bail!("face clustering produced duplicate persistence anchors");
        }
    }
    let mut seen_faces = HashSet::new();
    let mut matched_anchors = HashSet::new();
    let mut counts: HashMap<i32, u32> = HashMap::new();
    for assignment in assignments {
        if !seen_faces.insert(assignment.face_id)
            || !anchor_by_cluster.contains_key(&assignment.cluster_id)
        {
            anyhow::bail!("face clustering persistence plan is stale or incomplete");
        }
        *counts.entry(assignment.cluster_id).or_default() += 1;
        if anchor_cluster_by_face.get(&assignment.face_id) == Some(&assignment.cluster_id) {
            matched_anchors.insert(assignment.face_id);
        }
    }
    let mut current_count = 0i64;
    for chunk in assignments.chunks(900) {
        let placeholders = std::iter::repeat_n("?", chunk.len()).collect::<Vec<_>>().join(",");
        let sql = format!("SELECT COUNT(*) FROM face_prints WHERE id IN ({placeholders})");
        current_count += conn.query_row(
            &sql,
            rusqlite::params_from_iter(chunk.iter().map(|assignment| assignment.face_id)),
            |row| row.get::<_, i64>(0),
        )?;
    }
    if current_count != assignments.len() as i64 {
        anyhow::bail!("face clustering persistence plan references stale faces");
    }
    for anchor in anchors {
        if counts.get(&anchor.cluster_id).copied().unwrap_or(0) != anchor.member_count
            || !matched_anchors.contains(&anchor.anchor_face_id)
        {
            anyhow::bail!("face clustering persistence anchor does not match its members");
        }
    }
    Ok(())
}

fn protected_owner_ids_to_preserve(
    protected_owner_ids: &HashSet<i64>,
    unknown_owner_ids: &HashSet<i64>,
    pool_face_ids: &HashSet<i64>,
    face_to_owner: &HashMap<i64, i64>,
) -> HashSet<i64> {
    let owners_with_faces: HashSet<i64> = face_to_owner.values().copied().collect();
    let mut preserve = unknown_owner_ids.clone();
    preserve.extend(
        protected_owner_ids
            .iter()
            .copied()
            .filter(|owner_id| !owners_with_faces.contains(owner_id)),
    );
    preserve.extend(face_to_owner.iter().filter_map(|(&face_id, &owner_id)| {
        (protected_owner_ids.contains(&owner_id) && !pool_face_ids.contains(&face_id))
            .then_some(owner_id)
    }));
    preserve
}

fn matched_pool_face_count(
    assignments: &[ClusterAssignment],
    cid_to_person: &HashMap<i32, i64>,
    pool_face_ids: &HashSet<i64>,
    preserved_pool_face_count: usize,
) -> usize {
    assignments
        .iter()
        .filter(|assignment| {
            pool_face_ids.contains(&assignment.face_id)
                && cid_to_person.contains_key(&assignment.cluster_id)
        })
        .count()
        + preserved_pool_face_count
}

fn prior_identity_winners(
    cluster_votes: &HashMap<i32, HashMap<i64, u32>>,
    anchors: &[ClusterAnchor],
    face_to_prior: &HashMap<i64, i64>,
) -> HashMap<i64, i32> {
    let anchor_owner_by_cluster: HashMap<i32, Option<i64>> = anchors
        .iter()
        .map(|anchor| {
            (
                anchor.cluster_id,
                face_to_prior.get(&anchor.anchor_face_id).copied(),
            )
        })
        .collect();
    let mut winners: HashMap<i64, (i32, u32, bool)> = HashMap::new();
    for (&cluster_id, votes) in cluster_votes {
        let anchor_owner = anchor_owner_by_cluster
            .get(&cluster_id)
            .copied()
            .flatten();
        for (&prior_id, &count) in votes {
            let candidate = (cluster_id, count, anchor_owner == Some(prior_id));
            let replace = match winners.get(&prior_id) {
                None => true,
                Some(&(winner_cluster, winner_count, winner_anchor)) => {
                    (count, candidate.2, std::cmp::Reverse(cluster_id))
                        > (
                            winner_count,
                            winner_anchor,
                            std::cmp::Reverse(winner_cluster),
                        )
                }
            };
            if replace {
                winners.insert(prior_id, candidate);
            }
        }
    }
    winners
        .into_iter()
        .map(|(prior_id, winner)| (prior_id, winner.0))
        .collect()
}

fn release_active_before_terminal(active: &AtomicBool) {
    active.store(false, Ordering::Release);
}

pub(crate) async fn handle_run_face_clustering(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    active: Arc<AtomicBool>,
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
            // TRADE-OFF, recalibrated on the full 84,582-face Adlon corpus
            // (2026-07-14 re-cluster sweep, RTX 5080 box): the old 0.35 default —
            // tuned for precision on a 185-face labelled subset — sat at the top
            // of the geometry-capped 0.23–0.42 real-world quality range and left
            // 67% of detected faces unassigned. 0.25 with k_nn=32 doubles
            // assigned faces (27,921 → 53,955) while the biggest clusters get
            // TIGHTER (top-cluster mean cosine-to-centroid 0.606 → 0.642), i.e.
            // the recovered faces are the same people, not contamination. Faces
            // below 0.25 still embed as noise (same-person cosine ~0.14) and
            // stay gated. Raise it for precision-critical small libraries; 0
            // disables the gate entirely.
            let min_cluster_quality: f32 = std::env::var("FILEID_FACE_CLUSTER_MIN_QUALITY")
                .ok()
                .and_then(|s| s.trim().parse::<f32>().ok())
                .filter(|v| v.is_finite())
                .map(|v| v.clamp(0.0, 1.0))
                .unwrap_or(0.25);
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

            // The user-identity snapshot (prior names/title/is_unknown + the
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
        let (assignments, _raw_anchors) = cluster(&faces);

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
        let mut face_to_owner: HashMap<i64, i64> = HashMap::new();
        let mut face_to_prior: HashMap<i64, i64> = HashMap::new();
        {
            let mut stmt = tx.prepare(
                "SELECT id, name, title, first_name, middle_name, last_name, suffix, \
                        COALESCE(is_unknown, 0), created_at \
                 FROM persons \
                 WHERE name IS NOT NULL OR title IS NOT NULL OR first_name IS NOT NULL \
                    OR middle_name IS NOT NULL OR last_name IS NOT NULL OR suffix IS NOT NULL \
                    OR COALESCE(is_unknown, 0) = 1",
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
                face_to_owner.insert(face_id, pid);
                if prior_by_person.contains_key(&pid) {
                    face_to_prior.insert(face_id, pid);
                }
            }
        }
        let verdict_pairs = load_different_verdict_pairs(&tx)?;
        let verdict_owner_ids: HashSet<i64> = verdict_pairs
            .iter()
            .flat_map(|&(a, b)| [a, b])
            .filter_map(|face_id| face_to_owner.get(&face_id).copied())
            .collect();
        let pool_face_ids: HashSet<i64> = faces.iter().map(|face| face.face_id).collect();
        let protected_owner_ids: HashSet<i64> = prior_by_person
            .keys()
            .copied()
            .chain(verdict_owner_ids.iter().copied())
            .collect();
        let unknown_owner_ids: HashSet<i64> = prior_by_person
            .iter()
            .filter_map(|(&person_id, identity)| (identity.is_unknown != 0).then_some(person_id))
            .collect();
        let preserve_owner_ids = protected_owner_ids_to_preserve(
            &protected_owner_ids,
            &unknown_owner_ids,
            &pool_face_ids,
            &face_to_owner,
        );
        let excluded_face_ids: HashSet<i64> = face_to_owner
            .iter()
            .filter_map(|(&face_id, &owner_id)| {
                preserve_owner_ids.contains(&owner_id).then_some(face_id)
            })
            .collect();
        // A different-people verdict endpoint owned by a preserved identity is
        // dropped from the clustering pool (that owner is kept separate
        // wholesale via the preserve set), so it never lands in the partitioned
        // cluster set. Such a pair is already satisfied by preservation; keeping
        // it would make the downstream blocked-set build and
        // validate_protected_clusters fail closed on a face that legitimately
        // left the partition (permanently breaking the People tab, since
        // clustering re-fires after every scan). Retain only pairs whose BOTH
        // endpoints remain in the active pool — a non-excluded endpoint is
        // always re-assigned by partition, so cluster_of lookups below cannot
        // miss. Mirrors FaceClustering.swift's tolerant validate.
        let verdict_pairs: HashSet<(i64, i64)> = verdict_pairs
            .into_iter()
            .filter(|&(a, b)| {
                !excluded_face_ids.contains(&a) && !excluded_face_ids.contains(&b)
            })
            .collect();
        face_to_prior.retain(|_, owner_id| !preserve_owner_ids.contains(owner_id));
        let bucket_owner_by_face: HashMap<i64, i64> = face_to_owner
            .iter()
            .filter_map(|(&face_id, &owner_id)| {
                (!preserve_owner_ids.contains(&owner_id)
                    && (prior_by_person.contains_key(&owner_id)
                        || verdict_owner_ids.contains(&owner_id)))
                .then_some((face_id, owner_id))
            })
            .collect();
        let (assignments, anchors) =
            crate::pipeline::face_clustering::partition_protected_clusters_excluding(
                &faces,
                assignments,
                &bucket_owner_by_face,
                &verdict_pairs,
                &excluded_face_ids,
            );

        // Auto-consolidate near-certain duplicate clusters the over-split-safe
        // clusterer left fragmented (the "WAY too many similar faces" symptom),
        // RIGHT HERE under the persist lock — not in the lock-free phase 2 — so
        // the verification-aware blocked set is built from the same under-lock
        // snapshot the persist below uses. Protected prior owners and explicit
        // different-people anchors were partitioned before this pass and remain
        // transitive cannot-links during consolidation.
        let (assignments, anchors) = {
            let threshold = crate::pipeline::face_clustering::automerge_threshold();
            let cluster_of: HashMap<i64, i32> =
                assignments.iter().map(|a| (a.face_id, a.cluster_id)).collect();
            let mut blocked: std::collections::HashSet<(i32, i32)> =
                std::collections::HashSet::new();

            for &(face_a, face_b) in &verdict_pairs {
                let (Some(&cluster_a), Some(&cluster_b)) =
                    (cluster_of.get(&face_a), cluster_of.get(&face_b))
                else {
                    anyhow::bail!("different-people verdict endpoint left the protected partition");
                };
                if cluster_a == cluster_b {
                    anyhow::bail!("different-people verdict collapsed before consolidation");
                }
                blocked.insert(if cluster_a < cluster_b {
                    (cluster_a, cluster_b)
                } else {
                    (cluster_b, cluster_a)
                });
            }
            let protected_owner_by_cluster =
                crate::pipeline::face_clustering::protected_owner_by_cluster(
                    &face_to_prior,
                    &cluster_of,
                )
                .map_err(anyhow::Error::msg)?;

            let before = anchors.len();
            let (a, an) =
                crate::pipeline::face_clustering::consolidate_with_protected_owners(
                    &faces,
                    assignments,
                    anchors,
                    &blocked,
                    &protected_owner_by_cluster,
                    threshold,
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
            let protected_faces: HashSet<i64> = bucket_owner_by_face
                .keys()
                .copied()
                .chain(verdict_pairs.iter().flat_map(|&(a, b)| [a, b]))
                .collect();
            let always_keep = crate::pipeline::face_clustering::protected_cluster_ids(
                &a,
                &protected_faces,
            );
            let min_size = crate::pipeline::face_clustering::min_cluster_size();
            let q_floor = crate::pipeline::face_clustering::solo_quality_floor();
            let before_supp = an.len();
            let (a, an) =
                crate::pipeline::face_clustering::suppress_low_quality_micro_clusters_with_keep(
                    &faces,
                    a,
                    an,
                    min_size,
                    q_floor,
                    &always_keep,
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
            crate::pipeline::face_clustering::validate_protected_clusters(
                &a,
                &face_to_prior,
                &verdict_pairs,
            )
            .map_err(anyhow::Error::msg)?;
            (a, an)
        };

        validate_persist_plan(&tx, &assignments, &anchors)?;
        let preserved_person_count = preserve_owner_ids.len();
        let preserved_pool_face_count = faces
            .iter()
            .filter(|face| excluded_face_ids.contains(&face.face_id))
            .count();
        tx.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS face_cluster_preserve (id INTEGER PRIMARY KEY);\
             DELETE FROM face_cluster_preserve;",
        )?;
        {
            let mut preserve = tx.prepare("INSERT INTO face_cluster_preserve(id) VALUES (?1)")?;
            for person_id in &preserve_owner_ids {
                preserve.execute([person_id])?;
            }
        }

        // Persist clusters: clear/re-create only identities that are fully
        // represented in this clustering pool. Unknown identities and protected
        // identities with any out-of-pool face remain byte-for-byte intact.
        tx.execute(
            "UPDATE face_prints SET person_id = NULL \
             WHERE person_id IS NULL OR person_id NOT IN (SELECT id FROM face_cluster_preserve)",
            [],
        )?;
        tx.execute(
            "DELETE FROM persons WHERE id NOT IN (SELECT id FROM face_cluster_preserve)",
            [],
        )?;

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

        // One prior identity may span multiple new clusters when an explicit
        // different-person verdict separates two of its old faces. Carry that
        // identity to exactly one deterministic winner; duplicating a name or
        // Unknown marker would manufacture a second user identity.
        let prior_winner_cluster =
            prior_identity_winners(&cluster_votes, &anchors, &face_to_prior);

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
                    if prior_winner_cluster.get(&pid).copied() != Some(anchor.cluster_id)
                    {
                        continue;
                    }
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
        tx.execute("DROP TABLE face_cluster_preserve", [])?;
        tx.commit()?;

        // Faces that stayed person_id=NULL — either they never clustered, or their
        // micro-cluster was suppressed (below min-size / quality). = loaded faces
        // minus those assigned to a promoted person cluster.
        let matched = matched_pool_face_count(
            &assignments,
            &cid_to_person,
            &pool_face_ids,
            preserved_pool_face_count,
        );
        Ok(FaceClusteringResult {
            person_count: (anchors.len() + preserved_person_count) as u32,
            face_count,
            unmatched_faces: face_count.saturating_sub(matched as u64),
            duration_seconds: started.elapsed().as_secs_f64(),
        })
    })
    .await;

    release_active_before_terminal(&active);
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
mod tests {
    use super::*;

    #[test]
    fn terminal_publication_releases_single_flight_first() {
        let active = AtomicBool::new(true);
        release_active_before_terminal(&active);
        assert!(!active.load(Ordering::Acquire));
    }

    fn verdict_db() -> rusqlite::Connection {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE face_prints (id INTEGER PRIMARY KEY, file_id INTEGER, bbox TEXT);\
                 CREATE TABLE face_verifications (\
                    person_a INTEGER NOT NULL, person_b INTEGER NOT NULL,\
                    face_a INTEGER, face_b INTEGER, same_person INTEGER,\
                    file_a INTEGER, bbox_a TEXT, file_b INTEGER, bbox_b TEXT,\
                    PRIMARY KEY(person_a, person_b)\
                 );",
            )
            .unwrap();
        connection
    }

    #[test]
    fn different_verdict_resolves_fresh_stable_anchors() {
        let connection = verdict_db();
        connection
            .execute_batch(
                "INSERT INTO face_prints(id, file_id, bbox) VALUES\
                    (10, 1, 'a'), (20, 2, 'b');\
                 INSERT INTO face_verifications(\
                    person_a, person_b, face_a, face_b, same_person,\
                    file_a, bbox_a, file_b, bbox_b\
                 ) VALUES (100, 200, 1, 2, 0, 1, 'a', 2, 'b');",
            )
            .unwrap();
        let pairs = load_different_verdict_pairs(&connection).unwrap();
        assert_eq!(pairs, [(10, 20)].into_iter().collect());
    }

    #[test]
    fn verdict_limit_fails_closed_before_resolution_work() {
        let mut connection = verdict_db();
        connection
            .execute_batch("INSERT INTO face_prints(id, file_id, bbox) VALUES (1, 1, 'a'), (2, 2, 'b')")
            .unwrap();
        let transaction = connection.transaction().unwrap();
        {
            let mut insert = transaction
                .prepare(
                    "INSERT INTO face_verifications(\
                        person_a, person_b, face_a, face_b, same_person\
                     ) VALUES (?1, ?2, 1, 2, 0)",
                )
                .unwrap();
            for index in 0..=MAX_DIFFERENT_VERDICTS {
                insert
                    .execute(rusqlite::params![index as i64, index as i64 + 200_000])
                    .unwrap();
            }
        }
        transaction.commit().unwrap();
        assert!(load_different_verdict_pairs(&connection).is_err());
    }

    #[test]
    fn production_verdict_schema_without_rows_is_accepted() {
        let connection = verdict_db();
        assert!(load_different_verdict_pairs(&connection).unwrap().is_empty());
    }

    #[test]
    fn protected_owner_outside_pool_or_without_faces_is_preserved() {
        let protected = HashSet::from([10, 20, 30]);
        let unknown = HashSet::from([40]);
        let pool = HashSet::from([1, 3]);
        let face_to_owner = HashMap::from([(1, 10), (2, 20), (3, 40)]);
        assert_eq!(
            protected_owner_ids_to_preserve(&protected, &unknown, &pool, &face_to_owner),
            HashSet::from([20, 30, 40])
        );
    }

    #[test]
    fn one_prior_identity_has_one_deterministic_inheritance_winner() {
        let votes = HashMap::from([
            (7, HashMap::from([(42, 1)])),
            (3, HashMap::from([(42, 1)])),
        ]);
        let anchors = vec![
            ClusterAnchor {
                cluster_id: 7,
                anchor_face_id: 70,
                member_count: 1,
            },
            ClusterAnchor {
                cluster_id: 3,
                anchor_face_id: 30,
                member_count: 1,
            },
        ];
        let face_to_prior = HashMap::from([(70, 42), (30, 42)]);
        assert_eq!(
            prior_identity_winners(&votes, &anchors, &face_to_prior),
            HashMap::from([(42, 3)])
        );
    }

    #[test]
    fn matched_count_ignores_out_of_pool_verdict_singletons() {
        let assignments = vec![
            ClusterAssignment { face_id: 1, cluster_id: 1 },
            ClusterAssignment { face_id: 99, cluster_id: 2 },
        ];
        assert_eq!(
            matched_pool_face_count(
                &assignments,
                &HashMap::from([(1, 10), (2, 20)]),
                &HashSet::from([1, 2]),
                1,
            ),
            2
        );
    }

    #[test]
    fn destructive_persistence_sql_rolls_back_atomically() {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;\
                 CREATE TABLE persons (id INTEGER PRIMARY KEY);\
                 CREATE TABLE face_prints (\
                    id INTEGER PRIMARY KEY,\
                    person_id INTEGER REFERENCES persons(id)\
                 );\
                 INSERT INTO persons(id) VALUES (1), (2);\
                 INSERT INTO face_prints(id, person_id) VALUES (10, 1), (20, 2);",
            )
            .unwrap();
        let transaction = connection.transaction().unwrap();
        transaction
            .execute_batch(
                "CREATE TEMP TABLE face_cluster_preserve (id INTEGER PRIMARY KEY);\
                 INSERT INTO face_cluster_preserve(id) VALUES (1);\
                 UPDATE face_prints SET person_id = NULL \
                    WHERE person_id NOT IN (SELECT id FROM face_cluster_preserve);\
                 DELETE FROM persons WHERE id NOT IN (SELECT id FROM face_cluster_preserve);",
            )
            .unwrap();
        transaction.rollback().unwrap();
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM persons", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT COUNT(*) FROM face_prints WHERE person_id IS NOT NULL",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            2
        );
    }

    #[test]
    fn persistence_plan_rejects_missing_anchor_members() {
        let connection = verdict_db();
        connection
            .execute("INSERT INTO face_prints(id, file_id, bbox) VALUES (10, 1, 'a')", [])
            .unwrap();
        let assignments = vec![ClusterAssignment {
            face_id: 10,
            cluster_id: 1,
        }];
        let anchors = vec![ClusterAnchor {
            cluster_id: 1,
            anchor_face_id: 11,
            member_count: 1,
        }];
        assert!(validate_persist_plan(&connection, &assignments, &anchors).is_err());
    }

    #[test]
    fn ambiguous_stable_verdict_anchor_fails_closed() {
        let connection = verdict_db();
        connection
            .execute_batch(
                "INSERT INTO face_prints(id, file_id, bbox) VALUES\
                    (10, 1, 'a'), (11, 1, 'a'), (20, 2, 'b');\
                 INSERT INTO face_verifications(\
                    person_a, person_b, face_a, face_b, same_person,\
                    file_a, bbox_a, file_b, bbox_b\
                 ) VALUES (100, 200, 10, 20, 0, 1, 'a', 2, 'b');",
            )
            .unwrap();
        assert!(load_different_verdict_pairs(&connection).is_err());
    }
}
