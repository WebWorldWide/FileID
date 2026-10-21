//! Exact and perceptual duplicate detection over the indexed library.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{bail, Result};
use rusqlite::Connection;

use crate::util::content_hash::{group_exact_duplicates_until, ExactDuplicateCandidate, ExactDuplicateGroup};

pub const DEFAULT_SIMILAR_THRESHOLD: u32 = 8;
pub const MAX_SIMILAR_THRESHOLD: u32 = 16;
const MAX_GENERIC_INDEX_ENTRIES: usize = 2_000_000;
const MAX_SIMILAR_COMPARISONS: u64 = 5_000_000;

/// Verification caps for one exact pass: candidate files and bytes to re-hash.
#[derive(Clone, Copy, Debug)]
pub struct ExactCaps {
    pub max_candidates: u64,
    pub max_bytes: u64,
}

/// Same-size candidates admitted under [`ExactCaps`], plus what was left out.
#[derive(Debug, Default)]
pub struct ExactCandidates {
    pub candidates: Vec<ExactDuplicateCandidate>,
    pub skipped_candidates: u64,
    pub skipped_bytes: u64,
}

impl ExactCandidates {
    pub fn is_partial(&self) -> bool {
        self.skipped_candidates > 0
    }
}

/// Byte-verified exact groups (hash-identical, size-identical).
#[derive(Debug, Default)]
pub struct ExactListing {
    pub groups: Vec<ExactDuplicateGroup>,
    /// Candidates admitted under the caps and re-hashed.
    pub admitted: u64,
    /// Admitted candidates that were missing, unreadable, changed or cancelled.
    pub unreadable: usize,
    pub skipped_candidates: u64,
    pub skipped_bytes: u64,
}

impl ExactListing {
    pub fn is_partial(&self) -> bool {
        self.skipped_candidates > 0
    }
}

/// Every active file sharing its size with another — the macOS Cleanup rule:
/// membership is the whole same-size class, because stored content hashes are
/// never byte-proof (legacy and current recipes, stale or missing hashes).
/// The stored hash only ranks: classes sharing a stored (hash, size) —
/// near-certain duplicates — are admitted first, then the remaining same-size
/// classes by potential reclaim. Classes are taken whole or not at all, so a
/// split class can never pair; [`exact_groups`] re-hashes every admitted file.
pub fn exact_candidates(conn: &Connection, caps: ExactCaps) -> Result<ExactCandidates> {
    let mut stmt = conn.prepare(
        "WITH active AS ( \
             SELECT id, path_text, size_bytes, content_hash FROM files WHERE failed = 0 AND size_bytes >= 0 \
         ), \
         sizes AS (SELECT size_bytes AS s FROM active GROUP BY size_bytes HAVING COUNT(*) > 1), \
         twins AS ( \
             SELECT content_hash AS h, size_bytes AS s FROM active WHERE content_hash IS NOT NULL \
             GROUP BY content_hash, size_bytes HAVING COUNT(*) > 1 \
         ) \
         SELECT f.id, f.path_text, f.size_bytes, \
                (t.h IS NOT NULL) AS is_twin, \
                CASE WHEN t.h IS NOT NULL THEN HEX(f.content_hash) ELSE CAST(f.size_bytes AS TEXT) END AS class_key \
         FROM active f JOIN sizes z ON z.s = f.size_bytes \
         LEFT JOIN twins t ON t.h = f.content_hash AND t.s = f.size_bytes \
         ORDER BY is_twin DESC, f.size_bytes DESC, class_key, f.path_text, f.id",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                ExactDuplicateCandidate {
                    id: row.get(0)?,
                    path: PathBuf::from(row.get::<_, String>(1)?),
                    indexed_size: row.get(2)?,
                },
                row.get::<_, bool>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut out = ExactCandidates::default();
    let (mut admitted, mut budget) = (0u64, 0u64);
    let mut rows = rows.into_iter().peekable();
    while let Some(first) = rows.next() {
        let mut class = vec![first.0];
        while let Some(next) = rows.next_if(|next| next.1 == first.1 && next.2 == first.2) {
            class.push(next.0);
        }
        let count = class.len() as u64;
        let bytes: u64 = class.iter().map(|c| c.indexed_size.max(0) as u64).sum();
        if admitted + count <= caps.max_candidates && budget.saturating_add(bytes) <= caps.max_bytes {
            admitted += count;
            budget += bytes;
            out.candidates.extend(class);
        } else {
            out.skipped_candidates += count;
            out.skipped_bytes = out.skipped_bytes.saturating_add(bytes);
        }
    }
    Ok(out)
}

/// [`exact_candidates`] re-hashed and grouped. `cancel` stops hashing early;
/// files not reached count as unreadable.
pub fn exact_groups(conn: &Connection, caps: ExactCaps, cancel: impl Fn() -> bool) -> Result<ExactListing> {
    let selected = exact_candidates(conn, caps)?;
    let admitted = selected.candidates.len() as u64;
    let grouping = group_exact_duplicates_until(selected.candidates, cancel);
    Ok(ExactListing {
        groups: grouping.groups,
        admitted,
        unreadable: grouping.skipped,
        skipped_candidates: selected.skipped_candidates,
        skipped_bytes: selected.skipped_bytes,
    })
}

/// Perceptual near-duplicates: connected components of active files whose
/// dHashes differ by at most `threshold` bits (transitively chained). Each
/// component's ids are sorted; components come in ascending order of their
/// smallest id. `None` when no file has a perceptual hash. `phash = 0` is the
/// "no hash" sentinel and is ignored.
pub fn similar_groups(conn: &Connection, threshold: u32, cancel: impl Fn() -> bool) -> Result<Option<Vec<Vec<i64>>>> {
    let mut stmt = conn.prepare("SELECT id, phash FROM files WHERE failed = 0 AND phash IS NOT NULL AND phash != 0 ORDER BY id")?;
    let rows: Vec<(i64, i64)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if rows.is_empty() {
        return Ok(None);
    }
    Ok(Some(similar_components(&rows, threshold, &cancel)?.0))
}

fn similar_components(rows: &[(i64, i64)], threshold: u32, cancel: &dyn Fn() -> bool) -> Result<(Vec<Vec<i64>>, u64)> {
    if threshold > MAX_SIMILAR_THRESHOLD {
        bail!("similar threshold {threshold} exceeds the supported maximum of {MAX_SIMILAR_THRESHOLD} bits");
    }
    let mut parent: Vec<usize> = (0..rows.len()).collect();
    let comparisons = if threshold == DEFAULT_SIMILAR_THRESHOLD {
        radius_eight(rows, &mut parent, cancel)?
    } else {
        let entries = rows.len().saturating_mul((threshold + 1) as usize);
        if entries > MAX_GENERIC_INDEX_ENTRIES {
            bail!("similar candidate index would require {entries} entries; limit is {MAX_GENERIC_INDEX_ENTRIES}");
        }
        generic(rows, threshold, &mut parent, cancel)?
    };
    let mut components: BTreeMap<usize, Vec<i64>> = BTreeMap::new();
    for (index, row) in rows.iter().enumerate() {
        let root = find(&mut parent, index);
        components.entry(root).or_default().push(row.0);
    }
    let mut groups: Vec<Vec<i64>> = components
        .into_values()
        .filter(|ids| ids.len() > 1)
        .map(|mut ids| {
            ids.sort_unstable();
            ids
        })
        .collect();
    groups.sort_unstable_by_key(|ids| ids[0]);
    Ok((groups, comparisons))
}

/// Pigeonhole multi-index search: split 64 bits into 3 blocks (21/21/22); two
/// hashes within 8 bits agree within 2 bits on at least one block, so probing
/// each block's ≤2-bit neighbourhood finds every candidate.
fn radius_eight(rows: &[(i64, i64)], parent: &mut [usize], cancel: &dyn Fn() -> bool) -> Result<u64> {
    const BLOCKS: [(usize, usize); 3] = [(0, 21), (21, 21), (42, 22)];
    let masks = [masks_within_two(21), masks_within_two(21), masks_within_two(22)];
    let mut indexes: Vec<HashMap<u64, Vec<usize>>> = (0..BLOCKS.len()).map(|_| HashMap::new()).collect();
    let mut representative: HashMap<u64, usize> = HashMap::new();
    let mut comparisons = 0u64;
    for (index, row) in rows.iter().enumerate() {
        if index % 1024 == 0 && cancel() {
            bail!("similar search cancelled");
        }
        let hash = row.1 as u64;
        if let Some(&same) = representative.get(&hash) {
            union(parent, same, index);
            continue;
        }
        let mut candidates = HashSet::new();
        for (block, (&(lo, width), masks)) in BLOCKS.iter().zip(&masks).enumerate() {
            let key = (hash >> lo) & ((1u64 << width) - 1);
            for mask in masks {
                if let Some(prior) = indexes[block].get(&(key ^ mask)) {
                    candidates.extend(prior.iter().copied());
                }
            }
        }
        for other in candidates {
            comparisons += 1;
            if comparisons > MAX_SIMILAR_COMPARISONS {
                bail!("similar candidate comparisons exceeded the {MAX_SIMILAR_COMPARISONS} safety limit");
            }
            if (hash ^ rows[other].1 as u64).count_ones() <= DEFAULT_SIMILAR_THRESHOLD {
                union(parent, index, other);
            }
        }
        for (block, &(lo, width)) in BLOCKS.iter().enumerate() {
            indexes[block].entry((hash >> lo) & ((1u64 << width) - 1)).or_default().push(index);
        }
        representative.insert(hash, index);
    }
    Ok(comparisons)
}

fn masks_within_two(width: usize) -> Vec<u64> {
    let mut masks = Vec::with_capacity(1 + width + width * (width - 1) / 2);
    masks.push(0);
    for first in 0..width {
        masks.push(1u64 << first);
        for second in first + 1..width {
            masks.push((1u64 << first) | (1u64 << second));
        }
    }
    masks
}

/// Pigeonhole with `threshold + 1` blocks: within-threshold hashes match
/// exactly on at least one block.
fn generic(rows: &[(i64, i64)], threshold: u32, parent: &mut [usize], cancel: &dyn Fn() -> bool) -> Result<u64> {
    let blocks = (threshold + 1) as usize;
    let block_key = |hash: u64, block: usize| {
        let lo = (block * 64) / blocks;
        let width = ((block + 1) * 64) / blocks - lo;
        let mask = if width == 64 { u64::MAX } else { (1u64 << width) - 1 };
        (hash >> lo) & mask
    };
    let mut by_block: Vec<HashMap<u64, Vec<usize>>> = (0..blocks).map(|_| HashMap::new()).collect();
    let mut representative: HashMap<u64, usize> = HashMap::new();
    let mut comparisons = 0u64;
    for (index, row) in rows.iter().enumerate() {
        if index % 1024 == 0 && cancel() {
            bail!("similar search cancelled");
        }
        let hash = row.1 as u64;
        if let Some(&same) = representative.get(&hash) {
            union(parent, same, index);
            continue;
        }
        let mut candidates = HashSet::new();
        for (block, buckets) in by_block.iter().enumerate() {
            if let Some(prior) = buckets.get(&block_key(hash, block)) {
                candidates.extend(prior.iter().copied());
            }
        }
        for other in candidates {
            comparisons += 1;
            if comparisons > MAX_SIMILAR_COMPARISONS {
                bail!("similar candidate comparisons exceeded the {MAX_SIMILAR_COMPARISONS} safety limit");
            }
            if (hash ^ rows[other].1 as u64).count_ones() <= threshold {
                union(parent, index, other);
            }
        }
        for (block, buckets) in by_block.iter_mut().enumerate() {
            buckets.entry(block_key(hash, block)).or_default().push(index);
        }
        representative.insert(hash, index);
    }
    Ok(comparisons)
}

fn find(parent: &mut [usize], mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]];
        x = parent[x];
    }
    x
}

fn union(parent: &mut [usize], a: usize, b: usize) {
    let (ra, rb) = (find(parent, a), find(parent, b));
    if ra != rb {
        parent[ra] = rb;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn components(rows: &[(i64, i64)], threshold: u32) -> Result<Vec<Vec<i64>>> {
        Ok(similar_components(rows, threshold, &|| false)?.0)
    }

    fn temp_file(name: &str, bytes: &[u8]) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "fileid-read-dupes-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn library() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        conn
    }

    fn insert(conn: &Connection, path: &std::path::Path, size: i64, hash: Option<&[u8]>, phash: Option<i64>) {
        conn.execute(
            "INSERT INTO files (path_text, path_hash, size_bytes, modified_at, scanned_at, kind, extension, content_hash, phash) \
             VALUES (?1, 1, ?2, 1.0, 1.0, 'image', 'jpg', ?3, ?4)",
            params![path.to_string_lossy(), size, hash, phash],
        )
        .unwrap();
    }

    const WIDE: ExactCaps = ExactCaps { max_candidates: 100_000, max_bytes: 1 << 40 };

    #[test]
    fn identical_phashes_collapse_before_candidate_search() {
        let rows: Vec<(i64, i64)> = (0..100_000).map(|id| (id, 42)).collect();
        let groups = components(&rows, 8).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), rows.len());
    }

    #[test]
    fn hamming_components_preserve_transitive_groups() {
        let rows = vec![(1, 0b0000), (2, 0b0001), (3, 0b0011), (4, 0b1111_0000)];
        assert_eq!(components(&rows, 1).unwrap(), vec![vec![1, 2, 3]]);
    }

    #[test]
    fn generic_search_rejects_threshold_and_index_amplification() {
        assert!(components(&[(1, 0), (2, 1)], MAX_SIMILAR_THRESHOLD + 1)
            .unwrap_err()
            .to_string()
            .contains("supported maximum"));
        let rows = vec![(1, 0); MAX_GENERIC_INDEX_ENTRIES / 17 + 1];
        assert!(components(&rows, 16).unwrap_err().to_string().contains("candidate index"));
    }

    #[test]
    fn generic_search_stops_at_comparison_budget() {
        let rows = (0..3_200).map(|id| (id, id)).collect::<Vec<(i64, i64)>>();
        assert!(components(&rows, 1).unwrap_err().to_string().contains("comparisons exceeded"));
    }

    #[test]
    fn cancellation_stops_the_search() {
        let rows = (0..10).map(|id| (id, id)).collect::<Vec<(i64, i64)>>();
        assert!(similar_components(&rows, 8, &|| true).unwrap_err().to_string().contains("cancelled"));
    }

    fn brute(rows: &[(i64, i64)], threshold: u32) -> Vec<Vec<i64>> {
        let mut parent: Vec<usize> = (0..rows.len()).collect();
        for left in 0..rows.len() {
            for right in left + 1..rows.len() {
                if ((rows[left].1 as u64) ^ (rows[right].1 as u64)).count_ones() <= threshold {
                    union(&mut parent, left, right);
                }
            }
        }
        let mut groups: BTreeMap<usize, Vec<i64>> = BTreeMap::new();
        for (index, row) in rows.iter().enumerate() {
            let root = find(&mut parent, index);
            groups.entry(root).or_default().push(row.0);
        }
        let mut groups: Vec<Vec<i64>> = groups.into_values().filter(|g| g.len() > 1).collect();
        for group in &mut groups {
            group.sort_unstable();
        }
        groups.sort();
        groups
    }

    fn random_phashes(count: usize) -> Vec<(i64, i64)> {
        let mut state = 0xd1b5_4a32_d192_ed03u64;
        (0..count)
            .map(|id| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (id as i64, state as i64)
            })
            .collect()
    }

    #[test]
    fn indexed_search_matches_brute_force() {
        for threshold in [3, 8] {
            for count in [1, 7, 64, 257] {
                let mut rows = random_phashes(count);
                if count >= 7 {
                    rows[1].1 = rows[0].1 ^ 0xff;
                    rows[2].1 = rows[0].1 ^ 0x1ff;
                    rows[3].1 = rows[0].1;
                    rows[4].1 = i64::MIN;
                    rows[5].1 = (i64::MIN as u64 ^ 0b111) as i64;
                }
                let mut indexed = components(&rows, threshold).unwrap();
                indexed.sort();
                assert_eq!(indexed, brute(&rows, threshold), "threshold {threshold}, {count} rows");
            }
        }
    }

    #[test]
    fn radius_eight_segment_boundary_is_complete() {
        let eight = 0b111u64 | (0b111u64 << 21) | (0b11u64 << 42);
        let nine = eight | (1u64 << 44);
        assert_eq!(components(&[(1, 0), (2, eight as i64)], 8).unwrap(), vec![vec![1, 2]]);
        assert!(components(&[(1, 0), (3, nine as i64)], 8).unwrap().is_empty());
    }

    #[test]
    fn radius_eight_100k_stays_within_comparison_budget() {
        let (_, comparisons) = similar_components(&random_phashes(100_000), 8, &|| false).unwrap();
        assert!(comparisons < MAX_SIMILAR_COMPARISONS, "{comparisons}");
    }

    #[test]
    fn similar_groups_ignore_failed_rows_and_the_zero_sentinel() {
        let conn = library();
        assert!(similar_groups(&conn, 8, || false).unwrap().is_none());
        for (name, phash) in [("a", 0b1), ("b", 0b11), ("z1", 0), ("z2", 0)] {
            insert(&conn, std::path::Path::new(&format!("/lib/{name}.jpg")), 1, None, Some(phash));
        }
        conn.execute("UPDATE files SET failed = 1 WHERE path_text = '/lib/b.jpg'", []).unwrap();
        assert!(similar_groups(&conn, 8, || false).unwrap().unwrap().is_empty());
        conn.execute("UPDATE files SET failed = 0", []).unwrap();
        assert_eq!(similar_groups(&conn, 8, || false).unwrap().unwrap(), vec![vec![1, 2]]);
    }

    #[test]
    fn exact_groups_verify_every_same_size_file_whatever_its_stored_hash() {
        let conn = library();
        let twin_a = temp_file("twin-a", b"twin-bytes");
        let twin_b = temp_file("twin-b", b"twin-bytes");
        let legacy = temp_file("legacy", b"twin-bytes");
        let unhashed = temp_file("unhashed", b"twin-bytes");
        let straddle_a = temp_file("straddle-a", b"same-bytes!!");
        let straddle_b = temp_file("straddle-b", b"same-bytes!!");
        assert!(exact_groups(&conn, WIDE, || false).unwrap().groups.is_empty());
        insert(&conn, &twin_a, 10, Some(b"\x0a"), None);
        insert(&conn, &twin_b, 10, Some(b"\x0a"), None);
        insert(&conn, &legacy, 10, Some(b"\x0b"), None);
        insert(&conn, &unhashed, 10, None, None);
        insert(&conn, &straddle_a, 12, Some(b"\x01"), None);
        insert(&conn, &straddle_b, 12, Some(b"\x02"), None);

        let listing = exact_groups(&conn, WIDE, || false).unwrap();
        let mut groups: Vec<Vec<i64>> = listing.groups.iter().map(|g| g.files.iter().map(|f| f.id).collect()).collect();
        for group in &mut groups {
            group.sort_unstable();
        }
        groups.sort();
        assert_eq!(groups, vec![vec![1, 2, 3, 4], vec![5, 6]]);
        assert_eq!((listing.admitted, listing.unreadable), (6, 0));
        assert!(!listing.is_partial());
        for path in [twin_a, twin_b, legacy, unhashed, straddle_a, straddle_b] {
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn over_budget_classes_are_reported_not_fatal() {
        let conn = library();
        let a = temp_file("budget-a", b"twin-bytes");
        let b = temp_file("budget-b", b"twin-bytes");
        insert(&conn, &a, 10, Some(b"\x0a"), None);
        insert(&conn, &b, 10, Some(b"\x0a"), None);
        insert(&conn, std::path::Path::new("/absent/c1.bin"), 600_000_000_000, Some(b"\x0c\x01"), None);
        insert(&conn, std::path::Path::new("/absent/c2.bin"), 600_000_000_000, Some(b"\x0c\x02"), None);
        let listing = exact_groups(&conn, WIDE, || false).unwrap();
        assert_eq!(listing.groups.len(), 1);
        assert_eq!((listing.skipped_candidates, listing.skipped_bytes), (2, 1_200_000_000_000));
        let tight = exact_candidates(&conn, ExactCaps { max_candidates: 1, max_bytes: 1 << 40 }).unwrap();
        assert!(tight.candidates.is_empty() && tight.is_partial());
        let _ = std::fs::remove_file(a);
        let _ = std::fs::remove_file(b);
    }
}
