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
use crate::platform::SleepGuard;
use crate::pipeline::face_clustering::{cluster, ClusterAnchor, ClusterAssignment, FaceRow};
use rusqlite::OptionalExtension;

#[derive(Debug, Clone)]
struct PriorIdentity {
    name: Option<String>,
    title: Option<String>,
    first_name: Option<String>,
    middle_name: Option<String>,
    last_name: Option<String>,
    suffix: Option<String>,
    is_unknown: i64,
    created_at: f64,
    centroid: Option<Vec<f32>>,
    anchor_radius: Option<f32>,
    face_ids: HashSet<i64>,
}

fn decode_centroid(blob: Option<Vec<u8>>) -> Option<Vec<f32>> {
    let blob = blob?;
    if blob.is_empty() || blob.len() % 4 != 0 {
        return None;
    }
    let mut centroid = Vec::with_capacity(blob.len() / 4);
    for chunk in blob.chunks_exact(4) {
        let value = f32::from_le_bytes(chunk.try_into().ok()?);
        if !value.is_finite() {
            return None;
        }
        centroid.push(value);
    }
    let norm = centroid
        .iter()
        .map(|value| value * value)
        .sum::<f32>()
        .sqrt();
    if !norm.is_finite() || norm <= f32::MIN_POSITIVE {
        return None;
    }
    for value in &mut centroid {
        *value /= norm;
    }
    Some(centroid)
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct ManualMergeConstraint {
    person_a: i64,
    person_b: i64,
    face_a: i64,
    face_b: i64,
}

fn load_manual_merge_constraints(
    conn: &rusqlite::Connection,
) -> anyhow::Result<Vec<ManualMergeConstraint>> {
    let mut statement = conn.prepare(
        "SELECT person_a, person_b, face_a, face_b, file_a, bbox_a, file_b, bbox_b \
         FROM face_verifications \
         WHERE same_person = 1 AND vlm_model = 'user-merged' \
           AND ((face_a IS NOT NULL AND face_b IS NOT NULL) \
                OR (file_a IS NOT NULL AND bbox_a IS NOT NULL \
                    AND file_b IS NOT NULL AND bbox_b IS NOT NULL)) \
         ORDER BY person_a ASC, person_b ASC \
         LIMIT 100001",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.len() > MAX_DIFFERENT_VERDICTS {
        anyhow::bail!("manual-merge constraint count exceeds protected clustering limit");
    }
    let mut constraints = Vec::new();
    for (person_a, person_b, face_a, face_b, file_a, bbox_a, file_b, bbox_b) in rows {
        let (Some(face_a), Some(face_b)) = (
            resolve_verdict_face(conn, face_a, file_a, bbox_a)?,
            resolve_verdict_face(conn, face_b, file_b, bbox_b)?,
        ) else {
            continue;
        };
        if face_a == face_b || person_a == person_b {
            continue;
        }
        constraints.push(ManualMergeConstraint {
            person_a,
            person_b,
            face_a,
            face_b,
        });
    }
    Ok(constraints)
}

fn manual_merge_owner_map(
    constraints: &[ManualMergeConstraint],
    face_to_owner: &HashMap<i64, i64>,
    existing_person_ids: &HashSet<i64>,
) -> (HashMap<i64, i64>, HashSet<i64>) {
    let mut person_ids: Vec<i64> = constraints
        .iter()
        .flat_map(|constraint| [constraint.person_a, constraint.person_b])
        .collect();
    person_ids.sort_unstable();
    person_ids.dedup();
    let index_by_person: HashMap<i64, usize> = person_ids
        .iter()
        .enumerate()
        .map(|(index, &person_id)| (person_id, index))
        .collect();
    let mut parent: Vec<usize> = (0..person_ids.len()).collect();
    fn root(parent: &mut [usize], mut index: usize) -> usize {
        while parent[index] != index {
            parent[index] = parent[parent[index]];
            index = parent[index];
        }
        index
    }
    for constraint in constraints {
        let a = root(&mut parent, index_by_person[&constraint.person_a]);
        let b = root(&mut parent, index_by_person[&constraint.person_b]);
        if a != b {
            let (keep, drop) = if person_ids[a] <= person_ids[b] {
                (a, b)
            } else {
                (b, a)
            };
            parent[drop] = keep;
        }
    }
    let mut people_by_root: HashMap<usize, BTreeSet<i64>> = HashMap::new();
    let mut faces_by_root: HashMap<usize, BTreeSet<i64>> = HashMap::new();
    for constraint in constraints {
        let component = root(&mut parent, index_by_person[&constraint.person_a]);
        people_by_root
            .entry(component)
            .or_default()
            .extend([constraint.person_a, constraint.person_b]);
        faces_by_root
            .entry(component)
            .or_default()
            .extend([constraint.face_a, constraint.face_b]);
    }
    let mut owner_by_face = HashMap::new();
    let mut manual_owner_ids = HashSet::new();
    let mut canonical_by_current_owner = HashMap::new();
    for (component, component_faces) in faces_by_root {
        let current_owners: BTreeSet<i64> = component_faces
            .iter()
            .filter_map(|face_id| face_to_owner.get(face_id).copied())
            .collect();
        let canonical_owner = if current_owners.is_empty() {
            let existing: Vec<i64> = people_by_root[&component]
                .iter()
                .copied()
                .filter(|person_id| existing_person_ids.contains(person_id))
                .collect();
            (existing.len() == 1).then_some(existing[0])
        } else {
            current_owners
                .iter()
                .copied()
                .find(|owner_id| {
                    people_by_root[&component].contains(owner_id)
                        && existing_person_ids.contains(owner_id)
                })
                .or_else(|| {
                    current_owners
                        .iter()
                        .copied()
                        .find(|owner_id| existing_person_ids.contains(owner_id))
                })
        };
        let Some(canonical_owner) = canonical_owner else {
            continue;
        };
        manual_owner_ids.insert(canonical_owner);
        canonical_by_current_owner.insert(canonical_owner, canonical_owner);
        for owner_id in current_owners {
            canonical_by_current_owner.insert(owner_id, canonical_owner);
        }
        for face_id in component_faces {
            owner_by_face.insert(face_id, canonical_owner);
        }
    }
    for (&face_id, &owner_id) in face_to_owner {
        if let Some(&canonical_owner) = canonical_by_current_owner.get(&owner_id) {
            owner_by_face.insert(face_id, canonical_owner);
        }
    }
    (owner_by_face, manual_owner_ids)
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

fn match_prior_identities(
    assignments: &[ClusterAssignment],
    identity_anchors: &HashMap<i32, crate::pipeline::face_clustering::IdentityAnchor>,
    priors: &HashMap<i64, PriorIdentity>,
) -> HashMap<i32, i64> {
    let mut faces_by_cluster: HashMap<i32, HashSet<i64>> = HashMap::new();
    for assignment in assignments {
        faces_by_cluster
            .entry(assignment.cluster_id)
            .or_default()
            .insert(assignment.face_id);
    }
    let mut prior_ids: Vec<i64> = priors.keys().copied().collect();
    prior_ids.sort_unstable();
    let mut overlap_candidates = Vec::new();
    for prior_id in &prior_ids {
        let prior = &priors[prior_id];
        if prior.face_ids.is_empty() {
            continue;
        }
        let mut best: Option<(usize, i32)> = None;
        for (&cluster_id, face_ids) in &faces_by_cluster {
            let overlap = prior.face_ids.intersection(face_ids).count();
            if overlap == 0 {
                continue;
            }
            let candidate = (overlap, std::cmp::Reverse(cluster_id));
            if best
                .map(|(count, id)| candidate > (count, std::cmp::Reverse(id)))
                .unwrap_or(true)
            {
                best = Some((overlap, cluster_id));
            }
        }
        let threshold = prior.face_ids.len().div_ceil(2).max(1);
        if let Some((overlap, cluster_id)) = best.filter(|(overlap, _)| *overlap >= threshold) {
            overlap_candidates.push((overlap, *prior_id, cluster_id));
        }
    }
    overlap_candidates.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    let mut claimed_priors = HashSet::new();
    let mut claimed_clusters = HashSet::new();
    let mut matched = HashMap::new();
    for (_, prior_id, cluster_id) in overlap_candidates {
        if claimed_priors.contains(&prior_id) || claimed_clusters.contains(&cluster_id) {
            continue;
        }
        claimed_priors.insert(prior_id);
        claimed_clusters.insert(cluster_id);
        matched.insert(cluster_id, prior_id);
    }

    let mut centroid_candidates = Vec::new();
    for prior_id in prior_ids {
        if claimed_priors.contains(&prior_id) {
            continue;
        }
        let prior = &priors[&prior_id];
        let (Some(prior_centroid), Some(radius)) =
            (prior.centroid.as_ref(), prior.anchor_radius)
        else {
            continue;
        };
        if !radius.is_finite() || !(0.0..=1.0).contains(&radius) {
            continue;
        }
        for (&cluster_id, anchor) in identity_anchors {
            if claimed_clusters.contains(&cluster_id)
                || prior_centroid.len() != anchor.centroid.len()
            {
                continue;
            }
            let similarity: f32 = prior_centroid
                .iter()
                .zip(&anchor.centroid)
                .map(|(a, b)| a * b)
                .sum();
            if similarity.is_finite() && similarity >= radius {
                centroid_candidates.push((similarity, prior_id, cluster_id));
            }
        }
    }
    centroid_candidates.sort_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    for (_, prior_id, cluster_id) in centroid_candidates {
        if claimed_priors.contains(&prior_id) || claimed_clusters.contains(&cluster_id) {
            continue;
        }
        claimed_priors.insert(prior_id);
        claimed_clusters.insert(cluster_id);
        matched.insert(cluster_id, prior_id);
    }
    matched
}

fn insert_cluster_person(
    conn: &rusqlite::Connection,
    inherited: Option<&PriorIdentity>,
    anchor: &ClusterAnchor,
    identity_anchor: &crate::pipeline::face_clustering::IdentityAnchor,
    file_count: i64,
    now: f64,
) -> rusqlite::Result<i64> {
    let centroid_blob: Vec<u8> = identity_anchor
        .centroid
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect();
    let created = inherited.map(|identity| identity.created_at).unwrap_or(now);
    conn.execute(
        "INSERT INTO persons \
           (name, title, first_name, middle_name, last_name, suffix, is_unknown, \
            representative_face_id, file_count, created_at, centroid, anchor_radius, \
            last_clustered_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        rusqlite::params![
            inherited.and_then(|identity| identity.name.clone()),
            inherited.and_then(|identity| identity.title.clone()),
            inherited.and_then(|identity| identity.first_name.clone()),
            inherited.and_then(|identity| identity.middle_name.clone()),
            inherited.and_then(|identity| identity.last_name.clone()),
            inherited.and_then(|identity| identity.suffix.clone()),
            inherited.map(|identity| identity.is_unknown).unwrap_or(0),
            anchor.anchor_face_id,
            file_count,
            created,
            centroid_blob,
            identity_anchor.anchor_radius as f64,
            now,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn release_active_before_terminal(active: &AtomicBool) {
    active.store(false, Ordering::Release);
}

pub(crate) async fn handle_run_face_clustering(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    active: Arc<AtomicBool>,
) {
    let _sleep = SleepGuard::acquire();
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
                    "SELECT fp.id, fp.file_id, f.content_hash, fp.arcface_embedding, \
                            COALESCE(fp.face_quality, 0.0) \
                     FROM face_prints fp \
                     JOIN files f ON f.id = fp.file_id \
                     WHERE fp.arcface_embedding IS NOT NULL AND COALESCE(fp.excluded, 0) = 0 \
                     ORDER BY fp.id ASC",
                )?;
                let rows = stmt.query_map([], |r| {
                    let id: i64 = r.get(0)?;
                    let file_id: i64 = r.get(1)?;
                    let content_hash: Option<Vec<u8>> = r.get(2)?;
                    let blob: Vec<u8> = r.get(3)?;
                    let quality: f64 = r.get(4)?;
                    Ok((id, file_id, content_hash, blob, quality))
                })?;
                for row in rows {
                    let (id, file_id, content_hash, blob, quality) = row?;
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
                    let norm = embedding
                        .iter()
                        .map(|value| value * value)
                        .sum::<f32>()
                        .sqrt();
                    if !norm.is_finite()
                        || norm <= f32::MIN_POSITIVE
                        || embedding.iter().any(|value| !value.is_finite())
                    {
                        continue;
                    }
                    for value in &mut embedding {
                        *value /= norm;
                    }
                    faces.push(FaceRow {
                        face_id: id,
                        file_id,
                        content_hash,
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
                        COALESCE(is_unknown, 0), created_at, centroid, anchor_radius \
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
                        centroid: decode_centroid(r.get(9)?),
                        anchor_radius: r
                            .get::<_, Option<f64>>(10)?
                            .map(|value| value as f32)
                            .filter(|value| value.is_finite()),
                        face_ids: HashSet::new(),
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
                    if let Some(prior) = prior_by_person.get_mut(&pid) {
                        prior.face_ids.insert(face_id);
                    }
                        }
                    }
                }
                let manual_constraints = load_manual_merge_constraints(&tx)?;
                let existing_person_ids: HashSet<i64> = {
                    let mut statement = tx.prepare("SELECT id FROM persons")?;
                    let collected = statement
                        .query_map([], |row| row.get::<_, i64>(0))?
                        .collect::<rusqlite::Result<_>>()?;
                    collected
                };
                let (manual_owner_by_face, manual_owner_ids) = manual_merge_owner_map(
                    &manual_constraints,
                    &face_to_owner,
                    &existing_person_ids,
                );
                for &owner_id in &manual_owner_ids {
                    if prior_by_person.contains_key(&owner_id) {
                        continue;
                    }
                    let identity = tx
                        .query_row(
                            "SELECT name, title, first_name, middle_name, last_name, suffix, \
                                    COALESCE(is_unknown, 0), created_at, centroid, anchor_radius \
                             FROM persons WHERE id = ?1",
                            [owner_id],
                            |row| {
                                Ok(PriorIdentity {
                                    name: row.get(0)?,
                                    title: row.get(1)?,
                                    first_name: row.get(2)?,
                                    middle_name: row.get(3)?,
                                    last_name: row.get(4)?,
                                    suffix: row.get(5)?,
                                    is_unknown: row.get(6)?,
                                    created_at: row.get(7)?,
                                    centroid: decode_centroid(row.get(8)?),
                                    anchor_radius: row
                                        .get::<_, Option<f64>>(9)?
                                        .map(|value| value as f32)
                                        .filter(|value| value.is_finite()),
                                    face_ids: HashSet::new(),
                                })
                            },
                        )
                        .optional()?;
                    if let Some(identity) = identity {
                        prior_by_person.insert(owner_id, identity);
                    }
                }
                for (&face_id, &owner_id) in &manual_owner_by_face {
                    face_to_owner.insert(face_id, owner_id);
                    let Some(prior) = prior_by_person.get_mut(&owner_id) else {
                        continue;
                    };
                    face_to_prior.insert(face_id, owner_id);
                    prior.face_ids.insert(face_id);
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
                let mut bucket_owner_by_face: HashMap<i64, i64> = face_to_owner
                    .iter()
                    .filter_map(|(&face_id, &owner_id)| {
                        (!preserve_owner_ids.contains(&owner_id)
                    && (prior_by_person.contains_key(&owner_id)
                        || verdict_owner_ids.contains(&owner_id)))
                .then_some((face_id, owner_id))
                    })
                    .collect();
                bucket_owner_by_face.extend(
                    manual_owner_by_face
                        .into_iter()
                        .filter(|(face_id, owner_id)| {
                            pool_face_ids.contains(face_id)
                                && !preserve_owner_ids.contains(owner_id)
                        }),
                );
                let (assignments, anchors) =
            crate::pipeline::face_clustering::partition_protected_clusters_excluding(
                &faces,
                assignments,
                &bucket_owner_by_face,
                &verdict_pairs,
                &excluded_face_ids,
            );

        let protected_faces: HashSet<i64> = bucket_owner_by_face
            .keys()
            .copied()
            .chain(verdict_pairs.iter().flat_map(|&(a, b)| [a, b]))
            .collect();
        let protected_clusters = crate::pipeline::face_clustering::protected_cluster_ids(
            &assignments,
            &protected_faces,
        );
        let before_bimodal_split = anchors.len();
        let (assignments, anchors) =
            crate::pipeline::face_clustering::split_bimodal_mega_clusters(
                &faces,
                assignments,
                anchors,
                &protected_clusters,
            );
        if anchors.len() != before_bimodal_split {
            tracing::info!(
                before = before_bimodal_split,
                after = anchors.len(),
                split = anchors.len() - before_bimodal_split,
                "[CLUSTER] split strongly bimodal mega-clusters"
            );
        }

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
            let recovery_cluster_of: HashMap<i64, i32> = a
                .iter()
                .map(|assignment| (assignment.face_id, assignment.cluster_id))
                .collect();
            let recovery_blocked: HashSet<(i32, i32)> = verdict_pairs
                .iter()
                .filter_map(|&(face_a, face_b)| {
                    let (&cluster_a, &cluster_b) = (
                        recovery_cluster_of.get(&face_a)?,
                        recovery_cluster_of.get(&face_b)?,
                    );
                    (cluster_a != cluster_b).then_some(if cluster_a < cluster_b {
                        (cluster_a, cluster_b)
                    } else {
                        (cluster_b, cluster_a)
                    })
                })
                .collect();
            let recovery_owners =
                crate::pipeline::face_clustering::protected_owner_by_cluster(
                    &face_to_prior,
                    &recovery_cluster_of,
                )
                .map_err(anyhow::Error::msg)?;
            let recovery_blocked_targets: HashSet<i32> = face_to_owner
                .iter()
                .filter_map(|(face_id, owner_id)| {
                    unknown_owner_ids
                        .contains(owner_id)
                        .then(|| recovery_cluster_of.get(face_id).copied())
                        .flatten()
                })
                .collect();
            let before_recovery = an.len();
            let (a, an) = crate::pipeline::face_clustering::recover_small_fragments(
                &faces,
                a,
                an,
                &recovery_blocked,
                &recovery_owners,
                &recovery_blocked_targets,
            );
            if an.len() != before_recovery {
                tracing::info!(
                    before = before_recovery,
                    after = an.len(),
                    recovered = before_recovery - an.len(),
                    threshold =
                        crate::pipeline::face_clustering::fragment_recovery_threshold(),
                    max_fragment_faces =
                        crate::pipeline::face_clustering::fragment_recovery_max_faces(),
                    margin = crate::pipeline::face_clustering::fragment_recovery_margin(),
                    "[CLUSTER] recovered unambiguous small identity fragments"
                );
            }
            // Suppress unresolved low-quality micro-clusters only after recovery,
            // so corroborated doubleton fragments can join a recurring identity.
            let always_keep = crate::pipeline::face_clustering::protected_cluster_ids(
                &a,
                &protected_faces,
            );
            let outlier_floor = crate::pipeline::face_clustering::outlier_cosine_floor();
            let before_outlier_suppression = a.len();
            let (a, an) =
                crate::pipeline::face_clustering::suppress_embedding_outliers_with_keep(
                    &faces,
                    a,
                    an,
                    outlier_floor,
                    &always_keep,
                );
            if a.len() != before_outlier_suppression {
                tracing::info!(
                    before = before_outlier_suppression,
                    after = a.len(),
                    suppressed = before_outlier_suppression - a.len(),
                    outlier_floor,
                    "[CLUSTER] suppressed low-similarity embedding outliers"
                );
            }
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
        let identity_anchor_by_cluster: HashMap<i32, crate::pipeline::face_clustering::IdentityAnchor> =
            crate::pipeline::face_clustering::identity_anchors(&faces, &assignments)
                .into_iter()
                .map(|anchor| (anchor.cluster_id, anchor))
                .collect();
        if identity_anchor_by_cluster.len() != anchors.len() {
            anyhow::bail!("face clustering could not derive every identity anchor");
        }
        let inheritance_priors: HashMap<i64, PriorIdentity> = prior_by_person
            .iter()
            .filter(|(person_id, _)| !preserve_owner_ids.contains(person_id))
            .map(|(&person_id, identity)| (person_id, identity.clone()))
            .collect();
        let inherited_person_by_cluster =
            match_prior_identities(&assignments, &identity_anchor_by_cluster, &inheritance_priors);
        let cluster_by_face: HashMap<i64, i32> = assignments
            .iter()
            .map(|assignment| (assignment.face_id, assignment.cluster_id))
            .collect();
        let mut file_ids_by_cluster: HashMap<i32, HashSet<i64>> = HashMap::new();
        for face in &faces {
            if let Some(&cluster_id) = cluster_by_face.get(&face.face_id) {
                file_ids_by_cluster
                    .entry(cluster_id)
                    .or_default()
                    .insert(face.file_id);
            }
        }
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

        // One prior identity may span multiple new clusters when an explicit
        // different-person verdict separates two of its old faces. Carry that
        // identity to exactly one deterministic winner; duplicating a name or
        // Unknown marker would manufacture a second user identity.

        // Map cluster_id (1-based) → DB person row id.
        let mut cid_to_person: HashMap<i32, i64> = HashMap::new();
        for anchor in &anchors {
            let identity_anchor = identity_anchor_by_cluster
                .get(&anchor.cluster_id)
                .ok_or_else(|| anyhow::anyhow!("missing identity anchor for cluster"))?;
            // Winning prior person: most member faces; tie → owner of this
            // cluster's anchor face, else lowest prior person id (determinism).
            let inherited = inherited_person_by_cluster
                .get(&anchor.cluster_id)
                .and_then(|person_id| prior_by_person.get(person_id));
            let file_count = file_ids_by_cluster
                .get(&anchor.cluster_id)
                .map(|file_ids| file_ids.len() as i64)
                .unwrap_or_default();
            let person_id =
                insert_cluster_person(&tx, inherited, anchor, identity_anchor, file_count, now)?;
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
                        vlm_model TEXT,\
                        file_a INTEGER, bbox_a TEXT, file_b INTEGER, bbox_b TEXT,\
                        PRIMARY KEY(person_a, person_b)\
                    );",
            )
            .unwrap();
        connection
    }

    fn prior_identity(
        face_ids: impl IntoIterator<Item = i64>,
        centroid: Option<Vec<f32>>,
        anchor_radius: Option<f32>,
    ) -> PriorIdentity {
        PriorIdentity {
            name: Some("Alice".into()),
            title: None,
            first_name: None,
            middle_name: None,
            last_name: None,
            suffix: None,
            is_unknown: 0,
            created_at: 1.0,
            centroid,
            anchor_radius,
            face_ids: face_ids.into_iter().collect(),
        }
    }

    fn identity_anchor(
        cluster_id: i32,
        centroid: Vec<f32>,
    ) -> crate::pipeline::face_clustering::IdentityAnchor {
        crate::pipeline::face_clustering::IdentityAnchor {
            cluster_id,
            centroid,
            anchor_radius: 0.5,
        }
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
    fn manual_merge_constraint_resolves_fresh_stable_anchors() {
        let connection = verdict_db();
        connection
            .execute_batch(
                "INSERT INTO face_prints(id, file_id, bbox) VALUES \
                    (10, 1, 'a'), (20, 2, 'b');\
                 INSERT INTO face_verifications(\
                    person_a, person_b, face_a, face_b, same_person, vlm_model,\
                    file_a, bbox_a, file_b, bbox_b\
                 ) VALUES (100, 200, 1, 2, 1, 'user-merged', 1, 'a', 2, 'b');",
            )
            .unwrap();
        assert_eq!(
            load_manual_merge_constraints(&connection).unwrap(),
            [ManualMergeConstraint {
                person_a: 100,
                person_b: 200,
                face_a: 10,
                face_b: 20,
            }]
        );
    }

    #[test]
    fn manual_merge_groups_full_prior_membership_after_raw_resplit() {
        let constraints = [ManualMergeConstraint {
            person_a: 10,
            person_b: 20,
            face_a: 1,
            face_b: 4,
        }];
        let face_to_owner = (1..=6).map(|face_id| (face_id, 20)).collect();
        let (owner_by_face, owner_ids) =
            manual_merge_owner_map(&constraints, &face_to_owner, &HashSet::from([20]));
        assert_eq!(owner_ids, HashSet::from([20]));
        assert_eq!(owner_by_face.len(), 6);

        let faces: Vec<FaceRow> = (1..=6)
            .map(|face_id| FaceRow {
                face_id,
                file_id: face_id,
                content_hash: Some(face_id.to_le_bytes().to_vec()),
                embedding: vec![1.0, 0.0],
                quality: face_id as f32,
            })
            .collect();
        let raw_assignments: Vec<ClusterAssignment> = (1..=6)
            .map(|face_id| ClusterAssignment {
                face_id,
                cluster_id: face_id as i32,
            })
            .collect();
        let (assignments, anchors) =
            crate::pipeline::face_clustering::partition_protected_clusters(
                &faces,
                raw_assignments,
                &owner_by_face,
                &HashSet::new(),
            );
        assert_eq!(anchors.len(), 1);
        assert_eq!(anchors[0].member_count, 6);
        assert!(assignments
            .iter()
            .all(|assignment| assignment.cluster_id == anchors[0].cluster_id));
    }

    #[test]
    fn manual_merge_split_owners_reunify_full_membership_and_keep_cannot_links() {
        let constraints = [ManualMergeConstraint {
            person_a: 10,
            person_b: 20,
            face_a: 1,
            face_b: 2,
        }];
        let split_owners = HashMap::from([(1, 10), (2, 20), (3, 10), (4, 20)]);
        let (owner_by_face, owner_ids) =
            manual_merge_owner_map(&constraints, &split_owners, &HashSet::from([10, 20]));

        assert_eq!(owner_ids, HashSet::from([10]));
        assert_eq!(
            owner_by_face,
            HashMap::from([(1, 10), (2, 10), (3, 10), (4, 10)])
        );
        assert_eq!(
            protected_owner_ids_to_preserve(
                &owner_ids,
                &HashSet::from([10]),
                &HashSet::from([1, 2, 3, 4]),
                &owner_by_face,
            ),
            HashSet::from([10])
        );

        let faces: Vec<FaceRow> = (1..=4)
            .map(|face_id| FaceRow {
                face_id,
                file_id: face_id,
                content_hash: Some(face_id.to_le_bytes().to_vec()),
                embedding: vec![1.0, 0.0],
                quality: 0.9,
            })
            .collect();
        let raw_assignments: Vec<ClusterAssignment> = (1..=4)
            .map(|face_id| ClusterAssignment {
                face_id,
                cluster_id: face_id as i32,
            })
            .collect();
        let (assignments, _) = crate::pipeline::face_clustering::partition_protected_clusters(
            &faces,
            raw_assignments,
            &owner_by_face,
            &HashSet::from([(1, 2)]),
        );
        let cluster_by_face: HashMap<i64, i32> = assignments
            .iter()
            .map(|assignment| (assignment.face_id, assignment.cluster_id))
            .collect();

        assert_eq!(assignments.len(), 4);
        assert_ne!(cluster_by_face[&1], cluster_by_face[&2]);
    }

    #[test]
    fn manual_merge_prefers_historical_metadata_owner_over_lower_split_id() {
        let constraints = [ManualMergeConstraint {
            person_a: 10,
            person_b: 20,
            face_a: 1,
            face_b: 2,
        }];
        let split_owners = HashMap::from([(1, 5), (2, 20), (3, 5), (4, 20)]);

        let (owner_by_face, owner_ids) =
            manual_merge_owner_map(&constraints, &split_owners, &HashSet::from([5, 20]));

        assert_eq!(owner_ids, HashSet::from([20]));
        assert_eq!(
            owner_by_face,
            HashMap::from([(1, 20), (2, 20), (3, 20), (4, 20)])
        );
    }

    #[test]
    fn manual_merge_without_a_live_owner_stays_ambiguous() {
        let constraints = [ManualMergeConstraint {
            person_a: 10,
            person_b: 20,
            face_a: 1,
            face_b: 2,
        }];
        assert!(
            manual_merge_owner_map(&constraints, &HashMap::new(), &HashSet::from([10, 20]))
                .0
                .is_empty()
        );
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
        let assignments = vec![
            ClusterAssignment {
                face_id: 70,
                cluster_id: 7,
            },
            ClusterAssignment {
                face_id: 30,
                cluster_id: 3,
            },
        ];
        let priors = HashMap::from([(
            42,
            PriorIdentity {
                name: Some("Alice".into()),
                title: None,
                first_name: None,
                middle_name: None,
                last_name: None,
                suffix: None,
                is_unknown: 0,
                created_at: 1.0,
                centroid: None,
                anchor_radius: None,
                face_ids: HashSet::from([70, 30]),
            },
        )]);
        assert_eq!(
            match_prior_identities(&assignments, &HashMap::new(), &priors),
            HashMap::from([(3, 42)])
        );
    }

    #[test]
    fn overlap_inheritance_requires_half_of_prior_faces_rounded_up() {
        let prior = prior_identity(1..=5, None, None);
        let priors = HashMap::from([(42, prior)]);
        let two_of_five = vec![
            ClusterAssignment {
                face_id: 1,
                cluster_id: 1,
            },
            ClusterAssignment {
                face_id: 2,
                cluster_id: 1,
            },
        ];
        assert!(match_prior_identities(&two_of_five, &HashMap::new(), &priors).is_empty());
        let mut three_of_five = two_of_five;
        three_of_five.push(ClusterAssignment {
            face_id: 3,
            cluster_id: 1,
        });
        assert_eq!(
            match_prior_identities(&three_of_five, &HashMap::new(), &priors),
            HashMap::from([(1, 42)])
        );
    }

    #[test]
    fn centroid_anchor_inherits_after_complete_face_id_churn_at_radius() {
        let assignments = vec![ClusterAssignment {
            face_id: 100,
            cluster_id: 7,
        }];
        let priors = HashMap::from([(42, prior_identity([1, 2], Some(vec![1.0, 0.0]), Some(0.8)))]);
        for (x, expected) in [(0.799, false), (0.8, true), (0.9, true)] {
            let y = (1.0f32 - x * x).sqrt();
            let anchors = HashMap::from([(7, identity_anchor(7, vec![x, y]))]);
            assert_eq!(
                match_prior_identities(&assignments, &anchors, &priors).get(&7) == Some(&42),
                expected
            );
        }
    }

    #[test]
    fn centroid_inheritance_is_global_one_to_one_and_deterministic() {
        let assignments = vec![
            ClusterAssignment {
                face_id: 100,
                cluster_id: 1,
            },
            ClusterAssignment {
                face_id: 200,
                cluster_id: 2,
            },
        ];
        let priors = HashMap::from([
            (10, prior_identity([], Some(vec![1.0, 0.0]), Some(0.5))),
            (20, prior_identity([], Some(vec![0.8, 0.6]), Some(0.5))),
        ]);
        let anchors = HashMap::from([
            (1, identity_anchor(1, vec![1.0, 0.0])),
            (2, identity_anchor(2, vec![0.8, 0.6])),
        ]);
        assert_eq!(
            match_prior_identities(&assignments, &anchors, &priors),
            HashMap::from([(1, 10), (2, 20)])
        );

        let tied_priors = HashMap::from([
            (10, prior_identity([], Some(vec![1.0, 0.0]), Some(0.5))),
            (20, prior_identity([], Some(vec![1.0, 0.0]), Some(0.5))),
        ]);
        assert_eq!(
            match_prior_identities(
                &assignments[..1],
                &HashMap::from([(1, identity_anchor(1, vec![1.0, 0.0]))]),
                &tied_priors,
            ),
            HashMap::from([(1, 10)])
        );
    }

    #[test]
    fn malformed_prior_centroids_fail_closed() {
        assert!(decode_centroid(None).is_none());
        assert!(decode_centroid(Some(vec![0, 1, 2])).is_none());
        assert!(decode_centroid(Some(vec![0; 8])).is_none());
        let mut non_finite = 1.0f32.to_le_bytes().to_vec();
        non_finite.extend_from_slice(&f32::NAN.to_le_bytes());
        assert!(decode_centroid(Some(non_finite)).is_none());
        let decoded = decode_centroid(Some(
            [3.0f32, 4.0]
                .into_iter()
                .flat_map(f32::to_le_bytes)
                .collect(),
        ))
        .unwrap();
        assert!((decoded[0] - 0.6).abs() < 1e-6);
        assert!((decoded[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn centroid_inheritance_rejects_wrong_dimensions_and_invalid_radius() {
        let assignments = vec![ClusterAssignment {
            face_id: 100,
            cluster_id: 1,
        }];
        let anchors = HashMap::from([(1, identity_anchor(1, vec![1.0, 0.0]))]);
        for prior in [
            prior_identity([], Some(vec![1.0, 0.0, 0.0]), Some(0.5)),
            prior_identity([], Some(vec![1.0, 0.0]), Some(f32::NAN)),
            prior_identity([], Some(vec![1.0, 0.0]), Some(1.1)),
        ] {
            assert!(match_prior_identities(
                &assignments,
                &anchors,
                &HashMap::from([(42, prior)])
            )
            .is_empty());
        }
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
    fn identity_anchor_persistence_round_trips_all_fields() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE persons (\
                    id INTEGER PRIMARY KEY,\
                    name TEXT, title TEXT, first_name TEXT, middle_name TEXT,\
                    last_name TEXT, suffix TEXT, is_unknown INTEGER,\
                    representative_face_id INTEGER, file_count INTEGER, created_at REAL,\
                    centroid BLOB, anchor_radius REAL, last_clustered_at REAL\
                 );",
            )
            .unwrap();
        let mut inherited = prior_identity([1, 2], None, None);
        inherited.name = None;
        inherited.title = Some("Dr.".into());
        inherited.middle_name = Some("Quinn".into());
        inherited.suffix = Some("III".into());
        inherited.created_at = 4.0;
        let mut centroid = vec![0.0; 128];
        centroid[0] = 1.0;
        let identity_anchor =
            crate::pipeline::face_clustering::IdentityAnchor {
                cluster_id: 7,
                centroid,
                anchor_radius: 0.67,
            };
        let anchor = ClusterAnchor {
            cluster_id: 7,
            anchor_face_id: 99,
            member_count: 4,
        };
        let person_id = insert_cluster_person(
            &connection,
            Some(&inherited),
            &anchor,
            &identity_anchor,
            3,
            12.5,
        )
        .unwrap();
        let persisted = connection
            .query_row(
                "SELECT length(centroid), anchor_radius, last_clustered_at, file_count, \
                        created_at, title, middle_name, suffix \
                 FROM persons WHERE id = ?1",
                [person_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, f64>(1)?,
                        row.get::<_, f64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, f64>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(persisted.0, 512);
        assert!((persisted.1 - 0.67).abs() < 1e-6);
        assert!((persisted.2 - 12.5).abs() < f64::EPSILON);
        assert_eq!(persisted.3, 3);
        assert!((persisted.4 - 4.0).abs() < f64::EPSILON);
        assert_eq!(&persisted.5, "Dr.");
        assert_eq!(&persisted.6, "Quinn");
        assert_eq!(&persisted.7, "III");
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
