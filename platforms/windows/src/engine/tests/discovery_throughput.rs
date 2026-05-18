//! Synthetic walk-throughput benchmark for V15.9 Issue 1 acceptance.
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

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use fileid_engine::coordinator::ScanCoordinator;
use fileid_engine::pipeline::discovery::Discovery;

#[test]
#[ignore]
fn walk_10k_files_in_under_5_seconds() {
    let tmp = tempdir();
    let root = tmp.path().to_path_buf();

    // Lay out 100 subdirs × 100 files = 10,000 supported files.
    let start_gen = Instant::now();
    for d in 0..100 {
        let dir = root.join(format!("d{d:03}"));
        fs::create_dir_all(&dir).expect("mkdir");
        for f in 0..100 {
            // One-byte content keeps the test cheap on disk + still
            // exercises the metadata() stat path. Zero-byte would be
            // filtered out by Discovery.
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
        let disc = Discovery::new_with_skip(root, coord, Arc::new(HashSet::new()));
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
        // Both counts must agree — the FS-walk atomic counter is
        // incremented for every file that goes onto the channel.
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

/// Counter advances at FS-walk speed, not consumer drain speed. We stall
/// the consumer (don't drain the channel) and confirm the counter still
/// hits the full file count within a few seconds — proving the V15.9
/// decoupling actually decouples.
#[test]
#[ignore]
fn count_advances_independently_of_consumer_drain() {
    let tmp = tempdir();
    let root = tmp.path().to_path_buf();
    // 5K files: below the 32K channel cap, so the walker fills the
    // channel without ever blocking on blocking_send.
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
        let disc = Discovery::new_with_skip(root, coord, Arc::new(HashSet::new()));
        let handle = disc.spawn();
        let count = handle.count.clone();
        let done = handle.done.clone();
        let _rx = handle.rx; // hold but never drain — simulates a stalled tagger.
        // Poll for done; count must climb to 5K even though no consumer drains.
        for _ in 0..200 {
            if done.load(std::sync::atomic::Ordering::Acquire) { break; }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let n = count.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(n, 5_000, "discovery counter stalled — decouple invariant violated");
    });
}

// Minimal RAII tempdir helper (avoids the `tempfile` crate dep — already
// used inside src/ for the same reason).
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
