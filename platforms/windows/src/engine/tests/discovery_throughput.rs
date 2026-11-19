//! Synthetic walk-throughput benchmark for decoupled discovery.
//!
//! Acceptance: a 10,000-file synthetic tree completes the walk phase in
//! under 5 seconds on a CI runner. On the user's NVMe Test Data corpus
//! the equivalent target is ≥2,000 files/sec sustained — this synthetic
//! benchmark on a hot tempdir caches everything, so it should comfortably
//! clear 10K files/sec.
//!
//! Marked `#[ignore]` so it doesn't slow normal `cargo test` runs. Run
//! manually:
//!
//!     cargo test --release --test discovery_throughput -- --ignored --nocapture
//!
//! The release profile matters: the dev profile's debug assertions inside
//! jwalk are slow enough to fail the timing budget on slower runners.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use fileid_engine::coordinator::ScanCoordinator;
use fileid_engine::pipeline::discovery::Discovery;

#[test]
#[ignore = "throughput benchmark; gated behind --ignored to keep CI fast"]
fn walk_10k_files_in_under_5_seconds() {
    let tmp = tempdir();
    let root = tmp.path().to_path_buf();

    let start_gen = Instant::now();
    for d in 0..100 {
        let dir = root.join(format!("d{d:03}"));
        fs::create_dir_all(&dir).expect("mkdir");
        for f in 0..100 {
            // One-byte content keeps the test cheap while still exercising the metadata() stat path; zero-byte files
            // are filtered by Discovery.
            fs::write(dir.join(format!("img_{f:03}.jpg")), b"x").expect("write");
        }
    }
    eprintln!("generated 10K files in {:?}", start_gen.elapsed());

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let (count, elapsed) = rt.block_on(async {
        let coord = ScanCoordinator::new();
        let disc = Discovery::new_with_skip_and_exclusions(root, coord, Arc::new(HashMap::new()), Arc::new(Vec::new()));
        let handle = disc.spawn();
        let mut rx = handle.rx;
        let count_ref = handle.count.clone();
        let started = Instant::now();
        let mut drained: u64 = 0;
        while rx.recv().await.is_some() {
            drained += 1;
        }
        let elapsed = started.elapsed();
        let fs_count = count_ref.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(drained, fs_count, "drained vs. count mismatch (decouple invariant broke)");
        (fs_count, elapsed)
    });

    let rate = count as f64 / elapsed.as_secs_f64();
    eprintln!(
        "[walk] {} files in {:.2}s = {:.0} files/sec",
        count, elapsed.as_secs_f64(), rate
    );

    assert_eq!(count, 10_000, "expected 10K files; got {count}");
    assert!(
        elapsed.as_secs_f64() < 5.0,
        "walk took {:.2}s — exceeds 5s budget (target ≥2000 files/sec; observed {:.0}/s)",
        elapsed.as_secs_f64(),
        rate,
    );
}

/// The counter advances at FS-walk speed, not consumer drain speed: with the consumer stalled it must still reach the
/// full file count within seconds.
#[test]
#[ignore = "throughput benchmark; gated behind --ignored to keep CI fast"]
fn count_advances_independently_of_consumer_drain() {
    let tmp = tempdir();
    let root = tmp.path().to_path_buf();
    // 5K files: below the 32K channel cap, so the walker never blocks on blocking_send.
    for d in 0..50 {
        let dir = root.join(format!("d{d:03}"));
        fs::create_dir_all(&dir).expect("mkdir");
        for f in 0..100 {
            fs::write(dir.join(format!("img_{f:03}.jpg")), b"x").expect("write");
        }
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let coord = ScanCoordinator::new();
        let disc = Discovery::new_with_skip_and_exclusions(root, coord, Arc::new(HashMap::new()), Arc::new(Vec::new()));
        let handle = disc.spawn();
        let count = handle.count.clone();
        let done = handle.done.clone();
        let _rx = handle.rx;
        for _ in 0..200 {
            if done.load(std::sync::atomic::Ordering::Acquire) { break; }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let n = count.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(n, 5_000, "discovery counter stalled — decouple invariant violated");
    });
}

// Minimal RAII tempdir (avoids the `tempfile` dependency).
struct TempDir(PathBuf);
impl TempDir { fn path(&self) -> &std::path::Path { &self.0 } }
impl Drop for TempDir { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }

fn tempdir() -> TempDir {
    let p = std::env::temp_dir().join(format!(
        "fileid-throughput-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    fs::create_dir_all(&p).expect("mkdir tempdir");
    TempDir(p)
}
