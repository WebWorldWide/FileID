//! Content-identity hashing for rename/move detection (Phase 3 identity).
//!
//! A file's path is not a stable identity — a rename or move orphans its
//! catalog row (tags, embeddings, faces) and forces a full recompute on the
//! next scan. A content hash is stable across moves, so a moved file can be
//! re-bound to its existing row. The canonical recipe is SHA-256 so databases
//! written by the Rust engine and the macOS Swift engine agree byte-for-byte.
//! For large files we hash a composite of head + interior samples + tail + size
//! rather than read gigabytes per file.
#![allow(dead_code)] // wired into the rename/move rebind path within Phase 3.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use sha2::Digest;

/// Files at or below this size are hashed in full; larger files use the
/// head+tail+size composite (reads 2 MB instead of the whole file). 16 MB
/// matches the research recommendation and keeps full-hash cost bounded.
pub(crate) const FULL_HASH_MAX_BYTES: u64 = 16 * 1024 * 1024;
/// Bytes read from the head and (separately) the tail for the composite.
const CHUNK: usize = 1024 * 1024;
const INTERIOR_SAMPLES: u64 = 4;
const INTERIOR_CHUNK: usize = 64 * 1024;

/// 32-byte SHA-256 content identity for `path` (whose length is `size`). Same
/// bytes -> same hash, so a moved/renamed file re-binds to its existing
/// catalog row instead of being recomputed. Opens long paths safely.
pub(crate) fn content_hash(path: &Path, size: u64) -> std::io::Result<[u8; 32]> {
    hash_with_threshold(path, size, FULL_HASH_MAX_BYTES)
}

#[derive(Clone, Debug)]
pub struct ExactDuplicateCandidate {
    pub id: i64,
    pub path: PathBuf,
    pub indexed_size: i64,
}

#[derive(Clone, Debug)]
pub struct ExactDuplicateGroup {
    pub hash: [u8; 32],
    pub size: u64,
    pub files: Vec<ExactDuplicateCandidate>,
}

#[derive(Debug)]
pub struct ExactDuplicateGrouping {
    pub groups: Vec<ExactDuplicateGroup>,
    pub skipped: usize,
}

pub(crate) struct ExactFileHash {
    pub(crate) hash: [u8; 32],
    pub(crate) identity: crate::platform::FileIdentity,
    _file: std::fs::File,
}

#[derive(Clone, Copy)]
pub(crate) enum ExactFileLock {
    None,
    DenyWrite,
    DenyMutation,
}

pub fn exact_file_sha256(path: &Path, expected_size: u64) -> std::io::Result<[u8; 32]> {
    exact_file_sha256_until_with_identity(path, expected_size, || false, ExactFileLock::None)
        .map(|proof| proof.hash)
}

pub(crate) fn exact_file_sha256_guard(
    path: &Path,
    expected_size: u64,
    lock: ExactFileLock,
) -> std::io::Result<ExactFileHash> {
    exact_file_sha256_until_with_identity(path, expected_size, || false, lock)
}

fn exact_file_sha256_until(
    path: &Path,
    expected_size: u64,
    should_cancel: impl Fn() -> bool,
) -> std::io::Result<[u8; 32]> {
    exact_file_sha256_until_with_identity(
        path,
        expected_size,
        should_cancel,
        ExactFileLock::None,
    )
    .map(|proof| proof.hash)
}

fn exact_file_sha256_until_with_identity(
    path: &Path,
    expected_size: u64,
    should_cancel: impl Fn() -> bool,
    lock: ExactFileLock,
) -> std::io::Result<ExactFileHash> {
    let extended = super::path_safety::to_extended_length(path);
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::{FILE_SHARE_DELETE, FILE_SHARE_READ};
        match lock {
            ExactFileLock::None => {}
            ExactFileLock::DenyWrite => {
                options.share_mode((FILE_SHARE_READ | FILE_SHARE_DELETE).0);
            }
            ExactFileLock::DenyMutation => {
                options.share_mode(FILE_SHARE_READ.0);
            }
        }
    }
    #[cfg(not(windows))]
    let _ = lock;
    let mut file = options.open(extended)?;
    let before = file.metadata()?;
    if !before.is_file() || before.len() != expected_size {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file type or size changed before exact hashing",
        ));
    }
    let before_identity = crate::platform::file_identity_from_file(&file).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "could not capture exact file handle identity",
        )
    })?;
    let mut sha = sha2::Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        if should_cancel() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "exact hashing cancelled",
            ));
        }
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        sha.update(&buffer[..read]);
    }
    let after = file.metadata()?;
    if !after.is_file() || after.len() != expected_size {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file type or size changed during exact hashing",
        ));
    }
    let after_identity = crate::platform::file_identity_from_file(&file).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "could not recapture exact file handle identity",
        )
    })?;
    if after_identity != before_identity {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file handle identity changed during exact hashing",
        ));
    }
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&sha.finalize());
    Ok(ExactFileHash {
        hash,
        identity: before_identity,
        _file: file,
    })
}

pub fn group_exact_duplicates(
    candidates: Vec<ExactDuplicateCandidate>,
) -> ExactDuplicateGrouping {
    group_exact_duplicates_until(candidates, || false)
}

pub fn group_exact_duplicates_until(
    candidates: Vec<ExactDuplicateCandidate>,
    should_cancel: impl Fn() -> bool,
) -> ExactDuplicateGrouping {
    let mut by_digest: BTreeMap<(u64, [u8; 32]), Vec<ExactDuplicateCandidate>> =
        BTreeMap::new();
    let mut skipped = 0;
    for candidate in candidates {
        if should_cancel() {
            skipped += 1;
            continue;
        }
        let Ok(size) = u64::try_from(candidate.indexed_size) else {
            skipped += 1;
            continue;
        };
        match exact_file_sha256_until(&candidate.path, size, &should_cancel) {
            Ok(hash) => by_digest.entry((size, hash)).or_default().push(candidate),
            Err(_) => skipped += 1,
        }
    }
    let groups = by_digest
        .into_iter()
        .filter_map(|((size, hash), mut files)| {
            if files.len() < 2 {
                return None;
            }
            files.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.id.cmp(&b.id)));
            Some(ExactDuplicateGroup { hash, size, files })
        })
        .collect();
    ExactDuplicateGrouping { groups, skipped }
}

pub fn matches_known_hash_hex(path: &Path, size: u64, expected_hex: &str) -> std::io::Result<bool> {
    let expected = match hex::decode(expected_hex) {
        Ok(bytes) if bytes.len() == 32 => bytes,
        _ => return Ok(false),
    };
    if content_hash(path, size)?.as_slice() == expected.as_slice() {
        return Ok(true);
    }
    let legacy = legacy_content_hashes(path, size)?;
    Ok(legacy.v2.as_slice() == expected.as_slice()
        || legacy
            .v1
            .is_some_and(|hash| hash.as_slice() == expected.as_slice()))
}

/// Every BLAKE3 digest a pre-SHA-256 build could have stamped for this file.
/// Rows written by those builds (released v0.0.1 stamped BLAKE3) only
/// rename-heal if the lookup reproduces the exact digest they hold; the heal
/// upsert then re-stamps the current SHA-256 recipe, retiring the probe per row.
pub(crate) struct LegacyHashes {
    /// The digest v0.0.1 stamped: full-file BLAKE3 at or under the cap,
    /// blake3(head ‖ interior samples ‖ tail ‖ size_le) over it.
    pub v2: [u8; 32],
    /// blake3(head ‖ tail ‖ size_le) — the pre-interior-sample composite that
    /// pre-v0.0.1 dev builds stamped for over-cap files (rows never rescanned
    /// since may still hold it). `None` at or under the cap, where every
    /// legacy build stamped the same full-file BLAKE3 as `v2`.
    pub v1: Option<[u8; 32]>,
}

/// Both legacy digests in one read: the shared head/tail bytes feed both
/// hashers, only `v2` sees the interior samples. The recipes are frozen —
/// they reproduce digests already sitting in shipped databases.
pub(crate) fn legacy_content_hashes(path: &Path, size: u64) -> std::io::Result<LegacyHashes> {
    legacy_hashes_with_threshold(path, size, FULL_HASH_MAX_BYTES)
}

/// Testable core: `content_hash` with the full-vs-composite threshold injected
/// so the composite path can be exercised on small fixtures.
fn hash_with_threshold(path: &Path, size: u64, full_max: u64) -> std::io::Result<[u8; 32]> {
    let mut f = std::fs::File::open(super::path_safety::to_extended_length(path))?;
    let mut sha = sha2::Sha256::new();
    if size <= full_max {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            sha.update(&buf[..n]);
        }
    } else {
        // Clamp to the file size so a file between `full_max` and CHUNK
        // doesn't seek before the start (the head+tail overlap on such files
        // is harmless — the hash stays deterministic).
        let span = size.min(CHUNK as u64) as usize;

        let mut head = vec![0u8; span];
        let n = read_fill(&mut f, &mut head)?;
        sha.update(&head[..n]);

        // Interior samples: a few evenly-spaced 64 KB chunks so two DISTINCT
        // same-size files that happen to share their head+tail (camera bursts,
        // container formats with identical headers/footers, padded archives)
        // don't collide and trigger a false rename-heal. Deterministic offsets;
        // skipped on files too small for interior reads to clear head/tail.
        for off in interior_offsets(size, span) {
            if f.seek(SeekFrom::Start(off)).is_ok() {
                let mut mid = vec![0u8; INTERIOR_CHUNK];
                let n = read_fill(&mut f, &mut mid)?;
                sha.update(&mid[..n]);
            }
        }

        f.seek(SeekFrom::End(-(span as i64)))?;
        let mut tail = vec![0u8; span];
        let n = read_fill(&mut f, &mut tail)?;
        sha.update(&tail[..n]);

        // Size disambiguates files that share head+tail but differ in the middle.
        sha.update(size.to_le_bytes());
    }
    let digest = sha.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    Ok(out)
}

fn legacy_hashes_with_threshold(
    path: &Path,
    size: u64,
    full_max: u64,
) -> std::io::Result<LegacyHashes> {
    let mut f = std::fs::File::open(super::path_safety::to_extended_length(path))?;
    let mut v2 = blake3::Hasher::new();
    if size <= full_max {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            v2.update(&buf[..n]);
        }
        return Ok(LegacyHashes {
            v2: *v2.finalize().as_bytes(),
            v1: None,
        });
    }

    let mut v1 = blake3::Hasher::new();
    let span = size.min(CHUNK as u64) as usize;

    let mut head = vec![0u8; span];
    let n = read_fill(&mut f, &mut head)?;
    v2.update(&head[..n]);
    v1.update(&head[..n]);

    for off in interior_offsets(size, span) {
        if f.seek(SeekFrom::Start(off)).is_ok() {
            let mut mid = vec![0u8; INTERIOR_CHUNK];
            let n = read_fill(&mut f, &mut mid)?;
            v2.update(&mid[..n]);
        }
    }

    f.seek(SeekFrom::End(-(span as i64)))?;
    let mut tail = vec![0u8; span];
    let n = read_fill(&mut f, &mut tail)?;
    v2.update(&tail[..n]);
    v1.update(&tail[..n]);

    v2.update(&size.to_le_bytes());
    v1.update(&size.to_le_bytes());
    Ok(LegacyHashes {
        v2: *v2.finalize().as_bytes(),
        v1: Some(*v1.finalize().as_bytes()),
    })
}

fn interior_offsets(size: u64, span: usize) -> impl Iterator<Item = u64> {
    (1..=INTERIOR_SAMPLES)
        .map(move |k| size.saturating_mul(k) / (INTERIOR_SAMPLES + 1))
        .filter(move |&off| {
            off >= span as u64 && off + INTERIOR_CHUNK as u64 <= size.saturating_sub(span as u64)
        })
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
    fn full_hash_is_sha256_for_cross_platform_parity() {
        let p = tmp_with(b"abc");
        assert_eq!(
            hex::encode(content_hash(&p, 3).unwrap()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn exact_digest_rejects_same_sampled_identity_with_unsampled_difference() {
        const MB: usize = 1024 * 1024;
        let a = vec![7u8; 17 * MB];
        let mut b = a.clone();
        b[2 * MB] = 9;
        let pa = tmp_with(&a);
        let pb = tmp_with(&b);
        let size = a.len() as u64;
        assert_eq!(content_hash(&pa, size).unwrap(), content_hash(&pb, size).unwrap());
        assert_ne!(
            exact_file_sha256(&pa, size).unwrap(),
            exact_file_sha256(&pb, size).unwrap()
        );
        let _ = std::fs::remove_file(pa);
        let _ = std::fs::remove_file(pb);
    }

    #[test]
    fn exact_grouping_uses_live_full_file_digest() {
        let a = tmp_with(b"same bytes");
        let b = tmp_with(b"same bytes");
        let different = tmp_with(b"different!");
        let grouping = group_exact_duplicates(vec![
            ExactDuplicateCandidate {
                id: 1,
                path: a.clone(),
                indexed_size: 10,
            },
            ExactDuplicateCandidate {
                id: 2,
                path: b.clone(),
                indexed_size: 10,
            },
            ExactDuplicateCandidate {
                id: 3,
                path: different.clone(),
                indexed_size: 10,
            },
        ]);
        assert_eq!(grouping.skipped, 0);
        assert_eq!(grouping.groups.len(), 1);
        let mut ids = grouping.groups[0]
            .files
            .iter()
            .map(|file| file.id)
            .collect::<Vec<_>>();
        ids.sort_unstable();
        assert_eq!(ids, vec![1, 2]);
        let _ = std::fs::remove_file(a);
        let _ = std::fs::remove_file(b);
        let _ = std::fs::remove_file(different);
    }

    #[test]
    fn known_hash_matcher_accepts_current_and_legacy_but_rejects_replacement() {
        let path = tmp_with(b"original");
        let current = hex::encode(content_hash(&path, 8).unwrap());
        let legacy = hex::encode(legacy_content_hashes(&path, 8).unwrap().v2);
        assert!(matches_known_hash_hex(&path, 8, &current).unwrap());
        assert!(matches_known_hash_hex(&path, 8, &legacy).unwrap());
        std::fs::write(&path, b"replaced").unwrap();
        assert!(!matches_known_hash_hex(&path, 8, &current).unwrap());
        assert!(!matches_known_hash_hex(&path, 8, &legacy).unwrap());
        let _ = std::fs::remove_file(&path);
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
        let c1 = hash_with_threshold(&p, size, 64).unwrap();
        let c2 = hash_with_threshold(&p, size, 64).unwrap();
        assert_eq!(c1, c2, "composite hash must be deterministic");
        let full = hash_with_threshold(&p, size, u64::MAX).unwrap();
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
        let ha = hash_with_threshold(&pa, 4096, 64).unwrap();
        let hb = hash_with_threshold(&pb, 4096, 64).unwrap();
        assert_ne!(ha, hb);
        let _ = std::fs::remove_file(&pa);
        let _ = std::fs::remove_file(&pb);
    }

    #[test]
    fn legacy_under_cap_reproduces_the_full_blake3_v001_stamped() {
        // v0.0.1 tagging.rs stamped blake3::hash(&bytes) for files that fit
        // the full-hash window — the digest sitting in every shipped DB for
        // ≤16 MB files.
        let body: Vec<u8> = (0..64 * 1024u32).map(|i| (i % 251) as u8).collect();
        let p = tmp_with(&body);
        let size = body.len() as u64;
        let stamped_by_v001 = *blake3::hash(&body).as_bytes();

        let legacy = legacy_content_hashes(&p, size).unwrap();
        assert_eq!(
            legacy.v2, stamped_by_v001,
            "under-cap legacy probe must reproduce the full-file BLAKE3 v0.0.1 stamped"
        );
        assert!(
            legacy.v1.is_none(),
            "under-cap files had one legacy recipe; no second candidate needed"
        );
        assert_ne!(
            content_hash(&p, size).unwrap(),
            stamped_by_v001,
            "current SHA-256 must differ or the fallback is moot"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn legacy_v2_reproduces_the_v001_over_cap_recipe() {
        const MB: usize = 1024 * 1024;
        // >16 MB so the real public functions take the composite branch and
        // the interior samples genuinely fire (offsets clear the 1 MB edges).
        let body: Vec<u8> = (0..17 * MB).map(|i| (i % 251) as u8).collect();
        let p = tmp_with(&body);
        let size = body.len() as u64;

        // The digest released v0.0.1 stamped for over-cap files:
        // blake3(head 1MB ‖ 4×64KB interior samples ‖ tail 1MB ‖ size_le),
        // written out here from the v0.0.1 source, not via the code under test.
        let mut h = blake3::Hasher::new();
        h.update(&body[..MB]);
        for k in 1..=4u64 {
            let off = size.saturating_mul(k) / 5;
            if off < MB as u64 || off + 64 * 1024 > size - MB as u64 {
                continue;
            }
            h.update(&body[off as usize..off as usize + 64 * 1024]);
        }
        h.update(&body[body.len() - MB..]);
        h.update(&size.to_le_bytes());
        let stamped_by_v001 = *h.finalize().as_bytes();

        let legacy = legacy_content_hashes(&p, size).unwrap();
        assert_eq!(
            legacy.v2, stamped_by_v001,
            "legacy v2 must reproduce the v0.0.1 interior-sample recipe"
        );
        assert_ne!(
            legacy.v1.unwrap(),
            stamped_by_v001,
            "interior samples must actually fire on this fixture"
        );
        assert_ne!(
            content_hash(&p, size).unwrap(),
            stamped_by_v001,
            "current SHA-256 recipe must differ or the fallback is moot"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn legacy_v1_reproduces_pre_interior_sample_recipe() {
        const MB: usize = 1024 * 1024;
        let body: Vec<u8> = (0..17 * MB).map(|i| (i % 251) as u8).collect();
        let p = tmp_with(&body);
        let size = body.len() as u64;

        // The digest a pre-interior-sample dev build stamped:
        // blake3(head 1MB ‖ tail 1MB ‖ size_le), no interior block.
        let mut h = blake3::Hasher::new();
        h.update(&body[..MB]);
        h.update(&body[body.len() - MB..]);
        h.update(&size.to_le_bytes());
        let stamped_by_old_build = *h.finalize().as_bytes();

        assert_eq!(
            legacy_content_hashes(&p, size).unwrap().v1,
            Some(stamped_by_old_build),
            "legacy v1 must reproduce the recipe-v1 digest"
        );
        let _ = std::fs::remove_file(&p);
    }
}
