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
use crate::commands::face_review;
use crate::platform::SleepGuard;
use crate::pipeline::face_clustering::{cluster, ClusterAnchor, ClusterAssignment, FaceRow};
use rusqlite::OptionalExtension;

#[path = "face_clustering_skip.rs"]
mod skip;

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
    for chunk in blob.as_chunks::<4>().0 {
        let value = f32::from_le_bytes(*chunk);
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

/// The person row each new cluster keeps. A cluster that inherits a named
/// identity keeps that person's id; any other cluster keeps the id of the
/// stored person that owns most of its faces. Each id goes to one cluster, and
/// people outside this pass are never reused. Keeping ids lets faces whose
/// person did not change stay untouched.
fn reusable_person_ids(
    assignments: &[ClusterAssignment],
    stored_owner_by_face: &HashMap<i64, i64>,
    inherited_person_by_cluster: &HashMap<i32, i64>,
    preserve_owner_ids: &HashSet<i64>,
) -> HashMap<i32, i64> {
    let mut reused: HashMap<i32, i64> = HashMap::new();
    let mut taken: HashSet<i64> = HashSet::new();
    let mut inherited: Vec<(i32, i64)> = inherited_person_by_cluster.iter().map(|(&cluster, &person)| (cluster, person)).collect();
    inherited.sort_unstable();
    for (cluster, person) in inherited {
        if !preserve_owner_ids.contains(&person) && taken.insert(person) {
            reused.insert(cluster, person);
        }
    }
    let mut overlap: HashMap<(i32, i64), usize> = HashMap::new();
    for assignment in assignments {
        if let Some(&owner) = stored_owner_by_face.get(&assignment.face_id)
            && !preserve_owner_ids.contains(&owner)
        {
            *overlap.entry((assignment.cluster_id, owner)).or_default() += 1;
        }
    }
    let mut ranked: Vec<(usize, i32, i64)> = overlap.into_iter().map(|((cluster, owner), count)| (count, cluster, owner)).collect();
    ranked.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    for (_, cluster, owner) in ranked {
        if !reused.contains_key(&cluster) && !taken.contains(&owner) {
            taken.insert(owner);
            reused.insert(cluster, owner);
        }
    }
    reused
}

struct PersistPlan<'a> {
    assignments: &'a [ClusterAssignment],
    anchors: &'a [ClusterAnchor],
    identity_anchor_by_cluster: &'a HashMap<i32, crate::pipeline::face_clustering::IdentityAnchor>,
    inherited_person_by_cluster: &'a HashMap<i32, i64>,
    prior_by_person: &'a HashMap<i64, PriorIdentity>,
    file_ids_by_cluster: &'a HashMap<i32, HashSet<i64>>,
    reused_person_by_cluster: &'a HashMap<i32, i64>,
    stored_owner_by_face: &'a HashMap<i64, i64>,
    preserve_owner_ids: &'a HashSet<i64>,
}

/// Writes the clusters: people outside the pass stay byte-for-byte intact,
/// every other person is rewritten in place or removed, and only faces whose
/// person changes are updated (each update fires a trigger). Returns the
/// person row of each cluster.
fn persist_clusters(tx: &rusqlite::Connection, plan: &PersistPlan<'_>, now: f64) -> anyhow::Result<HashMap<i32, i64>> {
    tx.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS face_cluster_preserve (id INTEGER PRIMARY KEY);\
         DELETE FROM face_cluster_preserve;\
         CREATE TEMP TABLE IF NOT EXISTS face_cluster_keep (id INTEGER PRIMARY KEY);\
         DELETE FROM face_cluster_keep;",
    )?;
    {
        let mut preserve = tx.prepare("INSERT INTO face_cluster_preserve(id) VALUES (?1)")?;
        let mut keep = tx.prepare("INSERT OR IGNORE INTO face_cluster_keep(id) VALUES (?1)")?;
        for person_id in plan.preserve_owner_ids {
            preserve.execute([person_id])?;
            keep.execute([person_id])?;
        }
        for person_id in plan.reused_person_by_cluster.values() {
            anyhow::ensure!(!plan.preserve_owner_ids.contains(person_id), "face clustering tried to reuse a preserved person");
            keep.execute([person_id])?;
        }
    }
    // A person that keeps its row is still a new person as far as review
    // queues and other people's observations go: clear what deleting the row
    // used to clear through its foreign keys.
    tx.execute_batch(
        "DELETE FROM face_review_candidates WHERE person_id NOT IN (SELECT id FROM face_cluster_preserve);\
         UPDATE face_review_candidates SET runner_up_person_id = NULL \
            WHERE runner_up_person_id IS NOT NULL AND runner_up_person_id NOT IN (SELECT id FROM face_cluster_preserve);\
         DELETE FROM face_merge_suggestions WHERE person_id NOT IN (SELECT id FROM face_cluster_preserve) \
            OR other_person_id NOT IN (SELECT id FROM face_cluster_preserve);\
         DELETE FROM face_split_suggestions WHERE person_id NOT IN (SELECT id FROM face_cluster_preserve);\
         UPDATE catalog_observations SET person_id = NULL \
            WHERE person_id IS NOT NULL AND person_id NOT IN (SELECT id FROM face_cluster_preserve) \
              AND (id NOT LIKE 'faceprint:%' \
                   OR NOT EXISTS (SELECT 1 FROM face_prints fp WHERE fp.id = CAST(substr(catalog_observations.id, 11) AS INTEGER)));",
    )?;

    let mut cid_to_person: HashMap<i32, i64> = HashMap::new();
    for anchor in plan.anchors {
        let identity_anchor = plan
            .identity_anchor_by_cluster
            .get(&anchor.cluster_id)
            .ok_or_else(|| anyhow::anyhow!("missing identity anchor for cluster"))?;
        // One prior identity may span several new clusters when a
        // different-person verdict separates its old faces; exactly one
        // cluster inherits it.
        let inherited = plan
            .inherited_person_by_cluster
            .get(&anchor.cluster_id)
            .and_then(|person_id| plan.prior_by_person.get(person_id));
        let file_count = plan
            .file_ids_by_cluster
            .get(&anchor.cluster_id)
            .map(|file_ids| file_ids.len() as i64)
            .unwrap_or_default();
        let person_id = write_cluster_person(
            tx,
            plan.reused_person_by_cluster.get(&anchor.cluster_id).copied(),
            inherited,
            anchor,
            identity_anchor,
            file_count,
            now,
        )?;
        tx.execute("INSERT OR IGNORE INTO face_cluster_keep(id) VALUES (?1)", [person_id])?;
        cid_to_person.insert(anchor.cluster_id, person_id);
    }

    let mut target_by_face: HashMap<i64, i64> = HashMap::with_capacity(plan.assignments.len());
    for assignment in plan.assignments {
        if let Some(&person_id) = cid_to_person.get(&assignment.cluster_id) {
            target_by_face.insert(assignment.face_id, person_id);
        }
    }
    let mut changes: Vec<(i64, Option<i64>)> = Vec::new();
    for (&face_id, &target) in &target_by_face {
        if plan.stored_owner_by_face.get(&face_id) != Some(&target) {
            changes.push((face_id, Some(target)));
        }
    }
    for (&face_id, owner) in plan.stored_owner_by_face {
        if !plan.preserve_owner_ids.contains(owner) && !target_by_face.contains_key(&face_id) {
            changes.push((face_id, None));
        }
    }
    changes.sort_unstable();
    {
        let mut update = tx.prepare("UPDATE face_prints SET person_id = ?1 WHERE id = ?2")?;
        for (face_id, person_id) in &changes {
            update.execute(rusqlite::params![person_id, face_id])?;
        }
    }
    tracing::info!(changed = changes.len(), assigned = target_by_face.len(), "[CLUSTER] updated faces whose person changed");
    tx.execute("DELETE FROM persons WHERE id NOT IN (SELECT id FROM face_cluster_keep)", [])?;
    tx.execute_batch("DROP TABLE face_cluster_preserve; DROP TABLE face_cluster_keep;")?;
    Ok(cid_to_person)
}

/// Creates the cluster's person, or rewrites every column of the row it keeps.
fn write_cluster_person(
    conn: &rusqlite::Connection,
    reuse: Option<i64>,
    inherited: Option<&PriorIdentity>,
    anchor: &ClusterAnchor,
    identity_anchor: &crate::pipeline::face_clustering::IdentityAnchor,
    file_count: i64,
    now: f64,
) -> anyhow::Result<i64> {
    let Some(person_id) = reuse else {
        return Ok(insert_cluster_person(conn, inherited, anchor, identity_anchor, file_count, now)?);
    };
    let centroid_blob: Vec<u8> = identity_anchor.centroid.iter().flat_map(|value| value.to_le_bytes()).collect();
    let changed = conn.execute(
        "UPDATE persons SET name=?1, title=?2, first_name=?3, middle_name=?4, last_name=?5, suffix=?6, is_unknown=?7, \
            representative_face_id=?8, file_count=?9, created_at=?10, centroid=?11, anchor_radius=?12, last_clustered_at=?13 \
         WHERE id=?14",
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
            inherited.map(|identity| identity.created_at).unwrap_or(now),
            centroid_blob,
            identity_anchor.anchor_radius as f64,
            now,
            person_id,
        ],
    )?;
    anyhow::ensure!(changed == 1, "face clustering lost the person row it meant to keep");
    Ok(person_id)
}

fn release_active_before_terminal(active: &AtomicBool) {
    active.store(false, Ordering::Release);
}

/// Refuses to recluster unless every face cache shares one current model and
/// processing space with valid unit vectors, so incompatible embeddings never
/// reshape existing People.
/// The single model and processing space every clustered face must share.
#[derive(Default)]
struct FaceSpaceCheck {
    space: Option<(String, String)>,
    count: usize,
}

impl FaceSpaceCheck {
    fn admit(
        &mut self,
        blob: &[u8],
        model: Option<String>,
        processing: Option<String>,
        revision: Option<&str>,
        current: Option<&str>,
    ) -> anyhow::Result<()> {
        let compatible = model.as_ref().is_some_and(|v| !v.is_empty())
            && processing.as_ref().is_some_and(|v| !v.is_empty())
            && revision.is_some_and(|v| !v.is_empty())
            && revision == current
            && blob.len() == 512;
        anyhow::ensure!(compatible, "Face caches need a compatible refresh before clustering; existing People were preserved.");
        let key = (model.unwrap_or_default(), processing.unwrap_or_default());
        anyhow::ensure!(self.space.as_ref().is_none_or(|s| *s == key), "Mixed face-model or processing spaces cannot be compared; existing People were preserved.");
        self.space = Some(key);
        let norm: f64 = blob
            .as_chunks::<4>()
            .0
            .iter()
            .map(|v| f64::from(f32::from_le_bytes(*v)).powi(2))
            .sum();
        anyhow::ensure!(norm.is_finite() && (0.95..=1.05).contains(&norm), "Invalid face vector; existing People were preserved.");
        self.count += 1;
        anyhow::ensure!(self.count <= 200_000, "Face clustering exceeds the safe batch size; existing People were preserved.");
        Ok(())
    }
}

/// Refuses to recluster unless every face cache shares one current model and
/// processing space with valid unit vectors, so incompatible embeddings never
/// reshape existing People.
pub(crate) fn ensure_compatible_face_space(conn: &rusqlite::Connection) -> anyhow::Result<Option<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT fp.arcface_embedding, fp.embedding_model, fp.processing_version, fp.source_revision, r.revision \
         FROM face_prints fp LEFT JOIN catalog_revisions r ON r.file_id=fp.file_id \
         WHERE fp.arcface_embedding IS NOT NULL AND COALESCE(fp.excluded,0)=0 ORDER BY fp.id",
    )?;
    let mut cursor = stmt.query([])?;
    let mut check = FaceSpaceCheck::default();
    while let Some(row) = cursor.next()? {
        let blob = row.get_ref(0)?.as_blob()?;
        let revision: Option<String> = row.get(3)?;
        let current: Option<String> = row.get(4)?;
        check.admit(blob, row.get(1)?, row.get(2)?, revision.as_deref(), current.as_deref())?;
    }
    Ok(check.space)
}

/// One clustering pass over the portable face space: read the pool, cluster
/// without the writer lock, then persist under it.
fn run_portable(db: &parking_lot::Mutex<rusqlite::Connection>) -> anyhow::Result<FaceClusteringResult> {
        let started = Instant::now();
        let count: u64 = db.lock().query_row("SELECT COUNT(*) FROM face_prints WHERE arcface_embedding IS NOT NULL AND COALESCE(excluded,0)=0", [], |row| row.get(0))?;
        let _lease = crate::resource_scheduler::ResourceScheduler::shared().reserve(
            crate::resource_scheduler::Demand { memory_mb: 64 + count.saturating_mul(4096).div_ceil(1_048_576), cpu: 1, io: 1 },
            crate::resource_scheduler::Priority::Background, || false)?;

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
        let space;
        let held: HashSet<i64>;
        {
            let conn = db.lock();
            // The space check and the load share one read of the vectors.
            let mut check = FaceSpaceCheck::default();

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
            // Recalibrated on the isolated 41,855-face Family Photos catalog
            // (2026-08-08): 0.25 admitted visually confirmed sibling, unrelated-
            // adult, and object contamination. 0.33 separated every labelled
            // negative, retained the clear same-person pair, cut displayable
            // People groups from about 420 to 170, and improved every top-cluster
            // cohesion floor. Lower-quality detections remain searchable and can
            // be reviewed, but do not manufacture identities. 0 disables.
            let min_cluster_quality: f32 = std::env::var("FILEID_FACE_CLUSTER_MIN_QUALITY")
                .ok()
                .and_then(|s| s.trim().parse::<f32>().ok())
                .filter(|v| v.is_finite())
                .map(|v| v.clamp(0.0, 1.0))
                .unwrap_or(0.33);
            {
                let mut stmt = conn.prepare(
                    "SELECT fp.id, fp.file_id, f.content_hash, fp.arcface_embedding, \
                            COALESCE(fp.face_quality, 0.0), fp.embedding_model, fp.processing_version, \
                            fp.source_revision, r.revision, f.id \
                     FROM face_prints fp \
                     LEFT JOIN files f ON f.id = fp.file_id \
                     LEFT JOIN catalog_revisions r ON r.file_id = fp.file_id \
                     WHERE fp.arcface_embedding IS NOT NULL AND COALESCE(fp.excluded, 0) = 0 \
                     ORDER BY fp.id ASC",
                )?;
                let mut rows = stmt.query([])?;
                while let Some(r) = rows.next()? {
                    let blob: Vec<u8> = r.get(3)?;
                    let revision: Option<String> = r.get(7)?;
                    let current: Option<String> = r.get(8)?;
                    check.admit(&blob, r.get(5)?, r.get(6)?, revision.as_deref(), current.as_deref())?;
                    if r.get::<_, Option<i64>>(9)?.is_none() {
                        continue;
                    }
                    let id: i64 = r.get(0)?;
                    let file_id: i64 = r.get(1)?;
                    let content_hash: Option<Vec<u8>> = r.get(2)?;
                    let quality: f64 = r.get(4)?;
                    if (quality as f32) < min_cluster_quality {
                        continue;
                    }
                    if blob.len() % 4 != 0 || blob.is_empty() {
                        continue;
                    }
                    let mut embedding = Vec::with_capacity(blob.len() / 4);
                    for chunk in blob.as_chunks::<4>().0 {
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

            space = check.space;
            held = match &space {
                Some((model, processing)) => {
                    face_review::holdout(&conn, model, processing, f64::from(min_cluster_quality))?
                }
                None => HashSet::new(),
            };
            faces.retain(|face| !held.contains(&face.face_id));

            // The user-identity snapshot (prior names/title/is_unknown + the
            // face->person map) is read in PHASE 3 under the persist lock — NOT
            // here — so a People-tab edit (rename / merge / mark-unknown) that
            // commits during the lock-free phase 2 is carried forward instead of
            // being silently clobbered by a phase-1 snapshot that predates it.
            // (audit S0)
            drop(conn);
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
        // What `face_prints.person_id` holds now, before manual-merge overrides.
        let mut stored_owner_by_face: HashMap<i64, i64> = HashMap::new();
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
                stored_owner_by_face.insert(face_id, pid);
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
                    
                    statement
                        .query_map([], |row| row.get::<_, i64>(0))?
                        .collect::<rusqlite::Result<_>>()?
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
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let reused_person_by_cluster = reusable_person_ids(
            &assignments,
            &stored_owner_by_face,
            &inherited_person_by_cluster,
            &preserve_owner_ids,
        );
        let cid_to_person = persist_clusters(
            &tx,
            &PersistPlan {
                assignments: &assignments,
                anchors: &anchors,
                identity_anchor_by_cluster: &identity_anchor_by_cluster,
                inherited_person_by_cluster: &inherited_person_by_cluster,
                prior_by_person: &prior_by_person,
                file_ids_by_cluster: &file_ids_by_cluster,
                reused_person_by_cluster: &reused_person_by_cluster,
                stored_owner_by_face: &stored_owner_by_face,
                preserve_owner_ids: &preserve_owner_ids,
            },
            now,
        )?;
        face_review::mark_pool(&tx, faces.iter().map(|face| &face.face_id), now)?;
        if let Some((model, processing)) = &space {
            let stats = face_review::assess(&tx, &held, model, processing, &face_review::Settings::from_env(), now)?;
            if stats != face_review::AssessStats::default() {
                tracing::info!(queued = stats.queued, assigned = stats.assigned, unmatched = stats.unmatched, "[CLUSTER] assessed new faces for review");
            }
            let (merges, splits) = super::face_queues::refresh(&tx, model, processing, &super::face_queues::QueueSettings::from_env(), now)?;
            tracing::info!(merges, splits, "[CLUSTER] stored merge and split suggestions");
        }
        let review_queued = face_review::pending_count(&tx)?;
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
            deferred_faces: None,
            review_queued: Some(review_queued),
        })
}

pub(crate) async fn handle_run_face_clustering(
    sink: Sink,
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    active: Arc<AtomicBool>,
) {
    let _sleep = SleepGuard::acquire();
    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    {
        let extract_db = db.clone();
        let _ = tokio::task::spawn_blocking(move || crate::apple::faces::extract_before_clustering(&extract_db)).await;
    }
    let started = Instant::now();
    let skip_db = db.clone();
    if let Ok(Some(mut remembered)) = tokio::task::spawn_blocking(move || skip::with_reader(&skip_db, skip::unchanged)).await {
        tracing::info!("[CLUSTER] nothing changed since the last pass; skipped");
        remembered.duration_seconds = started.elapsed().as_secs_f64();
        release_active_before_terminal(&active);
        sink.send(IpcEvent::now(EventPayload::FaceClusteringComplete(Wrap::new(remembered)))).await;
        return;
    }
    let mac_db = db.clone();
    let mac = tokio::task::spawn_blocking(move || {
        let space = super::face_clustering_mac::selected_space(&mac_db.lock())?;
        let outcome = super::face_clustering_mac::run(&mac_db, &space);
        skip::with_reader(&mac_db, |conn| match outcome.error {
            None => skip::remember(conn, &outcome.result),
            Some(_) => skip::forget(conn),
        });
        Some(outcome)
    })
    .await;
    match mac {
        Ok(None) => {}
        Ok(Some(outcome)) => {
            release_active_before_terminal(&active);
            if let Some((kind, message)) = outcome.error {
                sink.send(EngineError::event(kind, message)).await;
            }
            sink.send(IpcEvent::now(EventPayload::FaceClusteringComplete(Wrap::new(outcome.result)))).await;
            return;
        }
        Err(err) => {
            release_active_before_terminal(&active);
            tracing::warn!(?err, "face clustering task did not complete");
            sink.send(EngineError::event("face_clustering_failed", "FileID couldn't finish grouping faces. Try again.")).await;
            return;
        }
    }
    let result = tokio::task::spawn_blocking(move || {
        let result = run_portable(&db);
        skip::with_reader(&db, |conn| match &result {
            Ok(result) => skip::remember(conn, result),
            Err(_) => skip::forget(conn),
        });
        result
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
            sink.send(EngineError::event(
                "face_clustering_failed",
                "FileID couldn't finish grouping faces. Try again.",
            ))
            .await;
        }
        Err(err) => {
            tracing::warn!(?err, "face clustering spawn failed");
            // PAR-111: emit a face_clustering error so the app-side auto-trigger
            // gate (_faceClusterAutoInFlight) is released even when the
            // clustering closure panics — a JoinError otherwise fires no
            // completion/error event, leaving auto-clustering stuck for the
            // session.
            sink.send(EngineError::event(
                "face_clustering_failed",
                "FileID couldn't finish grouping faces. Try again.",
            ))
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

    fn persist_fixture() -> rusqlite::Connection {
        let conn = crate::db::test_support::memory_db();
        conn.execute_batch(
            "INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension) VALUES
               (1,'/1.jpg',1,1,1,0,'image','jpg'),(2,'/2.jpg',2,1,1,0,'image','jpg'),(3,'/3.jpg',3,1,1,0,'image','jpg'),
               (4,'/4.jpg',4,1,1,0,'image','jpg'),(5,'/5.jpg',5,1,1,0,'image','jpg'),(6,'/6.jpg',6,1,1,0,'image','jpg'),
               (7,'/7.jpg',7,1,1,0,'image','jpg'),(8,'/8.jpg',8,1,1,0,'image','jpg');
             INSERT INTO catalog_revisions(file_id,revision,processing_version,updated_at) SELECT id,'r','p',0 FROM files;
             INSERT INTO persons(id,name,created_at) VALUES(1,'Ann',5),(2,NULL,6),(3,'Kept',7),(4,NULL,8);
             INSERT INTO face_prints(id,file_id,person_id,print_data,bbox,arcface_embedding,embedding_model,processing_version,source_revision) VALUES
               (10,1,1,x'00','a',zeroblob(512),'m','p','r'),(11,2,1,x'00','a',zeroblob(512),'m','p','r'),
               (20,3,2,x'00','a',zeroblob(512),'m','p','r'),(21,4,2,x'00','a',zeroblob(512),'m','p','r'),
               (30,5,3,x'00','a',zeroblob(512),'m','p','r'),(40,6,NULL,x'00','a',zeroblob(512),'m','p','r'),
               (50,7,4,x'00','a',zeroblob(512),'m','p','r'),(60,8,NULL,x'00','a',zeroblob(512),'m','p','r');
             INSERT INTO face_review_candidates(face_id,person_id,score,reason,runner_up_person_id,namespace,created_at) VALUES
               (40,1,0.5,'suggested',2,'m|p',0),(21,3,0.5,'suggested',1,'m|p',0);
             INSERT INTO face_merge_suggestions VALUES(1,2,0.9,'m|p',0),(3,1,0.9,'m|p',0);
             INSERT INTO face_split_suggestions VALUES(2,0.5,'[]','[]','m|p',0),(3,0.5,'[]','[]','m|p',0);
             INSERT INTO catalog_observations(id,file_id,person_id,source_revision,model_version,confidence) VALUES
               ('track:1',1,1,'r','v',1),('track:3',5,3,'r','v',1),('faceprint:999',1,1,'r','v',1);
             CREATE TEMP TABLE face_writes(face INTEGER);
             CREATE TEMP TRIGGER count_face_writes AFTER UPDATE OF person_id ON face_prints BEGIN INSERT INTO face_writes VALUES(new.id); END;",
        )
        .unwrap();
        conn
    }

    fn persist_fixture_plan(conn: &rusqlite::Connection) -> (HashMap<i32, i64>, Vec<i64>) {
        let assignments = vec![
            ClusterAssignment { face_id: 10, cluster_id: 1 },
            ClusterAssignment { face_id: 11, cluster_id: 1 },
            ClusterAssignment { face_id: 40, cluster_id: 1 },
            ClusterAssignment { face_id: 20, cluster_id: 2 },
            ClusterAssignment { face_id: 50, cluster_id: 3 },
            ClusterAssignment { face_id: 21, cluster_id: 3 },
            ClusterAssignment { face_id: 60, cluster_id: 4 },
        ];
        let anchors = vec![
            ClusterAnchor { cluster_id: 1, anchor_face_id: 10, member_count: 3 },
            ClusterAnchor { cluster_id: 2, anchor_face_id: 20, member_count: 1 },
            ClusterAnchor { cluster_id: 3, anchor_face_id: 50, member_count: 2 },
            ClusterAnchor { cluster_id: 4, anchor_face_id: 60, member_count: 1 },
        ];
        let mut centroid = vec![0.0; 128];
        centroid[0] = 1.0;
        let identity: HashMap<i32, crate::pipeline::face_clustering::IdentityAnchor> = (1..=4)
            .map(|cluster_id| (cluster_id, crate::pipeline::face_clustering::IdentityAnchor { cluster_id, centroid: centroid.clone(), anchor_radius: 0.5 }))
            .collect();
        let mut ann = prior_identity([10, 11], None, None);
        ann.name = Some("Ann".into());
        ann.created_at = 5.0;
        let priors = HashMap::from([(1, ann)]);
        let inherited = HashMap::from([(1, 1)]);
        let stored: HashMap<i64, i64> = HashMap::from([(10, 1), (11, 1), (20, 2), (21, 2), (30, 3), (50, 4)]);
        let preserve = HashSet::from([3]);
        let files: HashMap<i32, HashSet<i64>> = HashMap::from([(1, HashSet::from([1, 2, 6])), (2, HashSet::from([3])), (3, HashSet::from([7, 4]))]);
        let reused = reusable_person_ids(&assignments, &stored, &inherited, &preserve);
        assert_eq!(reused, HashMap::from([(1, 1), (2, 2), (3, 4)]), "a named identity keeps its id; others keep their majority owner's");
        let tx = conn.unchecked_transaction().unwrap();
        let people = persist_clusters(
            &tx,
            &PersistPlan {
                assignments: &assignments,
                anchors: &anchors,
                identity_anchor_by_cluster: &identity,
                inherited_person_by_cluster: &inherited,
                prior_by_person: &priors,
                file_ids_by_cluster: &files,
                reused_person_by_cluster: &reused,
                stored_owner_by_face: &stored,
                preserve_owner_ids: &preserve,
            },
            100.0,
        )
        .unwrap();
        tx.commit().unwrap();
        let mut written: Vec<i64> = conn
            .prepare("SELECT face FROM face_writes ORDER BY face")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .flatten()
            .collect();
        written.sort_unstable();
        (people, written)
    }

    #[test]
    fn persisting_writes_only_faces_whose_person_changes() {
        let conn = persist_fixture();
        let (people, written) = persist_fixture_plan(&conn);
        assert_eq!(people, HashMap::from([(1, 1), (2, 2), (3, 4), (4, 5)]));
        assert_eq!(written, vec![21, 40, 60], "faces 10, 11, 20, 30 and 50 keep their person and are not rewritten");
        let owners: Vec<(i64, Option<i64>)> = conn
            .prepare("SELECT id, person_id FROM face_prints ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(owners, vec![(10, Some(1)), (11, Some(1)), (20, Some(2)), (21, Some(4)), (30, Some(3)), (40, Some(1)), (50, Some(4)), (60, Some(5))]);
        let rows: Vec<(i64, Option<String>, i64, i64, f64, f64)> = conn
            .prepare("SELECT id,name,representative_face_id,file_count,created_at,last_clustered_at FROM persons ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get::<_, Option<i64>>(2)?.unwrap_or(0), row.get(3)?, row.get(4)?, row.get::<_, Option<f64>>(5)?.unwrap_or(0.0))))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(
            rows,
            vec![
                (1, Some("Ann".into()), 10, 3, 5.0, 100.0),
                (2, None, 20, 1, 100.0, 100.0),
                (3, Some("Kept".into()), 0, 0, 7.0, 0.0),
                (4, None, 50, 2, 100.0, 100.0),
                (5, None, 60, 0, 100.0, 100.0),
            ]
        );
        let count = |sql: &str| conn.query_row(sql, [], |row| row.get::<_, i64>(0)).unwrap();
        assert_eq!(count("SELECT COUNT(*) FROM face_review_candidates"), 1, "suggestions for re-formed people are cleared as a delete would");
        assert_eq!(count("SELECT COUNT(*) FROM face_review_candidates WHERE person_id=3 AND runner_up_person_id IS NULL"), 1);
        assert_eq!(count("SELECT COUNT(*) FROM face_merge_suggestions"), 0);
        assert_eq!(count("SELECT COUNT(*) FROM face_split_suggestions WHERE person_id=3"), 1);
        assert_eq!(count("SELECT COUNT(*) FROM face_split_suggestions"), 1);
        assert_eq!(count("SELECT COUNT(*) FROM catalog_observations WHERE id='track:1' AND person_id IS NULL"), 1);
        assert_eq!(count("SELECT COUNT(*) FROM catalog_observations WHERE id='faceprint:999' AND person_id IS NULL"), 1);
        assert_eq!(count("SELECT COUNT(*) FROM catalog_observations WHERE id='track:3' AND person_id=3"), 1);
        assert_eq!(
            count("SELECT COUNT(*) FROM catalog_observations o JOIN face_prints fp ON o.id='faceprint:' || fp.id WHERE o.person_id IS NOT fp.person_id"),
            0,
            "every face observation agrees with its face"
        );
        assert_eq!(count("SELECT COUNT(*) FROM catalog_observations WHERE id LIKE 'faceprint:%'"), 9);
        assert_eq!(count("SELECT COUNT(*) FROM pragma_foreign_key_check"), 0);
    }

    #[test]
    fn people_no_cluster_keeps_are_removed_and_their_faces_released() {
        let conn = persist_fixture();
        let stored: HashMap<i64, i64> = HashMap::from([(10, 1), (11, 1), (20, 2), (21, 2), (30, 3), (50, 4)]);
        let preserve = HashSet::from([3]);
        let tx = conn.unchecked_transaction().unwrap();
        let people = persist_clusters(
            &tx,
            &PersistPlan {
                assignments: &[],
                anchors: &[],
                identity_anchor_by_cluster: &HashMap::new(),
                inherited_person_by_cluster: &HashMap::new(),
                prior_by_person: &HashMap::new(),
                file_ids_by_cluster: &HashMap::new(),
                reused_person_by_cluster: &HashMap::new(),
                stored_owner_by_face: &stored,
                preserve_owner_ids: &preserve,
            },
            100.0,
        )
        .unwrap();
        tx.commit().unwrap();
        assert!(people.is_empty());
        let count = |sql: &str| conn.query_row(sql, [], |row| row.get::<_, i64>(0)).unwrap();
        assert_eq!(count("SELECT COUNT(*) FROM persons"), 1);
        assert_eq!(count("SELECT COUNT(*) FROM face_prints WHERE person_id IS NOT NULL"), 1);
        assert_eq!(count("SELECT COUNT(*) FROM face_writes"), 5);
    }

    /// `persist_clusters` clears by hand what deleting a person row clears
    /// through foreign keys, and rewrites every person column in place. A new
    /// reference to `persons` or a new column must be added there too.
    #[test]
    fn the_person_schema_the_in_place_rewrite_assumes_is_unchanged() {
        let conn = crate::db::test_support::memory_db();
        let mut references: Vec<(String, String, String)> = conn
            .prepare(
                "SELECT m.name, f.\"from\", f.on_delete FROM sqlite_master m, pragma_foreign_key_list(m.name) f \
                 WHERE m.type='table' AND f.\"table\"='persons'",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .flatten()
            .collect();
        references.sort();
        let expected = [
            ("catalog_observations", "person_id", "SET NULL"),
            ("face_merge_suggestions", "other_person_id", "CASCADE"),
            ("face_merge_suggestions", "person_id", "CASCADE"),
            ("face_prints", "person_id", "NO ACTION"),
            ("face_review_candidates", "person_id", "CASCADE"),
            ("face_review_candidates", "runner_up_person_id", "SET NULL"),
            ("face_split_suggestions", "person_id", "CASCADE"),
        ];
        assert_eq!(references, expected.map(|(a, b, c)| (a.to_string(), b.to_string(), c.to_string())).to_vec());
        let mut columns: Vec<String> =
            conn.prepare("SELECT name FROM pragma_table_info('persons')").unwrap().query_map([], |row| row.get(0)).unwrap().flatten().collect();
        columns.sort();
        assert_eq!(
            columns,
            [
                "anchor_radius", "centroid", "created_at", "file_count", "first_name", "id", "is_unknown", "last_clustered_at",
                "last_name", "middle_name", "name", "representative_face_id", "suffix", "title"
            ]
        );
    }

    fn add_face(conn: &rusqlite::Connection, id: i64, axis: usize, tilt: f32) {
        let mut vector = vec![0f32; 128];
        vector[axis] = 1.0;
        vector[100 + (id as usize % 20)] = tilt;
        let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
        let blob: Vec<u8> = vector.iter().flat_map(|v| (v / norm).to_le_bytes()).collect();
        conn.execute(
            "INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension) VALUES(?1,?2,?1,1,1,0,'image','jpg')",
            rusqlite::params![id, format!("/{id}.jpg")],
        )
        .unwrap();
        conn.execute("INSERT INTO catalog_revisions(file_id,revision,processing_version,updated_at) VALUES(?1,'r','p',0)", [id]).unwrap();
        conn.execute(
            "INSERT INTO face_prints(id,file_id,print_data,bbox,arcface_embedding,face_quality,embedding_model,processing_version,source_revision) \
             VALUES(?1,?1,x'00','0.1,0.1,0.2,0.2',?2,0.9,'m','p','r')",
            rusqlite::params![id, blob],
        )
        .unwrap();
    }

    #[test]
    fn a_second_pass_keeps_person_ids_and_rewrites_no_face() {
        let conn = crate::db::test_support::memory_db();
        for id in 1..=6 {
            add_face(&conn, id, 0, 0.05);
        }
        for id in 7..=12 {
            add_face(&conn, id, 1, 0.05);
        }
        conn.execute_batch(
            "CREATE TEMP TABLE face_writes(face INTEGER);
             CREATE TEMP TRIGGER count_face_writes AFTER UPDATE OF person_id ON face_prints BEGIN INSERT INTO face_writes VALUES(new.id); END;",
        )
        .unwrap();
        let db = parking_lot::Mutex::new(conn);
        let owners = |db: &parking_lot::Mutex<rusqlite::Connection>| -> Vec<(i64, Option<i64>)> {
            let conn = db.lock();
            let mut statement = conn.prepare("SELECT id, person_id FROM face_prints ORDER BY id").unwrap();
            statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?))).unwrap().flatten().collect()
        };
        let writes = |db: &parking_lot::Mutex<rusqlite::Connection>| -> i64 {
            let conn = db.lock();
            let count = conn.query_row("SELECT COUNT(*) FROM face_writes", [], |row| row.get(0)).unwrap();
            conn.execute("DELETE FROM face_writes", []).unwrap();
            count
        };

        let first = run_portable(&db).unwrap();
        assert_eq!((first.person_count, first.face_count, first.unmatched_faces), (2, 12, 0));
        let before = owners(&db);
        assert!(before.iter().all(|(_, person)| person.is_some()));
        assert_eq!(writes(&db), 12);

        let second = run_portable(&db).unwrap();
        assert_eq!((second.person_count, second.face_count, second.unmatched_faces), (2, 12, 0));
        assert_eq!(owners(&db), before, "people keep their ids across passes");
        assert_eq!(writes(&db), 0, "an unchanged pass rewrites no face");

        db.lock().execute("UPDATE persons SET name='Ann' WHERE id=?1", [before[0].1]).unwrap();
        add_face(&db.lock(), 13, 0, 0.05);
        let third = run_portable(&db).unwrap();
        assert_eq!(third.person_count, 2);
        let after = owners(&db);
        assert_eq!(after[..12], before[..], "existing faces keep their person");
        let name: Option<String> = db.lock().query_row("SELECT name FROM persons WHERE id=?1", [before[0].1], |row| row.get(0)).unwrap();
        assert_eq!(name.as_deref(), Some("Ann"));
        assert!(writes(&db) <= 1, "only the new face can be written");
    }
}
