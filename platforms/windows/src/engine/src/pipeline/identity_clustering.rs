// Two-pass density clustering + quality validation for face identity.
// Pass 1: connected components on a kNN graph at cosine ≥ pass1Cosine; size ≥ 2 become cores. Pass 2: each unassigned face joins its nearest core iff cosine ≥ pass2Cosine AND (top1 − top2 ≥ pass2Margin); the margin blocks bridge-face merges. Pass 3: clusters with low mean intra-cosine or high variance are split via 2-means and re-validated.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

#[derive(Debug, Clone, Copy)]
pub struct Hyperparameters {
    /// Minimum cosine for a Pass 1 edge; strict (0.40 is the verification threshold) so errors don't compound along a chain.
    pub pass1_cosine: f32,
    /// Minimum cosine to a core's centroid for Pass 2; looser than Pass 1 because it compares against a denoised centroid.
    pub pass2_cosine: f32,
    /// Minimum gap between nearest and second-nearest core; blocks "Adam 0.50 / Brother 0.49" ambiguous merges.
    pub pass2_margin: f32,
    pub pass3_variance_threshold: f32,
    pub pass3_min_mean_cosine: f32,
    pub pass3_max_splits: usize,
    pub k_nn: usize,
    /// A cluster of at least this many faces has to hold together more tightly than a small one, because a chain of look-alike faces grows large while its centroid drifts to the average face.
    pub oversized_faces: usize,
    /// The mean cosine to its centroid a cluster of `oversized_faces` needs to stay whole; the bar rises with size.
    pub oversized_min_mean_cosine: f32,
}

/// How much the cohesion bar rises for each tenfold growth of an oversized cluster.
const COHESION_RISE_PER_DECADE: f32 = 0.05;

impl Hyperparameters {
    /// The mean cosine to its centroid a cluster of `faces` must keep. A pure identity spreads out as it grows, but a chain
    /// of look-alikes spreads out faster, so the bar rises with size.
    pub fn required_mean_cosine(&self, faces: usize) -> f32 {
        if faces < self.oversized_faces {
            return self.pass3_min_mean_cosine;
        }
        let decades = (faces as f32 / self.oversized_faces as f32).log10();
        self.pass3_min_mean_cosine.max(self.oversized_min_mean_cosine + COHESION_RISE_PER_DECADE * decades)
    }
}

impl Default for Hyperparameters {
    fn default() -> Self {
        // SFace defaults, calibrated on a labelled 41,855-face catalog: 0.66/0.54 with mutual-kNN separates every reviewed negative pair. Env-overridable (FILEID_FACE_PASS1_COSINE and siblings) for corpus sweeps.
        // Non-finite env values are rejected (`q < NaN` is always false) and the cosine knobs are clamped so a fat-fingered env can't make every face a singleton (>1) or one mega-cluster (<0).
        let env_f32 = |key: &str, default: f32| -> f32 {
            std::env::var(key)
                .ok()
                .and_then(|s| s.trim().parse::<f32>().ok())
                .filter(|v| v.is_finite())
                .unwrap_or(default)
        };
        let env_cos = |key: &str, default: f32| -> f32 { env_f32(key, default).clamp(0.0, 1.0) };
        Self {
            pass1_cosine: env_cos("FILEID_FACE_PASS1_COSINE", 0.66),
            pass2_cosine: env_cos("FILEID_FACE_PASS2_COSINE", 0.54),
            pass2_margin: env_cos("FILEID_FACE_PASS2_MARGIN", 0.10),
            pass3_variance_threshold: env_f32("FILEID_FACE_PASS3_VARIANCE_THRESHOLD", 0.04),
            pass3_min_mean_cosine: env_cos("FILEID_FACE_PASS3_MIN_MEAN_COSINE", 0.60),
            pass3_max_splits: std::env::var("FILEID_FACE_PASS3_MAX_SPLITS")
                .ok()
                .and_then(|s| s.trim().parse::<usize>().ok())
                .map(|v| v.clamp(1, 64))
                .unwrap_or(7),
        // k=10 split dense identities into cores that could not rejoin; k=32 heals fragmentation without chaining identities (measured on 84,582 faces).
            k_nn: std::env::var("FILEID_FACE_KNN")
                .ok()
                .and_then(|s| s.trim().parse::<usize>().ok())
                .map(|v| v.clamp(4, 256))
                .unwrap_or(32),
            oversized_faces: std::env::var("FILEID_FACE_OVERSIZED_FACES")
                .ok()
                .and_then(|s| s.trim().parse::<usize>().ok())
                .map_or(300, |v| v.max(2)),
            oversized_min_mean_cosine: env_cos("FILEID_FACE_OVERSIZED_MEAN_COSINE", 0.72),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ClusterResult {
    pub cluster_ids: Vec<usize>,
    pub cluster_count: usize,
    pub core_count: usize,
    pub outliers_assigned: usize,
    pub outliers_as_singletons: usize,
    pub splits_applied: usize,
    pub duration_seconds: f64,
}

/// `similarity` is cosine on L2-normalized embeddings (a dot product); FaceClustering supplies HNSW-backed neighbors.
#[derive(Debug, Clone, Copy)]
pub struct Neighbor {
    pub idx: usize,
    pub similarity: f32,
}

pub fn cluster<E, F>(
    embeddings: &[E],
    searcher: F,
    params: Hyperparameters,
) -> ClusterResult
where
    E: AsRef<[f32]>,
    F: FnMut(usize) -> Vec<Neighbor>,
{
    cluster_with_cannot_links(embeddings, searcher, params, &[])
}

/// Enforces pairwise cannot-link evidence: a cannot-linked pair can never share a Pass-1 component or Pass-2 core, even through a transitive bridge. Face clustering supplies same-physical-file pairs; semantic Restructure uses the unconstrained wrapper.
pub fn cluster_with_cannot_links<E, F>(
    embeddings: &[E],
    searcher: F,
    params: Hyperparameters,
    cannot_links: &[(usize, usize)],
) -> ClusterResult
where
    E: AsRef<[f32]>,
    F: FnMut(usize) -> Vec<Neighbor>,
{
        // Mutual-kNN is the default: an edge is kept only when each face is in the other's neighbourhood, so one bridge face cannot chain two identities. FILEID_FACE_MUTUAL_KNN=0 falls back to single-linkage, which chains through bridge faces. Both fail toward over-split, the safe direction.
    let mutual_knn = std::env::var("FILEID_FACE_MUTUAL_KNN")
        .ok()
        .map(|s| !(s == "0" || s.eq_ignore_ascii_case("false")))
        .unwrap_or(true);
    cluster_with_options(embeddings, searcher, params, cannot_links, mutual_knn)
}

pub fn cluster_with_options<E, F>(
    embeddings: &[E],
    mut searcher: F,
    params: Hyperparameters,
    cannot_links: &[(usize, usize)],
    mutual_knn: bool,
) -> ClusterResult
where
    E: AsRef<[f32]>,
    F: FnMut(usize) -> Vec<Neighbor>,
{
    let started = Instant::now();
    let views: Vec<&[f32]> = embeddings.iter().map(AsRef::as_ref).collect();
    let embeddings = views.as_slice();
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
        // Every face is its own singleton so cluster_count == max(cluster_ids) + 1; cluster_count 0 with all ids 0 would make the caller skip People-tab population for ids >= cluster_count, orphaning all n faces.
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

    let mut cannot_neighbors = vec![HashSet::new(); n];
    for &(a, b) in cannot_links {
        if a >= n || b >= n || a == b {
            continue;
        }
        cannot_neighbors[a].insert(b);
        cannot_neighbors[b].insert(a);
    }
    let constrained = cannot_neighbors.iter().any(|neighbors| !neighbors.is_empty());
    let mut component_forbidden = cannot_neighbors.clone();
    let mut uf = UnionFind::new(n);
    if mutual_knn {
        let mut directed: std::collections::HashSet<(usize, usize)> =
            std::collections::HashSet::new();
        let mut candidates: Vec<(f32, usize, usize)> = Vec::new();
        for i in 0..n {
            for hit in searcher(i) {
                if hit.idx == i || hit.idx >= n {
                    continue;
                }
                if hit.similarity < params.pass1_cosine {
                    continue;
                }
                directed.insert((i, hit.idx));
                candidates.push((hit.similarity, i, hit.idx));
            }
        }
        if constrained {
            candidates.sort_by(|a, b| {
                b.0.total_cmp(&a.0)
                    .then_with(|| a.1.min(a.2).cmp(&b.1.min(b.2)))
                    .then_with(|| a.1.max(a.2).cmp(&b.1.max(b.2)))
            });
        }
        for (_, i, j) in candidates {
            if directed.contains(&(j, i)) {
                if constrained {
                    constrained_union(&mut uf, &mut component_forbidden, i, j);
                } else {
                    uf.union(i, j);
                }
            }
        }
    } else if constrained {
        let mut candidates = Vec::new();
        for i in 0..n {
            for hit in searcher(i) {
                if hit.idx == i || hit.idx >= n || hit.similarity < params.pass1_cosine {
                    continue;
                }
                candidates.push((hit.similarity, i, hit.idx));
            }
        }
        candidates.sort_by(|a, b| {
            b.0.total_cmp(&a.0)
                .then_with(|| a.1.min(a.2).cmp(&b.1.min(b.2)))
                .then_with(|| a.1.max(a.2).cmp(&b.1.max(b.2)))
        });
        for (_, i, j) in candidates {
            constrained_union(&mut uf, &mut component_forbidden, i, j);
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
    // Sorted-key iteration keeps cluster-ID assignment deterministic; HashMap order would leak into People-tab cluster numbers and change IDs on every re-scan.
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

        // Running unnormalized sum per core: an outlier add is O(dim) instead of recomputing the centroid over all members.
    let mut core_sums: Vec<Vec<f32>> = cores
        .iter()
        .map(|c| centroid_sum(c, embeddings, dim))
        .collect();
    let mut core_centroids: Vec<Vec<f32>> =
        core_sums.iter().map(|s| normalize_sum(s, dim)).collect();
    let mut core_of_face = vec![None; n];
    for (core_index, members) in cores.iter().enumerate() {
        for &member in members {
            core_of_face[member] = Some(core_index);
        }
    }
    let mut outliers_assigned = 0;
    let mut outliers_as_singletons = 0;
    // Snapshot the Pass-1 core count: an outlier may only join a genuine size>=2 core, never a singleton appended below, which would grow the inner scan to O(M²) at scale and let an outlier merge order-dependently into a prior outlier's singleton. Centroid updates write in place at index < core_n, so real cores are still re-scanned with fresh values.
    let core_n = core_centroids.len();
    for outlier in outliers {
        let v = &embeddings[outlier];
        let mut c1_idx: isize = -1;
        let mut c2_idx: isize = -1;
        let mut c1_sim: f32 = -2.0;
        let mut c2_sim: f32 = -2.0;
        for (idx, centroid) in core_centroids.iter().take(core_n).enumerate() {
            if cannot_neighbors[outlier]
                .iter()
                .any(|&blocked| core_of_face[blocked] == Some(idx))
            {
                continue;
            }
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
            core_of_face[outlier] = Some(target);
            outliers_assigned += 1;
        } else {
            let singleton_index = cores.len();
            cores.push(vec![outlier]);
            core_sums.push(v.to_vec());
            core_centroids.push(v.to_vec());
            core_of_face[outlier] = Some(singleton_index);
            outliers_as_singletons += 1;
        }
    }

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

fn centroid_normalized(indices: &[usize], embeddings: &[&[f32]], dim: usize) -> Vec<f32> {
    let sum = centroid_sum(indices, embeddings, dim);
    normalize_sum(&sum, dim)
}

fn centroid_sum(indices: &[usize], embeddings: &[&[f32]], dim: usize) -> Vec<f32> {
    let mut sum = vec![0f32; dim];
    for &i in indices {
        let v = &embeddings[i];
        // Callers guarantee uniform dim (the face loader filters to the modal dimension; restructure passes one CLIP space); `.min` is a release-safe backstop against out-of-bounds on a stray short vector.
        debug_assert_eq!(v.len(), dim, "centroid_sum: embedding dim mismatch");
        for d in 0..dim.min(v.len()) {
            sum[d] += v[d];
        }
    }
    sum
}

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

#[inline]
fn dot(a: &[f32], b: &[f32]) -> f32 {
    crate::util::vector_math::dot(a, b)
}

#[allow(clippy::similar_names)]
fn validate_and_split(
    cluster: Vec<usize>,
    embeddings: &[&[f32]],
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

    let mean_ok = mean >= params.required_mean_cosine(cluster.len());
    let var_ok = variance <= params.pass3_variance_threshold;
    if mean_ok && var_ok {
        return vec![cluster];
    }
    if splits_remaining == 0 {
        return vec![cluster];
    }

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
    let mut cent_a = embeddings[seed_a_idx].to_vec();
    let mut cent_b = embeddings[seed_b_idx].to_vec();
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

fn constrained_union(
    uf: &mut UnionFind,
    forbidden: &mut [HashSet<usize>],
    a: usize,
    b: usize,
) -> bool {
    let ra = uf.find(a);
    let rb = uf.find(b);
    if ra == rb || forbidden[ra].contains(&rb) || forbidden[rb].contains(&ra) {
        return false;
    }

    let (keep, drop) = match forbidden[ra].len().cmp(&forbidden[rb].len()) {
        std::cmp::Ordering::Greater => (ra, rb),
        std::cmp::Ordering::Less => (rb, ra),
        std::cmp::Ordering::Equal if ra <= rb => (ra, rb),
        std::cmp::Ordering::Equal => (rb, ra),
    };
    uf.parent[drop] = keep;
    let moved = std::mem::take(&mut forbidden[drop]);
    for neighbor in moved {
        let neighbor_root = uf.find(neighbor);
        if neighbor_root == keep {
            continue;
        }
        forbidden[neighbor_root].remove(&drop);
        forbidden[neighbor_root].insert(keep);
        forbidden[keep].insert(neighbor_root);
    }
    forbidden[keep].remove(&drop);
    forbidden[keep].remove(&keep);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(v: Vec<f32>) -> Vec<f32> {
        let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.into_iter().map(|x| x / n).collect()
    }

    fn no_split_params() -> Hyperparameters {
        Hyperparameters {
            pass1_cosine: 0.50,
            pass2_cosine: 0.45,
            pass2_margin: 0.0,
            pass3_variance_threshold: f32::MAX,
            pass3_min_mean_cosine: 0.0,
            pass3_max_splits: 1,
            k_nn: 32,
            oversized_faces: usize::MAX,
            oversized_min_mean_cosine: 0.0,
        }
    }

    fn graph_searcher(
        graph: Vec<Vec<(usize, f32)>>,
    ) -> impl FnMut(usize) -> Vec<Neighbor> {
        move |i| {
            graph[i]
                .iter()
                .map(|&(idx, similarity)| Neighbor { idx, similarity })
                .collect()
        }
    }

    /// Faces all `tilt_degrees` away from one axis and spread evenly around it: neighbours chain into one ring, yet every
    /// face sits at the same cosine from the centroid, so the spread test never fires.
    fn ring(count: usize, tilt_degrees: f32) -> Vec<Vec<f32>> {
        let tilt = tilt_degrees.to_radians();
        (0..count)
            .map(|i| {
                let around = std::f32::consts::TAU * i as f32 / count as f32;
                unit(vec![tilt.cos(), tilt.sin() * around.cos(), tilt.sin() * around.sin()])
            })
            .collect()
    }

    fn ring_clusters(faces: &[Vec<f32>]) -> usize {
        let searcher = |i: usize| {
            let mut hits: Vec<Neighbor> = (0..faces.len())
                .filter(|&j| j != i)
                .map(|j| Neighbor { idx: j, similarity: dot(&faces[i], &faces[j]) })
                .collect();
            hits.sort_by(|a, b| b.similarity.total_cmp(&a.similarity));
            hits.truncate(17);
            hits
        };
        cluster_with_options(faces, searcher, Hyperparameters::default(), &[], true).cluster_count
    }

    #[test]
    fn a_large_chain_of_look_alike_faces_is_split_while_a_tight_one_of_the_same_size_stays_whole() {
        assert!(ring_clusters(&ring(400, 45.0)) > 1, "a 400-face ring 45 degrees off its axis must split");
        assert_eq!(ring_clusters(&ring(400, 25.0)), 1, "a tight 400-face identity stays whole");
    }

    #[test]
    fn the_same_chain_below_the_oversized_size_is_left_alone() {
        assert_eq!(ring_clusters(&ring(200, 45.0)), 1);
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

    /// All-identical embeddings (same person photographed N times) must land in one cluster, guarding a regression that splits a singular identity into per-instance clusters.
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

    /// Orthogonal unit vectors (similarity 0) must each land in their own cluster.
    #[test]
    fn orthogonal_embeddings_each_in_own_cluster() {
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
        let unique_ids: std::collections::HashSet<_> =
            result.cluster_ids.iter().copied().collect();
        assert_eq!(
            unique_ids.len(),
            5,
            "orthogonal embeddings must produce 5 distinct cluster IDs, got {:?}",
            result.cluster_ids
        );
    }

    #[test]
    fn cannot_link_pair_never_clusters() {
        let embeddings = vec![unit(vec![1.0, 0.0]), unit(vec![1.0, 0.0])];
        let graph = vec![vec![(1, 1.0)], vec![(0, 1.0)]];

        let result = cluster_with_cannot_links(
            &embeddings,
            graph_searcher(graph),
            no_split_params(),
            &[(0, 1)],
        );

        assert_ne!(result.cluster_ids[0], result.cluster_ids[1]);
    }

    #[test]
    fn cannot_link_blocks_transitive_bridge() {
        let embeddings = vec![
            unit(vec![1.0, 0.0]),
            unit(vec![0.9, 0.1]),
            unit(vec![0.8, 0.2]),
        ];
        let graph = vec![
            vec![(1, 0.90)],
            vec![(0, 0.90), (2, 0.80)],
            vec![(1, 0.80)],
        ];

        let result = cluster_with_cannot_links(
            &embeddings,
            graph_searcher(graph),
            no_split_params(),
            &[(0, 2)],
        );

        assert_eq!(result.cluster_ids[0], result.cluster_ids[1]);
        assert_ne!(result.cluster_ids[0], result.cluster_ids[2]);
    }

    #[test]
    fn strongest_allowed_edge_wins_deterministically() {
        let embeddings = vec![
            unit(vec![1.0, 0.0]),
            unit(vec![0.9, 0.1]),
            unit(vec![0.8, 0.2]),
        ];
        let graph = vec![
            vec![(1, 0.80)],
            vec![(0, 0.80), (2, 0.90)],
            vec![(1, 0.90)],
        ];

        let result = cluster_with_cannot_links(
            &embeddings,
            graph_searcher(graph),
            no_split_params(),
            &[(0, 2)],
        );

        assert_ne!(result.cluster_ids[0], result.cluster_ids[1]);
        assert_eq!(result.cluster_ids[1], result.cluster_ids[2]);
    }

    #[test]
    fn pass2_rejects_outlier_from_conflicting_core() {
        let embeddings = vec![
            unit(vec![1.0, 0.0]),
            unit(vec![1.0, 0.0]),
            unit(vec![0.8, 0.6]),
        ];
        let graph = vec![
            vec![(1, 1.0), (2, 0.80)],
            vec![(0, 1.0), (2, 0.80)],
            vec![(0, 0.80), (1, 0.80)],
        ];
        let mut params = no_split_params();
        params.pass1_cosine = 0.95;

        let result = cluster_with_cannot_links(
            &embeddings,
            graph_searcher(graph),
            params,
            &[(0, 2)],
        );

        assert_eq!(result.cluster_ids[0], result.cluster_ids[1]);
        assert_ne!(result.cluster_ids[0], result.cluster_ids[2]);
        assert_eq!(result.outliers_assigned, 0);
    }

    #[test]
    fn empty_constraints_preserve_unconstrained_behavior() {
        let embeddings = vec![
            unit(vec![1.0, 0.0]),
            unit(vec![0.99, 0.01]),
            unit(vec![0.0, 1.0]),
        ];
        let graph = vec![
            vec![(1, 0.99), (2, 0.0)],
            vec![(0, 0.99), (2, 0.01)],
            vec![(1, 0.01), (0, 0.0)],
        ];

        let wrapper = cluster(
            &embeddings,
            graph_searcher(graph.clone()),
            no_split_params(),
        );
        let explicit = cluster_with_cannot_links(
            &embeddings,
            graph_searcher(graph),
            no_split_params(),
            &[],
        );

        assert_eq!(wrapper.cluster_ids, explicit.cluster_ids);
        assert_eq!(wrapper.cluster_count, explicit.cluster_count);
        assert_eq!(wrapper.core_count, explicit.core_count);
        assert_eq!(wrapper.outliers_assigned, explicit.outliers_assigned);
        assert_eq!(
            wrapper.outliers_as_singletons,
            explicit.outliers_as_singletons
        );
        assert_eq!(wrapper.splits_applied, explicit.splits_applied);
    }
}
