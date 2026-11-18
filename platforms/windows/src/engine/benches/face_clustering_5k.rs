//! Face clustering on a synthetic 5K-face corpus; the O(n²) similarity pass is the hot loop. `cargo bench -p
//! fileid-engine --bench face_clustering_5k`.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use fileid_engine::pipeline::face_clustering::{cluster, FaceRow};

/// Model embeddings are unit-length; reproducing that keeps the clusterer's cosine similarities realistic.
fn normalize(v: &mut [f32]) {
    let mag: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if mag > 0.0 {
        for x in v.iter_mut() {
            *x /= mag;
        }
    }
}

/// Deterministic via a simple LCG so runs compare across machines without pulling rand.
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
                file_id: i as i64 + 1,
                content_hash: Some((i as u64).to_le_bytes().to_vec()),
                embedding: e,
                quality: 0.9,
            }
        })
        .collect()
}

fn bench_cluster_5k(c: &mut Criterion) {
    let faces = synthetic_faces(5000, 50);
    // Sample size LOW: clustering 5K faces takes seconds, so criterion's default 100 samples would take minutes; 10
    // gives a stable median.
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
