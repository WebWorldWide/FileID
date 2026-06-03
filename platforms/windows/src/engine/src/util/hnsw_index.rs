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
#[derive(Clone, Debug)]
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
    Builder::default().build(embeds, values)
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
        let q = Embedding(query.to_vec());
        index
            .search(&q, &mut self.scratch)
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
}
