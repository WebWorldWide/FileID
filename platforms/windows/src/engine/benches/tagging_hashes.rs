//! V15.3 N3 — Bench hot-path image hashing primitives.
//!
//! Measures `compute_dhash` and `resize_rgb_nearest` against fixed
//! synthetic image buffers. These are pure CPU functions that run for
//! every image during scan, so any regression here hits scan throughput
//! linearly. Use `cargo bench -p fileid-engine --bench tagging_hashes`.

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use fileid_engine::pipeline::tagging::{compute_dhash, resize_rgb_nearest};

fn synthetic_rgb(size: usize) -> Vec<u8> {
    // Pseudo-random gradient. Deterministic + cheap — we're benching
    // the hash, not the RNG, so a fixed seed-equivalent pattern keeps
    // bench runs comparable across machines.
    let mut buf = Vec::with_capacity(size * size * 3);
    for y in 0..size {
        for x in 0..size {
            buf.push((x as u8).wrapping_mul(7).wrapping_add(y as u8));
            buf.push((y as u8).wrapping_mul(13).wrapping_add(x as u8));
            buf.push(((x ^ y) as u8).wrapping_mul(17));
        }
    }
    buf
}

fn bench_dhash(c: &mut Criterion) {
    let mut group = c.benchmark_group("dhash");
    for &size in &[64usize, 128, 256, 512] {
        let rgb = synthetic_rgb(size);
        group.throughput(Throughput::Bytes((size * size * 3) as u64));
        group.bench_function(format!("{size}x{size}"), |b| {
            b.iter(|| {
                let h = compute_dhash(black_box(&rgb), black_box(size), black_box(size));
                black_box(h);
            });
        });
    }
    group.finish();
}

fn bench_resize_nearest(c: &mut Criterion) {
    let mut group = c.benchmark_group("resize_rgb_nearest");
    let rgb = synthetic_rgb(256);
    group.throughput(Throughput::Bytes((256 * 256 * 3) as u64));
    group.bench_function("256x256_to_9x8", |b| {
        b.iter(|| {
            let out = resize_rgb_nearest(black_box(&rgb), 256, 256, 9, 8);
            black_box(out);
        });
    });
    group.finish();
}

criterion_group!(benches, bench_dhash, bench_resize_nearest);
criterion_main!(benches);
