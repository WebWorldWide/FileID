// Two-pass density clustering + quality validation for face identity.
// Port of macOS engine/Sources/FileIDEngine/Pipeline/IdentityClustering.swift.
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
        Self {
            pass1_cosine: 0.55,
            pass2_cosine: 0.45,
            pass2_margin: 0.05,
            pass3_variance_threshold: 0.05,
            pass3_min_mean_cosine: 0.50,
            pass3_max_splits: 3,
            k_nn: 10,
        }
    }
}

#[derive(Debug, Clone)]
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
        return ClusterResult {
            cluster_ids: vec![0; n],
            cluster_count: 0,
            core_count: 0,
            outliers_assigned: 0,
            outliers_as_singletons: 0,
            splits_applied: 0,
            duration_seconds: 0.0,
        };
    }

    // ── Pass 1: connected components above pass1_cosine ───────────
    let mut uf = UnionFind::new(n);
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
    let mut root_members: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..n {
        root_members.entry(uf.find(i)).or_default().push(i);
    }
    let mut cores: Vec<Vec<usize>> = Vec::new();
    let mut outliers: Vec<usize> = Vec::new();
    for (_, members) in root_members {
        if members.len() >= 2 {
            cores.push(members);
        } else {
            outliers.extend(members);
        }
    }
    let pass1_cores = cores.len();

    // ── Pass 2: outlier assignment with margin ────────────────────
    let mut core_centroids: Vec<Vec<f32>> = cores
        .iter()
        .map(|c| centroid_normalized(c, embeddings, dim))
        .collect();
    let mut outliers_assigned = 0;
    let mut outliers_as_singletons = 0;
    for outlier in outliers {
        let v = &embeddings[outlier];
        let mut c1_idx: isize = -1;
        let mut c2_idx: isize = -1;
        let mut c1_sim: f32 = -2.0;
        let mut c2_sim: f32 = -2.0;
        for (idx, centroid) in core_centroids.iter().enumerate() {
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
            core_centroids[target] = centroid_normalized(&cores[target], embeddings, dim);
            outliers_assigned += 1;
        } else {
            cores.push(vec![outlier]);
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
    let mut sum = vec![0f32; dim];
    for &i in indices {
        let v = &embeddings[i];
        for d in 0..dim {
            sum[d] += v[d];
        }
    }
    let mut norm: f32 = 0.0;
    for d in 0..dim {
        norm += sum[d] * sum[d];
    }
    let inv_n = 1.0 / norm.sqrt().max(f32::MIN_POSITIVE);
    for d in 0..dim {
        sum[d] *= inv_n;
    }
    sum
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

    let mean_ok = mean >= params.pass3_min_mean_cosine;
    let var_ok = variance <= params.pass3_variance_threshold;
    if mean_ok && var_ok {
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
}
