//! Pure-Rust approximate-nearest-neighbor index (`instant-distance` HNSW) replacing brute-force cosine past ~10 k
//! vectors (face clustering, CLIP/BGE search). Vectors must be **L2-normalized**: squared Euclidean distance is then
//! monotonic in `1 − cosine`. Approximate retrieval can still miss true neighbors.
// Wired into `pipeline::face_clustering` above ~5 k faces; below that the brute-force path wins because HNSW build
// overhead exceeds the O(n²) saving.

use instant_distance::{Builder, HnswMap, Point, Search};
use std::sync::OnceLock;

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct Embedding(pub(crate) Vec<f32>);

impl Point for Embedding {
    fn distance(&self, other: &Self) -> f32 {
        #[cfg(test)]
        if self.0.len() == tests::COUNTED_DIM {
            tests::EVALUATIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        // For unit vectors ||a−b||² = 2(1 − a·b), so ordering matches cosine; no sqrt needed.
        crate::util::vector_math::squared_l2(&self.0, &other.0)
    }
}

pub(crate) fn build<V: Clone + Send + Sync>(points: Vec<(Vec<f32>, V)>) -> HnswMap<Embedding, V> {
    build_with_search_limit(points, 100)
}

pub(crate) fn build_with_search_limit<V: Clone + Send + Sync>(points: Vec<(Vec<f32>, V)>, limit: usize) -> HnswMap<Embedding, V> {
    let (embeds, values): (Vec<_>, Vec<_>) =
        points.into_iter().map(|(e, v)| (Embedding(e), v)).unzip();
    // Fixed seed: instant-distance seeds its layer-shuffle RNG from `rand::random()`, so HNSW topology, and with it the
    // kNN sets face clustering derives cluster IDs and inherited People names from, would differ run-to-run. A fixed
    // seed does not order the parallel insertions, so construction is kept on one worker; queries retain their caller's
    // pool.
    static BUILDER_POOL: OnceLock<Option<rayon::ThreadPool>> = OnceLock::new();
    let build = || Builder::default().ef_search(limit).seed(0xF11E_1D00).build(embeds, values);
    match BUILDER_POOL.get_or_init(|| rayon::ThreadPoolBuilder::new().num_threads(1).build().ok()) {
        Some(pool) => pool.install(build),
        None => {
            tracing::warn!("HNSW construction worker unavailable; building on the caller's pool");
            build()
        }
    }
}

/// Search only, built on every core. Scheduling changes which near-equal neighbours a node keeps, so two builds can
/// differ; never use it where results must repeat (clustering).
pub(crate) fn build_search_index<V: Clone + Send + Sync>(points: Vec<(Vec<f32>, V)>, limit: usize) -> HnswMap<Embedding, V> {
    let (embeds, values): (Vec<_>, Vec<_>) = points.into_iter().map(|(e, v)| (Embedding(e), v)).unzip();
    Builder::default().ef_search(limit).seed(0xF11E_1D00).build(embeds, values)
}

/// A fresh `Search` re-reserves and zero-fills an n-byte visited set on EVERY query (instant-distance 0.6),
/// reintroducing the O(n²) term HNSW exists to remove; reusing the scratch makes that a no-op after the first query.
/// Sequential use.
#[derive(Default)]
pub(crate) struct Searcher {
    scratch: Search,
    query_buf: Embedding,
}

impl Searcher {
    /// Ascending squared-L2 distance; for unit vectors cosine = `1 − d/2`.
    pub(crate) fn top_k<V: Clone>(
        &mut self,
        index: &HnswMap<Embedding, V>,
        query: &[f32],
        k: usize,
    ) -> Vec<(V, f32)> {
        self.query_buf.0.clear();
        self.query_buf.0.extend_from_slice(query);
        index
            .search(&self.query_buf, &mut self.scratch)
            .take(k)
            .map(|item| (item.value.clone(), item.distance))
            .collect()
    }
}

/// byte-identical files give identical vectors, and instant-distance's graph degenerates on thousands of tied points
/// (5,021 vectors, 3,487 distinct: 37 s vs under a second). Identical vectors share one graph node and are expanded at
/// query time.
pub(crate) struct DedupIndex {
    members: Vec<Vec<usize>>,
    group_of: Vec<usize>,
    index: HnswMap<Embedding, usize>,
}

impl DedupIndex {
    pub(crate) fn build(vectors: &[Vec<f32>]) -> Self {
        let mut ids: std::collections::HashMap<Vec<u32>, usize> = std::collections::HashMap::new();
        let mut members: Vec<Vec<usize>> = Vec::new();
        let mut group_of = Vec::with_capacity(vectors.len());
        let mut points = Vec::new();
        for (i, v) in vectors.iter().enumerate() {
            let key: Vec<u32> = v.iter().map(|x| x.to_bits()).collect();
            let g = *ids.entry(key).or_insert_with(|| {
                members.push(Vec::new());
                points.push((v.clone(), members.len() - 1));
                members.len() - 1
            });
            members[g].push(i);
            group_of.push(g);
        }
        Self { members, group_of, index: build(points) }
    }

    /// `(member, squared L2)` excluding `i`: identical ones first, then whole groups by ascending distance until at
    /// least `k` (whole groups, so ties stay the caller's to break).
    pub(crate) fn neighbours(&self, searcher: &mut Searcher, vectors: &[Vec<f32>], i: usize, k: usize) -> Vec<(usize, f32)> {
        let own = self.group_of[i];
        let mut out: Vec<(usize, f32)> = self.members[own].iter().filter(|&&j| j != i).map(|&j| (j, 0.0)).collect();
        if out.len() >= k {
            return out;
        }
        for (g, d) in searcher.top_k(&self.index, &vectors[i], k + 1) {
            if g == own {
                continue;
            }
            out.extend(self.members[g].iter().map(|&j| (j, d)));
            if out.len() >= k {
                break;
            }
        }
        out
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    pub(crate) const COUNTED_DIM: usize = 37;
    pub(crate) static EVALUATIONS: AtomicU64 = AtomicU64::new(0);
    pub(crate) static COUNTING: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn norm(mut v: Vec<f32>) -> Vec<f32> {
        let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-8);
        for x in &mut v {
            *x /= n;
        }
        v
    }

    fn random_unit(state: &mut u64) -> Vec<f32> {
        norm((0..COUNTED_DIM)
            .map(|_| {
                *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                ((*state >> 33) as f32 / (1u64 << 31) as f32) - 0.5
            })
            .collect())
    }

    fn evaluations_for<R>(work: impl FnOnce() -> R) -> u64 {
        EVALUATIONS.store(0, std::sync::atomic::Ordering::Relaxed);
        let _ = work();
        EVALUATIONS.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Counts distance evaluations instead of time so the result does not depend on machine load.
    #[test]
    fn copies_of_a_vector_add_no_graph_work() {
        let _counting = COUNTING.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = 11u64;
        let distinct: Vec<Vec<f32>> = (0..3_500).map(|_| random_unit(&mut state)).collect();
        let mut with_copies = distinct.clone();
        for i in 0..1_700 {
            with_copies.push(distinct[i % 40].clone());
        }
        let plain = |vectors: &[Vec<f32>]| evaluations_for(|| build(vectors.iter().cloned().enumerate().map(|(i, v)| (v, i)).collect()));
        let base = plain(&distinct);
        let naive = plain(&with_copies);
        let deduped = evaluations_for(|| DedupIndex::build(&with_copies));
        eprintln!("EVALS distinct={base} naive-with-copies={naive} deduped-with-copies={deduped}");
        assert!(deduped <= base + base / 20, "copies cost {deduped} evaluations against {base} for the distinct vectors alone");
        let mut searcher = Searcher::default();
        let index = DedupIndex::build(&with_copies);
        let near = index.neighbours(&mut searcher, &with_copies, 3_500, 10);
        assert!(near.len() >= 10 && near.iter().filter(|(_, d)| *d == 0.0).count() >= 10, "a copy's nearest are its identical members");
    }

    #[test]
    fn nearest_neighbor_recovers_itself() {
        let points = vec![
            (norm(vec![1.0, 0.0, 0.0]), "x"),
            (norm(vec![0.0, 1.0, 0.0]), "y"),
            (norm(vec![0.0, 0.0, 1.0]), "z"),
        ];
        let idx = build(points);
        let hits = Searcher::default().top_k(&idx, &norm(vec![1.0, 0.0, 0.0]), 1);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, "x");
        assert!(hits[0].1 < 1e-3, "self-distance should be ~0, got {}", hits[0].1);
    }

    #[test]
    fn ranking_matches_cosine_similarity() {
        let points = vec![
            (norm(vec![1.0, 0.0]), "x"),
            (norm(vec![1.0, 1.0]), "xy"),
            (norm(vec![0.0, 1.0]), "y"),
        ];
        let idx = build(points);
        let hits = Searcher::default().top_k(&idx, &norm(vec![1.0, 0.0]), 3);
        let labels: Vec<&str> = hits.iter().map(|h| h.0).collect();
        assert_eq!(labels, vec!["x", "xy", "y"]);
    }

    #[test]
    fn empty_index_search_yields_no_hits() {
        let idx: HnswMap<Embedding, &str> = build(Vec::<(Vec<f32>, &str)>::new());
        let hits = Searcher::default().top_k(&idx, &norm(vec![1.0, 0.0]), 5);
        assert!(hits.is_empty());
    }

    #[test]
    fn build_is_deterministic_across_runs() {
        // Guards the fixed seed: near-collinear vectors make the neighbour set seed-sensitive, and two builds must
        // return identical orderings.
        let mk = || -> Vec<(Vec<f32>, usize)> {
            (0..256usize)
                .map(|i| {
                    let a = (i as f32) * 0.013;
                    (norm(vec![a.cos(), a.sin(), (a * 0.5).cos(), (a * 0.5).sin()]), i)
                })
                .collect()
        };
        let idx_a = build(mk());
        let q = norm(vec![1.0, 0.05, 0.9, 0.1]);
        let ha: Vec<usize> = Searcher::default().top_k(&idx_a, &q, 16).into_iter().map(|h| h.0).collect();
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let mk = &mk;
                let q = &q;
                let ha = &ha;
                scope.spawn(move || {
                    let idx_b = build(mk());
                    let hb: Vec<usize> = Searcher::default().top_k(&idx_b, q, 16).into_iter().map(|h| h.0).collect();
                    assert_eq!(*ha, hb, "HNSW kNN ordering must be deterministic across builds");
                });
            }
        });
    }
}
