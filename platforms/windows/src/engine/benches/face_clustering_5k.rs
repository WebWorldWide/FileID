//! V15.3 N3 — Bench face clustering on a synthetic 5K-face corpus.
//!
//! Mirrors the macOS reference workload. The brute-force O(n²) similarity
//! pass is the hot loop; this bench tracks its scaling as we move from
//! the current implementation to (eventually) HNSW.
//! Use `cargo bench -p fileid-engine --bench face_clustering_5k`.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use fileid_engine::pipeline::face_clustering::{cluster, FaceRow};

/// L2-normalize a vector in place. ArcFace embeddings are always
/// unit-length on the way out of the model; reproducing that invariant
/// here ensures the cosine similarities in the clusterer behave
/// realistically.
fn normalize(v: &mut [f32]) {
    let mag: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if mag > 0.0 {
        for x in v.iter_mut() {
            *x /= mag;
        }
    }
}

/// Build `n` synthetic 512-d embeddings clustered around `n / cluster_size`
/// hidden anchors with low-amplitude noise. Deterministic via a simple
/// LCG so bench runs are comparable across machines without pulling rand.
fn synthetic_faces(n: usize, cluster_size: usize) -> Vec<FaceRow> {
    const DIM: usize = 512;
    let mut seed: u64 = 0x9E3779B97F4A7C15;
    let mut lcg = || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((seed >> 33) as u32 as f32 / u32::MAX as f32) - 0.5
    };

    let n_anchors = (n / cluster_size).max(1);
    let anchors: Vec<Vec<f32>> = (0..n_anchors)
        .map(|_| {
            let mut a: Vec<f32> = (0..DIM).map(|_| lcg()).collect();
            normalize(&mut a);
            a
        })
        .collect();

    (0..n)
        .map(|i| {
            let a = &anchors[i % n_anchors];
            let mut e: Vec<f32> = a.iter().map(|&x| x + lcg() * 0.05).collect();
            normalize(&mut e);
            FaceRow {
                face_id: i as i64 + 1,
                file_id: (i as i64 / 5) + 1, // ~5 faces per file
                embedding: e,
                quality: 0.9,
            }
        })
        .collect()
}

fn bench_cluster_5k(c: &mut Criterion) {
    let faces = synthetic_faces(5000, 50);
    // Pre-size the group + set sample size LOW — clustering 5K faces is
    // a multi-second operation; criterion's default 100 samples would
    // take minutes. 10 samples is enough for a stable median.
    let mut group = c.benchmark_group("face_clustering");
    group.sample_size(10);
    group.bench_function("cluster_5000_faces", |b| {
        b.iter(|| {
            let (assigns, anchors) = cluster(black_box(&faces));
            black_box((assigns, anchors));
        });
    });
    group.finish();
}

criterion_group!(benches, bench_cluster_5k);
criterion_main!(benches);
