// Face clustering — density-clustering driver over SFace (128-d) embeddings.
//
// Source of truth for thresholds: `identity_clustering::Hyperparameters::default()`
// and the MERGE_SUGGEST_* / AUTOMERGE_* constants in THIS file. Do not
// re-document numeric thresholds elsewhere — they drift.
//
// Pipeline (see `cluster()` + `consolidate()` here, and the DB-side handler in
// `commands/face_clustering.rs`):
//   1. Load every `face_prints` row whose `arcface_embedding` (column, not a
//      table) is non-NULL and `excluded = 0`.
//   2. `cluster()` runs the 3-pass density algorithm (identity_clustering):
//      Pass-1 kNN connected components ≥ pass1_cosine, Pass-2 margin-gated
//      outlier assignment, Pass-3 2-means split of low-cohesion clusters.
//   3. `consolidate()` folds near-certain duplicate clusters by CENTROID cosine
//      ≥ FILEID_FACE_AUTOMERGE_COS (default 0.75), respecting user "different
//      people" verdicts. Anchor per cluster = highest-quality member face.
//   4. The handler persists `persons` + `face_prints.person_id` in one tx and
//      emits `FaceClusteringResult`. `face_verifications` is only READ here
//      (same_person = 0, to block auto-merge of user-confirmed splits).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// Lower bound for surfacing MERGE suggestions in the People tab. A 0.32 floor
/// flooded the sheet with anchor pairs deep in impostor territory, so a low floor
/// is mostly noise. 0.55 keeps the genuinely-uncertain band (plausible cross-pose
/// same person, plus the hardest impostors worth a human glance) and drops the
/// rest — fewer, more actionable suggestions.
///
/// NOTE (2026-07-29): the earlier claim here that "genuine same-person SFace
/// cosine sits at 0.88-0.95" is NOT true of a real family library, and was the
/// premise behind the old 0.88 auto-merge bar. Measured on the Adlon catalog,
/// clusters of <=150 faces (overwhelmingly single identities) have mean PAIRWISE
/// cosine 0.71-0.86 with p10 0.54-0.78, and centroid-to-centroid cosine between
/// genuinely-same-person fragments runs well below 0.88 — which is why
/// consolidate at 0.88 fired only 3 times across 3,092 clusters. See
/// AUTOMERGE_COS_DEFAULT below. This band (0.55..0.97) is unchanged and remains
/// the right "ask a human" range.
pub const MERGE_SUGGEST_COS_LOW: f32 = 0.55;

/// Upper bound for surfacing MERGE suggestions in the People tab. Previously
/// pinned at the Pass-1 core threshold (0.66) on the theory that anything above
/// 0.66 already auto-merged in Pass 1 — but Pass 1 is kNN-limited single-linkage
/// and Pass 3 can re-split, so genuine same-person FRAGMENTS routinely strand in
/// 0.66..0.95: too high to be suggested, too low/disconnected to have auto-merged.
/// Those are exactly the obvious duplicates a user wants to merge. Raising the
/// ceiling to 0.97 surfaces them (sorted to the top by similarity). The very
/// top of this band (centroid ≥ FILEID_FACE_AUTOMERGE_COS) is instead folded
/// automatically at clustering time, so in practice suggestions here are the
/// anchor-high / centroid-borderline residue that auto-consolidation skipped.
pub const MERGE_SUGGEST_COS_HIGH: f32 = 0.97;

#[derive(Debug, Clone)]
pub struct FaceRow {
    pub face_id: i64,
    pub file_id: i64,
    pub content_hash: Option<Vec<u8>>,
    pub embedding: Vec<f32>,
    pub quality: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum CaptureKey {
    Content(Vec<u8>),
    File(i64),
}

fn capture_key(face: &FaceRow) -> CaptureKey {
    match face.content_hash.as_ref().filter(|hash| !hash.is_empty()) {
        Some(hash) => CaptureKey::Content(hash.clone()),
        None => CaptureKey::File(face.file_id),
    }
}

#[derive(Debug, Clone)]
pub struct ClusterAssignment {
    pub face_id: i64,
    pub cluster_id: i32,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ClusterAnchor {
    pub cluster_id: i32,
    pub anchor_face_id: i64,
    pub member_count: u32,
}

/// Group `faces` into clusters via the 3-pass density algorithm in
/// `identity_clustering`.
///
/// Returns (assignments, anchors). Cluster IDs are 1-based and stable
/// in first-seen order.
pub fn cluster(faces: &[FaceRow]) -> (Vec<ClusterAssignment>, Vec<ClusterAnchor>) {
    if faces.is_empty() {
        return (Vec::new(), Vec::new());
    }

    // kNN searcher. Below ~5 k faces the brute-force O(n²) all-pairs cosine
    // beats the HNSW build overhead. Above it, we build an `instant-distance`
    // HNSW index once and serve each kNN query in O(log n) — turns People-tab
    // refresh from quadratic into log-linear on big libraries. ArcFace
    // embeddings are L2-normalized, so squared-L2 distance is monotonic in
    // `(1 − cosine)` (the index gives the same neighbor ranking as cosine).
    const HNSW_MIN: usize = 5_000;
    let embeddings: Vec<Vec<f32>> = faces.iter().map(|f| f.embedding.clone()).collect();
    let k = super::identity_clustering::Hyperparameters::default().k_nn;
    let hnsw_idx = (embeddings.len() >= HNSW_MIN).then(|| {
        let points: Vec<(Vec<f32>, usize)> = embeddings
            .iter()
            .enumerate()
            .map(|(i, e)| (e.clone(), i))
            .collect();
        crate::util::hnsw_index::build(points)
    });
    let mut knn_search = crate::util::hnsw_index::Searcher::default();
    let result = super::identity_clustering::cluster(
        &embeddings,
        |i| {
            let mut hits: Vec<super::identity_clustering::Neighbor> = if let Some(idx) = &hnsw_idx {
                // Query k+1 so we can drop the self-hit; convert squared-L2 →
                // cosine (vectors are unit-norm: d = 2(1 − cos)). Reuse one
                // Search scratch across the whole sweep — a fresh one re-zeros an
                // n-byte visited set per query, an O(n²) term over the pass.
                knn_search
                    .top_k(idx, &embeddings[i], k + 1)
                    .into_iter()
                    .filter(|(j, _)| *j != i)
                    .map(|(j, d)| super::identity_clustering::Neighbor {
                        idx: j,
                        similarity: 1.0 - d / 2.0,
                    })
                    .collect()
            } else {
                (0..embeddings.len())
                    .filter(|&j| j != i)
                    .map(|j| super::identity_clustering::Neighbor {
                        idx: j,
                        similarity: cosine(&embeddings[i], &embeddings[j]),
                    })
                    .collect()
            };
            // Keep only the top-k by similarity. select_nth_unstable partitions
            // in O(n), avoiding the O(n log n) full sort of all n-1 brute-force
            // neighbors when only k are used; then sort just those k for a stable
            // confidence-ordered result (identical top-k set + order).
            let cmp = |a: &super::identity_clustering::Neighbor,
                       b: &super::identity_clustering::Neighbor| {
                b.similarity
                    .partial_cmp(&a.similarity)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.idx.cmp(&b.idx))
            };
            if hits.len() > k {
                hits.select_nth_unstable_by(k, cmp);
                hits.truncate(k);
            }
            hits.sort_by(cmp);
            hits
        },
        super::identity_clustering::Hyperparameters::default(),
    );

    // Remap dense 0-based IDs to 1-based stable IDs in first-seen order
    // — preserves the on-disk schema and IPC contract that callers expect.
    let n = faces.len();
    let mut dense_to_stable: HashMap<usize, i32> = HashMap::new();
    let mut next_id: i32 = 1;
    let mut assignments = Vec::with_capacity(n);
    for i in 0..n {
        let dense = result.cluster_ids[i];
        let id = *dense_to_stable.entry(dense).or_insert_with(|| {
            let cur = next_id;
            next_id += 1;
            cur
        });
        assignments.push(ClusterAssignment {
            face_id: faces[i].face_id,
            cluster_id: id,
        });
    }

    // Anchors: highest-quality face per cluster.
    let mut by_cluster: HashMap<i32, Vec<usize>> = HashMap::new();
    for (i, a) in assignments.iter().enumerate() {
        by_cluster.entry(a.cluster_id).or_default().push(i);
    }
    let mut anchors = Vec::with_capacity(by_cluster.len());
    for (&cid, members) in &by_cluster {
        // identity_clustering shouldn't emit empty clusters; skip rather
        // than panic if it ever does.
        debug_assert!(!members.is_empty(), "empty cluster id {cid}");
        let Some(&best_idx) = members.iter().max_by(|&&a, &&b| {
            faces[a]
                .quality
                .partial_cmp(&faces[b].quality)
                .unwrap_or(std::cmp::Ordering::Equal)
        }) else {
            tracing::error!(cluster_id = cid, "skipping anchor for empty cluster");
            continue;
        };
        anchors.push(ClusterAnchor {
            cluster_id: cid,
            anchor_face_id: faces[best_idx].face_id,
            member_count: members.len() as u32,
        });
    }
    anchors.sort_by_key(|a| a.cluster_id);
    (assignments, anchors)
}

/// Default minimum CENTROID cosine to auto-fold two clusters into one person.
/// 0.88 is a CONSERVATIVE re-merge bar: only clusters whose centroids are near-
/// identical get folded, so it recovers a same-person over-split without ever
/// gluing two different people together. Override with `FILEID_FACE_AUTOMERGE_COS`
/// (clamped to [0.70, 1.0]; set 1.0 to disable and keep pure over-split).
/// Centroids (means of all member embeddings) are denoised, so this is safer
/// than any single anchor-to-anchor comparison.
///
/// **0.75 as of 2026-07-29, lowered from 0.88 on corpus measurement.** The prior
/// 0.88 rationale assumed "genuine same-person SFace cosine sits at 0.88-0.95",
/// which does not hold on a real family library: at 0.88 consolidate performed
/// only **3 merges across 3,092 non-mega clusters** — effectively inert, which is
/// why the same person stayed split across thousands of duplicate-burst clusters
/// (the owner's "tons are the same people" complaint).
///
/// Re-measured on the 2026-07-29 Adlon catalog, judging each candidate merge by a
/// LABEL-FREE safety criterion: a merge is unsafe if the merged cluster gains an
/// anti-correlated face pair (cosine < 0), which a single identity cannot contain.
///
/// ```text
/// automerge   merges   unsafe merges
///   0.88          3        0          (shipped before — inert)
///   0.78         78        0
///   0.75        140        0          <- chosen
///   0.70        329        1          <- safety margin ends here
/// ```
///
/// 0.75 gives 47x more same-person recovery than 0.88 with zero identity mixing,
/// and keeps a real margin above 0.70 where the first unsafe merge appears. It
/// cannot re-glue the known mega-clusters: their pairwise centroid cosines measure
/// 0.21-0.29, far below any threshold in this table.
///
/// Centroids (means of all member embeddings) are denoised, so this is safer than
/// any single anchor-to-anchor comparison. Consolidate also still respects user
/// "different people" verdicts and protected named clusters, so lowering the bar
/// cannot override an explicit human decision.
pub const AUTOMERGE_COS_DEFAULT: f32 = 0.75;

/// Resolve the auto-consolidation threshold from `FILEID_FACE_AUTOMERGE_COS`,
/// clamped to [0.70, 1.0]. A value ≥ 1.0 disables consolidation (no two
/// distinct centroids reach cosine 1.0). Unset/unparseable → the default.
pub fn automerge_threshold() -> f32 {
    std::env::var("FILEID_FACE_AUTOMERGE_COS")
        .ok()
        .and_then(|s| s.trim().parse::<f32>().ok())
        .map(|v| v.clamp(0.70, 1.0))
        .unwrap_or(AUTOMERGE_COS_DEFAULT)
}

/// Above this centroid count, consolidate() switches from brute O(C²) all-pairs
/// to an HNSW candidate sweep. Below it the brute path wins (HNSW build overhead
/// exceeds the O(C²) saving at small C).
const HNSW_CONSOLIDATE_THRESHOLD: usize = 2_000;

/// Brute O(C²) all-pairs over centroids. Returns merge edges `(cosine, i, j)`
/// with `i < j` and `cosine >= threshold`.
fn edges_brute(centroids: &[Vec<f32>], threshold: f32) -> Vec<(f32, usize, usize)> {
    let mut edges: Vec<(f32, usize, usize)> = Vec::new();
    for i in 0..centroids.len() {
        for j in (i + 1)..centroids.len() {
            let s = cosine(&centroids[i], &centroids[j]);
            if s >= threshold {
                edges.push((s, i, j));
            }
        }
    }
    edges
}

/// HNSW candidate sweep over centroids — the large-C path. Builds an index over
/// the per-cluster centroids, gathers top-k neighbors per centroid, then scores
/// each candidate with the EXACT cosine on the two centroids (the approximate
/// HNSW distance is used only to PROPOSE candidates, never as the edge weight).
/// Edges are deduped as `(min, max)` index pairs so each pair scores once.
/// Returns the same `(cosine, i, j)` shape as `edges_brute`, i < j.
fn edges_hnsw(centroids: &[Vec<f32>], threshold: f32) -> Vec<(f32, usize, usize)> {
    const INITIAL_K: usize = 64;
    const MAX_K: usize = 512;
    let points: Vec<(Vec<f32>, usize)> = centroids
        .iter()
        .enumerate()
        .map(|(i, c)| (c.clone(), i))
        .collect();
    let idx = crate::util::hnsw_index::build(points);
    let mut searcher = crate::util::hnsw_index::Searcher::default();
    let mut seen: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    let mut edges: Vec<(f32, usize, usize)> = Vec::new();
    for i in 0..centroids.len() {
        let mut k = (INITIAL_K + 1).min(centroids.len());
        let hits = loop {
            let hits = searcher.top_k(&idx, &centroids[i], k);
            let threshold_saturated = k < centroids.len()
                && hits
                    .iter()
                    .filter(|(j, _)| *j != i)
                    .all(|(j, _)| cosine(&centroids[i], &centroids[*j]) >= threshold);
            if threshold_saturated {
                if k < MAX_K.min(centroids.len()) {
                    k = (k * 2).min(MAX_K).min(centroids.len());
                    continue;
                }
                if k < centroids.len() {
                    return edges_brute(centroids, threshold);
                }
            }
            break hits;
        };
        for (j, _approx_dist) in hits {
            if j == i {
                continue;
            }
            let pair = if i < j { (i, j) } else { (j, i) };
            if !seen.insert(pair) {
                continue;
            }
            // EXACT cosine on the two centroids — not the approximate distance.
            let s = cosine(&centroids[pair.0], &centroids[pair.1]);
            if s >= threshold {
                edges.push((s, pair.0, pair.1));
            }
        }
    }
    edges
}

#[cfg(test)]
pub fn partition_protected_clusters<S1, S2>(
    faces: &[FaceRow],
    assignments: Vec<ClusterAssignment>,
    bucket_owner_by_face: &HashMap<i64, i64, S1>,
    different_pairs: &HashSet<(i64, i64), S2>,
) -> (Vec<ClusterAssignment>, Vec<ClusterAnchor>)
where
    S1: std::hash::BuildHasher,
    S2: std::hash::BuildHasher,
{
    partition_protected_clusters_excluding(
        faces,
        assignments,
        bucket_owner_by_face,
        different_pairs,
        &HashSet::new(),
    )
}

pub fn partition_protected_clusters_excluding<S1, S2, S3>(
    faces: &[FaceRow],
    assignments: Vec<ClusterAssignment>,
    bucket_owner_by_face: &HashMap<i64, i64, S1>,
    different_pairs: &HashSet<(i64, i64), S2>,
    excluded_face_ids: &HashSet<i64, S3>,
) -> (Vec<ClusterAssignment>, Vec<ClusterAnchor>)
where
    S1: std::hash::BuildHasher,
    S2: std::hash::BuildHasher,
    S3: std::hash::BuildHasher,
{
    let mut raw_groups: BTreeMap<i32, BTreeSet<i64>> = BTreeMap::new();
    for assignment in assignments {
        raw_groups
            .entry(assignment.cluster_id)
            .or_default()
            .insert(assignment.face_id);
    }

    let mut owner_groups: BTreeMap<i64, BTreeSet<i64>> = BTreeMap::new();
    for (&face_id, &owner_id) in bucket_owner_by_face {
        if !excluded_face_ids.contains(&face_id) {
            owner_groups.entry(owner_id).or_default().insert(face_id);
        }
    }
    let ownerless_endpoints: BTreeSet<i64> = different_pairs
        .iter()
        .flat_map(|&(a, b)| [a, b])
        .filter(|face_id| {
            !bucket_owner_by_face.contains_key(face_id) && !excluded_face_ids.contains(face_id)
        })
        .collect();
    let mut singleton_endpoints_by_owner: BTreeMap<i64, BTreeSet<i64>> = BTreeMap::new();
    for &(a, b) in different_pairs {
        if let (Some(owner_a), Some(owner_b)) = (
            bucket_owner_by_face.get(&a),
            bucket_owner_by_face.get(&b),
        ) {
            if owner_a == owner_b {
                singleton_endpoints_by_owner
                    .entry(*owner_a)
                    .or_default()
                    .extend([a, b]);
            }
        }
    }

    for members in raw_groups.values_mut() {
        members.retain(|face_id| {
            !excluded_face_ids.contains(face_id)
                && !bucket_owner_by_face.contains_key(face_id)
                && !ownerless_endpoints.contains(face_id)
        });
    }

    let mut buckets: Vec<Vec<i64>> = Vec::new();
    for (&owner_id, members) in &owner_groups {
        let singleton_endpoints = singleton_endpoints_by_owner
            .get(&owner_id)
            .cloned()
            .unwrap_or_default();
        let remainder: Vec<i64> = members
            .iter()
            .copied()
            .filter(|face_id| !singleton_endpoints.contains(face_id))
            .collect();
        if !remainder.is_empty() {
            buckets.push(remainder);
        }
        for face_id in singleton_endpoints {
            buckets.push(vec![face_id]);
        }
    }
    for face_id in ownerless_endpoints {
        buckets.push(vec![face_id]);
    }
    for members in raw_groups.into_values() {
        if !members.is_empty() {
            buckets.push(members.into_iter().collect());
        }
    }

    let quality: HashMap<i64, f32> = faces.iter().map(|face| (face.face_id, face.quality)).collect();
    let mut new_assignments = Vec::new();
    let mut new_anchors = Vec::new();
    for (index, mut members) in buckets.into_iter().enumerate() {
        members.sort_unstable();
        members.dedup();
        if members.is_empty() {
            continue;
        }
        let cluster_id = i32::try_from(index + 1).expect("protected cluster count exceeds i32");
        let anchor_face_id = members
            .iter()
            .copied()
            .max_by(|a, b| {
                let qa = quality.get(a).copied().unwrap_or(f32::NEG_INFINITY);
                let qb = quality.get(b).copied().unwrap_or(f32::NEG_INFINITY);
                qa.total_cmp(&qb).then_with(|| b.cmp(a))
            })
            .unwrap_or(members[0]);
        new_assignments.extend(members.iter().map(|&face_id| ClusterAssignment {
            face_id,
            cluster_id,
        }));
        new_anchors.push(ClusterAnchor {
            cluster_id,
            anchor_face_id,
            member_count: members.len() as u32,
        });
    }
    (new_assignments, new_anchors)
}

pub fn protected_owner_by_cluster<S1, S2>(
    identity_owner_by_face: &HashMap<i64, i64, S1>,
    cluster_of: &HashMap<i64, i32, S2>,
) -> Result<HashMap<i32, i64>, String>
where
    S1: std::hash::BuildHasher,
    S2: std::hash::BuildHasher,
{
    let mut owner_by_cluster = HashMap::new();
    for (&face_id, &owner_id) in identity_owner_by_face {
        let Some(&cluster_id) = cluster_of.get(&face_id) else {
            continue;
        };
        if let Some(existing) = owner_by_cluster.insert(cluster_id, owner_id) {
            if existing != owner_id {
                return Err(format!(
                    "cluster {cluster_id} contains distinct protected identities"
                ));
            }
        }
    }
    Ok(owner_by_cluster)
}

pub fn validate_protected_clusters<S1, S2>(
    assignments: &[ClusterAssignment],
    identity_owner_by_face: &HashMap<i64, i64, S1>,
    different_pairs: &HashSet<(i64, i64), S2>,
) -> Result<(), String>
where
    S1: std::hash::BuildHasher,
    S2: std::hash::BuildHasher,
{
    let cluster_of: HashMap<i64, i32> = assignments
        .iter()
        .map(|assignment| (assignment.face_id, assignment.cluster_id))
        .collect();
    let mut owner_of_cluster: HashMap<i32, i64> = HashMap::new();
    for (&face_id, &owner_id) in identity_owner_by_face {
        let Some(&cluster_id) = cluster_of.get(&face_id) else {
            return Err(format!("protected face {face_id} is missing from the final partition"));
        };
        if let Some(existing) = owner_of_cluster.insert(cluster_id, owner_id) {
            if existing != owner_id {
                return Err(format!(
                    "cluster {cluster_id} contains distinct protected identities"
                ));
            }
        }
    }
    for &(a, b) in different_pairs {
        let (Some(&ca), Some(&cb)) = (cluster_of.get(&a), cluster_of.get(&b)) else {
            return Err("a different-people verdict endpoint is missing from the final partition".into());
        };
        if ca == cb {
            return Err(format!(
                "cluster {ca} contains a user-confirmed different-people pair"
            ));
        }
    }
    Ok(())
}

pub fn protected_cluster_ids<S>(
    assignments: &[ClusterAssignment],
    protected_faces: &HashSet<i64, S>,
) -> HashSet<i32>
where
    S: std::hash::BuildHasher,
{
    assignments
        .iter()
        .filter_map(|assignment| {
            protected_faces
                .contains(&assignment.face_id)
                .then_some(assignment.cluster_id)
        })
        .collect()
}

/// Conservatively fold near-certain duplicate clusters that the over-split-safe
/// 3-pass clusterer left fragmented, using denoised per-cluster CENTROIDS.
///
/// Returns the same (assignments, anchors) shape with merged clusters collapsed
/// onto a single canonical id (the largest fragment, so its name + anchor face
/// survive; ties broken to the smallest id for determinism) and member counts
/// summed.
///
/// `blocked` holds normalized (min,max) cluster-id pairs the user has marked as
/// DIFFERENT people. A union is rejected if it would co-locate ANY blocked pair,
/// checked at every union step — so a blocked pair can never share a person even
/// transitively (X–Y both high to Z can't sneak a blocked X–Y together).
///
/// `threshold` ≥ 1.0 (or < 2 clusters) is a no-op: the inputs pass through
/// unchanged, preserving the pure over-split behavior.
#[cfg(test)]
pub fn consolidate<S: std::hash::BuildHasher>(
    faces: &[FaceRow],
    assignments: Vec<ClusterAssignment>,
    anchors: Vec<ClusterAnchor>,
    blocked: &std::collections::HashSet<(i32, i32), S>,
    threshold: f32,
) -> (Vec<ClusterAssignment>, Vec<ClusterAnchor>) {
    consolidate_with_protected_owners(
        faces,
        assignments,
        anchors,
        blocked,
        &HashMap::new(),
        threshold,
    )
}

pub fn consolidate_with_protected_owners<S1, S2>(
    faces: &[FaceRow],
    assignments: Vec<ClusterAssignment>,
    anchors: Vec<ClusterAnchor>,
    blocked: &HashSet<(i32, i32), S1>,
    protected_owner_by_cluster: &HashMap<i32, i64, S2>,
    threshold: f32,
) -> (Vec<ClusterAssignment>, Vec<ClusterAnchor>)
where
    S1: std::hash::BuildHasher,
    S2: std::hash::BuildHasher,
{
    // `>= 1.0` (not `> 1.0`): automerge_threshold() clamps to [0.70, 1.0], so the
    // documented "set FILEID_FACE_AUTOMERGE_COS=1.0 to disable" must hit this
    // no-op path. With a strict `>` the disable value still ran the full O(C²)
    // centroid scan (and could merge float-identical centroids).
    if threshold >= 1.0 || anchors.len() < 2 || faces.is_empty() {
        return (assignments, anchors);
    }
    let dim = faces[0].embedding.len();
    if dim == 0 {
        return (assignments, anchors);
    }

    // face_id → cluster_id (assignments are not assumed parallel to `faces`).
    let cluster_of: HashMap<i64, i32> =
        assignments.iter().map(|a| (a.face_id, a.cluster_id)).collect();

    // Per-cluster centroid = normalize(Σ member unit-embeddings). For unit
    // vectors the count cancels under renormalization, so the sum suffices.
    let mut sums: HashMap<i32, Vec<f32>> = HashMap::new();
    for f in faces {
        if f.embedding.len() != dim {
            continue;
        }
        if let Some(&cid) = cluster_of.get(&f.face_id) {
            let s = sums.entry(cid).or_insert_with(|| vec![0.0; dim]);
            for (acc, &x) in s.iter_mut().zip(f.embedding.iter()) {
                *acc += x;
            }
        }
    }
    let mut cids: Vec<i32> = sums.keys().copied().collect();
    cids.sort_unstable();
    let centroids: Vec<Vec<f32>> = cids
        .iter()
        .map(|cid| {
            let mut s = sums.get(cid).cloned().unwrap_or_else(|| vec![0.0; dim]);
            let n: f32 = s.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-8);
            for x in &mut s {
                *x /= n;
            }
            s
        })
        .collect();

    // Candidate merge edges over CENTROIDS (one vector per cluster, not per
    // face). Brute O(C²) below the threshold; an HNSW over the centroids above
    // it so a pathological over-split (tens of thousands of clusters) no longer
    // burns quadratic time — and, unlike the old 12k hard cap, never silently
    // skips consolidation. Both paths feed the SAME edge-sort + union-find +
    // blocked-conflict logic; only the candidate-generation strategy differs.
    let idx_of: HashMap<i32, usize> = cids.iter().enumerate().map(|(i, &c)| (c, i)).collect();
    let mut edges: Vec<(f32, usize, usize)> = if cids.len() <= HNSW_CONSOLIDATE_THRESHOLD {
        edges_brute(&centroids, threshold)
    } else {
        edges_hnsw(&centroids, threshold)
    };
    // Strongest merges first so canonical assignment is stable + greedy-optimal.
    edges.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut blocked_neighbors = vec![HashSet::new(); cids.len()];
    for &(cluster_a, cluster_b) in blocked {
        let (Some(&a), Some(&b)) = (idx_of.get(&cluster_a), idx_of.get(&cluster_b)) else {
            continue;
        };
        blocked_neighbors[a].insert(b);
        blocked_neighbors[b].insert(a);
    }
    let mut clusters_by_capture: HashMap<CaptureKey, BTreeSet<usize>> = HashMap::new();
    for face in faces {
        let Some(&cluster_id) = cluster_of.get(&face.face_id) else {
            continue;
        };
        let Some(&cluster_index) = idx_of.get(&cluster_id) else {
            continue;
        };
        clusters_by_capture
            .entry(capture_key(face))
            .or_default()
            .insert(cluster_index);
    }
    for clusters in clusters_by_capture.into_values() {
        let clusters: Vec<usize> = clusters.into_iter().collect();
        for (offset, &a) in clusters.iter().enumerate() {
            for &b in &clusters[(offset + 1)..] {
                blocked_neighbors[a].insert(b);
                blocked_neighbors[b].insert(a);
            }
        }
    }

    let mut parent: Vec<usize> = (0..cids.len()).collect();
    let mut component_size = vec![1usize; cids.len()];
    let mut forbidden = blocked_neighbors;
    let mut protected_owner: Vec<Option<i64>> = cids
        .iter()
        .map(|cluster_id| protected_owner_by_cluster.get(cluster_id).copied())
        .collect();
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]]; // path halving
            x = parent[x];
        }
        x
    }
    let mut any_merge = false;
    for (_s, i, j) in edges {
        let ri = find(&mut parent, i);
        let rj = find(&mut parent, j);
        if ri == rj {
            continue;
        }
        let explicit_conflict = forbidden[ri].contains(&rj) || forbidden[rj].contains(&ri);
        let owner_conflict = matches!(
            (protected_owner[ri], protected_owner[rj]),
            (Some(a), Some(b)) if a != b
        );
        if explicit_conflict || owner_conflict {
            continue;
        }
        // Keep the component with the larger forbidden set as the root. Every
        // explicit edge then moves only when it belongs to the smaller set.
        let (keep, drop) = match forbidden[ri].len().cmp(&forbidden[rj].len()) {
            std::cmp::Ordering::Greater => (ri, rj),
            std::cmp::Ordering::Less => (rj, ri),
            std::cmp::Ordering::Equal if component_size[ri] >= component_size[rj] => (ri, rj),
            std::cmp::Ordering::Equal => (rj, ri),
        };
        parent[drop] = keep;
        component_size[keep] += component_size[drop];
        protected_owner[keep] = protected_owner[keep].or(protected_owner[drop]);
        let moved = std::mem::take(&mut forbidden[drop]);
        for neighbor in moved {
            let neighbor_root = find(&mut parent, neighbor);
            if neighbor_root == keep {
                continue;
            }
            forbidden[neighbor_root].remove(&drop);
            forbidden[neighbor_root].insert(keep);
            forbidden[keep].insert(neighbor_root);
        }
        forbidden[keep].remove(&drop);
        forbidden[keep].remove(&keep);
        any_merge = true;
    }
    if !any_merge {
        return (assignments, anchors);
    }

    let count_of: HashMap<i32, u32> =
        anchors.iter().map(|a| (a.cluster_id, a.member_count)).collect();
    // Group cluster ids by union root, pick the canonical id per group.
    let mut groups: HashMap<usize, Vec<i32>> = HashMap::new();
    for (i, &c) in cids.iter().enumerate() {
        groups.entry(find(&mut parent, i)).or_default().push(c);
    }
    let mut remap: HashMap<i32, i32> = HashMap::new();
    for members in groups.values() {
        // Canonical = largest fragment (its name + anchor survive); tie → lowest id.
        let canon = *members
            .iter()
            .max_by(|&&a, &&b| {
                let ca = count_of.get(&a).copied().unwrap_or(0);
                let cb = count_of.get(&b).copied().unwrap_or(0);
                ca.cmp(&cb).then(b.cmp(&a))
            })
            .expect("non-empty group");
        for &c in members {
            remap.insert(c, canon);
        }
    }

    let new_assignments: Vec<ClusterAssignment> = assignments
        .into_iter()
        .map(|a| ClusterAssignment {
            face_id: a.face_id,
            cluster_id: remap.get(&a.cluster_id).copied().unwrap_or(a.cluster_id),
        })
        .collect();

    // Surviving anchor = the canonical fragment's anchor; member_count summed
    // across the merged group.
    let anchor_by_cid: HashMap<i32, ClusterAnchor> =
        anchors.into_iter().map(|a| (a.cluster_id, a)).collect();
    let mut summed: HashMap<i32, u32> = HashMap::new();
    for (&old, &canon) in &remap {
        *summed.entry(canon).or_insert(0) +=
            anchor_by_cid.get(&old).map(|a| a.member_count).unwrap_or(0);
    }
    let mut new_anchors: Vec<ClusterAnchor> = summed
        .into_iter()
        .filter_map(|(canon, total)| {
            let base = anchor_by_cid.get(&canon)?;
            Some(ClusterAnchor {
                cluster_id: canon,
                anchor_face_id: base.anchor_face_id,
                member_count: total,
            })
        })
        .collect();
    new_anchors.extend(
        anchor_by_cid
            .into_iter()
            .filter_map(|(cid, anchor)| (!remap.contains_key(&cid)).then_some(anchor)),
    );
    new_anchors.sort_by_key(|a| a.cluster_id);
    (new_assignments, new_anchors)
}

pub const FRAGMENT_RECOVERY_COS_DEFAULT: f32 = 0.75;

const FRAGMENT_RECOVERY_MAX_FACES_DEFAULT: u32 = 12;
const FRAGMENT_RECOVERY_MARGIN_DEFAULT: f32 = 0.05;
const FRAGMENT_SOURCE_MIN_COHESION: f32 = 0.70;
const FRAGMENT_TARGET_MIN_COHESION: f32 = 0.70;
const FRAGMENT_MEMBER_COS: f32 = 0.75;
const LARGE_FRAGMENT_MAX_FACES: u32 = 256;
const LARGE_FRAGMENT_MIN_CENTROID_COS: f32 = 0.85;
const LARGE_FRAGMENT_TARGET_SIZE_RATIO: u32 = 16;
const LARGE_FRAGMENT_MIN_SOURCE_VOTERS: usize = 16;
const LARGE_FRAGMENT_MIN_SOURCE_VOTER_RATIO: f32 = 0.80;
const LARGE_FRAGMENT_MIN_DISTINCT_CAPTURE_RATIO: f32 = 0.25;
const FRAGMENT_MIN_SOURCE_VOTERS: usize = 2;
const FRAGMENT_MIN_TARGET_VOTERS: usize = 3;
const FRAGMENT_EVIDENCE_SAMPLE_LIMIT: usize = 64;
const FRAGMENT_BRUTE_TARGET_LIMIT: usize = 4_096;
const FRAGMENT_HNSW_CANDIDATES: usize = 64;
const FRAGMENT_HNSW_MAX_CANDIDATES: usize = 512;

pub fn fragment_recovery_threshold() -> f32 {
    std::env::var("FILEID_FACE_FRAGMENT_COS")
        .ok()
        .and_then(|value| value.trim().parse::<f32>().ok())
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(FRAGMENT_RECOVERY_COS_DEFAULT, 1.0))
        .unwrap_or(FRAGMENT_RECOVERY_COS_DEFAULT)
}

pub fn fragment_recovery_max_faces() -> u32 {
    std::env::var("FILEID_FACE_FRAGMENT_MAX_FACES")
        .ok()
        .and_then(|value| value.trim().parse::<u32>().ok())
        .map(|value| value.clamp(3, 64))
        .unwrap_or(FRAGMENT_RECOVERY_MAX_FACES_DEFAULT)
}

pub fn fragment_recovery_margin() -> f32 {
    std::env::var("FILEID_FACE_FRAGMENT_MARGIN")
        .ok()
        .and_then(|value| value.trim().parse::<f32>().ok())
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(FRAGMENT_RECOVERY_MARGIN_DEFAULT, 0.25))
        .unwrap_or(FRAGMENT_RECOVERY_MARGIN_DEFAULT)
}

#[derive(Debug, Clone)]
struct ClusterVectorStats {
    cluster_id: i32,
    sum: Vec<f32>,
    centroid: Vec<f32>,
    member_count: u32,
    cohesion: f32,
    anchor_radius: f32,
    capture_keys: HashSet<CaptureKey>,
    member_indices: Vec<usize>,
}

fn cluster_vector_stats(
    faces: &[FaceRow],
    assignments: &[ClusterAssignment],
) -> Vec<ClusterVectorStats> {
    let Some(dim) = faces
        .iter()
        .find_map(|face| (!face.embedding.is_empty()).then_some(face.embedding.len()))
    else {
        return Vec::new();
    };
    let cluster_of: HashMap<i64, i32> = assignments
        .iter()
        .map(|assignment| (assignment.face_id, assignment.cluster_id))
        .collect();
    let mut sums: BTreeMap<i32, (Vec<f32>, u32, HashSet<CaptureKey>, Vec<usize>)> =
        BTreeMap::new();
    for (face_index, face) in faces.iter().enumerate() {
        if face.embedding.len() != dim {
            continue;
        }
        let Some(&cluster_id) = cluster_of.get(&face.face_id) else {
            continue;
        };
        let (sum, member_count, capture_keys, member_indices) = sums
            .entry(cluster_id)
            .or_insert_with(|| (vec![0.0; dim], 0, HashSet::new(), Vec::new()));
        for (slot, value) in sum.iter_mut().zip(&face.embedding) {
            *slot += value;
        }
        *member_count += 1;
        capture_keys.insert(capture_key(face));
        member_indices.push(face_index);
    }
    sums.into_iter()
        .filter_map(|(cluster_id, (sum, member_count, capture_keys, member_indices))| {
            if member_count == 0 {
                return None;
            }
            let norm = sum.iter().map(|value| value * value).sum::<f32>().sqrt();
            if !norm.is_finite() || norm <= f32::MIN_POSITIVE {
                return None;
            }
            let centroid: Vec<f32> = sum.iter().map(|value| value / norm).collect();
            let anchor_radius = if member_indices.len() < 2 {
                0.50
            } else {
                let mut similarities: Vec<f32> = member_indices
                    .iter()
                    .map(|&index| cosine(&faces[index].embedding, &centroid))
                    .collect();
                similarities.sort_by(f32::total_cmp);
                let p10 = (similarities.len() as f32 * 0.10).floor() as usize;
                similarities[p10.min(similarities.len() - 1)].clamp(0.45, 0.85)
            };
            Some(ClusterVectorStats {
                cluster_id,
                sum,
                centroid,
                member_count,
                cohesion: norm / member_count as f32,
                anchor_radius,
                capture_keys,
                member_indices,
            })
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct IdentityAnchor {
    pub cluster_id: i32,
    pub centroid: Vec<f32>,
    pub anchor_radius: f32,
}

pub fn identity_anchors(
    faces: &[FaceRow],
    assignments: &[ClusterAssignment],
) -> Vec<IdentityAnchor> {
    cluster_vector_stats(faces, assignments)
        .into_iter()
        .map(|stat| IdentityAnchor {
            cluster_id: stat.cluster_id,
            centroid: stat.centroid,
            anchor_radius: stat.anchor_radius,
        })
        .collect()
}

fn distinct_evidence_members(faces: &[FaceRow], stat: &ClusterVectorStats) -> Vec<usize> {
    select_distinct_evidence_members(faces, stat.member_indices.clone())
}

fn select_distinct_evidence_members(
    faces: &[FaceRow],
    mut members: Vec<usize>,
) -> Vec<usize> {
    members.sort_by(|&a, &b| {
        faces[b]
            .quality
            .total_cmp(&faces[a].quality)
            .then_with(|| faces[a].face_id.cmp(&faces[b].face_id))
    });
    let mut seen_content = HashSet::new();
    let mut seen_embeddings = HashSet::new();
    let mut distinct = Vec::new();
    for index in members {
        let Some(content_hash) = faces[index].content_hash.as_ref() else {
            continue;
        };
        let embedding_bits: Vec<u32> = faces[index]
            .embedding
            .iter()
            .map(|value| value.to_bits())
            .collect();
        if !seen_content.insert(content_hash.clone()) || !seen_embeddings.insert(embedding_bits) {
            continue;
        }
        distinct.push(index);
        if distinct.len() == FRAGMENT_EVIDENCE_SAMPLE_LIMIT {
            break;
        }
    }
    distinct
}

fn merged_evidence_members(
    faces: &[FaceRow],
    target: &[usize],
    source: &[usize],
) -> Vec<usize> {
    let mut members = Vec::with_capacity(target.len() + source.len());
    members.extend_from_slice(target);
    members.extend_from_slice(source);
    select_distinct_evidence_members(faces, members)
}

#[derive(Debug, Clone, Copy)]
struct FragmentEvidence {
    source_voters: usize,
    target_voters: usize,
    robust_similarity: f32,
}

fn fragment_evidence(
    faces: &[FaceRow],
    source: &ClusterVectorStats,
    target: &ClusterVectorStats,
) -> Option<FragmentEvidence> {
    let source_members = distinct_evidence_members(faces, source);
    let target_members = distinct_evidence_members(faces, target);
    fragment_evidence_from_members(faces, &source_members, &target_members)
}

fn fragment_evidence_from_members(
    faces: &[FaceRow],
    source_members: &[usize],
    target_members: &[usize],
) -> Option<FragmentEvidence> {
    if source_members.len() < FRAGMENT_MIN_SOURCE_VOTERS
        || target_members.len() < FRAGMENT_MIN_TARGET_VOTERS
    {
        return None;
    }
    let mut source_scores = Vec::new();
    let mut target_voters = HashSet::new();
    for &source_index in source_members {
        let mut similarities: Vec<(f32, usize)> = target_members
            .iter()
            .map(|&target_index| {
                (
                    cosine(
                        &faces[source_index].embedding,
                        &faces[target_index].embedding,
                    ),
                    target_index,
                )
            })
            .filter(|(similarity, _)| *similarity >= FRAGMENT_MEMBER_COS)
            .collect();
        similarities.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        if similarities.len() < FRAGMENT_MIN_TARGET_VOTERS {
            return None;
        }
        source_scores.push(similarities[FRAGMENT_MIN_TARGET_VOTERS - 1].0);
        target_voters.extend(similarities.into_iter().map(|(_, index)| index));
    }
    if source_scores.len() < FRAGMENT_MIN_SOURCE_VOTERS
        || target_voters.len() < FRAGMENT_MIN_TARGET_VOTERS
    {
        return None;
    }
    source_scores.sort_by(|a, b| b.total_cmp(a));
    Some(FragmentEvidence {
        source_voters: source_scores.len(),
        target_voters: target_voters.len(),
        robust_similarity: source_scores[FRAGMENT_MIN_SOURCE_VOTERS - 1],
    })
}

fn large_fragment_evidence(
    faces: &[FaceRow],
    source: &ClusterVectorStats,
    target: &ClusterVectorStats,
) -> Option<FragmentEvidence> {
    let source_members = distinct_evidence_members(faces, source);
    let target_members = distinct_evidence_members(faces, target);
    large_fragment_evidence_from_members(faces, &source_members, &target_members)
}

fn large_fragment_evidence_from_members(
    faces: &[FaceRow],
    source_members: &[usize],
    target_members: &[usize],
) -> Option<FragmentEvidence> {
    if source_members.len() < LARGE_FRAGMENT_MIN_SOURCE_VOTERS
        || target_members.len() < FRAGMENT_MIN_TARGET_VOTERS
    {
        return None;
    }
    let required_source_voters = ((source_members.len() as f32
        * LARGE_FRAGMENT_MIN_SOURCE_VOTER_RATIO)
        .ceil() as usize)
        .max(LARGE_FRAGMENT_MIN_SOURCE_VOTERS);
    let mut source_scores = Vec::new();
    let mut target_voters = HashSet::new();
    for &source_index in source_members {
        let mut similarities: Vec<(f32, usize)> = target_members
            .iter()
            .map(|&target_index| {
                (
                    cosine(
                        &faces[source_index].embedding,
                        &faces[target_index].embedding,
                    ),
                    target_index,
                )
            })
            .filter(|(similarity, _)| *similarity >= FRAGMENT_MEMBER_COS)
            .collect();
        similarities.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        if similarities.len() < FRAGMENT_MIN_TARGET_VOTERS {
            continue;
        }
        source_scores.push(similarities[FRAGMENT_MIN_TARGET_VOTERS - 1].0);
        target_voters.extend(similarities.into_iter().map(|(_, index)| index));
    }
    if source_scores.len() < required_source_voters
        || target_voters.len() < FRAGMENT_MIN_TARGET_VOTERS
    {
        return None;
    }
    source_scores.sort_by(|a, b| b.total_cmp(a));
    Some(FragmentEvidence {
        source_voters: source_scores.len(),
        target_voters: target_voters.len(),
        robust_similarity: source_scores[required_source_voters - 1],
    })
}

pub fn corroborated_fragment_match(source: &[FaceRow], target: &[FaceRow]) -> bool {
    if source.len() < FRAGMENT_MIN_SOURCE_VOTERS
        || source.len() > FRAGMENT_RECOVERY_MAX_FACES_DEFAULT as usize
        || target.len() <= FRAGMENT_RECOVERY_MAX_FACES_DEFAULT as usize
    {
        return false;
    }
    let source_captures: HashSet<CaptureKey> = source.iter().map(capture_key).collect();
    let target_captures: HashSet<CaptureKey> = target.iter().map(capture_key).collect();
    if source_captures.len() != source.len() || !source_captures.is_disjoint(&target_captures) {
        return false;
    }
    let cohesion = |faces: &[FaceRow]| {
        let Some(dim) = faces.first().map(|face| face.embedding.len()) else {
            return 0.0;
        };
        if dim == 0 || faces.iter().any(|face| face.embedding.len() != dim) {
            return 0.0;
        }
        let mut sum = vec![0.0; dim];
        for face in faces {
            for (slot, value) in sum.iter_mut().zip(&face.embedding) {
                *slot += value;
            }
        }
        sum.iter().map(|value| value * value).sum::<f32>().sqrt() / faces.len() as f32
    };
    if cohesion(source) < FRAGMENT_SOURCE_MIN_COHESION
        || cohesion(target) < FRAGMENT_TARGET_MIN_COHESION
    {
        return false;
    }
    let distinct = |faces: &[FaceRow]| {
        let stat = ClusterVectorStats {
            cluster_id: 0,
            sum: Vec::new(),
            centroid: Vec::new(),
            member_count: faces.len() as u32,
            cohesion: 0.0,
            anchor_radius: 0.0,
            capture_keys: HashSet::new(),
            member_indices: (0..faces.len()).collect(),
        };
        distinct_evidence_members(faces, &stat)
    };
    let source_members = distinct(source);
    let target_members = distinct(target);
    if source_members.len() < FRAGMENT_MIN_SOURCE_VOTERS
        || target_members.len() < FRAGMENT_MIN_TARGET_VOTERS
    {
        return false;
    }
    source_members.into_iter().all(|source_index| {
        target_members
            .iter()
            .filter(|&&target_index| {
                cosine(
                    &source[source_index].embedding,
                    &target[target_index].embedding,
                ) >= FRAGMENT_MEMBER_COS
            })
            .take(FRAGMENT_MIN_TARGET_VOTERS)
            .count()
            == FRAGMENT_MIN_TARGET_VOTERS
    })
}

#[derive(Debug, Clone, Copy)]
struct FragmentProposal {
    centroid_similarity: f32,
    evidence: FragmentEvidence,
    required_similarity: f32,
    source: usize,
    target: usize,
}

pub fn recover_small_fragments<S1, S2, S3>(
    faces: &[FaceRow],
    assignments: Vec<ClusterAssignment>,
    anchors: Vec<ClusterAnchor>,
    blocked: &HashSet<(i32, i32), S1>,
    protected_owner_by_cluster: &HashMap<i32, i64, S2>,
    blocked_target_clusters: &HashSet<i32, S3>,
) -> (Vec<ClusterAssignment>, Vec<ClusterAnchor>)
where
    S1: std::hash::BuildHasher,
    S2: std::hash::BuildHasher,
    S3: std::hash::BuildHasher,
{
    recover_small_fragments_with_params(
        faces,
        assignments,
        anchors,
        blocked,
        protected_owner_by_cluster,
        blocked_target_clusters,
        fragment_recovery_threshold(),
        fragment_recovery_max_faces(),
        fragment_recovery_margin(),
    )
}

fn recover_small_fragments_with_params<S1, S2, S3>(
    faces: &[FaceRow],
    assignments: Vec<ClusterAssignment>,
    anchors: Vec<ClusterAnchor>,
    blocked: &HashSet<(i32, i32), S1>,
    protected_owner_by_cluster: &HashMap<i32, i64, S2>,
    blocked_target_clusters: &HashSet<i32, S3>,
    threshold: f32,
    max_fragment_faces: u32,
    margin_floor: f32,
) -> (Vec<ClusterAssignment>, Vec<ClusterAnchor>)
where
    S1: std::hash::BuildHasher,
    S2: std::hash::BuildHasher,
    S3: std::hash::BuildHasher,
{
    if threshold >= 1.0 || anchors.len() < 2 {
        return (assignments, anchors);
    }
    let stats = cluster_vector_stats(faces, &assignments);
    if stats.len() < 2 {
        return (assignments, anchors);
    }
    let eligible_targets: Vec<usize> = stats
        .iter()
        .enumerate()
        .filter_map(|(index, stat)| {
            (stat.member_count > max_fragment_faces
                && stat.cohesion >= FRAGMENT_TARGET_MIN_COHESION)
                .then_some(index)
        })
        .collect();
    if eligible_targets.is_empty() {
        return (assignments, anchors);
    }
    let direct_allowed = |source: usize, target: usize| {
        let source_owner = protected_owner_by_cluster
            .get(&stats[source].cluster_id)
            .copied();
        let pair = if stats[source].cluster_id < stats[target].cluster_id {
            (stats[source].cluster_id, stats[target].cluster_id)
        } else {
            (stats[target].cluster_id, stats[source].cluster_id)
        };
        source != target
            && source_owner.is_none()
            && !blocked_target_clusters.contains(&stats[target].cluster_id)
            && !blocked.contains(&pair)
            && stats[source]
                .capture_keys
                .is_disjoint(&stats[target].capture_keys)
    };
    let hnsw = (eligible_targets.len() > FRAGMENT_BRUTE_TARGET_LIMIT).then(|| {
        crate::util::hnsw_index::build(
            eligible_targets
                .iter()
                .map(|&index| (stats[index].centroid.clone(), index))
                .collect(),
        )
    });
    let mut searcher = crate::util::hnsw_index::Searcher::default();
    let mut proposals = Vec::new();
    for (source, stat) in stats.iter().enumerate() {
        let is_small_fragment = stat.member_count <= max_fragment_faces
            && stat.capture_keys.len() == stat.member_count as usize;
        let is_large_fragment = stat.member_count >= LARGE_FRAGMENT_MIN_SOURCE_VOTERS as u32
            && stat.member_count > max_fragment_faces
            && stat.member_count <= LARGE_FRAGMENT_MAX_FACES
            && stat.capture_keys.len() as f32 / stat.member_count as f32
                >= LARGE_FRAGMENT_MIN_DISTINCT_CAPTURE_RATIO;
        if stat.member_count < FRAGMENT_MIN_SOURCE_VOTERS as u32
            || (!is_small_fragment && !is_large_fragment)
            || stat.cohesion < FRAGMENT_SOURCE_MIN_COHESION
            || protected_owner_by_cluster.contains_key(&stat.cluster_id)
        {
            continue;
        }
        let (mut hits, unresolved_dense_band): (Vec<(f32, usize)>, bool) =
            if let Some(index) = &hnsw {
                let mut k = (FRAGMENT_HNSW_CANDIDATES + 1).min(eligible_targets.len());
                loop {
                    let hits: Vec<(f32, usize)> = searcher
                        .top_k(index, &stat.centroid, k)
                        .into_iter()
                        .map(|(target, _)| {
                            (
                                cosine(&stat.centroid, &stats[target].centroid),
                                target,
                            )
                        })
                        .collect();
                    let saturated = k < eligible_targets.len()
                        && hits
                            .iter()
                            .filter(|(_, target)| *target != source)
                            .all(|(similarity, _)| *similarity >= threshold);
                    if saturated
                        && k < FRAGMENT_HNSW_MAX_CANDIDATES.min(eligible_targets.len())
                    {
                        k = (k * 2)
                            .min(FRAGMENT_HNSW_MAX_CANDIDATES)
                            .min(eligible_targets.len());
                        continue;
                    }
                    break (
                        hits,
                        saturated
                            && k == FRAGMENT_HNSW_MAX_CANDIDATES
                            && k < eligible_targets.len(),
                    );
                }
            } else {
                (
                    eligible_targets
                        .iter()
                        .map(|&target| {
                            (
                                cosine(&stat.centroid, &stats[target].centroid),
                                target,
                            )
                        })
                        .collect(),
                    false,
                )
            };
        if unresolved_dense_band {
            continue;
        }
        let required_similarity = if is_large_fragment {
            threshold.max(LARGE_FRAGMENT_MIN_CENTROID_COS)
        } else {
            threshold
        };
        hits.retain(|&(similarity, target)| {
            similarity >= required_similarity
                && (!is_large_fragment
                    || stats[target].member_count
                        >= stat
                            .member_count
                            .saturating_mul(LARGE_FRAGMENT_TARGET_SIZE_RATIO))
                && direct_allowed(source, target)
        });
        hits.sort_by(|a, b| {
            b.0.total_cmp(&a.0)
                .then_with(|| stats[a.1].cluster_id.cmp(&stats[b.1].cluster_id))
        });
        let Some(&(centroid_similarity, target)) = hits.first() else {
            continue;
        };
        let second = hits
            .get(1)
            .map(|candidate| candidate.0)
            .unwrap_or(-1.0);
        if centroid_similarity - second < margin_floor {
            continue;
        }
        let evidence = if is_large_fragment {
            large_fragment_evidence(faces, stat, &stats[target])
        } else {
            fragment_evidence(faces, stat, &stats[target])
        };
        if let Some(evidence) = evidence {
            proposals.push(FragmentProposal {
                centroid_similarity,
                evidence,
                required_similarity,
                source,
                target,
            });
        }
    }
    proposals.sort_by(|a, b| {
        stats[b.source]
            .member_count
            .cmp(&stats[a.source].member_count)
            .then_with(|| stats[a.source].cluster_id.cmp(&stats[b.source].cluster_id))
            .then_with(|| b.centroid_similarity.total_cmp(&a.centroid_similarity))
            .then_with(|| {
                b.evidence
                    .robust_similarity
                    .total_cmp(&a.evidence.robust_similarity)
            })
            .then_with(|| b.evidence.source_voters.cmp(&a.evidence.source_voters))
            .then_with(|| b.evidence.target_voters.cmp(&a.evidence.target_voters))
    });

    let mut group_members: Vec<Vec<usize>> =
        (0..stats.len()).map(|index| vec![index]).collect();
    let mut group_captures: Vec<HashSet<CaptureKey>> =
        stats.iter().map(|stat| stat.capture_keys.clone()).collect();
    let mut group_sums: Vec<Vec<f32>> = stats.iter().map(|stat| stat.sum.clone()).collect();
    let mut group_counts: Vec<u32> = stats.iter().map(|stat| stat.member_count).collect();
    let mut group_centroids: Vec<Vec<f32>> =
        stats.iter().map(|stat| stat.centroid.clone()).collect();
    let mut group_evidence_members: Vec<Option<Vec<usize>>> = vec![None; stats.len()];
    let mut group_blocked_target: Vec<bool> = stats
        .iter()
        .map(|stat| blocked_target_clusters.contains(&stat.cluster_id))
        .collect();
    let mut group_owner: Vec<Option<i64>> = stats
        .iter()
        .map(|stat| {
            protected_owner_by_cluster
                .get(&stat.cluster_id)
                .copied()
        })
        .collect();
    let mut parent: Vec<usize> = (0..stats.len()).collect();
    fn find_root(parent: &mut [usize], mut index: usize) -> usize {
        while parent[index] != index {
            parent[index] = parent[parent[index]];
            index = parent[index];
        }
        index
    }
    fn cached_evidence_members(
        faces: &[FaceRow],
        stats: &[ClusterVectorStats],
        cache: &mut [Option<Vec<usize>>],
        index: usize,
    ) -> Vec<usize> {
        if let Some(members) = &cache[index] {
            return members.clone();
        }
        let members = distinct_evidence_members(faces, &stats[index]);
        cache[index] = Some(members.clone());
        members
    }
    let mut any_merge = false;
    let mut dirty_target_roots = BTreeSet::new();
    for proposal in proposals {
        let source = find_root(&mut parent, proposal.source);
        let proposed_target = find_root(&mut parent, proposal.target);
        if source == proposed_target
            || source != proposal.source
            || group_owner[source].is_some()
        {
            continue;
        }

        let mut target_roots = BTreeSet::new();
        let mut unresolved_dense_band = false;
        if let Some(index) = &hnsw {
            if dirty_target_roots.len() > FRAGMENT_HNSW_MAX_CANDIDATES {
                break;
            }
            let mut k = (FRAGMENT_HNSW_CANDIDATES + 1).min(eligible_targets.len());
            let hits = loop {
                let hits = searcher.top_k(index, &stats[source].centroid, k);
                let saturated = k < eligible_targets.len()
                    && hits
                        .iter()
                        .filter(|(candidate, _)| *candidate != source)
                        .all(|(candidate, _)| {
                            cosine(
                                &stats[source].centroid,
                                &stats[*candidate].centroid,
                            ) >= proposal.required_similarity
                        });
                if saturated
                    && k < FRAGMENT_HNSW_MAX_CANDIDATES.min(eligible_targets.len())
                {
                    k = (k * 2)
                        .min(FRAGMENT_HNSW_MAX_CANDIDATES)
                        .min(eligible_targets.len());
                    continue;
                }
                unresolved_dense_band = saturated
                    && k == FRAGMENT_HNSW_MAX_CANDIDATES
                    && k < eligible_targets.len();
                break hits;
            };
            for (candidate, _) in hits {
                target_roots.insert(find_root(&mut parent, candidate));
            }
            for &candidate in &dirty_target_roots {
                target_roots.insert(find_root(&mut parent, candidate));
            }
        } else {
            for &candidate in &eligible_targets {
                target_roots.insert(find_root(&mut parent, candidate));
            }
        }
        if unresolved_dense_band {
            continue;
        }
        target_roots.insert(proposed_target);

        let is_large_fragment = stats[source].member_count > max_fragment_faces;
        let mut current_hits = Vec::new();
        for target in target_roots {
            if source == target
                || group_counts[target] <= max_fragment_faces
                || group_blocked_target[target]
            {
                continue;
            }
            if is_large_fragment
                && group_counts[target]
                    < stats[source]
                        .member_count
                        .saturating_mul(LARGE_FRAGMENT_TARGET_SIZE_RATIO)
            {
                continue;
            }
            let blocked_by_group = group_members[source].iter().any(|&source_member| {
                group_members[target].iter().any(|&target_member| {
                    let pair =
                        if stats[source_member].cluster_id < stats[target_member].cluster_id {
                            (
                                stats[source_member].cluster_id,
                                stats[target_member].cluster_id,
                            )
                        } else {
                            (
                                stats[target_member].cluster_id,
                                stats[source_member].cluster_id,
                            )
                        };
                    blocked.contains(&pair)
                })
            });
            if blocked_by_group
                || !group_captures[source].is_disjoint(&group_captures[target])
            {
                continue;
            }
            let target_norm = group_sums[target]
                .iter()
                .map(|value| value * value)
                .sum::<f32>()
                .sqrt();
            if !target_norm.is_finite()
                || target_norm / (group_counts[target] as f32)
                    < FRAGMENT_TARGET_MIN_COHESION
            {
                continue;
            }
            let similarity = cosine(&stats[source].centroid, &group_centroids[target]);
            if similarity >= proposal.required_similarity {
                current_hits.push((similarity, target));
            }
        }
        current_hits.sort_by(|a, b| {
            b.0.total_cmp(&a.0).then_with(|| {
                let a_cluster = group_members[a.1]
                    .iter()
                    .map(|&member| stats[member].cluster_id)
                    .min()
                    .unwrap_or(i32::MAX);
                let b_cluster = group_members[b.1]
                    .iter()
                    .map(|&member| stats[member].cluster_id)
                    .min()
                    .unwrap_or(i32::MAX);
                a_cluster.cmp(&b_cluster)
            })
        });
        let Some(&(current_similarity, target)) = current_hits.first() else {
            continue;
        };
        if target != proposed_target {
            continue;
        }
        let current_second = current_hits
            .get(1)
            .map(|candidate| candidate.0)
            .unwrap_or(-1.0);
        if current_similarity - current_second < margin_floor {
            continue;
        }

        let source_evidence_members =
            cached_evidence_members(faces, &stats, &mut group_evidence_members, source);
        let target_evidence_members =
            cached_evidence_members(faces, &stats, &mut group_evidence_members, target);
        let current_evidence = if is_large_fragment {
            large_fragment_evidence_from_members(
                faces,
                &source_evidence_members,
                &target_evidence_members,
            )
        } else {
            fragment_evidence_from_members(
                faces,
                &source_evidence_members,
                &target_evidence_members,
            )
        };
        if current_evidence.is_none() {
            continue;
        }

        let combined_sum: Vec<f32> = group_sums[target]
            .iter()
            .zip(&group_sums[source])
            .map(|(a, b)| a + b)
            .collect();
        let combined_norm = combined_sum
            .iter()
            .map(|value| value * value)
            .sum::<f32>()
            .sqrt();
        let combined_count = group_counts[target] + group_counts[source];
        if !combined_norm.is_finite()
            || combined_norm / (combined_count as f32) < FRAGMENT_TARGET_MIN_COHESION
        {
            continue;
        }
        parent[source] = target;
        group_sums[target] = combined_sum;
        group_counts[target] = combined_count;
        group_centroids[target] = group_sums[target]
            .iter()
            .map(|value| value / combined_norm)
            .collect();
        group_evidence_members[target] = Some(merged_evidence_members(
            faces,
            &target_evidence_members,
            &source_evidence_members,
        ));
        let source_captures = std::mem::take(&mut group_captures[source]);
        group_captures[target].extend(source_captures);
        group_blocked_target[target] =
            group_blocked_target[target] || group_blocked_target[source];
        group_owner[target] = group_owner[target].or(group_owner[source]);
        let source_members = std::mem::take(&mut group_members[source]);
        group_members[target].extend(source_members);
        dirty_target_roots.insert(target);
        any_merge = true;
    }
    if !any_merge {
        return (assignments, anchors);
    }
    let index_by_cluster: HashMap<i32, usize> = stats
        .iter()
        .enumerate()
        .map(|(index, stat)| (stat.cluster_id, index))
        .collect();
    let mut remap = HashMap::new();
    for (index, stat) in stats.iter().enumerate() {
        let root = find_root(&mut parent, index);
        remap.insert(stat.cluster_id, stats[root].cluster_id);
    }
    let new_assignments = assignments
        .into_iter()
        .map(|assignment| ClusterAssignment {
            face_id: assignment.face_id,
            cluster_id: remap
                .get(&assignment.cluster_id)
                .copied()
                .unwrap_or(assignment.cluster_id),
        })
        .collect();
    let mut anchor_by_cluster: HashMap<i32, ClusterAnchor> = anchors
        .into_iter()
        .map(|anchor| (anchor.cluster_id, anchor))
        .collect();
    let mut counts_by_root: BTreeMap<i32, u32> = BTreeMap::new();
    for stat in &stats {
        let root_cluster = remap[&stat.cluster_id];
        let count = anchor_by_cluster
            .get(&stat.cluster_id)
            .map(|anchor| anchor.member_count)
            .unwrap_or(stat.member_count);
        *counts_by_root.entry(root_cluster).or_default() += count;
    }
    let mut new_anchors = Vec::with_capacity(counts_by_root.len());
    for (root_cluster, member_count) in counts_by_root {
        let Some(mut anchor) = anchor_by_cluster.remove(&root_cluster) else {
            continue;
        };
        anchor.member_count = member_count;
        new_anchors.push(anchor);
    }
    for (cluster_id, anchor) in anchor_by_cluster {
        if !index_by_cluster.contains_key(&cluster_id) {
            new_anchors.push(anchor);
        }
    }
    new_anchors.sort_by_key(|anchor| anchor.cluster_id);
    (new_assignments, new_anchors)
}

/// Minimum CORROBORATED cluster size kept regardless of per-face quality:
/// ≥ this many mutually-similar faces is a real recurring identity even when
/// every frame is mediocre. `FILEID_FACE_MIN_CLUSTER_SIZE` (clamped [1,10000];
/// default 3).
///
/// Stays 3 (NOT lowered to 2): a 2026-07-05 scale test on ~44k faces from
/// F:\TrueNAS showed min=2 keeps ~3,800 size-2 clusters (HNSW at scale produces
/// a flood of pairs), so min=2 traded the singleton flood for a pair flood.
/// min=3 + `solo_quality_floor` 0.40 cut that scale run to ~1,566 persons with
/// no pair flood. Recurrence is still the primary "is this a person" signal;
/// the quality floor only rescues exceptional single faces.
pub fn min_cluster_size() -> u32 {
    std::env::var("FILEID_FACE_MIN_CLUSTER_SIZE")
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .map(|v| v.clamp(1, 10_000))
        .unwrap_or(3)
}

/// A cluster below `min_cluster_size` must contain a face at/above this quality
/// to be persisted as a person; otherwise it is left UNCLUSTERED (no spurious
/// singleton). `FILEID_FACE_SOLO_QUALITY` (clamped [0,1]; default 0.40). 0
/// disables the quality escape entirely (pure recurrence gate).
///
/// Default 0.12→0.40 (2026-07-05, RTX 5080 / F:\TrueNAS calibration). The old
/// 0.12 was a macOS Apple-Vision guess and the comment ASSUMED Windows scored
/// on the same 0..~0.95 range. It does not: `face_quality` here is the YuNet
/// detection score × landmark geometry (`scrfd::validate_face_geometry`, whose
/// `geom_conf` structurally caps ~0.42), so measured on 84,629 real faces it is
/// compressed into ~0.23..0.42 and 0.12 admitted EVERY single face — 91% of
/// "persons" were one-off singletons
/// (10,208 persons on the full corpus / 438 on the 1k-file subset). Worse,
/// singleton quality (median 0.33) barely separates from genuine recurring-face
/// quality (median 0.37), and 55% of singletons sit &lt;0.40 cosine from ANY real
/// cluster centroid — i.e. they are genuine distinct one-off faces (crowds,
/// backgrounds), NOT fragments a looser merge would recover. So quality alone
/// can't gate them; recurrence (`min_cluster_size`) does the work and 0.40 (the
/// measured ~90th percentile of the quality range) is a narrow escape that keeps
/// only the crispest true solos. Subset (min=3): 438 → ~34 persons; ~44k-face
/// scale run (min=3): singleton flood gone. macOS keeps its own Apple-Vision-
/// calibrated floor (different quality scale — intentional lockstep divergence).
///
/// SCOPE NOTE: this floor is a POST-clustering suppression of small low-quality
/// clusters. It works alongside two other levers added in the 2026-07-05
/// label-driven retune: the PRE-clustering quality gate
/// FILEID_FACE_CLUSTER_MIN_QUALITY (drops noise faces before they can chain into
/// cones) and the Pass-1 threshold + mutual-kNN in identity_clustering.rs (which
/// DID fix the bridge-face over-merge — see there). On the owner's labelled set
/// that combination reached precision/recall 1.0; cross-corpus + full-84k recall
/// confirmation is tracked in NEXT.md.
pub fn solo_quality_floor() -> f32 {
    std::env::var("FILEID_FACE_SOLO_QUALITY")
        .ok()
        .and_then(|s| s.trim().parse::<f32>().ok())
        .map(|v| v.clamp(0.0, 1.0))
        .unwrap_or(0.40)
}

/// Drop "junk" micro-clusters so the People tab isn't flooded with spurious
/// singleton/doubleton persons built from unrecognizable faces (blurry / motion
/// / tiny / heavy-profile). A cluster is KEPT iff it has ≥ `min_cluster_size`
/// members OR its best member quality ≥ `solo_quality_floor`; otherwise its
/// faces are removed from the assignment (the caller's persist NULLs every
/// unassigned face, so they land unclustered — still a candidate, never deleted).
///
/// This is the SAFE, high-impact fragmentation fix: it NEVER merges two clusters,
/// so it cannot bridge distinct identities — it only suppresses low-confidence
/// lone faces. On the macOS reference library it takes 407→~285 persons by
/// dropping 127 junk micro-clusters with zero identity merges. `solo_quality_floor
/// <= 0` is a no-op (every cluster kept). Mirrors `FaceClustering.swift`'s
/// `suppressLowQualityClusters`.
#[cfg(test)]
pub fn suppress_low_quality_micro_clusters(
    faces: &[FaceRow],
    assignments: Vec<ClusterAssignment>,
    anchors: Vec<ClusterAnchor>,
    min_cluster_size: u32,
    solo_quality_floor: f32,
) -> (Vec<ClusterAssignment>, Vec<ClusterAnchor>) {
    suppress_low_quality_micro_clusters_with_keep(
        faces,
        assignments,
        anchors,
        min_cluster_size,
        solo_quality_floor,
        &HashSet::new(),
    )
}

pub fn suppress_low_quality_micro_clusters_with_keep<S>(
    faces: &[FaceRow],
    assignments: Vec<ClusterAssignment>,
    anchors: Vec<ClusterAnchor>,
    min_cluster_size: u32,
    solo_quality_floor: f32,
    always_keep: &HashSet<i32, S>,
) -> (Vec<ClusterAssignment>, Vec<ClusterAnchor>)
where
    S: std::hash::BuildHasher,
{
    if solo_quality_floor <= 0.0 {
        return (assignments, anchors);
    }
    let face_of: HashMap<i64, &FaceRow> = faces.iter().map(|face| (face.face_id, face)).collect();
    // Per-cluster distinct captures + best (max) member quality, taken from the FINAL
    // assignments (post-consolidation cluster ids) — the source of truth for
    // what would be persisted.
    let mut captures: HashMap<i32, HashSet<CaptureKey>> = HashMap::new();
    let mut max_q: HashMap<i32, f32> = HashMap::new();
    for a in &assignments {
        if let Some(face) = face_of.get(&a.face_id) {
            captures
                .entry(a.cluster_id)
                .or_default()
                .insert(capture_key(face));
        }
        let q = face_of
            .get(&a.face_id)
            .map(|face| face.quality)
            .unwrap_or(f32::NEG_INFINITY);
        let e = max_q.entry(a.cluster_id).or_insert(f32::NEG_INFINITY);
        if q > *e {
            *e = q;
        }
    }
    let keep = |cid: i32| -> bool {
        always_keep.contains(&cid)
            || captures
                .get(&cid)
                .is_some_and(|keys| keys.len() >= min_cluster_size as usize)
            || max_q.get(&cid).copied().unwrap_or(f32::NEG_INFINITY) >= solo_quality_floor
    };
    let new_assignments: Vec<ClusterAssignment> =
        assignments.into_iter().filter(|a| keep(a.cluster_id)).collect();
    let new_anchors: Vec<ClusterAnchor> =
        anchors.into_iter().filter(|an| keep(an.cluster_id)).collect();
    (new_assignments, new_anchors)
}

/// Derive the name-based auto-merge blocked pairs from a face→name snapshot.
///
/// `face_name` maps each named face's id to the user-assigned name of the person
/// it currently belongs to; `cluster_of` maps face ids to their freshly-computed
/// cluster ids. Each cluster gets a single name by majority vote (ties broken to
/// the lexicographically smallest name, never HashMap order, for determinism),
/// then every pair of clusters carrying DIFFERENT names is blocked.
///
/// The caller MUST pass the snapshot read under the phase-3 persist lock (not a
/// pre-clustering phase-1 capture): a rename committed in the lock-free window
/// would otherwise leave a stale name in the guard and could unblock a
/// wrong-cluster auto-merge. (audit C1-023) Same-named fragments and
/// named+unnamed pairs are intentionally NOT blocked — they still consolidate.
#[cfg(test)]
pub fn name_blocked_pairs<S: std::hash::BuildHasher>(
    face_name: &HashMap<i64, String, S>,
    cluster_of: &HashMap<i64, i32, S>,
) -> std::collections::HashSet<(i32, i32)> {
    let mut cname_votes: HashMap<i32, HashMap<String, u32>> = HashMap::new();
    for (fid, name) in face_name {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(&cid) = cluster_of.get(fid) {
            *cname_votes
                .entry(cid)
                .or_default()
                .entry(trimmed.to_owned())
                .or_insert(0) += 1;
        }
    }
    let cluster_name: Vec<(i32, String)> = cname_votes
        .into_iter()
        .filter_map(|(cid, votes)| {
            votes
                .into_iter()
                .max_by(|(na, ca), (nb, cb)| ca.cmp(cb).then_with(|| nb.cmp(na)))
                .map(|(n, _)| (cid, n))
        })
        .collect();
    let mut blocked = std::collections::HashSet::new();
    for i in 0..cluster_name.len() {
        for j in (i + 1)..cluster_name.len() {
            if cluster_name[i].1 != cluster_name[j].1 {
                let (a, b) = (cluster_name[i].0, cluster_name[j].0);
                blocked.insert(if a < b { (a, b) } else { (b, a) });
            }
        }
    }
    blocked
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    let mut acc = 0.0f32;
    for i in 0..a.len() {
        acc += a[i] * b[i];
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(coords: &[f32]) -> Vec<f32> {
        let n: f32 = coords.iter().map(|x| x * x).sum::<f32>().sqrt();
        coords.iter().map(|&x| x / n).collect()
    }

    fn row(id: i64, file_id: i64, e: Vec<f32>, q: f32) -> FaceRow {
        row_with_content(id, file_id, Some(file_id as u64), e, q)
    }

    fn row_with_content(
        id: i64,
        file_id: i64,
        content: Option<u64>,
        e: Vec<f32>,
        q: f32,
    ) -> FaceRow {
        FaceRow {
            face_id: id,
            file_id,
            content_hash: content.map(|value| value.to_le_bytes().to_vec()),
            embedding: e,
            quality: q,
        }
    }

    #[test]
    fn empty_input_yields_empty_output() {
        let (a, c) = cluster(&[]);
        assert!(a.is_empty() && c.is_empty());
    }

    #[test]
    fn identical_vectors_cluster_together() {
        let v = unit(&[1.0, 0.0, 0.0]);
        let faces = vec![
            row(1, 1, v.clone(), 0.9),
            row(2, 2, v.clone(), 0.8),
            row(3, 3, v.clone(), 0.7),
        ];
        let (assignments, anchors) = cluster(&faces);
        let cid = assignments[0].cluster_id;
        assert!(assignments.iter().all(|a| a.cluster_id == cid));
        assert_eq!(anchors.len(), 1);
        assert_eq!(anchors[0].member_count, 3);
        assert_eq!(anchors[0].anchor_face_id, 1); // highest quality
    }

    #[test]
    fn orthogonal_vectors_separate() {
        let faces = vec![
            row(1, 1, unit(&[1.0, 0.0, 0.0]), 0.9),
            row(2, 2, unit(&[0.0, 1.0, 0.0]), 0.9),
            row(3, 3, unit(&[0.0, 0.0, 1.0]), 0.9),
        ];
        let (assignments, anchors) = cluster(&faces);
        let mut ids: Vec<i32> = assignments.iter().map(|a| a.cluster_id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 3);
        assert_eq!(anchors.len(), 3);
    }

    #[test]
    fn cluster_ids_are_one_based_and_stable() {
        let v = unit(&[1.0, 0.0, 0.0]);
        let faces = vec![row(1, 1, v.clone(), 0.5), row(2, 2, v, 0.9)];
        let (assignments, _) = cluster(&faces);
        assert!(assignments.iter().all(|a| a.cluster_id == 1));
    }

    // Helper for property tests: deterministic LCG to spread vectors over
    // the unit sphere so proptest can shrink to reproducible counterexamples.
    fn random_faces(seed: u64, count: usize) -> Vec<FaceRow> {
        let mut state = seed | 1;
        (0..count)
            .map(|i| {
                state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                let a = (state >> 32) as i32 as f32 / 2_147_483_647.0;
                state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                let b = (state >> 32) as i32 as f32 / 2_147_483_647.0;
                state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                let c = (state >> 32) as i32 as f32 / 2_147_483_647.0;
                let v = unit(&[a, b, c]);
                row(i as i64 + 1, i as i64 + 1, v, 0.9)
            })
            .collect()
    }

    // Property tests proving bookkeeping invariants on randomized embeddings.
    // The two-pass density algorithm doesn't guarantee every cluster member
    // is high-cosine-close to its anchor (clusters can chain transitively).
    proptest::proptest! {
        // Invariant: every face_id appears in exactly one cluster
        // assignment, and the assignment count equals the input count.
        #[test]
        fn each_face_assigned_exactly_once(
            count in 2usize..15,
            seed in proptest::num::u64::ANY,
        ) {
            let faces = random_faces(seed, count);
            let (assignments, _) = cluster(&faces);
            proptest::prop_assert_eq!(assignments.len(), faces.len());
            let mut ids: Vec<i64> = assignments.iter().map(|a| a.face_id).collect();
            ids.sort_unstable();
            ids.dedup();
            proptest::prop_assert_eq!(ids.len(), faces.len());
        }

        // Invariant: clustering is deterministic — the same input set
        // produces identical output across runs. People tab can't have
        // clusters that shuffle on every scan.
        #[test]
        fn clustering_is_deterministic(
            count in 2usize..15,
            seed in proptest::num::u64::ANY,
        ) {
            let faces = random_faces(seed, count);
            let (a1, anchors1) = cluster(&faces);
            let (a2, anchors2) = cluster(&faces);
            proptest::prop_assert_eq!(a1.len(), a2.len());
            for (x, y) in a1.iter().zip(a2.iter()) {
                proptest::prop_assert_eq!(x.face_id, y.face_id);
                proptest::prop_assert_eq!(x.cluster_id, y.cluster_id);
            }
            proptest::prop_assert_eq!(anchors1.len(), anchors2.len());
        }

        // Invariant: anchor member_count totals equal the input face count.
        // (Every face goes into exactly one cluster's member count.)
        #[test]
        fn anchor_member_counts_sum_to_input(
            count in 2usize..15,
            seed in proptest::num::u64::ANY,
        ) {
            let faces = random_faces(seed, count);
            let (_, anchors) = cluster(&faces);
            let total: u32 = anchors.iter().map(|a| a.member_count).sum();
            proptest::prop_assert_eq!(total as usize, faces.len());
        }

        // Invariant: anchor cluster_ids are unique within the result.
        #[test]
        fn anchor_cluster_ids_are_unique(
            count in 2usize..15,
            seed in proptest::num::u64::ANY,
        ) {
            let faces = random_faces(seed, count);
            let (_, anchors) = cluster(&faces);
            let mut ids: Vec<i32> = anchors.iter().map(|a| a.cluster_id).collect();
            ids.sort_unstable();
            ids.dedup();
            proptest::prop_assert_eq!(ids.len(), anchors.len());
        }
    }

    #[test]
    fn clustering_partition_is_stable_across_input_order() {
        fn normalized_partition(assignments: &[ClusterAssignment]) -> Vec<Vec<i64>> {
            let mut groups: BTreeMap<i32, Vec<i64>> = BTreeMap::new();
            for assignment in assignments {
                groups
                    .entry(assignment.cluster_id)
                    .or_default()
                    .push(assignment.face_id);
            }
            let mut groups: Vec<Vec<i64>> = groups
                .into_values()
                .map(|mut members| {
                    members.sort_unstable();
                    members
                })
                .collect();
            groups.sort();
            groups
        }

        let mut faces = Vec::new();
        for family in 0..4usize {
            for member in 0..24usize {
                let mut embedding = vec![0.0; 4];
                embedding[family] = 1.0;
                embedding[(family + 1) % 4] = (member as f32 - 11.5) * 0.000_1;
                let face_id = (family * 24 + member + 1) as i64;
                faces.push(row(face_id, face_id, unit(&embedding), 0.9));
            }
        }

        let (ordered, _) = cluster(&faces);
        let mut shuffled = faces.clone();
        shuffled.sort_by_key(|face| (face.face_id * 37).rem_euclid(97));
        let (permuted, _) = cluster(&shuffled);

        assert_eq!(
            normalized_partition(&ordered),
            normalized_partition(&permuted)
        );
    }

    fn anchor(cid: i32, face: i64, _e: Vec<f32>, count: u32) -> ClusterAnchor {
        ClusterAnchor {
            cluster_id: cid,
            anchor_face_id: face,
            member_count: count,
        }
    }

    #[test]
    fn name_guard_is_derived_from_the_passed_snapshot() {
        // Two clusters of near-identical faces (would auto-merge by centroid).
        // cluster_of: faces 1,2 → cluster 1; faces 3,4 → cluster 2.
        let mut cluster_of: HashMap<i64, i32> = HashMap::new();
        cluster_of.insert(1, 1);
        cluster_of.insert(2, 1);
        cluster_of.insert(3, 2);
        cluster_of.insert(4, 2);

        // STALE (phase-1) snapshot: both clusters are still named "Alice" — the
        // user hadn't split them yet. The name guard finds no DIFFERING names, so
        // nothing is blocked and the two clusters would auto-merge.
        let mut stale: HashMap<i64, String> = HashMap::new();
        stale.insert(1, "Alice".into());
        stale.insert(2, "Alice".into());
        stale.insert(3, "Alice".into());
        stale.insert(4, "Alice".into());
        let blocked_stale = name_blocked_pairs(&stale, &cluster_of);
        assert!(
            blocked_stale.is_empty(),
            "same-named clusters are not blocked (they consolidate) — \
             so a stale snapshot would let the merge through"
        );

        // FRESH (under phase-3 lock) snapshot: a rename committed in the lock-free
        // window renamed cluster 2's people to "Bob". Re-derived from THIS
        // snapshot, the guard now blocks the (1,2) pair — the merge can't sneak
        // through off the stale name. This is exactly the regression: the guard
        // must reflect whichever snapshot it's handed, and production now hands it
        // the under-lock one.
        let mut fresh: HashMap<i64, String> = HashMap::new();
        fresh.insert(1, "Alice".into());
        fresh.insert(2, "Alice".into());
        fresh.insert(3, "Bob".into());
        fresh.insert(4, "Bob".into());
        let blocked_fresh = name_blocked_pairs(&fresh, &cluster_of);
        assert!(
            blocked_fresh.contains(&(1, 2)),
            "differently-named clusters must be blocked when derived from the fresh snapshot"
        );

        // And the blocked pair, fed to consolidate, actually keeps the two apart.
        let v = unit(&[1.0, 0.0, 0.0]);
        let faces = vec![
            row(1, 1, v.clone(), 0.9),
            row(2, 2, v.clone(), 0.9),
            row(3, 3, v.clone(), 0.9),
            row(4, 4, v.clone(), 0.9),
        ];
        let assignments = vec![
            ClusterAssignment { face_id: 1, cluster_id: 1 },
            ClusterAssignment { face_id: 2, cluster_id: 1 },
            ClusterAssignment { face_id: 3, cluster_id: 2 },
            ClusterAssignment { face_id: 4, cluster_id: 2 },
        ];
        let anchors = vec![anchor(1, 1, v.clone(), 2), anchor(2, 3, v.clone(), 2)];
        let (a, an) = consolidate(&faces, assignments, anchors, &blocked_fresh, 0.75);
        assert_eq!(an.len(), 2, "the name guard kept the renamed cluster apart");
        let cid = |f: i64| a.iter().find(|x| x.face_id == f).unwrap().cluster_id;
        assert_ne!(cid(1), cid(3), "Alice and Bob clusters never co-located");
    }

    #[test]
    fn name_guard_ignores_blank_names_and_majority_votes() {
        // Cluster 1 is majority "Alice" (one stray blank); cluster 2 is "Alice"
        // too → same name, not blocked. Cluster 3 is "Carol" → blocked vs both.
        let mut cluster_of: HashMap<i64, i32> = HashMap::new();
        for (f, c) in [(1, 1), (2, 1), (3, 1), (10, 2), (20, 3)] {
            cluster_of.insert(f, c);
        }
        let mut names: HashMap<i64, String> = HashMap::new();
        names.insert(1, "Alice".into());
        names.insert(2, "Alice".into());
        names.insert(3, "  ".into()); // blank — ignored, doesn't win the vote
        names.insert(10, "Alice".into());
        names.insert(20, "Carol".into());
        let blocked = name_blocked_pairs(&names, &cluster_of);
        assert!(!blocked.contains(&(1, 2)), "same majority name → not blocked");
        assert!(blocked.contains(&(1, 3)), "Alice vs Carol blocked");
        assert!(blocked.contains(&(2, 3)), "Alice vs Carol blocked");
    }

    #[test]
    fn preserved_faces_are_removed_from_the_rebuild_plan() {
        let faces = vec![
            row(1, 1, vec![1.0, 0.0, 0.0], 0.1),
            row(2, 2, vec![1.0, 0.0, 0.0], 0.1),
        ];
        let assignments = vec![
            ClusterAssignment { face_id: 1, cluster_id: 1 },
            ClusterAssignment { face_id: 2, cluster_id: 1 },
        ];
        let (assignments, anchors) = partition_protected_clusters_excluding(
            &faces,
            assignments,
            &HashMap::new(),
            &HashSet::new(),
            &HashSet::from([1]),
        );
        assert_eq!(assignments.len(), 1);
        assert_eq!(assignments[0].face_id, 2);
        assert_eq!(anchors.len(), 1);
        assert_eq!(anchors[0].anchor_face_id, 2);
    }

    #[test]
    fn protected_named_bridge_is_split_and_stays_transitively_safe() {
        let v = unit(&[1.0, 0.0, 0.0]);
        let faces = vec![
            row(1, 1, v.clone(), 0.9),
            row(2, 2, v.clone(), 0.8),
            row(3, 3, v.clone(), 0.7),
        ];
        let assignments = vec![
            ClusterAssignment { face_id: 1, cluster_id: 1 },
            ClusterAssignment { face_id: 2, cluster_id: 1 },
            ClusterAssignment { face_id: 3, cluster_id: 1 },
        ];
        let owner_by_face: HashMap<i64, i64> = [(1, 10), (3, 20)].into_iter().collect();
        let different = HashSet::new();
        let (assignments, anchors) =
            partition_protected_clusters(&faces, assignments, &owner_by_face, &different);
        validate_protected_clusters(&assignments, &owner_by_face, &different).unwrap();

        let cluster_of: HashMap<i64, i32> = assignments
            .iter()
            .map(|assignment| (assignment.face_id, assignment.cluster_id))
            .collect();
        let protected_owners = protected_owner_by_cluster(&owner_by_face, &cluster_of).unwrap();
        let (assignments, anchors) = consolidate_with_protected_owners(
            &faces,
            assignments,
            anchors,
            &HashSet::new(),
            &protected_owners,
            0.85,
        );
        validate_protected_clusters(&assignments, &owner_by_face, &different).unwrap();
        assert_eq!(anchors.len(), 2, "the bridge may join one identity, never both");
        let cid = |face_id| {
            assignments
                .iter()
                .find(|assignment| assignment.face_id == face_id)
                .unwrap()
                .cluster_id
        };
        assert_ne!(cid(1), cid(3));
    }

    #[test]
    fn protected_explicit_bridge_is_split_before_consolidation() {
        let v = unit(&[1.0, 0.0, 0.0]);
        let faces = vec![
            row(1, 1, v.clone(), 0.9),
            row(2, 2, v.clone(), 0.8),
            row(3, 3, v.clone(), 0.7),
        ];
        let assignments = vec![
            ClusterAssignment { face_id: 1, cluster_id: 4 },
            ClusterAssignment { face_id: 2, cluster_id: 4 },
            ClusterAssignment { face_id: 3, cluster_id: 4 },
        ];
        let owner_by_face = HashMap::new();
        let different: HashSet<(i64, i64)> = [(1, 3)].into_iter().collect();
        let (assignments, anchors) =
            partition_protected_clusters(&faces, assignments, &owner_by_face, &different);
        let cluster_of: HashMap<i64, i32> = assignments
            .iter()
            .map(|assignment| (assignment.face_id, assignment.cluster_id))
            .collect();
        let mut blocked = HashSet::new();
        let (a, b) = (cluster_of[&1], cluster_of[&3]);
        blocked.insert(if a < b { (a, b) } else { (b, a) });
        let (assignments, _) = consolidate(&faces, assignments, anchors, &blocked, 0.85);
        validate_protected_clusters(&assignments, &owner_by_face, &different).unwrap();
    }

    #[test]
    fn protected_missing_face_survives_consolidation_and_suppression() {
        let v = unit(&[1.0, 0.0, 0.0]);
        let faces = vec![row(1, 1, v.clone(), 0.05), row(2, 2, v.clone(), 0.05)];
        let assignments = vec![
            ClusterAssignment { face_id: 1, cluster_id: 1 },
            ClusterAssignment { face_id: 2, cluster_id: 2 },
        ];
        let owner_by_face: HashMap<i64, i64> = [(9, 90)].into_iter().collect();
        let different = HashSet::new();
        let (assignments, anchors) =
            partition_protected_clusters(&faces, assignments, &owner_by_face, &different);
        let (assignments, anchors) =
            consolidate(&faces, assignments, anchors, &HashSet::new(), 0.85);
        let protected_faces: HashSet<i64> = owner_by_face.keys().copied().collect();
        let keep = protected_cluster_ids(&assignments, &protected_faces);
        let (assignments, anchors) = suppress_low_quality_micro_clusters_with_keep(
            &faces,
            assignments,
            anchors,
            3,
            0.40,
            &keep,
        );
        validate_protected_clusters(&assignments, &owner_by_face, &different).unwrap();
        assert!(assignments.iter().any(|assignment| assignment.face_id == 9));
        assert!(anchors.iter().any(|anchor| anchor.anchor_face_id == 9));
    }

    #[test]
    fn protected_validator_rejects_unsafe_persistence_partition() {
        let assignments = vec![
            ClusterAssignment { face_id: 1, cluster_id: 7 },
            ClusterAssignment { face_id: 2, cluster_id: 7 },
        ];
        let owners: HashMap<i64, i64> = [(1, 10), (2, 20)].into_iter().collect();
        let different: HashSet<(i64, i64)> = [(1, 2)].into_iter().collect();
        assert!(validate_protected_clusters(&assignments, &owners, &different).is_err());
    }

    #[test]
    fn preserved_verdict_endpoint_is_prefiltered_before_validate() {
        // Regression (blocker): a different-people verdict whose endpoint face is
        // owned by a PRESERVED identity is dropped from the clustering pool, so
        // that face is absent from the final partition. The command must
        // pre-filter any verdict pair touching an excluded face before building
        // the blocked set or validating; without that filter the absent endpoint
        // fails the whole clustering run every time, permanently breaking the
        // People tab (clustering re-fires after every scan).
        let assignments = vec![
            ClusterAssignment { face_id: 10, cluster_id: 1 },
            ClusterAssignment { face_id: 20, cluster_id: 2 },
        ];
        let owner_by_face: HashMap<i64, i64> = [(10, 100), (20, 200)].into_iter().collect();
        // Face 99 belongs to a preserved owner and left the pool; the verdict
        // 10≠99 still references it.
        let raw_pairs: HashSet<(i64, i64)> = [(10, 99)].into_iter().collect();
        // Unfiltered: the absent endpoint 99 makes validation fail closed.
        assert!(validate_protected_clusters(&assignments, &owner_by_face, &raw_pairs).is_err());
        // The command drops pairs touching excluded faces; the preserved owner is
        // kept separate wholesale, so the constraint is already satisfied.
        let excluded: HashSet<i64> = [99].into_iter().collect();
        let active: HashSet<(i64, i64)> = raw_pairs
            .into_iter()
            .filter(|&(a, b)| !excluded.contains(&a) && !excluded.contains(&b))
            .collect();
        assert!(active.is_empty());
        validate_protected_clusters(&assignments, &owner_by_face, &active).unwrap();
    }

    #[test]
    fn consolidate_merges_near_identical_clusters() {
        let v = unit(&[1.0, 0.0, 0.0]);
        // Cluster 1 (2 faces) + cluster 2 (3 faces), both centered on the same
        // direction → centroid cosine ≈ 1.0, well above 0.85.
        let faces = vec![
            row(1, 1, v.clone(), 0.9),
            row(2, 2, v.clone(), 0.8),
            row(3, 3, v.clone(), 0.95),
            row(4, 4, v.clone(), 0.7),
            row(5, 5, v.clone(), 0.6),
        ];
        let assignments = vec![
            ClusterAssignment { face_id: 1, cluster_id: 1 },
            ClusterAssignment { face_id: 2, cluster_id: 1 },
            ClusterAssignment { face_id: 3, cluster_id: 2 },
            ClusterAssignment { face_id: 4, cluster_id: 2 },
            ClusterAssignment { face_id: 5, cluster_id: 2 },
        ];
        let anchors = vec![
            anchor(1, 1, v.clone(), 2),
            anchor(2, 3, v.clone(), 3),
        ];
        let (a, an) =
            consolidate(&faces, assignments, anchors, &std::collections::HashSet::new(), 0.85);
        assert_eq!(an.len(), 1, "the two same-person fragments fold into one");
        // Larger fragment (cluster 2, 3 members) wins the canonical id + anchor.
        assert_eq!(an[0].cluster_id, 2);
        assert_eq!(an[0].anchor_face_id, 3);
        assert_eq!(an[0].member_count, 5, "member counts sum");
        assert!(a.iter().all(|x| x.cluster_id == 2), "all faces map to the survivor");
    }

    #[test]
    fn consolidate_respects_blocked_pair() {
        let v = unit(&[1.0, 0.0, 0.0]);
        let faces = vec![
            row(1, 1, v.clone(), 0.9),
            row(2, 2, v.clone(), 0.95),
        ];
        let assignments = vec![
            ClusterAssignment { face_id: 1, cluster_id: 1 },
            ClusterAssignment { face_id: 2, cluster_id: 2 },
        ];
        let anchors = vec![anchor(1, 1, v.clone(), 1), anchor(2, 2, v.clone(), 1)];
        let mut blocked = std::collections::HashSet::new();
        blocked.insert((1, 2));
        let (a, an) = consolidate(&faces, assignments, anchors, &blocked, 0.85);
        assert_eq!(an.len(), 2, "a 'different people' verdict blocks the merge");
        assert_ne!(a[0].cluster_id, a[1].cluster_id);
    }

    #[test]
    fn consolidate_blocked_pair_is_transitively_safe() {
        // Three near-identical clusters; 1–2 is blocked. A merge via 3 must not
        // sneak 1 and 2 into the same person.
        let v = unit(&[1.0, 0.0, 0.0]);
        let faces = vec![
            row(1, 1, v.clone(), 0.9),
            row(2, 2, v.clone(), 0.9),
            row(3, 3, v.clone(), 0.9),
        ];
        let assignments = vec![
            ClusterAssignment { face_id: 1, cluster_id: 1 },
            ClusterAssignment { face_id: 2, cluster_id: 2 },
            ClusterAssignment { face_id: 3, cluster_id: 3 },
        ];
        let anchors = vec![
            anchor(1, 1, v.clone(), 1),
            anchor(2, 2, v.clone(), 1),
            anchor(3, 3, v.clone(), 1),
        ];
        let mut blocked = std::collections::HashSet::new();
        blocked.insert((1, 2));
        let (a, an) = consolidate(&faces, assignments, anchors, &blocked, 0.85);
        assert_eq!(an.len(), 2, "one merge allowed, the blocked pair kept apart");
        let cid = |face: i64| a.iter().find(|x| x.face_id == face).unwrap().cluster_id;
        assert_ne!(cid(1), cid(2), "blocked pair never co-located, even transitively");
    }

    #[test]
    fn forbidden_reverse_edge_follows_a_dropped_root() {
        let v1 = unit(&[1.0, 0.0, 0.0]);
        let v2 = unit(&[0.999, 0.01, 0.0]);
        let v3 = unit(&[0.0, 1.0, 0.0]);
        let v4 = unit(&[0.98, 0.2, 0.0]);
        let faces = vec![
            row(1, 1, v1.clone(), 0.9),
            row(2, 2, v2.clone(), 0.9),
            row(3, 3, v3.clone(), 0.9),
            row(4, 4, v4.clone(), 0.9),
        ];
        let assignments = (1..=4)
            .map(|id| ClusterAssignment { face_id: id, cluster_id: id as i32 })
            .collect();
        let anchors = vec![
            anchor(1, 1, v1, 1),
            anchor(2, 2, v2, 1),
            anchor(3, 3, v3, 1),
            anchor(4, 4, v4, 1),
        ];
        let blocked = HashSet::from([(1, 3), (2, 4)]);
        let (assignments, _) = consolidate(&faces, assignments, anchors, &blocked, 0.95);
        let cid = |face: i64| {
            assignments
                .iter()
                .find(|assignment| assignment.face_id == face)
                .unwrap()
                .cluster_id
        };
        assert_eq!(cid(1), cid(2), "the strongest allowed edge merges first");
        assert_ne!(cid(2), cid(4), "the dropped root's verdict follows its survivor");
    }

    #[test]
    fn consolidate_disabled_above_one_is_noop() {
        let v = unit(&[1.0, 0.0, 0.0]);
        let faces = vec![row(1, 1, v.clone(), 0.9), row(2, 2, v.clone(), 0.9)];
        let assignments = vec![
            ClusterAssignment { face_id: 1, cluster_id: 1 },
            ClusterAssignment { face_id: 2, cluster_id: 2 },
        ];
        let anchors = vec![anchor(1, 1, v.clone(), 1), anchor(2, 2, v.clone(), 1)];
        // 1.0 is the documented "disable" value (the clamp ceiling) and must
        // no-op; clone the inputs so we can also re-check at 1.01.
        let (_, an10) = consolidate(
            &faces,
            assignments.clone(),
            anchors.clone(),
            &std::collections::HashSet::new(),
            1.0,
        );
        assert_eq!(an10.len(), 2, "threshold == 1.0 (documented disable) is a no-op");
        let (_, an) =
            consolidate(&faces, assignments, anchors, &std::collections::HashSet::new(), 1.01);
        assert_eq!(an.len(), 2, "threshold > 1.0 disables consolidation");
    }

    #[test]
    fn consolidate_leaves_distinct_clusters_apart() {
        let faces = vec![
            row(1, 1, unit(&[1.0, 0.0, 0.0]), 0.9),
            row(2, 2, unit(&[0.0, 1.0, 0.0]), 0.9),
        ];
        let assignments = vec![
            ClusterAssignment { face_id: 1, cluster_id: 1 },
            ClusterAssignment { face_id: 2, cluster_id: 2 },
        ];
        let anchors = vec![
            anchor(1, 1, unit(&[1.0, 0.0, 0.0]), 1),
            anchor(2, 2, unit(&[0.0, 1.0, 0.0]), 1),
        ];
        let (_, an) =
            consolidate(&faces, assignments, anchors, &std::collections::HashSet::new(), 0.85);
        assert_eq!(an.len(), 2, "orthogonal centroids (cosine 0) never merge");
    }

    #[test]
    fn automerge_threshold_clamps_and_defaults() {
        // Unset → default. (Other env-dependent cases aren't asserted here to
        // avoid process-global env races with parallel tests.)
        std::env::remove_var("FILEID_FACE_AUTOMERGE_COS");
        assert!((automerge_threshold() - AUTOMERGE_COS_DEFAULT).abs() < 1e-6);
    }

    #[test]
    fn suppression_drops_low_quality_micro_clusters() {
        let v = unit(&[1.0, 0.0, 0.0]);
        // c1: junk pair (q .05/.06) -> drop; c2: good singleton (.40) -> keep;
        // c3: junk singleton (.05) -> drop; c4: size-3 all-low -> keep (size wins).
        let faces = vec![
            row(1, 1, v.clone(), 0.05),
            row(2, 2, v.clone(), 0.06),
            row(3, 3, v.clone(), 0.40),
            row(4, 4, v.clone(), 0.05),
            row(5, 5, v.clone(), 0.04),
            row(6, 6, v.clone(), 0.03),
            row(7, 7, v.clone(), 0.02),
        ];
        let assignments = vec![
            ClusterAssignment { face_id: 1, cluster_id: 1 },
            ClusterAssignment { face_id: 2, cluster_id: 1 },
            ClusterAssignment { face_id: 3, cluster_id: 2 },
            ClusterAssignment { face_id: 4, cluster_id: 3 },
            ClusterAssignment { face_id: 5, cluster_id: 4 },
            ClusterAssignment { face_id: 6, cluster_id: 4 },
            ClusterAssignment { face_id: 7, cluster_id: 4 },
        ];
        let anchors = vec![
            anchor(1, 1, v.clone(), 2),
            anchor(2, 3, v.clone(), 1),
            anchor(3, 4, v.clone(), 1),
            anchor(4, 5, v.clone(), 3),
        ];
        let (a, an) = suppress_low_quality_micro_clusters(&faces, assignments, anchors, 3, 0.12);
        let kept: std::collections::HashSet<i32> = an.iter().map(|x| x.cluster_id).collect();
        assert_eq!(kept, [2, 4].into_iter().collect());
        let assigned: std::collections::HashSet<i64> = a.iter().map(|x| x.face_id).collect();
        assert_eq!(
            assigned,
            [3, 5, 6, 7].into_iter().collect(),
            "suppressed faces are unassigned (persist NULLs them)"
        );
    }

    #[test]
    fn suppression_does_not_count_copied_content_as_recurrence() {
        let v = unit(&[1.0, 0.0, 0.0]);
        let faces = vec![
            row_with_content(1, 10, Some(77), v.clone(), 0.05),
            row_with_content(2, 11, Some(77), v.clone(), 0.06),
            row_with_content(3, 12, Some(77), v.clone(), 0.07),
        ];
        let assignments = vec![
            ClusterAssignment {
                face_id: 1,
                cluster_id: 1,
            },
            ClusterAssignment {
                face_id: 2,
                cluster_id: 1,
            },
            ClusterAssignment {
                face_id: 3,
                cluster_id: 1,
            },
        ];
        let anchors = vec![anchor(1, 1, v, 3)];

        let (assignments, anchors) =
            suppress_low_quality_micro_clusters(&faces, assignments, anchors, 3, 0.40);

        assert!(assignments.is_empty());
        assert!(anchors.is_empty());
    }

    #[test]
    fn suppression_keeps_copied_content_when_protected() {
        let v = unit(&[1.0, 0.0, 0.0]);
        let faces = vec![
            row_with_content(1, 10, Some(77), v.clone(), 0.05),
            row_with_content(2, 11, Some(77), v.clone(), 0.06),
            row_with_content(3, 12, Some(77), v.clone(), 0.07),
        ];
        let assignments = vec![
            ClusterAssignment {
                face_id: 1,
                cluster_id: 1,
            },
            ClusterAssignment {
                face_id: 2,
                cluster_id: 1,
            },
            ClusterAssignment {
                face_id: 3,
                cluster_id: 1,
            },
        ];
        let anchors = vec![anchor(1, 1, v, 3)];

        let (assignments, anchors) = suppress_low_quality_micro_clusters_with_keep(
            &faces,
            assignments,
            anchors,
            3,
            0.40,
            &HashSet::from([1]),
        );

        assert_eq!(assignments.len(), 3);
        assert_eq!(anchors.len(), 1);
    }

    #[test]
    fn suppression_keeps_copied_content_via_quality_escape() {
        let v = unit(&[1.0, 0.0, 0.0]);
        let faces = vec![
            row_with_content(1, 10, Some(77), v.clone(), 0.05),
            row_with_content(2, 11, Some(77), v.clone(), 0.40),
            row_with_content(3, 12, Some(77), v.clone(), 0.07),
        ];
        let assignments = vec![
            ClusterAssignment {
                face_id: 1,
                cluster_id: 1,
            },
            ClusterAssignment {
                face_id: 2,
                cluster_id: 1,
            },
            ClusterAssignment {
                face_id: 3,
                cluster_id: 1,
            },
        ];
        let anchors = vec![anchor(1, 2, v, 3)];

        let (assignments, anchors) =
            suppress_low_quality_micro_clusters(&faces, assignments, anchors, 3, 0.40);

        assert_eq!(assignments.len(), 3);
        assert_eq!(anchors.len(), 1);
    }

    #[test]
    fn suppression_uses_file_ids_when_content_hash_is_missing() {
        let v = unit(&[1.0, 0.0, 0.0]);
        let faces = vec![
            row_with_content(1, 10, None, v.clone(), 0.05),
            row_with_content(2, 11, None, v.clone(), 0.06),
            row_with_content(3, 12, None, v.clone(), 0.07),
        ];
        let assignments = vec![
            ClusterAssignment {
                face_id: 1,
                cluster_id: 1,
            },
            ClusterAssignment {
                face_id: 2,
                cluster_id: 1,
            },
            ClusterAssignment {
                face_id: 3,
                cluster_id: 1,
            },
        ];
        let anchors = vec![anchor(1, 1, v, 3)];

        let (assignments, anchors) =
            suppress_low_quality_micro_clusters(&faces, assignments, anchors, 3, 0.40);

        assert_eq!(assignments.len(), 3);
        assert_eq!(anchors.len(), 1);
    }

    #[test]
    fn suppression_counts_one_missing_hash_file_once() {
        let v = unit(&[1.0, 0.0, 0.0]);
        let faces = vec![
            row_with_content(1, 10, None, v.clone(), 0.05),
            row_with_content(2, 10, None, v.clone(), 0.06),
            row_with_content(3, 10, None, v.clone(), 0.07),
        ];
        let assignments = vec![
            ClusterAssignment {
                face_id: 1,
                cluster_id: 1,
            },
            ClusterAssignment {
                face_id: 2,
                cluster_id: 1,
            },
            ClusterAssignment {
                face_id: 3,
                cluster_id: 1,
            },
        ];
        let anchors = vec![anchor(1, 1, v, 3)];

        let (assignments, anchors) =
            suppress_low_quality_micro_clusters(&faces, assignments, anchors, 3, 0.40);

        assert!(assignments.is_empty());
        assert!(anchors.is_empty());
    }

    #[test]
    fn suppression_keeps_pair_with_one_good_face() {
        let v = unit(&[1.0, 0.0, 0.0]);
        let faces = vec![row(1, 1, v.clone(), 0.05), row(2, 2, v.clone(), 0.40)];
        let assignments = vec![
            ClusterAssignment { face_id: 1, cluster_id: 9 },
            ClusterAssignment { face_id: 2, cluster_id: 9 },
        ];
        let anchors = vec![anchor(9, 2, v.clone(), 2)];
        let (a, an) = suppress_low_quality_micro_clusters(&faces, assignments, anchors, 3, 0.12);
        assert_eq!(an.len(), 1, "the best face (0.40) rescues the pair");
        assert_eq!(a.len(), 2);
    }

    #[test]
    fn suppression_floor_zero_is_noop() {
        let v = unit(&[1.0, 0.0, 0.0]);
        let faces = vec![row(1, 1, v.clone(), 0.01)];
        let assignments = vec![ClusterAssignment { face_id: 1, cluster_id: 1 }];
        let anchors = vec![anchor(1, 1, v.clone(), 1)];
        let (a, an) = suppress_low_quality_micro_clusters(&faces, assignments, anchors, 3, 0.0);
        assert_eq!(an.len(), 1, "floor 0 disables suppression");
        assert_eq!(a.len(), 1);
    }

    #[test]
    fn suppression_env_defaults() {
        std::env::remove_var("FILEID_FACE_MIN_CLUSTER_SIZE");
        std::env::remove_var("FILEID_FACE_SOLO_QUALITY");
        assert_eq!(min_cluster_size(), 3);
        assert!((solo_quality_floor() - 0.40).abs() < 1e-6);
    }

    // Collapse a set of merge edges into a canonical per-centroid grouping via
    // the same greedy union-find consolidate() uses downstream, then return a
    // normalized labeling (each centroid → its group's min index) so two edge
    // sets can be compared for IDENTICAL final clustering regardless of edge
    // order or which representative each path happened to pick.
    fn union_find_labels(n: usize, edges: &[(f32, usize, usize)]) -> Vec<usize> {
        let mut sorted = edges.to_vec();
        sorted.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut parent: Vec<usize> = (0..n).collect();
        fn find(parent: &mut [usize], mut x: usize) -> usize {
            while parent[x] != x {
                parent[x] = parent[parent[x]];
                x = parent[x];
            }
            x
        }
        for (_s, i, j) in sorted {
            let ri = find(&mut parent, i);
            let rj = find(&mut parent, j);
            if ri != rj {
                parent[ri] = rj;
            }
        }
        // Normalize: label each centroid by the minimum index in its group.
        let mut group_min: HashMap<usize, usize> = HashMap::new();
        for x in 0..n {
            let r = find(&mut parent, x);
            let e = group_min.entry(r).or_insert(x);
            if x < *e {
                *e = x;
            }
        }
        (0..n).map(|x| group_min[&find(&mut parent, x)]).collect()
    }

    #[test]
    fn hnsw_dense_band_above_candidate_cap_matches_brute() {
        let cross_cosine = 0.89f32;
        let cross_sine = (1.0 - cross_cosine * cross_cosine).sqrt();
        let mut centroids = Vec::new();
        for member in 0..513usize {
            let z = (member as f32 - 256.0) * 0.000_000_1;
            centroids.push(unit(&[1.0, 0.0, z]));
        }
        for member in 0..513usize {
            let z = (member as f32 - 256.0) * 0.000_000_1;
            centroids.push(unit(&[cross_cosine, cross_sine, z]));
        }

        let brute_labels = union_find_labels(centroids.len(), &edges_brute(&centroids, 0.88));
        let hnsw_labels = union_find_labels(centroids.len(), &edges_hnsw(&centroids, 0.88));

        assert_eq!(
            brute_labels, hnsw_labels,
            "a saturated >512-neighbor band must not silently split"
        );
        assert_eq!(
            brute_labels.iter().copied().collect::<HashSet<_>>().len(),
            1
        );
    }

    #[test]
    fn hnsw_dense_fallback_preserves_transitive_cannot_link() {
        let cross_cosine = 0.89f32;
        let cross_sine = (1.0 - cross_cosine * cross_cosine).sqrt();
        let mut faces = Vec::new();
        let mut assignments = Vec::new();
        let mut anchors = Vec::new();

        for member in 0..513usize {
            let face_id = member as i64 + 1;
            let z = (member as f32 - 256.0) * 0.000_000_1;
            let embedding = unit(&[1.0, 0.0, z]);
            faces.push(row(face_id, face_id, embedding.clone(), 0.9));
            assignments.push(ClusterAssignment {
                face_id,
                cluster_id: face_id as i32,
            });
            anchors.push(anchor(face_id as i32, face_id, embedding, 1));
        }
        for member in 0..513usize {
            let face_id = member as i64 + 514;
            let z = (member as f32 - 256.0) * 0.000_000_1;
            let embedding = unit(&[cross_cosine, cross_sine, z]);
            faces.push(row(face_id, face_id, embedding.clone(), 0.9));
            assignments.push(ClusterAssignment {
                face_id,
                cluster_id: face_id as i32,
            });
            anchors.push(anchor(face_id as i32, face_id, embedding, 1));
        }
        for member in 0..975usize {
            let face_id = member as i64 + 1_027;
            let embedding = unit(&[0.0, 0.0, 1.0]);
            faces.push(row(face_id, face_id, embedding.clone(), 0.9));
            assignments.push(ClusterAssignment {
                face_id,
                cluster_id: face_id as i32,
            });
            anchors.push(anchor(face_id as i32, face_id, embedding, 1));
        }

        let blocked = HashSet::from([(1, 514)]);
        let (assignments, anchors) =
            consolidate(&faces, assignments, anchors, &blocked, 0.88);

        assert_eq!(anchors.len(), 3);
        assert_eq!(cluster_for(&assignments, 1), cluster_for(&assignments, 513));
        assert_eq!(
            cluster_for(&assignments, 514),
            cluster_for(&assignments, 1_026)
        );
        assert_ne!(cluster_for(&assignments, 1), cluster_for(&assignments, 514));
        assert_ne!(
            cluster_for(&assignments, 1),
            cluster_for(&assignments, 1_027)
        );
    }

    #[test]
    fn hnsw_and_brute_edge_paths_agree() {
        // Deterministic fixture: 60 synthetic unit-normalized centroids built as
        // 12 tight families of 5 near-identical vectors each. Each family's base
        // direction is a distinct ORTHOGONAL axis (one-hot in 12-d), so
        // cross-family cosine is ~0 — provably below threshold, no flaky
        // near-collisions. Within a family, tiny jitter keeps cosine ~0.999, far
        // above threshold. The final union-find clustering is therefore
        // unambiguous and both edge paths must reproduce it.
        let mut state: u64 = 0x9E3779B97F4A7C15;
        let mut next = || {
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            ((state >> 33) as f32 / (1u64 << 31) as f32) - 1.0 // ~[-1, 1)
        };
        const FAMILIES: usize = 12;
        const DIM: usize = FAMILIES;
        const PER_FAMILY: usize = 5;
        let mut centroids: Vec<Vec<f32>> = Vec::with_capacity(FAMILIES * PER_FAMILY);
        for fam in 0..FAMILIES {
            let mut base = [0.0f32; DIM];
            base[fam] = 1.0;
            for _ in 0..PER_FAMILY {
                // Jitter on the non-axis dims keeps within-family cosine ~0.999
                // while the one-hot base direction dominates; cross-family stays ~0.
                let v: Vec<f32> = base.iter().map(|&b| b + 0.01 * next()).collect();
                centroids.push(unit(&v));
            }
        }

        let threshold = 0.90;
        let n = centroids.len();
        let brute = edges_brute(&centroids, threshold);
        let hnsw = edges_hnsw(&centroids, threshold);

        let brute_labels = union_find_labels(n, &brute);
        let hnsw_labels = union_find_labels(n, &hnsw);
        assert_eq!(
            brute_labels, hnsw_labels,
            "HNSW and brute edge paths must yield identical final clustering"
        );

        // Sanity: the fixture really does merge into FAMILIES groups (otherwise
        // the parity assertion above could pass trivially on all-singletons).
        let distinct: std::collections::HashSet<usize> = brute_labels.iter().copied().collect();
        assert_eq!(distinct.len(), FAMILIES, "fixture should fold into {FAMILIES} families");
    }

    fn recovery_fixture(
        include_runner_up: bool,
    ) -> (Vec<FaceRow>, Vec<ClusterAssignment>, Vec<ClusterAnchor>) {
        let mut faces = vec![
            row_with_content(1, 1, Some(1), unit(&[1.0, 0.0, 0.0]), 0.4),
            row_with_content(2, 2, Some(2), unit(&[0.98, 0.20, 0.0]), 0.4),
        ];
        let mut assignments = vec![
            ClusterAssignment {
                face_id: 1,
                cluster_id: 1,
            },
            ClusterAssignment {
                face_id: 2,
                cluster_id: 1,
            },
        ];
        let mut anchors = vec![anchor(1, 1, Vec::new(), 2)];
        for offset in 0..13i64 {
            let face_id = 100 + offset;
            faces.push(row_with_content(
                face_id,
                face_id,
                Some(1_000 + offset as u64),
                unit(&[1.0, (offset % 3) as f32 * 0.02, 0.0]),
                0.4,
            ));
            assignments.push(ClusterAssignment {
                face_id,
                cluster_id: 2,
            });
        }
        anchors.push(anchor(2, 100, Vec::new(), 13));
        if include_runner_up {
            for offset in 0..13i64 {
                let face_id = 200 + offset;
                faces.push(row_with_content(
                    face_id,
                    face_id,
                    Some(2_000 + offset as u64),
                    unit(&[0.80, 0.60, (offset % 3) as f32 * 0.01]),
                    0.4,
                ));
                assignments.push(ClusterAssignment {
                    face_id,
                    cluster_id: 3,
                });
            }
            anchors.push(anchor(3, 200, Vec::new(), 13));
        }
        (faces, assignments, anchors)
    }

    fn cluster_for(assignments: &[ClusterAssignment], face_id: i64) -> i32 {
        assignments
            .iter()
            .find(|assignment| assignment.face_id == face_id)
            .unwrap()
            .cluster_id
    }

    #[test]
    fn recovery_attaches_unnamed_fragment_to_exactly_one_named_owner() {
        let (faces, assignments, anchors) = recovery_fixture(true);
        let owners = HashMap::from([(2, 20), (3, 30)]);
        let (assignments, anchors) = recover_small_fragments_with_params(
            &faces,
            assignments,
            anchors,
            &HashSet::new(),
            &owners,
            &HashSet::new(),
            0.75,
            12,
            0.05,
        );
        assert_eq!(cluster_for(&assignments, 1), cluster_for(&assignments, 100));
        assert_ne!(cluster_for(&assignments, 1), cluster_for(&assignments, 200));
        assert_eq!(anchors.len(), 2);
        assert_eq!(
            anchors
                .iter()
                .find(|anchor| anchor.cluster_id == 2)
                .unwrap()
                .member_count,
            15
        );
    }

    #[test]
    fn recovery_preserves_a_corroborated_low_quality_doubleton_before_suppression() {
        let (mut faces, assignments, anchors) = recovery_fixture(false);
        faces[0].quality = 0.1;
        faces[1].quality = 0.1;

        let (assignments, anchors) = recover_small_fragments_with_params(
            &faces,
            assignments,
            anchors,
            &HashSet::new(),
            &HashMap::new(),
            &HashSet::new(),
            0.75,
            12,
            0.05,
        );
        let (assignments, anchors) =
            suppress_low_quality_micro_clusters(&faces, assignments, anchors, 3, 0.4);

        assert_eq!(cluster_for(&assignments, 1), cluster_for(&assignments, 100));
        assert_eq!(anchors.len(), 1);
        assert_eq!(anchors[0].member_count, 15);
    }

    #[test]
    fn recovery_never_moves_a_named_source_or_unions_named_targets() {
        let (faces, assignments, anchors) = recovery_fixture(true);
        let owners = HashMap::from([(1, 10), (2, 20), (3, 30)]);
        let (assignments, anchors) = recover_small_fragments_with_params(
            &faces,
            assignments,
            anchors,
            &HashSet::new(),
            &owners,
            &HashSet::new(),
            0.75,
            12,
            0.05,
        );
        assert_eq!(anchors.len(), 3);
        assert_ne!(cluster_for(&assignments, 1), cluster_for(&assignments, 100));
        assert_ne!(cluster_for(&assignments, 100), cluster_for(&assignments, 200));
    }

    #[test]
    fn recovery_never_attaches_to_an_unknown_target() {
        let (faces, assignments, anchors) = recovery_fixture(false);
        let (_, anchors) = recover_small_fragments_with_params(
            &faces,
            assignments,
            anchors,
            &HashSet::new(),
            &HashMap::from([(2, 20)]),
            &HashSet::from([2]),
            0.75,
            12,
            0.05,
        );
        assert_eq!(anchors.len(), 2);
    }

    #[test]
    fn recovery_requires_three_distinct_target_contents() {
        let (mut faces, assignments, anchors) = recovery_fixture(false);
        for face in &mut faces[2..] {
            face.content_hash = Some((9_999u64 + (face.face_id as u64 % 2)).to_le_bytes().to_vec());
        }
        let (_, anchors) = recover_small_fragments_with_params(
            &faces,
            assignments,
            anchors,
            &HashSet::new(),
            &HashMap::new(),
            &HashSet::new(),
            0.75,
            12,
            0.05,
        );
        assert_eq!(anchors.len(), 2);
    }

    #[test]
    fn recovery_deduplicates_source_content_and_fails_closed_without_hashes() {
        for remove_hash in [false, true] {
            let (mut faces, assignments, anchors) = recovery_fixture(false);
            if remove_hash {
                faces[1].content_hash = None;
            } else {
                faces[1].content_hash = faces[0].content_hash.clone();
            }
            let (_, anchors) = recover_small_fragments_with_params(
                &faces,
                assignments,
                anchors,
                &HashSet::new(),
                &HashMap::new(),
                &HashSet::new(),
                0.75,
                12,
                0.05,
            );
            assert_eq!(anchors.len(), 2);
        }
    }

    #[test]
    fn recovery_heals_validated_large_fragment_shape() {
        let fragment_cosine = 0.879497f32;
        let fragment_vector = unit(&[
            fragment_cosine,
            (1.0 - fragment_cosine * fragment_cosine).sqrt(),
            0.0,
        ]);
        let target_vector = unit(&[1.0, 0.0, 0.0]);
        let mut faces = Vec::new();
        let mut assignments = Vec::new();
        for offset in 0..154i64 {
            let face_id = offset + 1;
            let embedding = unit(&[
                fragment_cosine,
                (1.0 - fragment_cosine * fragment_cosine).sqrt(),
                (offset as f32 - 76.5) * 0.000_001,
            ]);
            faces.push(row_with_content(
                face_id,
                face_id,
                Some(face_id as u64),
                embedding,
                0.4,
            ));
            assignments.push(ClusterAssignment {
                face_id,
                cluster_id: 1,
            });
        }
        for offset in 0..8_267i64 {
            let face_id = 10_000 + offset;
            let embedding =
                unit(&[1.0, 0.0, (offset as f32 - 4_133.0) * 0.000_000_1]);
            faces.push(row_with_content(
                face_id,
                face_id,
                Some(face_id as u64),
                embedding,
                0.4,
            ));
            assignments.push(ClusterAssignment {
                face_id,
                cluster_id: 2,
            });
        }
        let anchors = vec![
            anchor(1, 1, fragment_vector, 154),
            anchor(2, 10_000, target_vector, 8_267),
        ];

        let (assignments, anchors) = recover_small_fragments_with_params(
            &faces,
            assignments,
            anchors,
            &HashSet::new(),
            &HashMap::from([(2, 20)]),
            &HashSet::new(),
            0.75,
            12,
            0.05,
        );

        assert_eq!(
            cluster_for(&assignments, 1),
            cluster_for(&assignments, 10_000)
        );
        assert_eq!(anchors.len(), 1);
        assert_eq!(anchors[0].member_count, 8_421);
    }

    #[test]
    fn recovery_revalidates_large_fragment_threshold_after_target_changes() {
        let source_cosine = 0.86f32;
        let source_sine = (1.0 - source_cosine * source_cosine).sqrt();
        let source_a = unit(&[source_cosine, -source_sine, 0.0]);
        let source_b = unit(&[source_cosine, source_sine, 0.0]);
        let target = unit(&[1.0, 0.0, 0.0]);
        let mut faces = Vec::new();
        let mut assignments = Vec::new();

        for offset in 0..16i64 {
            let face_id = offset + 1;
            let embedding = unit(&[
                source_cosine,
                -source_sine,
                (offset as f32 - 7.5) * 0.000_001,
            ]);
            faces.push(row_with_content(
                face_id,
                face_id,
                Some(face_id as u64),
                embedding,
                0.4,
            ));
            assignments.push(ClusterAssignment {
                face_id,
                cluster_id: 1,
            });
        }
        for offset in 0..17i64 {
            let face_id = 100 + offset;
            let embedding = unit(&[
                source_cosine,
                source_sine,
                (offset as f32 - 8.0) * 0.000_001,
            ]);
            faces.push(row_with_content(
                face_id,
                face_id,
                Some(face_id as u64),
                embedding,
                0.4,
            ));
            assignments.push(ClusterAssignment {
                face_id,
                cluster_id: 2,
            });
        }
        for offset in 0..272i64 {
            let face_id = 1_000 + offset;
            let embedding = unit(&[1.0, 0.0, (offset as f32 - 135.5) * 0.000_000_1]);
            faces.push(row_with_content(
                face_id,
                face_id,
                Some(face_id as u64),
                embedding,
                0.4,
            ));
            assignments.push(ClusterAssignment {
                face_id,
                cluster_id: 3,
            });
        }
        let anchors = vec![
            anchor(1, 1, source_a, 16),
            anchor(2, 100, source_b, 17),
            anchor(3, 1_000, target, 272),
        ];

        let (assignments, anchors) = recover_small_fragments_with_params(
            &faces,
            assignments,
            anchors,
            &HashSet::new(),
            &HashMap::from([(3, 30)]),
            &HashSet::new(),
            0.75,
            12,
            0.05,
        );

        assert_eq!(
            cluster_for(&assignments, 100),
            cluster_for(&assignments, 1_000)
        );
        assert_ne!(
            cluster_for(&assignments, 1),
            cluster_for(&assignments, 1_000)
        );
        assert_eq!(anchors.len(), 2);
    }

    #[test]
    fn recovery_revalidates_margin_after_competing_target_changes() {
        let vector = |degrees: f32, z: f32| {
            let radians = degrees.to_radians();
            unit(&[radians.cos(), radians.sin(), z])
        };
        let mut faces = Vec::new();
        let mut assignments = Vec::new();
        let mut push_cluster = |cluster_id: i32,
                                first_face_id: i64,
                                count: i64,
                                degrees: f32,
                                content_base: u64,
                                shared_first_content: Option<u64>| {
            for offset in 0..count {
                let face_id = first_face_id + offset;
                let content = if offset == 0 {
                    shared_first_content.unwrap_or(content_base)
                } else {
                    content_base + offset as u64
                };
                let z = (offset as f32 - (count - 1) as f32 / 2.0) * 0.000_001;
                faces.push(row_with_content(
                    face_id,
                    face_id,
                    Some(content),
                    vector(degrees, z),
                    0.4,
                ));
                assignments.push(ClusterAssignment {
                    face_id,
                    cluster_id,
                });
            }
        };

        push_cluster(1, 1, 2, 0.0, 100, None);
        push_cluster(2, 100, 3, 1.0, 200, Some(9_000));
        push_cluster(3, 1_000, 13, 30.0, 300, Some(9_000));
        push_cluster(4, 2_000, 13, 42.0, 400, None);
        let anchors = vec![
            anchor(1, 1, vector(0.0, 0.0), 2),
            anchor(2, 100, vector(1.0, 0.0), 3),
            anchor(3, 1_000, vector(30.0, 0.0), 13),
            anchor(4, 2_000, vector(42.0, 0.0), 13),
        ];

        let (assignments, anchors) = recover_small_fragments_with_params(
            &faces,
            assignments,
            anchors,
            &HashSet::new(),
            &HashMap::new(),
            &HashSet::new(),
            0.75,
            12,
            0.05,
        );

        assert_eq!(
            cluster_for(&assignments, 100),
            cluster_for(&assignments, 2_000)
        );
        assert_ne!(
            cluster_for(&assignments, 1),
            cluster_for(&assignments, 1_000)
        );
        assert_ne!(
            cluster_for(&assignments, 1),
            cluster_for(&assignments, 2_000)
        );
        assert_eq!(anchors.len(), 3);
    }

    #[test]
    fn recovery_rejects_large_fragment_without_enough_member_evidence() {
        let matching = unit(&[1.0, 0.0, 0.0]);
        let mut faces = Vec::new();
        let mut assignments = Vec::new();
        for offset in 0..154i64 {
            let face_id = offset + 1;
            let variation = offset as f32 * 0.000_001;
            let (embedding, quality) = if offset < 51 {
                (unit(&[1.0, 0.0, variation]), 0.9)
            } else if offset < 64 {
                (unit(&[0.0, 1.0, variation]), 0.9)
            } else {
                (unit(&[1.0, 0.0, variation]), 0.1)
            };
            faces.push(row_with_content(
                face_id,
                face_id,
                Some(face_id as u64),
                embedding,
                quality,
            ));
            assignments.push(ClusterAssignment {
                face_id,
                cluster_id: 1,
            });
        }
        for offset in 0..8_267i64 {
            let face_id = 10_000 + offset;
            let embedding = unit(&[1.0, 0.0, (offset as f32 + 10_000.0) * 0.000_000_01]);
            faces.push(row_with_content(
                face_id,
                face_id,
                Some(face_id as u64),
                embedding,
                0.4,
            ));
            assignments.push(ClusterAssignment {
                face_id,
                cluster_id: 2,
            });
        }
        let anchors = vec![
            anchor(1, 1, matching.clone(), 154),
            anchor(2, 10_000, matching, 8_267),
        ];

        let (_, anchors) = recover_small_fragments_with_params(
            &faces,
            assignments,
            anchors,
            &HashSet::new(),
            &HashMap::from([(2, 20)]),
            &HashSet::new(),
            0.75,
            12,
            0.05,
        );

        assert_eq!(anchors.len(), 2);
    }

    #[test]
    fn copied_same_capture_and_manual_different_are_cannot_links() {
        let (mut faces, assignments, anchors) = recovery_fixture(false);
        faces[0].content_hash = faces[2].content_hash.clone();
        let (_, kept) = recover_small_fragments_with_params(
            &faces,
            assignments.clone(),
            anchors.clone(),
            &HashSet::new(),
            &HashMap::new(),
            &HashSet::new(),
            0.75,
            12,
            0.05,
        );
        assert_eq!(kept.len(), 2);

        let (fresh_faces, _, _) = recovery_fixture(false);
        let (_, kept) = recover_small_fragments_with_params(
            &fresh_faces,
            assignments,
            anchors,
            &HashSet::from([(1, 2)]),
            &HashMap::new(),
            &HashSet::new(),
            0.75,
            12,
            0.05,
        );
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn consolidation_rejects_copied_same_capture() {
        let v = unit(&[1.0, 0.0, 0.0]);
        let faces = vec![
            row_with_content(1, 10, Some(77), v.clone(), 0.9),
            row_with_content(2, 20, Some(77), v, 0.9),
        ];
        let assignments = vec![
            ClusterAssignment {
                face_id: 1,
                cluster_id: 1,
            },
            ClusterAssignment {
                face_id: 2,
                cluster_id: 2,
            },
        ];
        let anchors = vec![
            anchor(1, 1, Vec::new(), 1),
            anchor(2, 2, Vec::new(), 1),
        ];
        let (_, anchors) = consolidate(&faces, assignments, anchors, &HashSet::new(), 0.85);
        assert_eq!(anchors.len(), 2);
    }

    #[test]
    fn identity_anchor_centroid_is_unit_and_radius_matches_p10_policy() {
        let v = unit(&[1.0, 0.0, 0.0]);
        let faces = vec![
            row(1, 1, v.clone(), 0.9),
            row(2, 2, v, 0.9),
            row(3, 3, unit(&[0.0, 1.0, 0.0]), 0.9),
        ];
        let assignments = vec![
            ClusterAssignment {
                face_id: 1,
                cluster_id: 1,
            },
            ClusterAssignment {
                face_id: 2,
                cluster_id: 1,
            },
            ClusterAssignment {
                face_id: 3,
                cluster_id: 2,
            },
        ];
        let anchors = identity_anchors(&faces, &assignments);
        let repeated = anchors
            .iter()
            .find(|anchor| anchor.cluster_id == 1)
            .unwrap();
        let norm = repeated
            .centroid
            .iter()
            .map(|value| value * value)
            .sum::<f32>()
            .sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
        assert!((repeated.anchor_radius - 0.85).abs() < 1e-6);
        assert!(
            (anchors
                .iter()
                .find(|anchor| anchor.cluster_id == 2)
                .unwrap()
                .anchor_radius
                - 0.50)
                .abs()
                < 1e-6
        );
    }

    #[test]
    fn dense_hnsw_band_larger_than_32_stays_connected() {
        let centroids: Vec<Vec<f32>> = (0..96)
            .map(|index| unit(&[1.0, index as f32 * 0.0001, 0.0]))
            .collect();
        let labels = union_find_labels(centroids.len(), &edges_hnsw(&centroids, 0.99));
        assert_eq!(labels.into_iter().collect::<HashSet<_>>().len(), 1);
    }
}
