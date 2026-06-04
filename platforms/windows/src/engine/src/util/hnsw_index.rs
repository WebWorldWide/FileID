//! Pure-Rust approximate-nearest-neighbor index over L2-normalized float
//! embeddings (`instant-distance` HNSW). Replaces brute-force cosine when the
//! corpus grows past ~10 k vectors (face-clustering, CLIP/BGE semantic
//! search). No C/C++ build dependency.
//!
//! Vectors are expected to be **L2-normalized** before indexing; the squared
//! Euclidean distance instant-distance computes is then monotonic in
//! `(1 − cosine_similarity)`, so the nearest-neighbor ordering matches a
//! true cosine-similarity ranking exactly.
// Wired into `pipeline::face_clustering` above ~5 k faces. Below threshold
// the brute-force cosine path wins because HNSW build overhead exceeds the
// O(n²) saving at small N.

use instant_distance::{Builder, HnswMap, Point, Search};

/// Owned f32 embedding; `Point` implements squared-L2 over the vector.
#[derive(Clone, Debug, Default)]
pub(crate) struct Embedding(pub(crate) Vec<f32>);

impl Point for Embedding {
    fn distance(&self, other: &Self) -> f32 {
        // Squared L2. For unit vectors ||a−b||² = 2(1 − a·b), so the
        // ordering matches cosine similarity (no sqrt needed for ranking).
        self.0
            .iter()
            .zip(other.0.iter())
            .map(|(a, b)| (a - b).powi(2))
            .sum()
    }
}

/// Build an HNSW index from `(embedding, value)` pairs. `value` is whatever
/// the caller wants to recover at search time (e.g. a `file_id` or `face_id`).
pub(crate) fn build<V: Clone>(points: Vec<(Vec<f32>, V)>) -> HnswMap<Embedding, V> {
    let (embeds, values): (Vec<_>, Vec<_>) =
        points.into_iter().map(|(e, v)| (Embedding(e), v)).unzip();
    // Fixed seed: instant-distance's Builder::default() seeds its layer-shuffle
    // RNG from `rand::random()`, so the HNSW topology — and thus the approximate
    // kNN neighbour sets — would differ run-to-run. Face clustering derives
    // cluster IDs and inherited People names from those neighbours, so an
    // entropy seed makes identities hop on every re-cluster. Pin it. (audit E0)
    Builder::default().seed(0xF11E_1D00).build(embeds, values)
}

/// Reusable kNN searcher: owns the `instant-distance` scratch buffer so a
/// per-item sweep amortizes its internal allocations. A fresh `Search`
/// re-`reserve_capacity`s and zero-fills an n-byte visited set on EVERY query
/// (instant-distance 0.6), reintroducing an O(n²) term over a full clustering
/// pass — exactly the quadratic the HNSW path exists to remove. Reusing it
/// across the sweep makes that a no-op after the first query. Sequential use.
#[derive(Default)]
pub(crate) struct Searcher {
    scratch: Search,
    // Reused across queries so a per-item sweep doesn't heap-allocate a fresh
    // query Vec each call (search takes &Embedding, which owns its Vec).
    query_buf: Embedding,
}

impl Searcher {
    /// Top-k nearest neighbors of `query`, ascending distance (most similar
    /// first). Distance is squared-L2; for unit-normalized vectors map to cosine
    /// via `1 − d/2`. Reuses the scratch buffer across calls (see the type doc).
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

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(mut v: Vec<f32>) -> Vec<f32> {
        let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-8);
        for x in &mut v {
            *x /= n;
        }
        v
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
        // Self-distance is ~0 (squared L2 of a unit vector with itself).
        assert!(hits[0].1 < 1e-3, "self-distance should be ~0, got {}", hits[0].1);
    }

    #[test]
    fn ranking_matches_cosine_similarity() {
        // Query "x" is closest to itself, then to the "xy" mix, then to "y".
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
        // Guards the fixed seed (audit E0): two builds of the same points must
        // return byte-identical kNN orderings, else face clustering is
        // nondeterministic on large libraries. A spread of near-collinear
        // vectors makes the approximate neighbour set seed-sensitive.
        let mk = || -> Vec<(Vec<f32>, usize)> {
            (0..256usize)
                .map(|i| {
                    let a = (i as f32) * 0.013;
                    (norm(vec![a.cos(), a.sin(), (a * 0.5).cos(), (a * 0.5).sin()]), i)
                })
                .collect()
        };
        let idx_a = build(mk());
        let idx_b = build(mk());
        let q = norm(vec![1.0, 0.05, 0.9, 0.1]);
        let ha: Vec<usize> = Searcher::default().top_k(&idx_a, &q, 16).into_iter().map(|h| h.0).collect();
        let hb: Vec<usize> = Searcher::default().top_k(&idx_b, &q, 16).into_iter().map(|h| h.0).collect();
        assert_eq!(ha, hb, "HNSW kNN ordering must be deterministic across builds");
    }
}
