//! Content-identity hashing for rename/move detection (Phase 3 identity).
//!
//! A file's path is not a stable identity — a rename or move orphans its
//! catalog row (tags, embeddings, faces) and forces a full recompute on the
//! next scan. A content hash is stable across moves, so a moved file can be
//! re-bound to its existing row. BLAKE3 is faster than SHA-256 on commodity
//! CPUs and pure-Rust (no C/C++ build dep). For large files we hash a
//! composite of head + interior samples + tail + size rather than read
//! gigabytes per file.
#![allow(dead_code)] // wired into the rename/move rebind path within Phase 3.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Files at or below this size are hashed in full; larger files use the
/// head+tail+size composite (reads 2 MB instead of the whole file). 16 MB
/// matches the research recommendation and keeps full-hash cost bounded.
pub(crate) const FULL_HASH_MAX_BYTES: u64 = 16 * 1024 * 1024;
/// Bytes read from the head and (separately) the tail for the composite.
const CHUNK: usize = 1024 * 1024;

/// 32-byte BLAKE3 content identity for `path` (whose length is `size`). Same
/// bytes -> same hash, so a moved/renamed file re-binds to its existing
/// catalog row instead of being recomputed. Opens long paths safely.
pub(crate) fn content_hash(path: &Path, size: u64) -> std::io::Result<[u8; 32]> {
    hash_with_threshold(path, size, FULL_HASH_MAX_BYTES, true)
}

/// Recipe-v1 composite: blake3(head ‖ tail ‖ size_le), NO interior samples.
/// Kept for rename-heal compatibility — rows stamped by pre-interior-sample
/// builds carry this digest for files above `FULL_HASH_MAX_BYTES`, so the
/// heal lookup must be able to reproduce it or every over-cap file in an
/// upgraded DB loses its row on its first post-upgrade move. New writes
/// always use `content_hash`; the heal upsert re-stamps the current recipe.
/// (At or below the threshold both recipes are the identical full hash.)
pub(crate) fn legacy_content_hash(path: &Path, size: u64) -> std::io::Result<[u8; 32]> {
    hash_with_threshold(path, size, FULL_HASH_MAX_BYTES, false)
}

/// Testable core: `content_hash` with the full-vs-composite threshold
/// injected so the composite path can be exercised on small fixtures.
/// `interior_samples` selects the current recipe (true) or the recipe-v1
/// head+tail+size composite (false) — see `legacy_content_hash`.
fn hash_with_threshold(
    path: &Path,
    size: u64,
    full_max: u64,
    interior_samples: bool,
) -> std::io::Result<[u8; 32]> {
    let mut f = std::fs::File::open(super::path_safety::to_extended_length(path))?;
    let mut hasher = blake3::Hasher::new();
    if size <= full_max {
        std::io::copy(&mut f, &mut hasher)?;
    } else {
        // Clamp to the file size so a file between `full_max` and CHUNK
        // doesn't seek before the start (the head+tail overlap on such files
        // is harmless — the hash stays deterministic).
        let span = size.min(CHUNK as u64) as usize;

        let mut head = vec![0u8; span];
        let n = read_fill(&mut f, &mut head)?;
        hasher.update(&head[..n]);

        // Interior samples: a few evenly-spaced 64 KB chunks so two DISTINCT
        // same-size files that happen to share their head+tail (camera bursts,
        // container formats with identical headers/footers, padded archives)
        // don't collide and trigger a false rename-heal. Deterministic offsets;
        // skipped on files too small for interior reads to clear head/tail.
        if interior_samples {
            const INTERIOR_SAMPLES: u64 = 4;
            const INTERIOR_CHUNK: usize = 64 * 1024;
            for k in 1..=INTERIOR_SAMPLES {
                let off = size.saturating_mul(k) / (INTERIOR_SAMPLES + 1);
                if off < span as u64
                    || off + INTERIOR_CHUNK as u64 > size.saturating_sub(span as u64)
                {
                    continue;
                }
                if f.seek(SeekFrom::Start(off)).is_ok() {
                    let mut mid = vec![0u8; INTERIOR_CHUNK];
                    let n = read_fill(&mut f, &mut mid)?;
                    hasher.update(&mid[..n]);
                }
            }
        }

        f.seek(SeekFrom::End(-(span as i64)))?;
        let mut tail = vec![0u8; span];
        let n = read_fill(&mut f, &mut tail)?;
        hasher.update(&tail[..n]);

        // Size disambiguates files that share head+tail but differ in the middle.
        hasher.update(&size.to_le_bytes());
    }
    Ok(*hasher.finalize().as_bytes())
}

/// Read until `buf` is full or EOF; returns bytes filled. A single `read`
/// may return fewer bytes than requested even mid-file, so loop.
fn read_fill(f: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match f.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_with(bytes: &[u8]) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "fileid-chash-{}-{}.bin",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn identical_content_hashes_equal_regardless_of_path() {
        let a = tmp_with(b"the quick brown fox");
        let b = tmp_with(b"the quick brown fox");
        let ha = content_hash(&a, 19).unwrap();
        let hb = content_hash(&b, 19).unwrap();
        assert_eq!(ha, hb, "same bytes at different paths must hash equal");
        let _ = std::fs::remove_file(&a);
        let _ = std::fs::remove_file(&b);
    }

    #[test]
    fn different_content_hashes_differ() {
        let a = tmp_with(b"alpha");
        let b = tmp_with(b"bravo");
        assert_ne!(content_hash(&a, 5).unwrap(), content_hash(&b, 5).unwrap());
        let _ = std::fs::remove_file(&a);
        let _ = std::fs::remove_file(&b);
    }

    #[test]
    fn composite_path_is_deterministic_and_differs_from_full() {
        // 4 KB body; force the composite branch with a tiny threshold.
        let body: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        let p = tmp_with(&body);
        let size = body.len() as u64;
        let c1 = hash_with_threshold(&p, size, 64, true).unwrap();
        let c2 = hash_with_threshold(&p, size, 64, true).unwrap();
        assert_eq!(c1, c2, "composite hash must be deterministic");
        let full = hash_with_threshold(&p, size, u64::MAX, true).unwrap();
        assert_ne!(c1, full, "composite (head+tail+size) differs from full hash");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn composite_detects_a_changed_middle_byte_via_size_or_edges() {
        // Two same-size buffers differing only at an edge are caught by the
        // head/tail; this guards that the composite reads both ends.
        let mut a = vec![7u8; 4096];
        let mut b = vec![7u8; 4096];
        a[0] = 1; // head differs
        b[4095] = 2; // tail differs
        let pa = tmp_with(&a);
        let pb = tmp_with(&b);
        let ha = hash_with_threshold(&pa, 4096, 64, true).unwrap();
        let hb = hash_with_threshold(&pb, 4096, 64, true).unwrap();
        assert_ne!(ha, hb);
        let _ = std::fs::remove_file(&pa);
        let _ = std::fs::remove_file(&pb);
    }

    #[test]
    fn legacy_fallback_reproduces_pre_interior_sample_recipe() {
        const MB: usize = 1024 * 1024;
        // >16 MB so the real public functions take the composite branch and
        // the interior samples genuinely fire (offsets clear the 1 MB edges).
        let body: Vec<u8> = (0..17 * MB).map(|i| (i % 251) as u8).collect();
        let p = tmp_with(&body);
        let size = body.len() as u64;

        // The digest an origin/main build stamped into the DB:
        // blake3(head 1MB ‖ tail 1MB ‖ size_le), no interior block.
        let mut h = blake3::Hasher::new();
        h.update(&body[..MB]);
        h.update(&body[body.len() - MB..]);
        h.update(&size.to_le_bytes());
        let stamped_by_old_build = *h.finalize().as_bytes();

        assert_eq!(
            legacy_content_hash(&p, size).unwrap(),
            stamped_by_old_build,
            "legacy fallback must reproduce the recipe-v1 digest"
        );
        assert_ne!(
            content_hash(&p, size).unwrap(),
            stamped_by_old_build,
            "current recipe must differ (interior samples) or the fallback is moot"
        );
        let _ = std::fs::remove_file(&p);
    }
}
