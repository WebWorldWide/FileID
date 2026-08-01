// Two-pass density clustering + quality validation for face identity.
// Replaces Chinese Whispers, which collapsed bridge faces into mega-clusters.
//
//   Pass 1 — connected components on a kNN graph at cosine ≥ pass1Cosine.
//            Components of size ≥ 2 become identity "cores".
//   Pass 2 — each unassigned face joins its nearest core iff
//            cosine ≥ pass2Cosine AND (top1 − top2 ≥ pass2Margin).
//            The margin rule blocks the bridge-face merges that
//            sank Chinese Whispers.
//   Pass 3 — any cluster with low mean intra-cosine or high variance
//            is split via 2-means in cosine space and re-validated;
//            hard floor against mega-cluster collapse.
//
// Convergent with Immich (DBSCAN), FaceNet (agglomerative + verification
// net), and InsightFace reference (DBSCAN/HDBSCAN, cosine 0.4–0.5).

use std::collections::HashMap;
use std::time::Instant;

#[derive(Debug, Clone, Copy)]
pub struct Hyperparameters {
    /// Minimum cosine similarity for a Pass 1 edge. Strict per ArcFace
    /// clustering literature; 0.40 is the verification threshold, where
    /// errors don't compound across a chain.
    pub pass1_cosine: f32,
    /// Minimum cosine to a core's centroid for Pass 2 assignment.
    /// Looser than Pass 1 because we're comparing to a denoised centroid.
    pub pass2_cosine: f32,
    /// Minimum gap between nearest and second-nearest core. The margin
    /// rule blocks "Adam 0.50 / Brother 0.49" ambiguous merges.
    pub pass2_margin: f32,
    /// Pass 3 splits if variance of cosines-to-centroid exceeds this.
    pub pass3_variance_threshold: f32,
    /// Pass 3 splits if mean cosine to centroid drops below this.
    pub pass3_min_mean_cosine: f32,
    /// Recursive split depth cap.
    pub pass3_max_splits: usize,
    /// kNN k for Pass 1.
    pub k_nn: usize,
}

impl Default for Hyperparameters {
    fn default() -> Self {
        // SFace (128-d) defaults, calibrated on-hardware against F:\TrueNAS with a
        // GROUND-TRUTH LABELLED set (RTX 5080, 2026-07-05: the owner labelled ~185
        // faces across a dozen people via the face-labeler tool). The labels
        // overturned the earlier cohesion-only guess (pass1=0.82) — see below.
        //
        // What the labels showed, on REAL same-age same-person pairs: SFace works
        // WELL — same-person cosine median ~0.59 (p90 0.82), different-person
        // median 0.16 with a MAX of only 0.47. So the classes separate cleanly and
        // the optimal link threshold is ~0.43–0.50, NOT 0.82. At 0.82 recall was
        // ~1% (it only linked near-duplicate shots) — which is exactly why real
        // people fragmented into many clusters. Dropping to 0.50 took the labelled
        // People-tab F1 from ~0.02 to 1.00 (precision 1.0, recall 1.0).
        //
        // Two confounds that had masked this and MUST stay in mind:
        //  (1) A person across a big AGE gap (child↔adult) is genuinely unmatchable
        //      by any face model (their embeddings differ like different people) —
        //      those legitimately land in separate clusters; only manual naming /
        //      "Suggest merges" unites them. Not a bug.
        //  (2) LOW-QUALITY faces (this corpus is scanned/old — quality caps ~0.42)
        //      produce noise embeddings: same-person cosine on quality<0.35 faces
        //      is ~0.14 (== different-person), and they chain into cones. Handled
        //      by the PRE-clustering quality gate FILEID_FACE_CLUSTER_MIN_QUALITY
        //      (commands/face_clustering.rs, default 0.35) — a mild gate that drops
        //      only the deepest noise and lifted labelled F1 to a clean 1.00.
        //
        // So: pass1=0.50 (link threshold in the same/diff gap), pass2=0.45, and
        // MUTUAL-kNN default-ON (each edge needs both faces in the other's above-
        // threshold neighbourhood — kills the last single-bridge chaining; lifted
        // recall to 1.0 with no fragmentation). All env-overridable per corpus
        // (unset → these defaults): FILEID_FACE_PASS1_COSINE / _PASS2_COSINE /
        // _PASS2_MARGIN / _MUTUAL_KNN / _PASS3_MIN_MEAN_COSINE /
        // _PASS3_VARIANCE_THRESHOLD / _PASS3_MAX_SPLITS, plus the quality gate.
        // On a higher-quality (modern-photo) corpus these thresholds still hold
        // (same-person there is even higher, ~0.85+); loosen only if a corpus is
        // unusually low-quality. Further gains want a stronger face embedder +
        // cross-corpus labels — see NEXT.md.
        // Reject non-finite (NaN/inf) env values — they'd silently poison
        // comparisons (e.g. `q < NaN` is always false). `clamp` for the cosine
        // knobs keeps a fat-fingered env from making pass1 a value that makes
        // every face a singleton (>1) or one mega-cluster (<0).
        let env_f32 = |key: &str, default: f32| -> f32 {
            std::env::var(key)
                .ok()
                .and_then(|s| s.trim().parse::<f32>().ok())
                .filter(|v| v.is_finite())
                .unwrap_or(default)
        };
        let env_cos = |key: &str, default: f32| -> f32 { env_f32(key, default).clamp(0.0, 1.0) };
        Self {
            pass1_cosine: env_cos("FILEID_FACE_PASS1_COSINE", 0.50),
            pass2_cosine: env_cos("FILEID_FACE_PASS2_COSINE", 0.45),
            pass2_margin: env_cos("FILEID_FACE_PASS2_MARGIN", 0.10),
            pass3_variance_threshold: env_f32("FILEID_FACE_PASS3_VARIANCE_THRESHOLD", 0.04),
            pass3_min_mean_cosine: env_cos("FILEID_FACE_PASS3_MIN_MEAN_COSINE", 0.60),
            pass3_max_splits: std::env::var("FILEID_FACE_PASS3_MAX_SPLITS")
                .ok()
                .and_then(|s| s.trim().parse::<usize>().ok())
                .map(|v| v.clamp(1, 64))
                .unwrap_or(7),
            // k_nn caps how many neighbors Pass 1 sees per face; 10 truncated
            // dense identities (anyone with >10 similar faces) into split cores
            // that Pass 2/consolidate can't always rejoin. Measured on the full
            // 84,582-face Adlon corpus (2026-07-14): k 10→32 alone cut person
            // clusters 2,272 → 864 while the top clusters got TIGHTER (mean
            // cosine-to-centroid up), i.e. it heals real fragmentation instead
            // of chaining identities. Env-tunable for corpus-specific sweeps.
            k_nn: std::env::var("FILEID_FACE_KNN")
                .ok()
                .and_then(|s| s.trim().parse::<usize>().ok())
                .map(|v| v.clamp(4, 256))
                .unwrap_or(32),
        }
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ClusterResult {
    /// `cluster_ids[i]` = dense cluster ID for embedding i. Always
    /// non-negative; every face gets a cluster (singleton if alone).
    pub cluster_ids: Vec<usize>,
    pub cluster_count: usize,
    /// Components of size ≥ 2 from Pass 1 — pre-Pass-2-merging.
    pub core_count: usize,
    /// Outliers merged into an existing core in Pass 2.
    pub outliers_assigned: usize,
    /// Outliers that became their own singleton clusters.
    pub outliers_as_singletons: usize,
    /// Total Pass-3 splits applied across all clusters.
    pub splits_applied: usize,
    pub duration_seconds: f64,
}

/// One kNN hit. `similarity` is cosine on pre-L2-normalized embeddings
/// (i.e. dot product). FaceClustering supplies HNSW-backed neighbors.
#[derive(Debug, Clone, Copy)]
pub struct Neighbor {
    pub idx: usize,
    pub similarity: f32,
}

/// Run the full pipeline. `embeddings[i]` must be L2-normalized.
/// `searcher(i)` returns the kNN of face `i` with cosine similarities.
pub fn cluster<F>(
    embeddings: &[Vec<f32>],
    mut searcher: F,
    params: Hyperparameters,
) -> ClusterResult
where
    F: FnMut(usize) -> Vec<Neighbor>,
{
    let started = Instant::now();
    let n = embeddings.len();
    if n == 0 {
        return ClusterResult {
            cluster_ids: Vec::new(),
            cluster_count: 0,
            core_count: 0,
            outliers_assigned: 0,
            outliers_as_singletons: 0,
            splits_applied: 0,
            duration_seconds: 0.0,
        };
    }
    let dim = embeddings[0].len();
    if dim == 0 {
        // Treat every face as its own singleton cluster so cluster_count ==
        // cluster_ids.iter().max() + 1 and the caller can iterate 0..cluster_count
        // without missing any id. Returning cluster_count: 0 with cluster_ids all
        // set to 0 was contradictory: the caller skips People-tab population for
        // ids >= cluster_count, orphaning all n faces.
        return ClusterResult {
            cluster_ids: (0..n).collect(),
            cluster_count: n,
            core_count: 0,
            outliers_assigned: 0,
            outliers_as_singletons: n,
            splits_applied: 0,
            duration_seconds: 0.0,
        };
    }

    // ── Pass 1: connected components above pass1_cosine ───────────
    // MUTUAL-kNN is now the DEFAULT (label-validated 2026-07-05): an edge i—j is
    // kept only when EACH face is in the other's above-threshold neighbourhood,
    // which breaks the single "bridge" face that would otherwise chain two
    // identities into one mega-cluster. Set FILEID_FACE_MUTUAL_KNN=0 to fall back
    // to plain single-linkage (union i with every kNN hit above the threshold —
    // simpler, zero extra allocation, but chains through bridge faces). Both fail
    // toward over-split (UI-mergeable, the safe direction); Pass 3's 2-means split
    // is unchanged either way.
    let mutual_knn = std::env::var("FILEID_FACE_MUTUAL_KNN")
        .ok()
        .map(|s| !(s == "0" || s.eq_ignore_ascii_case("false")))
        .unwrap_or(true);
    let mut uf = UnionFind::new(n);
    if mutual_knn {
        let mut directed: std::collections::HashSet<(usize, usize)> =
            std::collections::HashSet::new();
        let mut candidates: Vec<(usize, usize)> = Vec::new();
        for i in 0..n {
            for hit in searcher(i) {
                if hit.idx == i || hit.idx >= n {
                    continue;
                }
                if hit.similarity < params.pass1_cosine {
                    continue;
                }
                directed.insert((i, hit.idx));
                candidates.push((i, hit.idx));
            }
        }
        for (i, j) in candidates {
            // Union only mutual edges (j also lists i above threshold).
            if directed.contains(&(j, i)) {
                uf.union(i, j);
            }
        }
    } else {
        for i in 0..n {
            for hit in searcher(i) {
                if hit.idx == i || hit.idx >= n {
                    continue;
                }
                if hit.similarity < params.pass1_cosine {
                    continue;
                }
                uf.union(i, hit.idx);
            }
        }
    }
    let mut root_members: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..n {
        root_members.entry(uf.find(i)).or_default().push(i);
    }
    // Iterate in sorted-key order so cluster-ID assignment is deterministic
    // across runs. Otherwise HashMap iteration order leaks all the way into
    // People-tab cluster numbers, and a re-scan of the same library produces
    // different IDs each time.
    let mut sorted_groups: Vec<(usize, Vec<usize>)> = root_members.into_iter().collect();
    sorted_groups.sort_by_key(|(root, _)| *root);
    let mut cores: Vec<Vec<usize>> = Vec::new();
    let mut outliers: Vec<usize> = Vec::new();
    for (_, members) in sorted_groups {
        if members.len() >= 2 {
            cores.push(members);
        } else {
            outliers.extend(members);
        }
    }
    let pass1_cores = cores.len();

    // ── Pass 2: outlier assignment with margin ────────────────────
    // Maintain a parallel unnormalized running sum per core alongside the
    // normalized centroid. Recomputing centroid_normalized over the full
    // membership on every outlier add is O(S) per add → O(S^2) over a pass;
    // folding the outlier into the running sum is O(dim) per add and the
    // normalized copy is recomputed from that sum. Mathematically identical
    // (only floating-point reassociation differs).
    let mut core_sums: Vec<Vec<f32>> = cores
        .iter()
        .map(|c| centroid_sum(c, embeddings, dim))
        .collect();
    let mut core_centroids: Vec<Vec<f32>> =
        core_sums.iter().map(|s| normalize_sum(s, dim)).collect();
    let mut outliers_assigned = 0;
    let mut outliers_as_singletons = 0;
    // R4-02/R4-08: snapshot the Pass-1 core count. An outlier may only join a
    // genuine size>=2 Pass-1 core, never a singleton appended below. Without the
    // bound the inner scan grows by every appended singleton (O(M^2) at scale)
    // AND an outlier can order-dependently merge into a PRIOR outlier's singleton.
    // Assigned-outlier centroid updates write in place at index < core_n, so real
    // cores keep being re-scanned with fresh values; only the singletons are excluded.
    let core_n = core_centroids.len();
    for outlier in outliers {
        let v = &embeddings[outlier];
        let mut c1_idx: isize = -1;
        let mut c2_idx: isize = -1;
        let mut c1_sim: f32 = -2.0;
        let mut c2_sim: f32 = -2.0;
        for (idx, centroid) in core_centroids.iter().take(core_n).enumerate() {
            let s = dot(v, centroid);
            if s > c1_sim {
                c2_sim = c1_sim;
                c2_idx = c1_idx;
                c1_sim = s;
                c1_idx = idx as isize;
            } else if s > c2_sim {
                c2_sim = s;
                c2_idx = idx as isize;
            }
        }
        let passes_floor = c1_idx >= 0 && c1_sim >= params.pass2_cosine;
        let passes_margin = c2_idx < 0 || (c1_sim - c2_sim) >= params.pass2_margin;
        if passes_floor && passes_margin {
            let target = c1_idx as usize;
            cores[target].push(outlier);
            let sum = &mut core_sums[target];
            for d in 0..dim.min(v.len()) {
                sum[d] += v[d];
            }
            core_centroids[target] = normalize_sum(sum, dim);
            outliers_assigned += 1;
        } else {
            cores.push(vec![outlier]);
            // A singleton's unnormalized sum is the embedding itself; its
            // normalized centroid is the (already L2-normalized) embedding.
            core_sums.push(v.clone());
            core_centroids.push(v.clone());
            outliers_as_singletons += 1;
        }
    }

    // ── Pass 3: quality validation + 2-means split ────────────────
    let mut splits_applied = 0;
    let mut refined: Vec<Vec<usize>> = Vec::with_capacity(cores.len());
    for cluster_members in cores {
        let parts = validate_and_split(
            cluster_members,
            embeddings,
            dim,
            params,
            params.pass3_max_splits,
        );
        if parts.len() > 1 {
            splits_applied += parts.len() - 1;
        }
        refined.extend(parts);
    }

    // ── Materialize result ────────────────────────────────────────
    let mut cluster_ids = vec![0usize; n];
    for (cid, members) in refined.iter().enumerate() {
        for &m in members {
            cluster_ids[m] = cid;
        }
    }
    ClusterResult {
        cluster_ids,
        cluster_count: refined.len(),
        core_count: pass1_cores,
        outliers_assigned,
        outliers_as_singletons,
        splits_applied,
        duration_seconds: started.elapsed().as_secs_f64(),
    }
}

/// L2-normalized mean of the indexed embeddings.
fn centroid_normalized(indices: &[usize], embeddings: &[Vec<f32>], dim: usize) -> Vec<f32> {
    let sum = centroid_sum(indices, embeddings, dim);
    normalize_sum(&sum, dim)
}

/// Unnormalized component-wise sum of the indexed embeddings. Pass 2 keeps this
/// alongside the normalized centroid so an outlier add is O(dim), not O(S).
fn centroid_sum(indices: &[usize], embeddings: &[Vec<f32>], dim: usize) -> Vec<f32> {
    let mut sum = vec![0f32; dim];
    for &i in indices {
        let v = &embeddings[i];
        // Callers guarantee uniform dim (the face loader filters to the modal
        // dimension; restructure passes one CLIP space). The `.min` is a
        // release-safe backstop so a stray short vector can never index out of
        // bounds and panic here.
        debug_assert_eq!(v.len(), dim, "centroid_sum: embedding dim mismatch");
        for d in 0..dim.min(v.len()) {
            sum[d] += v[d];
        }
    }
    sum
}

/// L2-normalize a running sum vector into a unit centroid.
fn normalize_sum(sum: &[f32], dim: usize) -> Vec<f32> {
    let mut out = vec![0f32; dim];
    let mut norm: f32 = 0.0;
    for d in 0..dim {
        norm += sum[d] * sum[d];
    }
    let inv_n = 1.0 / norm.sqrt().max(f32::MIN_POSITIVE);
    for d in 0..dim {
        out[d] = sum[d] * inv_n;
    }
    out
}

/// Cosine on pre-normalized vectors = dot product.
#[inline]
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let mut s: f32 = 0.0;
    for i in 0..n {
        s += a[i] * b[i];
    }
    s
}

/// Recursively split a cluster if it fails the variance / mean-cosine
/// quality bar. Returns one or more sub-clusters.
// The 2-means split uses paired `seed_a_*` / `seed_b_*` names; the
// pairing is the algorithm.
#[allow(clippy::similar_names)]
fn validate_and_split(
    cluster: Vec<usize>,
    embeddings: &[Vec<f32>],
    dim: usize,
    params: Hyperparameters,
    splits_remaining: usize,
) -> Vec<Vec<usize>> {
    if cluster.len() < 2 {
        return vec![cluster];
    }
    let centroid = centroid_normalized(&cluster, embeddings, dim);
    let mut sims: Vec<f32> = Vec::with_capacity(cluster.len());
    let mut sum_s: f32 = 0.0;
    for &i in &cluster {
        let s = dot(&embeddings[i], &centroid);
        sims.push(s);
        sum_s += s;
    }
    let mean = sum_s / cluster.len() as f32;
    let mut variance: f32 = 0.0;
    for &s in &sims {
        let d = s - mean;
        variance += d * d;
    }
    variance /= cluster.len() as f32;

    let mut has_anti_correlated_pair = false;
    if cluster.len() >= 2 {
        for idx1 in 0..(cluster.len() - 1) {
            for idx2 in (idx1 + 1)..cluster.len() {
                if dot(&embeddings[cluster[idx1]], &embeddings[cluster[idx2]]) < 0.0 {
                    has_anti_correlated_pair = true;
                    break;
                }
            }
            if has_anti_correlated_pair {
                break;
            }
        }
    }

    let mean_ok = mean >= params.pass3_min_mean_cosine;
    let var_ok = variance <= params.pass3_variance_threshold;
    if mean_ok && var_ok && !has_anti_correlated_pair {
        return vec![cluster];
    }
    if splits_remaining == 0 {
        return vec![cluster];
    }

    // 2-means seeds: face farthest from centroid (lowest cosine), and
    // the face farthest from THAT seed.
    let mut seed_a_idx = cluster[0];
    let mut seed_a_sim = sims[0];
    for (k, &s) in sims.iter().enumerate() {
        if s < seed_a_sim {
            seed_a_sim = s;
            seed_a_idx = cluster[k];
        }
    }
    let a_vec = &embeddings[seed_a_idx];
    let mut seed_b_idx: isize = -1;
    let mut seed_b_sim: f32 = 2.0;
    for &i in &cluster {
        if i == seed_a_idx {
            continue;
        }
        let s = dot(&embeddings[i], a_vec);
        if s < seed_b_sim {
            seed_b_sim = s;
            seed_b_idx = i as isize;
        }
    }
    if seed_b_idx < 0 {
        return vec![cluster];
    }
    let seed_b_idx = seed_b_idx as usize;

    let mut group_a: Vec<usize> = Vec::new();
    let mut group_b: Vec<usize> = Vec::new();
    let mut cent_a = embeddings[seed_a_idx].clone();
    let mut cent_b = embeddings[seed_b_idx].clone();
    for _ in 0..10 {
        group_a.clear();
        group_b.clear();
        for &i in &cluster {
            let v = &embeddings[i];
            if dot(v, &cent_a) >= dot(v, &cent_b) {
                group_a.push(i);
            } else {
                group_b.push(i);
            }
        }
        if group_a.is_empty() || group_b.is_empty() {
            break;
        }
        let new_a = centroid_normalized(&group_a, embeddings, dim);
        let new_b = centroid_normalized(&group_b, embeddings, dim);
        // Convergence: both centroids barely moved.
        let converged = dot(&new_a, &cent_a) > 0.999 && dot(&new_b, &cent_b) > 0.999;
        cent_a = new_a;
        cent_b = new_b;
        if converged {
            break;
        }
    }
    if group_a.is_empty() || group_b.is_empty() {
        return vec![cluster];
    }
    let mut left = validate_and_split(group_a, embeddings, dim, params, splits_remaining - 1);
    let right = validate_and_split(group_b, embeddings, dim, params, splits_remaining - 1);
    left.extend(right);
    left
}

// ── Union-Find with path compression + union by rank ───────────────

struct UnionFind {
    parent: Vec<usize>,
    rank: Vec<u8>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
            rank: vec![0; n],
        }
    }
    fn find(&mut self, x: usize) -> usize {
        let mut r = x;
        while self.parent[r] != r {
            r = self.parent[r];
        }
        // Path compression.
        let mut cur = x;
        while self.parent[cur] != r {
            let next = self.parent[cur];
            self.parent[cur] = r;
            cur = next;
        }
        r
    }
    fn union(&mut self, a: usize, b: usize) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra == rb {
            return;
        }
        match self.rank[ra].cmp(&self.rank[rb]) {
            std::cmp::Ordering::Less => self.parent[ra] = rb,
            std::cmp::Ordering::Greater => self.parent[rb] = ra,
            std::cmp::Ordering::Equal => {
                self.parent[rb] = ra;
                self.rank[ra] += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(v: Vec<f32>) -> Vec<f32> {
        let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.into_iter().map(|x| x / n).collect()
    }

    #[test]
    fn empty_input_yields_empty_result() {
        let empty: Vec<Vec<f32>> = Vec::new();
        let result = cluster(&empty, |_| Vec::new(), Hyperparameters::default());
        assert_eq!(result.cluster_count, 0);
        assert_eq!(result.cluster_ids.len(), 0);
    }

    #[test]
    fn two_clear_identities_separate() {
        let east_1 = unit(vec![1.0, 0.0]);
        let east_2 = unit(vec![1.001, 0.001]);
        let north_1 = unit(vec![0.0, 1.0]);
        let north_2 = unit(vec![0.001, 1.001]);
        let embeddings = vec![east_1, east_2, north_1, north_2];
        let embeddings_ref = embeddings.clone();
        let searcher = |i: usize| {
            (0..embeddings_ref.len())
                .filter(|&j| j != i)
                .map(|j| Neighbor {
                    idx: j,
                    similarity: dot(&embeddings_ref[i], &embeddings_ref[j]),
                })
                .collect()
        };
        let result = cluster(&embeddings, searcher, Hyperparameters::default());
        assert_eq!(result.cluster_count, 2);
        assert_eq!(result.cluster_ids[0], result.cluster_ids[1]);
        assert_eq!(result.cluster_ids[2], result.cluster_ids[3]);
        assert_ne!(result.cluster_ids[0], result.cluster_ids[2]);
    }

    /// All-identical embeddings (same person photographed N times) must
    /// land in exactly one cluster. Guards against a regression where
    /// the union-find or the post-DBSCAN refinement step splits a
    /// genuinely-singular identity into per-instance clusters.
    #[test]
    fn all_identical_embeddings_form_one_cluster() {
        let unit_vec = unit(vec![1.0, 0.0, 0.0, 0.0]);
        let embeddings: Vec<Vec<f32>> = (0..6).map(|_| unit_vec.clone()).collect();
        let embeddings_ref = embeddings.clone();
        let searcher = |i: usize| {
            (0..embeddings_ref.len())
                .filter(|&j| j != i)
                .map(|j| Neighbor {
                    idx: j,
                    similarity: dot(&embeddings_ref[i], &embeddings_ref[j]),
                })
                .collect()
        };
        let result = cluster(&embeddings, searcher, Hyperparameters::default());
        assert_eq!(result.cluster_count, 1, "all-identical must yield 1 cluster");
        let first_id = result.cluster_ids[0];
        for id in &result.cluster_ids {
            assert_eq!(*id, first_id, "every embedding must share the single cluster_id");
        }
    }

    /// Embeddings on orthogonal unit vectors (similarity = 0) must each
    /// land in their own cluster — they're maximally dissimilar. With
    /// dimension D ≥ N, we have N orthogonal basis vectors available.
    #[test]
    fn orthogonal_embeddings_each_in_own_cluster() {
        // 5 orthogonal unit vectors in 5-d space.
        let embeddings: Vec<Vec<f32>> = (0..5)
            .map(|i| {
                let mut v = vec![0.0_f32; 5];
                v[i] = 1.0;
                v
            })
            .collect();
        let embeddings_ref = embeddings.clone();
        let searcher = |i: usize| {
            (0..embeddings_ref.len())
                .filter(|&j| j != i)
                .map(|j| Neighbor {
                    idx: j,
                    similarity: dot(&embeddings_ref[i], &embeddings_ref[j]),
                })
                .collect()
        };
        let result = cluster(&embeddings, searcher, Hyperparameters::default());
        // 5 orthogonal embeddings: each one is its own singleton OR
        // outlier-as-singleton. Either way, the visible distinct
        // cluster_ids count must be 5.
        let unique_ids: std::collections::HashSet<_> =
            result.cluster_ids.iter().copied().collect();
        assert_eq!(
            unique_ids.len(),
            5,
            "orthogonal embeddings must produce 5 distinct cluster IDs, got {:?}",
            result.cluster_ids
        );
    }
}
