// Face clustering — IdentityClustering driver.
//
// Reads ArcFace embeddings from `arcface_embeddings`, runs deterministic
// agglomerative clustering by cosine similarity, persists per-face cluster
// IDs into `face_verifications`, and emits `FaceClusteringResult` IPC.
//
// Algorithm:
//   1. Load every face row that has an embedding (skip low-quality/extreme-pose).
//   2. Build pairs by cosine ≥ 0.45; route 0.45–0.70 to VLM verify.
//   3. Connected-components on the high-similarity graph → clusters.
//   4. Anchor selection per cluster: highest-quality embedding, persisted
//      to `identity_anchors` so future faces compare against a stable ref.
//   5. Emit IPC + write to DB in a single tx so app sidebar refresh is atomic.

use std::collections::HashMap;

/// Cosine threshold for "definitely same person". High enough that false
/// positives are vanishingly rare on ArcFace's 512-d unit hypersphere.
pub const COS_HIGH: f32 = 0.70;

/// Cosine threshold for "definitely different person". The 0.45..=0.70
/// band is the uncertain range that routes through VLM verification.
pub const COS_LOW: f32 = 0.45;

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
    let result = super::identity_clustering::cluster(
        &embeddings,
        |i| {
            let mut hits: Vec<super::identity_clustering::Neighbor> = if let Some(idx) = &hnsw_idx {
                // Query k+1 so we can drop the self-hit; convert squared-L2 →
                // cosine (vectors are unit-norm: d = 2(1 − cos)).
                crate::util::hnsw_index::search_top_k(idx, &embeddings[i], k + 1)
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
            hits.sort_by(|a, b| {
                b.similarity
                    .partial_cmp(&a.similarity)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            hits.truncate(k);
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
        // ~0.55 cosine via small angle.
        let a = unit(&[1.0, 0.0]);
        let b = unit(&[0.55, 0.835]); // dot ≈ 0.55
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
}
