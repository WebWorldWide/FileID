// Face clustering — density-clustering driver over SFace (128-d) embeddings.
//
// Source of truth for thresholds: `identity_clustering::Hyperparameters::default()`
// and the COS_* / MERGE_SUGGEST_* / AUTOMERGE_* constants in THIS file. Do not
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

use std::collections::HashMap;

/// Cosine threshold for "definitely same person". Tuned for SFace's 128-d
/// embeddings — OpenCV's published same-identity cosine for SFace is 0.363, so
/// genuine pairs sit lower than ArcFace's old 512-d distribution. PROVISIONAL:
/// anchored on the OpenCV reference; calibrate against a labeled library.
pub const COS_HIGH: f32 = 0.50;

/// Cosine threshold for "definitely different person". The 0.32..=0.50 band is
/// the uncertain range that routes through VLM verification. SFace default
/// (provisional — calibrate with labeled faces).
pub const COS_LOW: f32 = 0.32;

/// Lower bound for surfacing MERGE suggestions in the People tab. The old code
/// reused COS_LOW (0.32) as the floor, which flooded the sheet with anchor pairs
/// deep in impostor territory — empirically (identity_clustering.rs) genuine
/// same-person SFace cosine sits at 0.88–0.95 and the hardest different-person
/// (lookalike) pairs top out near ~0.55, so a 0.32 floor is mostly noise. 0.55
/// keeps the genuinely-uncertain band (plausible cross-pose same person, plus
/// the hardest impostors worth a human glance) and drops the rest — fewer, more
/// actionable suggestions. Distinct from COS_LOW so the VLM-verifier band is
/// unaffected.
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
#[allow(dead_code)]
pub struct FaceRow {
    pub face_id: i64,
    pub file_id: i64,
    pub embedding: Vec<f32>,
    pub quality: f32,
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
    pub anchor_embedding: Vec<f32>,
    pub member_count: u32,
}

/// Group `faces` into clusters via the two-pass density algorithm in
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
            anchor_embedding: faces[best_idx].embedding.clone(),
            member_count: members.len() as u32,
        });
    }
    anchors.sort_by_key(|a| a.cluster_id);
    (assignments, anchors)
}

/// Default minimum CENTROID cosine to auto-fold two clusters into one person —
/// the Balanced default. 0.75 sits inside genuine same-person territory
/// (empirical SFace median 0.88–0.95) yet comfortably above the hardest
/// cross-identity / lookalike matches (~0.55, well under the 0.66 Pass-1 core
/// threshold), so fragments of the SAME identity that the over-split-safe
/// clusterer left apart get rejoined — the "WAY too many similar faces" the
/// People tab otherwise shows — while distinct people stay apart. Lowered from
/// 0.85 to claw back more over-split without inviting cross-identity merges
/// (the ~0.55 lookalike ceiling leaves a wide margin). Override with
/// `FILEID_FACE_AUTOMERGE_COS` (clamped to [0.70, 1.0]; set 1.0 to disable and
/// keep pure over-split). Centroids (means of all member embeddings) are
/// denoised, so this is safer than any single anchor-to-anchor comparison.
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
    const K: usize = 32;
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
        // Query K+1 so the self-hit doesn't crowd out a real neighbor.
        for (j, _approx_dist) in searcher.top_k(&idx, &centroids[i], K + 1) {
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
pub fn consolidate<S: std::hash::BuildHasher>(
    faces: &[FaceRow],
    assignments: Vec<ClusterAssignment>,
    anchors: Vec<ClusterAnchor>,
    blocked: &std::collections::HashSet<(i32, i32), S>,
    threshold: f32,
) -> (Vec<ClusterAssignment>, Vec<ClusterAnchor>) {
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

    let blocked_idx: Vec<(usize, usize)> = blocked
        .iter()
        .filter_map(|&(a, b)| Some((*idx_of.get(&a)?, *idx_of.get(&b)?)))
        .collect();

    let mut parent: Vec<usize> = (0..cids.len()).collect();
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
        // Reject if merging ri,rj would put a "different people" pair together.
        let conflict = blocked_idx.iter().any(|&(a, b)| {
            let ra = find(&mut parent, a);
            let rb = find(&mut parent, b);
            (ra == ri && rb == rj) || (ra == rj && rb == ri)
        });
        if conflict {
            continue;
        }
        parent[ri] = rj;
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
                anchor_embedding: base.anchor_embedding.clone(),
                member_count: total,
            })
        })
        .collect();
    new_anchors.sort_by_key(|a| a.cluster_id);
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

/// Pairs in the uncertain similarity band 0.45..=0.70. The VLM verifier
/// is invoked on these — outputs go back into the union-find.
#[allow(dead_code)]
pub fn uncertain_pairs(faces: &[FaceRow]) -> Vec<(i64, i64, f32)> {
    let mut pairs = Vec::new();
    for i in 0..faces.len() {
        for j in (i + 1)..faces.len() {
            let sim = cosine(&faces[i].embedding, &faces[j].embedding);
            if (COS_LOW..COS_HIGH).contains(&sim) {
                pairs.push((faces[i].face_id, faces[j].face_id, sim));
            }
        }
    }
    pairs
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

    fn row(id: i64, file: i64, e: Vec<f32>, q: f32) -> FaceRow {
        FaceRow { face_id: id, file_id: file, embedding: e, quality: q }
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
    fn uncertain_band_pairs_collected() {
        // ~0.40 cosine — inside the SFace uncertain band (COS_LOW..COS_HIGH).
        let a = unit(&[1.0, 0.0]);
        let b = unit(&[0.40, 0.9165]); // dot ≈ 0.40
        let faces = vec![row(1, 1, a, 0.9), row(2, 2, b, 0.9)];
        let pairs = uncertain_pairs(&faces);
        assert_eq!(pairs.len(), 1);
        assert!(pairs[0].2 > COS_LOW && pairs[0].2 < COS_HIGH);
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
    // is COS_HIGH-close to its anchor (clusters can chain transitively).
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

    fn anchor(cid: i32, face: i64, e: Vec<f32>, count: u32) -> ClusterAnchor {
        ClusterAnchor {
            cluster_id: cid,
            anchor_face_id: face,
            anchor_embedding: e,
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
}
