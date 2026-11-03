//! Groups burst frames and RAW+JPEG pairs so Deep Analyze runs the VLM once per
//! group. Mirror of the macOS `NearDuplicateGrouping.swift` with the same
//! thresholds; change both together.

use std::collections::{HashMap, HashSet};
use std::path::Path;

/// How close two frames must be to join one group. `NEAR_DUPLICATE` is "the
/// same picture" (Deep Analyze describes it once); `TAKES` is "another try of
/// the same shot" (Best Takes compares them); `CLIPS` is repeated video clips.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    pub max_gap_seconds: f64,
    pub max_span_seconds: f64,
    pub min_cosine: f32,
    pub min_seed_cosine: f32,
    pub max_hamming: u32,
    pub max_group_size: usize,
}

pub const NEAR_DUPLICATE: Thresholds = Thresholds {
    max_gap_seconds: 2.0,
    max_span_seconds: 12.0,
    min_cosine: 0.97,
    min_seed_cosine: 0.955,
    max_hamming: 8,
    max_group_size: 24,
};
pub const TAKES: Thresholds = Thresholds {
    max_gap_seconds: 4.0,
    max_span_seconds: 30.0,
    min_cosine: 0.93,
    min_seed_cosine: 0.88,
    max_hamming: 12,
    max_group_size: 60,
};
pub const CLIPS: Thresholds = Thresholds {
    max_gap_seconds: 900.0,
    max_span_seconds: 3_600.0,
    min_cosine: 0.90,
    min_seed_cosine: 0.86,
    max_hamming: 0,
    max_group_size: 24,
};

pub const MAX_GAP_SECONDS: f64 = NEAR_DUPLICATE.max_gap_seconds;
pub const MAX_SPAN_SECONDS: f64 = NEAR_DUPLICATE.max_span_seconds;
pub const MIN_COSINE: f32 = NEAR_DUPLICATE.min_cosine;
pub const MIN_SEED_COSINE: f32 = NEAR_DUPLICATE.min_seed_cosine;
pub const MAX_HAMMING: u32 = NEAR_DUPLICATE.max_hamming;
pub const MAX_GROUP_SIZE: usize = NEAR_DUPLICATE.max_group_size;
/// A same-named file (RAW+JPEG, or a later edited JPEG export of the RAW) this
/// similar counts as the same shot even when its file dates differ.
pub const SIDECAR_COSINE: f32 = 0.90;

const RAW_EXTENSIONS: &[&str] = &[
    "3fr", "arw", "cr2", "cr3", "dng", "iiq", "nef", "nrw", "orf", "pef", "raf", "raw", "rw2", "srw", "x3f",
];

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub id: i64,
    pub path: String,
    pub time: Option<f64>,
    pub phash: Option<u64>,
    pub embedding: Option<Vec<f32>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub representative: i64,
    /// Every file in the group, representative included, in capture order.
    pub members: Vec<i64>,
}

pub fn stem(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

pub fn is_raw(path: &str) -> bool {
    Path::new(path)
        .extension()
        .map(|e| RAW_EXTENSIONS.contains(&e.to_string_lossy().to_lowercase().as_str()))
        .unwrap_or(false)
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0f32, 0f32, 0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na <= 0.0 || nb <= 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// Same-stem twins that look alike share the earlier capture time, so a JPEG
/// exported hours after the RAW still sorts beside it.
pub fn align_sidecar_times(items: &[Item]) -> Vec<Item> {
    let mut by_stem: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, item) in items.iter().enumerate() {
        by_stem.entry(stem(&item.path)).or_default().push(i);
    }
    let mut result = items.to_vec();
    for indices in by_stem.values().filter(|v| v.len() > 1) {
        for &a in indices {
            for &b in indices.iter().filter(|&&b| b != a) {
                let (Some(ea), Some(eb)) = (items[a].embedding.as_deref(), items[b].embedding.as_deref()) else { continue };
                let (Some(ta), Some(tb)) = (result[a].time, items[b].time) else { continue };
                if tb < ta && cosine(ea, eb) >= SIDECAR_COSINE {
                    result[a].time = Some(tb);
                }
            }
        }
    }
    result
}

pub fn groups(items: &[Item]) -> Vec<Group> {
    groups_with(items, &NEAR_DUPLICATE)
}

pub fn groups_with(items: &[Item], limits: &Thresholds) -> Vec<Group> {
    let aligned = align_sidecar_times(items);
    let mut ordered: Vec<&Item> = aligned.iter().collect();
    ordered.sort_by(|l, r| {
        let by_time = match (l.time, r.time) {
            (Some(a), Some(b)) => a.total_cmp(&b),
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (Some(_), None) => std::cmp::Ordering::Less,
            _ => std::cmp::Ordering::Equal,
        };
        by_time
            .then_with(|| stem(&l.path).cmp(&stem(&r.path)))
            .then_with(|| l.path.cmp(&r.path))
            .then_with(|| l.id.cmp(&r.id))
    });
    let mut result = Vec::new();
    let mut current: Vec<&Item> = Vec::new();
    for item in ordered {
        if !current.is_empty() && joins(item, &current, limits) {
            current.push(item);
        } else {
            if !current.is_empty() {
                result.push(group(&current));
            }
            current = vec![item];
        }
    }
    if !current.is_empty() {
        result.push(group(&current));
    }
    result
}

fn joins(item: &Item, current: &[&Item], limits: &Thresholds) -> bool {
    let last = current[current.len() - 1];
    let (Some(time), Some(last_time), Some(first_time)) = (item.time, last.time, current[0].time) else {
        return false;
    };
    if current.len() >= limits.max_group_size
        || time - last_time > limits.max_gap_seconds
        || time - first_time > limits.max_span_seconds
    {
        return false;
    }
    let item_stem = stem(&item.path);
    if current.iter().any(|c| stem(&c.path) == item_stem && c.path != item.path) {
        return true;
    }
    let (Some(embedding), Some(last_embedding), Some(seed)) =
        (item.embedding.as_deref(), last.embedding.as_deref(), current[0].embedding.as_deref())
    else {
        return false;
    };
    let to_last = cosine(embedding, last_embedding);
    if cosine(embedding, seed) < limits.min_seed_cosine {
        return false;
    }
    if to_last >= limits.min_cosine {
        return true;
    }
    match (item.phash, last.phash) {
        (Some(p), Some(q)) if p != 0 && q != 0 && (p ^ q).count_ones() <= limits.max_hamming => {
            to_last >= limits.min_seed_cosine
        }
        _ => false,
    }
}

fn group(items: &[&Item]) -> Group {
    let mut best = 0;
    let mut best_score = f32::NEG_INFINITY;
    for (i, candidate) in items.iter().enumerate() {
        let mut score = 0f32;
        if let Some(e) = candidate.embedding.as_deref() {
            for other in items.iter().filter(|o| o.id != candidate.id) {
                if let Some(o) = other.embedding.as_deref() {
                    score += cosine(e, o);
                }
            }
        }
        if is_raw(&candidate.path) {
            score -= 0.001;
        }
        if score > best_score + 1e-6 {
            best = i;
            best_score = score;
        }
    }
    Group { representative: items[best].id, members: items.iter().map(|i| i.id).collect() }
}

/// ` 1`, ` 2`… per distinct shot (RAW+JPEG of one shot share a number), zero-padded
/// when the group has ten or more shots. Empty for a single shot.
pub fn sequence_suffixes(paths: &[String]) -> Vec<String> {
    let mut order: HashMap<String, usize> = HashMap::new();
    for path in paths {
        let next = order.len() + 1;
        order.entry(stem(path)).or_insert(next);
    }
    if order.len() <= 1 {
        return vec![String::new(); paths.len()];
    }
    let width = if order.len() >= 10 { 2 } else { 1 };
    paths.iter().map(|p| format!(" {:0width$}", order[&stem(p)], width = width)).collect()
}

pub fn appending_sequence(name: &str, suffix: &str) -> String {
    if suffix.is_empty() {
        return name.to_string();
    }
    let budget = 60usize.saturating_sub(suffix.chars().count()).max(1);
    let base: String = name.chars().take(budget).collect();
    format!("{}{}", base.trim_matches(|c| c == ' ' || c == '-' || c == '_'), suffix)
}

pub fn decode_embedding(blob: &[u8]) -> Option<Vec<f32>> {
    if blob.len() < 4 || blob.len() % 4 != 0 {
        return None;
    }
    Some(blob.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
}

/// Image metadata for `ids`; CLIP vectors only for files within `MAX_GAP_SECONDS`
/// of another, so a large library never loads every embedding.
pub fn load_items(conn: &rusqlite::Connection, ids: &[i64]) -> rusqlite::Result<Vec<Item>> {
    let mut meta: Vec<(i64, String, Option<f64>, Option<u64>)> = Vec::new();
    let mut stmt = conn.prepare_cached(
        "SELECT id, path_text, created_at, phash FROM files WHERE id = ?1 AND kind = 'image'",
    )?;
    for id in ids {
        let mut rows = stmt.query([id])?;
        if let Some(row) = rows.next()? {
            let phash: Option<i64> = row.get(3)?;
            meta.push((row.get(0)?, row.get(1)?, row.get(2)?, phash.map(|p| p as u64)));
        }
    }
    let mut timed: Vec<(i64, f64)> = meta.iter().filter_map(|m| m.2.map(|t| (m.0, t))).collect();
    timed.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut burst = HashSet::new();
    let mut stems: HashMap<String, Vec<i64>> = HashMap::new();
    for m in &meta {
        stems.entry(stem(&m.1)).or_default().push(m.0);
    }
    for ids in stems.values().filter(|v| v.len() > 1) {
        burst.extend(ids.iter().copied());
    }
    for pair in timed.windows(2) {
        if pair[1].1 - pair[0].1 <= MAX_GAP_SECONDS {
            burst.insert(pair[0].0);
            burst.insert(pair[1].0);
        }
    }
    let mut embed = conn.prepare_cached("SELECT embedding FROM clip_embeddings WHERE file_id = ?1")?;
    meta.into_iter()
        .map(|(id, path, time, phash)| {
            let embedding = if burst.contains(&id) {
                let mut rows = embed.query([id])?;
                match rows.next()? {
                    Some(row) => decode_embedding(&row.get::<_, Vec<u8>>(0)?),
                    None => None,
                }
            } else {
                None
            };
            Ok(Item { id, path, time, phash, embedding })
        })
        .collect()
}

#[derive(Debug, Default, Clone)]
pub struct Plan {
    pub ordered: Vec<i64>,
    pub group_of: HashMap<i64, usize>,
    pub suffixes: HashMap<i64, String>,
    pub group_count: usize,
}

pub fn grouping_enabled() -> bool {
    std::env::var("FILEID_DEEP_GROUPING").map(|v| v != "0").unwrap_or(true)
}

/// Orders `ids` so each group's representative runs first and its members follow.
pub fn plan(conn: &rusqlite::Connection, ids: &[i64]) -> Plan {
    let mut plan = Plan { ordered: ids.to_vec(), ..Plan::default() };
    if ids.len() < 2 || !grouping_enabled() {
        return plan;
    }
    let Ok(items) = load_items(conn, ids) else { return plan };
    let paths: HashMap<i64, String> = items.iter().map(|i| (i.id, i.path.clone())).collect();
    let found: Vec<Group> = groups(&items).into_iter().filter(|g| g.members.len() > 1).collect();
    if found.is_empty() {
        return plan;
    }
    for (index, group) in found.iter().enumerate() {
        let member_paths: Vec<String> = group.members.iter().map(|m| paths.get(m).cloned().unwrap_or_default()).collect();
        for (id, suffix) in group.members.iter().zip(sequence_suffixes(&member_paths)) {
            plan.group_of.insert(*id, index);
            plan.suffixes.insert(*id, suffix);
        }
    }
    plan.group_count = found.len();
    let mut emitted = HashSet::new();
    let mut ordered = Vec::with_capacity(ids.len());
    for id in ids {
        if emitted.contains(id) {
            continue;
        }
        if let Some(index) = plan.group_of.get(id) {
            let group = &found[*index];
            let rest = group.members.iter().copied().filter(|m| *m != group.representative);
            for member in std::iter::once(group.representative).chain(rest) {
                if emitted.insert(member) {
                    ordered.push(member);
                }
            }
        } else {
            emitted.insert(*id);
            ordered.push(*id);
        }
    }
    plan.ordered = ordered;
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vector(angle: f32) -> Vec<f32> {
        let mut v = vec![0.0; 8];
        v[0] = angle.cos();
        v[1] = angle.sin();
        v
    }

    fn item(id: i64, path: &str, time: Option<f64>, angle: Option<f32>) -> Item {
        Item { id, path: path.into(), time, phash: None, embedding: angle.map(vector) }
    }

    #[test]
    fn groups_a_burst_and_raw_sidecars_but_not_the_next_shot() {
        let items = vec![
            item(1, "/c/JPEGs/DSC001.JPG", Some(100.0), Some(0.0)),
            item(2, "/c/RAWs/DSC001.ARW", Some(100.0), Some(0.0)),
            item(3, "/c/JPEGs/DSC002.JPG", Some(101.0), Some(0.05)),
            item(4, "/c/RAWs/DSC002.ARW", Some(101.0), Some(0.05)),
            item(5, "/c/JPEGs/DSC003.JPG", Some(102.0), Some(1.2)),
        ];
        let found = groups(&items);
        assert_eq!(found.iter().map(|g| g.members.clone()).collect::<Vec<_>>(), vec![vec![1, 2, 3, 4], vec![5]]);
        assert_eq!(found[0].representative, 1);
    }

    #[test]
    fn a_later_export_of_the_raw_joins_its_shot() {
        let found = groups(&[
            item(1, "/c/RAWs/DSC9.ARW", Some(100.0), Some(0.0)),
            item(2, "/c/JPEGs/DSC9.JPG", Some(15_000.0), Some(0.3)),
            item(3, "/c/JPEGs/DSC8.JPG", Some(15_000.0), Some(0.0)),
        ]);
        assert_eq!(found.iter().map(|g| g.members.clone()).collect::<Vec<_>>(), vec![vec![2, 1], vec![3]]);
    }

    #[test]
    fn time_gaps_and_missing_times_never_join() {
        let found = groups(&[
            item(1, "/a/1.jpg", Some(10.0), Some(0.0)),
            item(2, "/a/2.jpg", Some(13.0), Some(0.0)),
            item(3, "/a/3.jpg", None, Some(0.0)),
            item(4, "/a/4.jpg", None, Some(0.0)),
        ]);
        assert!(found.iter().all(|g| g.members.len() == 1));
    }

    #[test]
    fn missing_embeddings_keep_files_separate_except_sidecars() {
        let found = groups(&[
            item(1, "/a/IMG_1.HEIC", Some(10.0), None),
            item(2, "/a/IMG_1.DNG", Some(10.0), None),
            item(3, "/a/IMG_2.HEIC", Some(10.5), None),
        ]);
        assert_eq!(found.iter().map(|g| g.members.clone()).collect::<Vec<_>>(), vec![vec![2, 1], vec![3]]);
        assert_eq!(found[0].representative, 1);
    }

    #[test]
    fn drift_from_the_first_frame_splits_a_long_pan() {
        let items: Vec<Item> = (0..10).map(|i| item(i, &format!("/a/{i}.jpg"), Some(i as f64), Some(i as f32 * 0.2))).collect();
        let found = groups(&items);
        assert!(found.len() > 1);
        for g in found {
            let angles: Vec<f32> = g.members.iter().map(|m| *m as f32 * 0.2).collect();
            let spread = angles.iter().copied().fold(f32::MIN, f32::max) - angles.iter().copied().fold(f32::MAX, f32::min);
            assert!(spread.cos() >= MIN_SEED_COSINE);
        }
    }

    #[test]
    fn sequence_suffixes_match_macos() {
        let p = |v: &[&str]| v.iter().map(|&s| s.to_owned()).collect::<Vec<_>>();
        assert_eq!(sequence_suffixes(&p(&["/j/A1.JPG", "/r/A1.ARW", "/j/A2.JPG"])), vec![" 1", " 1", " 2"]);
        assert_eq!(sequence_suffixes(&p(&["/j/A1.JPG", "/r/A1.ARW"])), vec!["", ""]);
        let many: Vec<String> = (1..=10).map(|i| format!("/j/S{i}.jpg")).collect();
        let s = sequence_suffixes(&many);
        assert_eq!(s.first().unwrap(), " 01");
        assert_eq!(s.last().unwrap(), " 10");
        let named = appending_sequence(&"Word ".repeat(20), " 07");
        assert!(named.chars().count() <= 60 && named.ends_with(" 07"));
    }

    #[test]
    fn embedding_blob_decodes_little_endian() {
        let mut blob = Vec::new();
        blob.extend_from_slice(&1f32.to_le_bytes());
        blob.extend_from_slice(&(-2.5f32).to_le_bytes());
        assert_eq!(decode_embedding(&blob), Some(vec![1.0, -2.5]));
        assert_eq!(decode_embedding(&[1, 2, 3]), None);
    }
}
