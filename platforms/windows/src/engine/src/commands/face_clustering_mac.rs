//! `runFaceClustering` over macOS face spaces (Vision boxes and landmarks, the
//! Swift alignment). A port of the Swift engine's `FaceClustering.runClustering`
//! without its vector extraction, together with the `FaceClusterPersistence`,
//! `StablePersonAssignments`, `ExactCosineJoin` and `FaceReview` holdout and
//! assessment it relies on, so a Swift catalog clustered here keeps the same
//! people, person ids, names, verdicts and queues. Keep in step with those
//! Swift files; the portable pipeline in `face_clustering.rs` stays the default
//! for every other face space.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ops::RangeInclusive;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, ensure, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};

use super::face_review;
use crate::ipc::FaceClusteringResult;
use crate::pipeline::identity_clustering::{self, Hyperparameters, Neighbor};

pub(crate) const MAC_PREFIX: &str = "macos-";
const MAX_PERSONS: usize = 8000;
const MAX_FACES_PER_RUN: usize = 200_000;
const DIM: usize = 128;
const BRUTE_FORCE_MAX: usize = 5_000;
const PENDING: &str = "('suggested','uncertain','ambiguous','rejected')";
const REVIEW_REASONS: [&str; 3] = ["suggested", "uncertain", "ambiguous"];
const CACHE_ERROR: &str = "Face caches need a compatible refresh before clustering. Existing People and assignments were preserved. Continue face processing; stale, legacy or mixed model vectors cannot be compared.";
const PERSIST_ERROR: &str = "Face evidence changed or could not be saved. Refresh People and try again.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Space {
    pub model: String,
    pub processing: String,
}

pub(crate) struct Outcome {
    pub result: FaceClusteringResult,
    pub error: Option<(&'static str, &'static str)>,
}

fn env_cos(key: &str, default: f32) -> f32 {
    std::env::var(key).ok().and_then(|v| v.parse::<f32>().ok()).filter(|v| (0.0..=1.0).contains(v)).unwrap_or(default)
}

fn env_int(key: &str, default: usize, range: RangeInclusive<i64>) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| range.contains(v))
        .map_or(default, |v| v as usize)
}

fn env_double(key: &str, default: f64, range: RangeInclusive<f64>) -> f64 {
    std::env::var(key).ok().and_then(|v| v.parse::<f64>().ok()).filter(|v| range.contains(v)).unwrap_or(default)
}

fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key).map_or(default, |v| v == "1" || v.eq_ignore_ascii_case("true"))
}

/// The face space to cluster with the Swift pipeline, or `None` for the
/// portable one. The app-hosted engine on a Mac clusters its own namespace with
/// the installed face model (or the one model the catalog used there); any
/// engine clusters a catalog whose faces all sit in one macOS space the same way.
pub(crate) fn selected_space(conn: &Connection) -> Option<Space> {
    let platform = super::face_refresh::processing();
    if platform.starts_with(MAC_PREFIX) {
        let present: bool = conn
            .query_row("SELECT EXISTS(SELECT 1 FROM face_prints WHERE processing_version=?1)", [platform], |row| row.get(0))
            .unwrap_or(false);
        let model = super::face_refresh::current_model().or_else(|| single_model(conn, platform));
        if let Some(model) = model.filter(|_| present) {
            return Some(Space { model, processing: platform.to_string() });
        }
    }
    match super::face_clustering::ensure_compatible_face_space(conn) {
        Ok(Some((model, processing))) if processing.starts_with(MAC_PREFIX) => Some(Space { model, processing }),
        _ => None,
    }
}

fn single_model(conn: &Connection, processing: &str) -> Option<String> {
    let mut statement = conn
        .prepare("SELECT DISTINCT embedding_model FROM face_prints WHERE processing_version=?1 AND COALESCE(embedding_model,'')<>'' LIMIT 2")
        .ok()?;
    let models: Vec<String> = statement.query_map([processing], |row| row.get(0)).ok()?.flatten().collect();
    (models.len() == 1).then(|| models[0].clone())
}

// MARK: - Provenance (FaceCacheProvenance)

fn source_revision(size: i64, modified: f64) -> String {
    format!("{size}:{}", modified.to_bits())
}

/// Foundation's `contentModificationDate.timeIntervalSince1970`: seconds since
/// 2001 plus nanoseconds, shifted back to 1970. `Duration::as_secs_f64` rounds
/// differently in about a quarter of files, which would defer them all.
pub(crate) fn foundation_mtime(metadata: &std::fs::Metadata) -> Option<f64> {
    #[cfg(unix)]
    let (seconds, nanos) = {
        use std::os::unix::fs::MetadataExt;
        (metadata.mtime(), metadata.mtime_nsec())
    };
    #[cfg(not(unix))]
    let (seconds, nanos) = {
        let duration = metadata.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
        (duration.as_secs() as i64, i64::from(duration.subsec_nanos()))
    };
    let value = foundation_time(seconds, nanos);
    value.is_finite().then_some(value)
}

fn foundation_time(seconds: i64, nanos: i64) -> f64 {
    const REFERENCE: f64 = 978_307_200.0;
    ((seconds as f64 - REFERENCE) + nanos as f64 * 1e-9) + REFERENCE
}

fn current_source_revision(path: &str) -> Option<String> {
    let metadata = std::fs::metadata(Path::new(path)).ok()?;
    if !metadata.is_file() {
        return None;
    }
    let size = i64::try_from(metadata.len()).ok()?;
    Some(source_revision(size, foundation_mtime(&metadata)?))
}

fn normalized_embedding(blob: &[u8]) -> bool {
    if blob.len() != DIM * 4 {
        return false;
    }
    let mut sum = 0.0f64;
    for chunk in blob.as_chunks::<4>().0 {
        let value = f32::from_le_bytes(*chunk);
        if !value.is_finite() {
            return false;
        }
        sum += f64::from(value) * f64::from(value);
    }
    (0.95..=1.05).contains(&sum)
}

fn decode(blob: &[u8]) -> Vec<f32> {
    blob.as_chunks::<4>().0.iter().map(|chunk| f32::from_le_bytes(*chunk)).collect()
}

fn encode(vector: &[f32]) -> Vec<u8> {
    vector.iter().flat_map(|value| value.to_le_bytes()).collect()
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut sum = 0.0f32;
    for index in 0..a.len().min(b.len()) {
        sum += a[index] * b[index];
    }
    sum
}

// MARK: - Selected partition

#[derive(Debug, Clone, PartialEq)]
struct FaceRow {
    id: i64,
    blob: Vec<u8>,
    quality: f64,
    held: bool,
}

#[derive(Debug, Clone, PartialEq)]
struct Snapshot {
    id: i64,
    file_id: i64,
    bbox: String,
    path: String,
    size: i64,
    modified: f64,
    file_ref: Option<i64>,
    person_id: Option<i64>,
    model: String,
    processing: String,
    revision: String,
}

#[derive(Debug, Clone, PartialEq)]
struct Partition {
    rows: Vec<FaceRow>,
    snapshots: Vec<Snapshot>,
    deferred: u64,
    overflowed: bool,
}

fn load_partition(conn: &Connection, space: &Space, skip: &HashSet<i64>, min_quality: f64, limit: usize) -> rusqlite::Result<Partition> {
    let mut statement = conn.prepare(
        "SELECT fp.id,fp.file_id,fp.person_id,fp.bbox,fp.face_quality,fp.excluded, \
                CASE WHEN LENGTH(fp.arcface_embedding)=512 THEN fp.arcface_embedding END, \
                fp.embedding_model,fp.processing_version,fp.source_revision, \
                r.revision,f.path_text,f.size_bytes,f.modified_at,f.file_ref,f.failed \
         FROM face_prints fp LEFT JOIN persons p ON p.id=fp.person_id \
         LEFT JOIN files f ON f.id=fp.file_id LEFT JOIN catalog_revisions r ON r.file_id=fp.file_id \
         WHERE COALESCE(p.is_unknown,0)=0 ORDER BY fp.id",
    )?;
    let mut cursor = statement.query([])?;
    let mut partition = Partition { rows: Vec::new(), snapshots: Vec::new(), deferred: 0, overflowed: false };
    let mut revisions: HashMap<String, Option<String>> = HashMap::new();
    while let Some(row) = cursor.next()? {
        let id: i64 = row.get(0)?;
        if skip.contains(&id) {
            continue;
        }
        let quality = row.get::<_, Option<f64>>(4)?.unwrap_or(-1.0);
        let embedding: Option<Vec<u8>> = row.get(6)?;
        let model: Option<String> = row.get(7)?;
        let processing: Option<String> = row.get(8)?;
        let revision: Option<String> = row.get(9)?;
        let catalog_revision: Option<String> = row.get(10)?;
        let path: Option<String> = row.get(11)?;
        let size: Option<i64> = row.get(12)?;
        let modified: Option<f64> = row.get(13)?;
        let failed: Option<i64> = row.get(15)?;
        let current = quality.is_finite()
            && (min_quality <= 0.0 || quality >= min_quality)
            && failed == Some(0)
            && path.as_deref().is_some_and(|p| !p.is_empty())
            && is_current(embedding.as_deref(), model.as_deref(), processing.as_deref(), revision.as_deref(), catalog_revision.as_deref(), size, modified, space);
        if !current {
            partition.deferred += 1;
            continue;
        }
        let (Some(path), Some(size), Some(modified), Some(embedding), Some(model), Some(processing), Some(revision)) =
            (path, size, modified, embedding, model, processing, revision)
        else {
            partition.deferred += 1;
            continue;
        };
        let actual = revisions.entry(path.clone()).or_insert_with(|| current_source_revision(&path));
        if actual.as_deref() != Some(revision.as_str()) {
            partition.deferred += 1;
            continue;
        }
        if partition.rows.len() >= limit {
            partition.deferred += 1;
            partition.overflowed = true;
            continue;
        }
        let excluded = row.get::<_, Option<i64>>(5)?.unwrap_or(0);
        partition.rows.push(FaceRow { id, blob: embedding, quality, held: excluded != 0 });
        partition.snapshots.push(Snapshot {
            id,
            file_id: row.get(1)?,
            bbox: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
            path,
            size,
            modified,
            file_ref: row.get(14)?,
            person_id: row.get(2)?,
            model,
            processing,
            revision,
        });
    }
    Ok(partition)
}

#[allow(clippy::too_many_arguments)]
fn is_current(
    embedding: Option<&[u8]>,
    model: Option<&str>,
    processing: Option<&str>,
    revision: Option<&str>,
    catalog_revision: Option<&str>,
    size: Option<i64>,
    modified: Option<f64>,
    space: &Space,
) -> bool {
    if space.model.is_empty() || space.model.chars().count() > 150 || space.processing.is_empty() {
        return false;
    }
    if model != Some(space.model.as_str()) || processing != Some(space.processing.as_str()) {
        return false;
    }
    let (Some(size), Some(modified), Some(revision)) = (size, modified, revision) else { return false };
    size >= 0
        && modified.is_finite()
        && Some(revision) == catalog_revision
        && revision == source_revision(size, modified)
        && embedding.is_some_and(normalized_embedding)
}

fn require_unchanged(conn: &Connection, original: &Partition, space: &Space, skip: &HashSet<i64>, min_quality: f64) -> Result<u64> {
    let current = load_partition(conn, space, skip, min_quality, MAX_FACES_PER_RUN)?;
    ensure!(
        !current.overflowed && current.rows == original.rows && current.snapshots == original.snapshots,
        "the face evidence changed during clustering"
    );
    Ok(current.deferred)
}

// MARK: - Identities and verdicts

#[derive(Debug, Clone, PartialEq)]
struct Prior {
    id: i64,
    radius: Option<f32>,
    faces: BTreeSet<i64>,
    title: Option<String>,
    first: Option<String>,
    middle: Option<String>,
    last: Option<String>,
    suffix: Option<String>,
    legacy: Option<String>,
    unknown: bool,
}

impl Prior {
    fn has_name(&self) -> bool {
        self.unknown
            || [&self.title, &self.first, &self.middle, &self.last, &self.suffix, &self.legacy]
                .iter()
                .any(|field| field.as_deref().is_some_and(|text| !text.is_empty()))
    }
}

fn priors(conn: &Connection) -> rusqlite::Result<Vec<Prior>> {
    let mut faces: HashMap<i64, BTreeSet<i64>> = HashMap::new();
    {
        let mut statement = conn.prepare("SELECT id, person_id FROM face_prints WHERE person_id IS NOT NULL")?;
        let rows = statement.query_map([], |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, Option<i64>>(1)?)))?;
        for row in rows {
            let (face, person) = row?;
            let (face, person) = (face.unwrap_or(0), person.unwrap_or(0));
            if face != 0 && person != 0 {
                faces.entry(person).or_default().insert(face);
            }
        }
    }
    let mut statement = conn.prepare(
        "SELECT id, anchor_radius, title, first_name, middle_name, last_name, suffix, name, is_unknown FROM persons ORDER BY id",
    )?;
    let rows = statement.query_map([], |row| {
        let id: i64 = row.get::<_, Option<i64>>(0)?.unwrap_or(0);
        Ok(Prior {
            id,
            radius: row.get::<_, Option<f64>>(1)?.map(|value| value as f32),
            faces: BTreeSet::new(),
            title: row.get(2)?,
            first: row.get(3)?,
            middle: row.get(4)?,
            last: row.get(5)?,
            suffix: row.get(6)?,
            legacy: row.get(7)?,
            unknown: row.get::<_, Option<i64>>(8)?.unwrap_or(0) != 0,
        })
    })?;
    let mut out: Vec<Prior> = rows.collect::<rusqlite::Result<_>>()?;
    for prior in &mut out {
        prior.faces = faces.remove(&prior.id).unwrap_or_default();
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Pair(i64, i64);

impl Pair {
    fn new(a: i64, b: i64) -> Self {
        Self(a.min(b), a.max(b))
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Verdict {
    pair: Pair,
    same: bool,
}

type VerdictRow = (Option<i64>, Option<i64>, Option<i64>, Option<String>, Option<i64>, Option<String>, i64);

fn verdicts(conn: &Connection, participants: Option<&HashSet<i64>>) -> Result<Vec<Verdict>> {
    let rows: Vec<VerdictRow> = {
        let mut statement = conn.prepare(
            "SELECT face_a,face_b,file_a,bbox_a,file_b,bbox_b,same_person FROM face_verifications \
             WHERE (face_a IS NOT NULL AND face_b IS NOT NULL) \
                OR (file_a IS NOT NULL AND bbox_a IS NOT NULL AND file_b IS NOT NULL AND bbox_b IS NOT NULL) \
             ORDER BY person_a,person_b LIMIT 100001",
        )?;
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    ensure!(rows.len() <= 100_000, "too many face verdicts");
    let resolve = |legacy: Option<i64>, file: Option<i64>, bbox: Option<&str>| -> Result<Option<i64>> {
        if let (Some(file), Some(bbox)) = (file, bbox) {
            let mut statement = conn.prepare_cached("SELECT id FROM face_prints WHERE file_id=?1 AND bbox=?2 ORDER BY id")?;
            let ids: Vec<i64> = statement.query_map(params![file, bbox], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?;
            if ids.len() > 1 {
                if participants.is_none_or(|set| ids.iter().any(|id| set.contains(id))) {
                    bail!("a face verdict matches more than one face");
                }
                return Ok(None);
            }
            if let Some(id) = ids.first() {
                return Ok(Some(*id));
            }
        }
        let Some(legacy) = legacy.filter(|id| *id != 0) else { return Ok(None) };
        Ok(conn.query_row("SELECT id FROM face_prints WHERE id=?1", [legacy], |row| row.get(0)).optional()?)
    };
    let mut out = Vec::new();
    for (face_a, face_b, file_a, bbox_a, file_b, bbox_b, same) in rows {
        let Some(a) = resolve(face_a, file_a, bbox_a.as_deref())? else { continue };
        let Some(b) = resolve(face_b, file_b, bbox_b.as_deref())? else { continue };
        if a == b {
            if same == 0 && participants.is_none_or(|set| set.contains(&a)) {
                bail!("a different-people verdict names one face twice");
            }
            continue;
        }
        out.push(Verdict { pair: Pair::new(a, b), same: same != 0 });
    }
    Ok(out)
}

// MARK: - Clustering pass

fn hyperparameters() -> (Hyperparameters, bool) {
    (
        Hyperparameters {
            pass1_cosine: env_cos("FILEID_FACE_PASS1_COS", 0.66),
            pass2_cosine: 0.54,
            pass2_margin: 0.10,
            pass3_variance_threshold: 0.04,
            pass3_min_mean_cosine: 0.60,
            pass3_max_splits: 7,
            k_nn: env_int("FILEID_FACE_KNN", 10, 4..=256),
        },
        env_bool("FILEID_FACE_MUTUAL_KNN", false),
    )
}

fn compare(a: &Neighbor, b: &Neighbor) -> std::cmp::Ordering {
    b.similarity.total_cmp(&a.similarity).then_with(|| a.idx.cmp(&b.idx))
}

/// Cosine as the Swift engine derives it from its HNSW index: 1 − L2²/2.
fn l2_cosine(a: &[f32], b: &[f32]) -> f32 {
    let mut squared = 0.0f32;
    for index in 0..a.len().min(b.len()) {
        let delta = a[index] - b[index];
        squared += delta * delta;
    }
    let distance = squared.sqrt();
    1.0 - distance * distance / 2.0
}

/// Dense cluster id → dense indexes, through the shared identity pipeline with
/// the Swift engine's calibration (exact kNN up to 5,000 faces, HNSW above).
fn cluster(vectors: &[Vec<f32>], files: &[i64]) -> BTreeMap<usize, Vec<usize>> {
    let (params, mutual) = hyperparameters();
    let k = params.k_nn + 1;
    let mut by_file: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
    for (index, file) in files.iter().enumerate() {
        by_file.entry(*file).or_default().push(index);
    }
    let mut cannot_links = Vec::new();
    for members in by_file.values() {
        for (offset, &a) in members.iter().enumerate() {
            for &b in &members[offset + 1..] {
                cannot_links.push((a, b));
            }
        }
    }
    let index = (vectors.len() > BRUTE_FORCE_MAX).then(|| {
        crate::util::hnsw_index::build_with_search_limit(vectors.iter().cloned().enumerate().map(|(i, v)| (v, i)).collect(), 50)
    });
    let mut search = crate::util::hnsw_index::Searcher::default();
    let result = identity_clustering::cluster_with_options(
        vectors,
        |i| {
            let mut hits: Vec<Neighbor> = match &index {
                Some(index) => search
                    .top_k(index, &vectors[i], k)
                    .into_iter()
                    .map(|(j, squared)| Neighbor { idx: j, similarity: 1.0 - squared / 2.0 })
                    .collect(),
                None => (0..vectors.len()).map(|j| Neighbor { idx: j, similarity: l2_cosine(&vectors[i], &vectors[j]) }).collect(),
            };
            if hits.len() > k {
                hits.select_nth_unstable_by(k, compare);
                hits.truncate(k);
            }
            hits.sort_by(compare);
            hits.retain(|hit| hit.idx != i);
            hits
        },
        params,
        &cannot_links,
        mutual,
    );
    tracing::info!(
        faces = vectors.len(),
        clusters = result.cluster_count,
        cores = result.core_count,
        outliers_assigned = result.outliers_assigned,
        outliers_as_singletons = result.outliers_as_singletons,
        splits = result.splits_applied,
        "[FACES] macOS identity clustering pass"
    );
    let mut out: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (dense, cluster) in result.cluster_ids.iter().enumerate() {
        out.entry(*cluster).or_default().push(dense);
    }
    out
}

// MARK: - Persist

#[derive(Debug, Default)]
struct Stats {
    person_count: i64,
    suppressed_faces: usize,
    truncated_faces: usize,
    deferred: u64,
}

fn centroid(members: &[usize], vectors: &[Vec<f32>]) -> Vec<f32> {
    let mut sum = vec![0.0f32; DIM];
    for &member in members {
        for (d, value) in sum.iter_mut().enumerate() {
            *value += vectors[member][d];
        }
    }
    let mut norm = 0.0f32;
    for value in &sum {
        norm += value * value;
    }
    let inverse = 1.0 / norm.sqrt().max(f32::from_bits(1));
    for value in &mut sum {
        *value *= inverse;
    }
    sum
}

fn anchor_radius(members: &[usize], vectors: &[Vec<f32>], center: &[f32]) -> f32 {
    if members.len() < 2 {
        return 0.50;
    }
    let mut sims: Vec<f32> = members.iter().map(|&member| dot(&vectors[member], center)).collect();
    sims.sort_by(f32::total_cmp);
    let index = ((sims.len() as f32 * 0.10).floor() as usize).min(sims.len() - 1);
    sims[index].clamp(0.45, 0.85)
}

fn representative(faces: &[i64], quality: &HashMap<i64, f64>) -> i64 {
    let Some(&first) = faces.first() else { return 0 };
    let (mut best, mut best_quality) = (first, quality.get(&first).copied().unwrap_or(-1.0));
    for &face in &faces[1..] {
        let value = quality.get(&face).copied().unwrap_or(-1.0);
        if value > best_quality {
            best = face;
            best_quality = value;
        }
    }
    if best_quality < 0.0 { first } else { best }
}

struct Protected {
    clusters: BTreeMap<usize, Vec<usize>>,
    faces: HashSet<i64>,
}

fn partition_protected(
    raw: &BTreeMap<usize, Vec<usize>>,
    face_ids: &[i64],
    bucket_owner: &HashMap<i64, i64>,
    different: &HashSet<Pair>,
    excluded: &HashSet<i64>,
) -> Protected {
    let dense_by_face: HashMap<i64, usize> = face_ids.iter().enumerate().map(|(dense, face)| (*face, dense)).collect();
    let ownerless: BTreeSet<i64> = different
        .iter()
        .flat_map(|pair| [pair.0, pair.1])
        .filter(|face| !bucket_owner.contains_key(face) && !excluded.contains(face))
        .collect();
    let mut owner_groups: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
    for (face, owner) in bucket_owner {
        if excluded.contains(face) {
            continue;
        }
        if let Some(&dense) = dense_by_face.get(face) {
            owner_groups.entry(*owner).or_default().push(dense);
        }
    }
    let mut singletons: HashMap<i64, BTreeSet<i64>> = HashMap::new();
    for pair in different {
        if let (Some(a), Some(b)) = (bucket_owner.get(&pair.0), bucket_owner.get(&pair.1))
            && a == b
        {
            singletons.entry(*a).or_default().extend([pair.0, pair.1]);
        }
    }
    let mut buckets: Vec<Vec<usize>> = Vec::new();
    for (owner, members) in &owner_groups {
        let alone = singletons.get(owner).cloned().unwrap_or_default();
        let remainder: Vec<usize> = members.iter().copied().filter(|dense| !alone.contains(&face_ids[*dense])).collect();
        if !remainder.is_empty() {
            buckets.push(remainder);
        }
        for face in &alone {
            if let Some(&dense) = dense_by_face.get(face) {
                buckets.push(vec![dense]);
            }
        }
    }
    for face in &ownerless {
        if let Some(&dense) = dense_by_face.get(face) {
            buckets.push(vec![dense]);
        }
    }
    for members in raw.values() {
        let kept: Vec<usize> = members
            .iter()
            .copied()
            .filter(|dense| {
                let face = face_ids[*dense];
                !excluded.contains(&face) && !bucket_owner.contains_key(&face) && !ownerless.contains(&face)
            })
            .collect();
        if !kept.is_empty() {
            buckets.push(kept);
        }
    }
    let mut clusters = BTreeMap::new();
    for (id, mut members) in buckets.into_iter().enumerate() {
        members.sort_by_key(|dense| face_ids[*dense]);
        clusters.insert(id, members);
    }
    let mut faces: HashSet<i64> = bucket_owner.keys().copied().collect();
    for pair in different {
        faces.insert(pair.0);
        faces.insert(pair.1);
    }
    faces.retain(|face| !excluded.contains(face) && dense_by_face.contains_key(face));
    Protected { clusters, faces }
}

fn suppress(
    clusters: BTreeMap<usize, Vec<usize>>,
    face_ids: &[i64],
    quality: &HashMap<i64, f64>,
    min_size: usize,
    floor: f64,
    keep: &HashSet<usize>,
) -> (BTreeMap<usize, Vec<usize>>, usize) {
    if floor <= 0.0 {
        return (clusters, 0);
    }
    let mut kept = BTreeMap::new();
    let mut suppressed = 0;
    for (id, members) in clusters {
        let best = members.iter().map(|dense| quality.get(&face_ids[*dense]).copied().unwrap_or(-1.0)).fold(f64::MIN, f64::max);
        if keep.contains(&id) || members.len() >= min_size || best >= floor {
            kept.insert(id, members);
        } else {
            suppressed += members.len();
        }
    }
    (kept, suppressed)
}

struct Attach<'a> {
    vectors: &'a [Vec<f32>],
    held: &'a [bool],
    files: &'a [i64],
    face_ids: &'a [i64],
    different: &'a HashSet<Pair>,
    min_size: usize,
}

impl Attach<'_> {
    fn run(&self, mut clusters: BTreeMap<usize, Vec<usize>>, candidates: &[usize]) -> BTreeMap<usize, Vec<usize>> {
        let (min_cos, min_margin) = (env_cos("FILEID_FACE_ATTACH_COS", 0.55), env_cos("FILEID_FACE_ATTACH_MARGIN", 0.05));
        if self.vectors.first().is_none_or(Vec::is_empty) || candidates.is_empty() || min_cos >= 1.0 {
            return clusters;
        }
        let mut targets: Vec<(usize, Vec<f32>)> = Vec::new();
        let mut files: HashMap<usize, HashSet<i64>> = HashMap::new();
        let mut faces: HashMap<usize, HashSet<i64>> = HashMap::new();
        for (id, members) in &clusters {
            files.insert(*id, members.iter().map(|dense| self.files[*dense]).collect());
            faces.insert(*id, members.iter().map(|dense| self.face_ids[*dense]).collect());
            let anchors: Vec<usize> = members.iter().copied().filter(|dense| !self.held[*dense]).collect();
            if anchors.len() >= self.min_size.max(1) {
                targets.push((*id, centroid(&anchors, self.vectors)));
            }
        }
        if targets.is_empty() {
            return clusters;
        }
        let mut blocked: HashMap<i64, HashSet<i64>> = HashMap::new();
        for pair in self.different {
            blocked.entry(pair.0).or_default().insert(pair.1);
            blocked.entry(pair.1).or_default().insert(pair.0);
        }
        let empty = HashSet::new();
        let mut ordered = candidates.to_vec();
        ordered.sort_unstable();
        for dense in ordered {
            if self.vectors[dense].len() != DIM {
                continue;
            }
            let barred = blocked.get(&self.face_ids[dense]).unwrap_or(&empty);
            let (mut best, mut best_sim, mut second) = (None, -2.0f32, -2.0f32);
            for (id, center) in &targets {
                if files[id].contains(&self.files[dense]) || !barred.is_disjoint(&faces[id]) {
                    continue;
                }
                let sim = dot(&self.vectors[dense], center);
                if sim > best_sim {
                    second = best_sim;
                    best = Some(*id);
                    best_sim = sim;
                } else if sim > second {
                    second = sim;
                }
            }
            let Some(id) = best else { continue };
            if best_sim < min_cos || best_sim - second < min_margin {
                continue;
            }
            clusters.entry(id).or_default().push(dense);
            files.get_mut(&id).map(|set| set.insert(self.files[dense]));
            faces.get_mut(&id).map(|set| set.insert(self.face_ids[dense]));
        }
        clusters
    }
}

fn cap(
    clusters: BTreeMap<usize, Vec<usize>>,
    protected: &HashSet<usize>,
    preserved: usize,
) -> Result<(BTreeMap<usize, Vec<usize>>, usize)> {
    ensure!(preserved <= MAX_PERSONS, "too many protected people");
    let slots = MAX_PERSONS - preserved;
    let protected_ids: HashSet<usize> = protected.iter().copied().filter(|id| clusters.contains_key(id)).collect();
    ensure!(protected_ids.len() <= slots, "too many protected people");
    if clusters.len() <= slots {
        return Ok((clusters, 0));
    }
    let mut unprotected: Vec<(&usize, &Vec<usize>)> = clusters.iter().filter(|(id, _)| !protected_ids.contains(id)).collect();
    unprotected.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(b.0)));
    let keep: HashSet<usize> = unprotected.iter().take(slots - protected_ids.len()).map(|(id, _)| **id).collect();
    let mut truncated = 0;
    let mut kept = BTreeMap::new();
    for (id, members) in clusters {
        if protected_ids.contains(&id) || keep.contains(&id) {
            kept.insert(id, members);
        } else {
            truncated += members.len();
        }
    }
    Ok((kept, truncated))
}

/// Stable person ids (`StablePersonAssignments.resolve`): a cluster keeps the
/// person that owned at least half of that person's faces, largest overlap first.
fn stable_ids(priors: &[(i64, Vec<i64>)], clusters: &[(Vec<i64>, i64)], fixed: &[Option<i64>], preserved: &HashSet<i64>) -> Result<Vec<Option<i64>>> {
    ensure!(fixed.len() == clusters.len(), "invalid partition");
    let mut owner: HashMap<i64, i64> = HashMap::new();
    let mut sizes: HashMap<i64, usize> = HashMap::new();
    for (id, faces) in priors {
        ensure!(*id > 0 && !sizes.contains_key(id), "invalid partition");
        sizes.insert(*id, faces.len());
        for face in faces {
            ensure!(*face > 0 && owner.insert(*face, *id).is_none(), "invalid partition");
        }
    }
    let mut selected = fixed.to_vec();
    let mut claimed: HashSet<i64> = HashSet::new();
    let mut seen: HashSet<i64> = HashSet::new();
    let mut edges: Vec<(usize, bool, i64, usize)> = Vec::new();
    for (index, (faces, rep)) in clusters.iter().enumerate() {
        ensure!(!faces.is_empty() && faces.contains(rep), "invalid partition");
        let mut counts: HashMap<i64, usize> = HashMap::new();
        for face in faces {
            ensure!(*face > 0 && seen.insert(*face), "invalid partition");
            if let Some(person) = owner.get(face) {
                ensure!(!preserved.contains(person), "invalid partition");
                *counts.entry(*person).or_default() += 1;
            }
        }
        if let Some(person) = fixed[index] {
            ensure!(
                sizes.contains_key(&person) && !preserved.contains(&person) && claimed.insert(person) && counts.get(&person).copied().unwrap_or(0) > 0,
                "invalid partition"
            );
        } else {
            for (person, overlap) in counts {
                if overlap >= sizes.get(&person).copied().unwrap_or(0).div_ceil(2).max(1) {
                    edges.push((overlap, owner.get(rep) == Some(&person), person, index));
                }
            }
        }
    }
    edges.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)).then(a.3.cmp(&b.3)));
    for (_, _, person, cluster) in edges {
        if selected[cluster].is_none() && !claimed.contains(&person) {
            selected[cluster] = Some(person);
            claimed.insert(person);
        }
    }
    Ok(selected)
}

struct Planned {
    person: Option<i64>,
    faces: Vec<i64>,
    representative: i64,
    centroid: Vec<f32>,
    radius: f32,
}

/// `FaceClusterPersistence.apply`: reuses or creates person rows, never deletes.
fn apply(tx: &Connection, clusters: &[Planned], pool: &HashSet<i64>, preserved: &HashSet<i64>, now: f64) -> Result<i64> {
    ensure!(now.is_finite(), "invalid plan");
    let mut seen: HashSet<i64> = HashSet::new();
    let mut reused: HashSet<i64> = HashSet::new();
    for cluster in clusters {
        ensure!(
            !cluster.faces.is_empty()
                && cluster.faces.contains(&cluster.representative)
                && cluster.centroid.len() == DIM
                && cluster.centroid.iter().all(|value| value.is_finite())
                && cluster.radius.is_finite(),
            "invalid plan"
        );
        for face in &cluster.faces {
            ensure!(*face > 0 && pool.contains(face) && seen.insert(*face), "invalid plan");
        }
        if let Some(person) = cluster.person {
            let live: i64 =
                tx.query_row("SELECT COUNT(*) FROM persons WHERE id=?1 AND COALESCE(is_unknown,0)=0", [person], |row| row.get(0))?;
            ensure!(person > 0 && !preserved.contains(&person) && reused.insert(person) && live == 1, "invalid plan");
        }
    }
    let existing: i64 = tx.query_row("SELECT COUNT(*) FROM persons", [], |row| row.get(0))?;
    let created = clusters.iter().filter(|cluster| cluster.person.is_none()).count();
    ensure!(existing as usize + created <= MAX_PERSONS, "too many people");
    let selected = face_review::json_ids(pool);
    let protected = face_review::json_ids(preserved);
    let output = face_review::json_ids(&seen);
    let free: i64 = tx.query_row(
        "SELECT COUNT(*) FROM face_prints WHERE id IN (SELECT value FROM json_each(?1)) \
           AND (person_id IS NULL OR person_id NOT IN (SELECT value FROM json_each(?2)))",
        params![output, protected],
        |row| row.get(0),
    )?;
    ensure!(free as usize == seen.len(), "invalid plan");
    tx.execute(
        "UPDATE face_prints SET person_id=NULL WHERE id IN (SELECT value FROM json_each(?1)) \
           AND (person_id IS NULL OR person_id NOT IN (SELECT value FROM json_each(?2)))",
        params![selected, protected],
    )?;
    for cluster in clusters {
        let blob = encode(&cluster.centroid);
        let radius = f64::from(cluster.radius);
        let person = match cluster.person {
            Some(id) => {
                tx.execute(
                    "UPDATE persons SET representative_face_id=?1,centroid=?2,anchor_radius=?3,last_clustered_at=?4 WHERE id=?5",
                    params![cluster.representative, blob, radius, now, id],
                )?;
                id
            }
            None => {
                tx.execute(
                    "INSERT INTO persons(representative_face_id,file_count,created_at,centroid,anchor_radius,last_clustered_at) VALUES(?1,0,?2,?3,?4,?5)",
                    params![cluster.representative, now, blob, radius, now],
                )?;
                tx.last_insert_rowid()
            }
        };
        tx.execute(
            "UPDATE face_prints SET person_id=?1 WHERE id IN (SELECT value FROM json_each(?2))",
            params![person, face_review::json_ids(&cluster.faces)],
        )?;
    }
    tx.execute("UPDATE persons SET file_count=(SELECT COUNT(DISTINCT file_id) FROM face_prints WHERE person_id=persons.id)", [])?;
    tx.execute(
        "UPDATE persons SET representative_face_id=(SELECT id FROM face_prints WHERE person_id=persons.id ORDER BY id LIMIT 1) \
         WHERE representative_face_id IS NULL OR representative_face_id NOT IN (SELECT id FROM face_prints WHERE person_id=persons.id)",
        [],
    )?;
    Ok(tx.query_row("SELECT COUNT(*) FROM persons", [], |row| row.get(0))?)
}

/// Prior identity per new cluster by face overlap (at least half of the prior's faces).
fn match_priors(clusters: &[Vec<i64>], candidates: &[Prior]) -> Vec<Option<i64>> {
    let mut matches = vec![None; clusters.len()];
    let sets: Vec<HashSet<i64>> = clusters.iter().map(|faces| faces.iter().copied().collect()).collect();
    let mut by_cluster: HashMap<usize, (i64, usize)> = HashMap::new();
    for prior in candidates.iter().filter(|prior| prior.has_name() && !prior.faces.is_empty()) {
        let (mut best, mut count) = (None, 0);
        for (index, set) in sets.iter().enumerate() {
            let overlap = prior.faces.iter().filter(|face| set.contains(face)).count();
            if overlap > count {
                count = overlap;
                best = Some(index);
            }
        }
        let Some(best) = best else { continue };
        if count < prior.faces.len().div_ceil(2).max(1) {
            continue;
        }
        match by_cluster.get(&best) {
            Some((_, other)) if count <= *other => {}
            _ => {
                by_cluster.insert(best, (prior.id, count));
            }
        }
    }
    for (index, (prior, _)) in by_cluster {
        matches[index] = Some(prior);
    }
    matches
}

#[allow(clippy::too_many_arguments)]
fn persist(
    tx: &Connection,
    partition: &Partition,
    space: &Space,
    raw: &BTreeMap<usize, Vec<usize>>,
    skip: &HashSet<i64>,
    min_quality: f64,
    min_size: usize,
    solo_floor: f64,
) -> Result<Stats> {
    let deferred = require_unchanged(tx, partition, space, skip, min_quality)?;
    let face_ids: Vec<i64> = partition.rows.iter().map(|row| row.id).collect();
    let vectors: Vec<Vec<f32>> = partition.rows.iter().map(|row| decode(&row.blob)).collect();
    let held: Vec<bool> = partition.rows.iter().map(|row| row.held).collect();
    let files: Vec<i64> = partition.snapshots.iter().map(|snapshot| snapshot.file_id).collect();
    let quality: HashMap<i64, f64> = partition.rows.iter().map(|row| (row.id, row.quality)).collect();
    let rep_quality: HashMap<i64, f64> = partition.rows.iter().map(|row| (row.id, if row.held { -1.0 } else { row.quality })).collect();
    let pool: HashSet<i64> = face_ids.iter().copied().collect();
    let fresh = priors(tx)?;
    let verdicts = verdicts(tx, Some(&pool))?;
    let different: HashSet<Pair> = verdicts.iter().filter(|v| !v.same).map(|v| v.pair).collect();
    let mut owner_by_face: HashMap<i64, i64> = HashMap::new();
    for prior in &fresh {
        for face in &prior.faces {
            owner_by_face.insert(*face, prior.id);
        }
    }
    let verdict_owners: HashSet<i64> =
        verdicts.iter().flat_map(|v| [owner_by_face.get(&v.pair.0), owner_by_face.get(&v.pair.1)]).flatten().copied().collect();
    let named: HashSet<i64> = fresh.iter().filter(|p| p.has_name() && !p.unknown).map(|p| p.id).collect();
    let partial: HashSet<i64> = fresh
        .iter()
        .filter(|p| p.faces.iter().any(|f| pool.contains(f)) && !p.faces.iter().all(|f| pool.contains(f)))
        .map(|p| p.id)
        .collect();
    let protected_owners: HashSet<i64> = named.iter().chain(&verdict_owners).chain(&partial).copied().collect();
    let outside_pool: HashSet<i64> = fresh
        .iter()
        .filter(|p| protected_owners.contains(&p.id) && !p.faces.iter().any(|f| pool.contains(f)))
        .map(|p| p.id)
        .collect();
    let preserve: HashSet<i64> = fresh.iter().filter(|p| p.unknown).map(|p| p.id).chain(outside_pool.iter().copied()).collect();
    let preserved_count = fresh.iter().filter(|p| preserve.contains(&p.id)).count();
    let excluded: HashSet<i64> = fresh.iter().filter(|p| preserve.contains(&p.id)).flat_map(|p| p.faces.iter().copied()).collect();
    let active: HashSet<i64> = protected_owners.difference(&preserve).copied().collect();
    let bucket_owner: HashMap<i64, i64> =
        owner_by_face.iter().filter(|(face, owner)| active.contains(owner) && pool.contains(face)).map(|(f, o)| (*f, *o)).collect();
    let identity_owner = bucket_owner.clone();
    let protected = partition_protected(raw, &face_ids, &bucket_owner, &different, &excluded);
    let protected_clusters: HashSet<usize> = protected
        .clusters
        .iter()
        .filter(|(_, members)| members.iter().any(|dense| protected.faces.contains(&face_ids[*dense])))
        .map(|(id, _)| *id)
        .collect();
    let (kept, suppressed_faces) = suppress(protected.clusters, &face_ids, &quality, min_size, solo_floor, &protected_clusters);
    let assigned: HashSet<usize> = kept.values().flatten().copied().collect();
    let candidates: Vec<usize> = (0..face_ids.len())
        .filter(|dense| !assigned.contains(dense) && !excluded.contains(&face_ids[*dense]) && !protected.faces.contains(&face_ids[*dense]))
        .collect();
    let attach = Attach { vectors: &vectors, held: &held, files: &files, face_ids: &face_ids, different: &different, min_size };
    let attached = attach.run(kept, &candidates);
    let (finals, truncated_faces) = cap(attached, &protected_clusters, preserved_count)?;

    let mut cluster_of: HashMap<i64, usize> = HashMap::new();
    for (id, members) in &finals {
        for dense in members {
            cluster_of.insert(face_ids[*dense], *id);
        }
    }
    let mut owner_cluster: HashMap<usize, i64> = HashMap::new();
    for (face, owner) in &identity_owner {
        let Some(cluster) = cluster_of.get(face) else { continue };
        if owner_cluster.insert(*cluster, *owner).is_some_and(|existing| existing != *owner) {
            bail!("distinct named identities share a cluster");
        }
    }
    for pair in &different {
        if let (Some(a), Some(b)) = (cluster_of.get(&pair.0), cluster_of.get(&pair.1))
            && a == b
        {
            bail!("different-people anchors share a cluster");
        }
    }

    let next: Vec<(Vec<f32>, f32, Vec<i64>, i64)> = finals
        .values()
        .map(|members| {
            let center = centroid(members, &vectors);
            let radius = anchor_radius(members, &vectors, &center);
            let faces: Vec<i64> = members.iter().map(|dense| face_ids[*dense]).collect();
            let rep = representative(&faces, &rep_quality);
            (center, radius, faces, rep)
        })
        .collect();
    let inheritance: Vec<Prior> = fresh
        .iter()
        .filter(|p| !preserve.contains(&p.id))
        .map(|p| Prior { faces: p.faces.iter().copied().filter(|f| pool.contains(f)).collect(), ..p.clone() })
        .collect();
    let cluster_faces: Vec<Vec<i64>> = next.iter().map(|cluster| cluster.2.clone()).collect();
    let mut fixed = match_priors(&cluster_faces, &inheritance);
    let positive: Vec<(i64, i64)> = verdicts
        .iter()
        .filter(|v| v.same)
        .filter_map(|v| {
            let (a, b) = (v.pair.0, v.pair.1);
            if pool.contains(&a) && !pool.contains(&b) && let Some(owner) = owner_by_face.get(&b) && owner_by_face.get(&a) == Some(owner) {
                return Some((a, *owner));
            }
            if pool.contains(&b) && !pool.contains(&a) && let Some(owner) = owner_by_face.get(&a) && owner_by_face.get(&b) == Some(owner) {
                return Some((b, *owner));
            }
            None
        })
        .collect();
    let mut owners: Vec<i64> = protected_owners.difference(&preserve).copied().collect();
    owners.sort_unstable();
    for owner in owners {
        let count = |index: usize| next[index].2.iter().filter(|face| owner_by_face.get(face) == Some(&owner)).count();
        let candidates: Vec<usize> = (0..next.len()).filter(|index| count(*index) > 0).collect();
        let linked: Vec<usize> = candidates
            .iter()
            .copied()
            .filter(|index| positive.iter().any(|(face, linked)| *linked == owner && next[*index].2.contains(face)))
            .collect();
        ensure!(linked.len() <= 1, "conflicting positive identity links");
        let chosen = linked.first().copied().or_else(|| {
            let mut best: Option<(usize, usize)> = None;
            for &index in &candidates {
                let value = count(index);
                if best.is_none_or(|(_, top)| value > top) {
                    best = Some((index, value));
                }
            }
            best.map(|(index, _)| index)
        });
        let Some(chosen) = chosen else { continue };
        for slot in &mut fixed {
            if *slot == Some(owner) {
                *slot = None;
            }
        }
        ensure!(fixed[chosen].is_none_or(|current| current == owner), "identity claim collision");
        fixed[chosen] = Some(owner);
    }

    let planned_faces: HashSet<i64> = next.iter().flat_map(|cluster| cluster.2.iter().copied()).collect();
    let mut reps = HashSet::new();
    for cluster in &next {
        ensure!(!cluster.2.is_empty() && cluster.2.contains(&cluster.3) && reps.insert(cluster.3), "invalid persistence anchor");
    }
    ensure!(planned_faces.len() == next.iter().map(|cluster| cluster.2.len()).sum::<usize>(), "duplicate persistence member");
    let live: i64 = tx.query_row(
        "SELECT COUNT(*) FROM face_prints WHERE id IN (SELECT value FROM json_each(?1))",
        [face_review::json_ids(&planned_faces)],
        |row| row.get(0),
    )?;
    ensure!(live as usize == planned_faces.len(), "persistence plan references stale faces");

    let scoped: Vec<(i64, Vec<i64>)> =
        fresh.iter().map(|p| (p.id, p.faces.iter().copied().filter(|f| pool.contains(f)).collect())).collect();
    let shapes: Vec<(Vec<i64>, i64)> = next.iter().map(|cluster| (cluster.2.clone(), cluster.3)).collect();
    let ids = stable_ids(&scoped, &shapes, &fixed, &preserve)?;
    let planned: Vec<Planned> = next
        .into_iter()
        .zip(ids)
        .map(|((center, radius, faces, rep), person)| Planned { person, faces, representative: rep, centroid: center, radius })
        .collect();
    let person_count = apply(tx, &planned, &pool, &preserve, face_review::now())?;
    Ok(Stats { person_count, suppressed_faces, truncated_faces, deferred })
}

// MARK: - Centroid auto-merge (tightPairAutoMerge)

struct VpNode {
    vantage: usize,
    threshold: f64,
    inner: Option<Box<VpNode>>,
    outer: Option<Box<VpNode>>,
}

struct Join<'a> {
    vectors: &'a [Vec<f32>],
    small: &'a [bool],
    tight: f32,
    loose: f32,
    evaluations: usize,
    exceeded: bool,
    found: Vec<(f32, usize, usize)>,
}

const JOIN_EVALUATIONS: usize = 12_500_000;
const JOIN_EDGES: usize = 1_000_000;
const JOIN_DIRECT_PAIRS: usize = 250_000;

impl Join<'_> {
    fn qualifies(&self, a: usize, b: usize, cosine: f32) -> bool {
        cosine >= self.tight || (cosine >= self.loose && (self.small[a] || self.small[b]))
    }

    fn metric(&mut self, a: usize, b: usize) -> f64 {
        self.evaluations += 1;
        if self.evaluations > JOIN_EVALUATIONS {
            self.exceeded = true;
            return f64::INFINITY;
        }
        let mut squared = 0.0f64;
        for (x, y) in self.vectors[a].iter().zip(&self.vectors[b]) {
            let delta = f64::from(*x) - f64::from(*y);
            squared += delta * delta;
        }
        squared.sqrt()
    }

    fn build(&mut self, indices: Vec<usize>) -> Option<Box<VpNode>> {
        if indices.is_empty() || self.exceeded {
            return None;
        }
        let vantage = indices[indices.len() - 1];
        if indices.len() == 1 {
            return Some(Box::new(VpNode { vantage, threshold: 0.0, inner: None, outer: None }));
        }
        let mut measured: Vec<(f64, usize)> = Vec::with_capacity(indices.len() - 1);
        for &index in &indices[..indices.len() - 1] {
            let distance = self.metric(vantage, index);
            if self.exceeded {
                return None;
            }
            measured.push((distance, index));
        }
        measured.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        let split = measured.len() / 2;
        let threshold = measured[split].0;
        let inner = self.build(measured[..split].iter().map(|m| m.1).collect());
        let outer = self.build(measured[split..].iter().map(|m| m.1).collect());
        Some(Box::new(VpNode { vantage, threshold, inner, outer }))
    }

    fn query(&mut self, node: Option<&VpNode>, target: usize, radius: f64) {
        let Some(node) = node else { return };
        if self.exceeded {
            return;
        }
        let distance = self.metric(target, node.vantage);
        if self.exceeded {
            return;
        }
        if node.vantage > target && distance <= radius {
            let cosine = dot(&self.vectors[target], &self.vectors[node.vantage]);
            if self.qualifies(target, node.vantage, cosine) {
                self.found.push((cosine, target, node.vantage));
                if self.found.len() > JOIN_EDGES {
                    self.exceeded = true;
                    return;
                }
            }
        }
        if node.inner.is_some() && distance - radius <= node.threshold {
            self.query(node.inner.as_deref(), target, radius);
        }
        if node.outer.is_some() && distance + radius >= node.threshold {
            self.query(node.outer.as_deref(), target, radius);
        }
    }

    /// `ExactCosineJoin.edges`: every qualifying pair, strongest first, or
    /// `None` when a budget runs out.
    fn edges(mut self) -> Option<Vec<(f32, usize, usize)>> {
        let n = self.vectors.len();
        if n < 2 {
            return Some(Vec::new());
        }
        let dimension = self.vectors[0].len();
        if dimension == 0 || self.vectors.iter().any(|v| v.len() != dimension || v.iter().any(|x| !x.is_finite())) {
            return Some(Vec::new());
        }
        if n * (n - 1) / 2 <= JOIN_DIRECT_PAIRS {
            for i in 0..n {
                for j in i + 1..n {
                    self.evaluations += 1;
                    if self.evaluations > JOIN_EVALUATIONS {
                        return None;
                    }
                    let cosine = dot(&self.vectors[i], &self.vectors[j]);
                    if self.qualifies(i, j, cosine) {
                        self.found.push((cosine, i, j));
                        if self.found.len() > JOIN_EDGES {
                            return None;
                        }
                    }
                }
            }
        } else {
            let root = self.build((0..n).collect());
            if self.exceeded {
                return None;
            }
            let max_norm = self
                .vectors
                .iter()
                .map(|v| v.iter().map(|x| f64::from(*x) * f64::from(*x)).sum::<f64>())
                .fold(0.0f64, f64::max);
            let roundoff = f64::from(f32::EPSILON) / 2.0;
            let operations = (dimension * 2) as f64;
            let denominator = 1.0 - operations * roundoff;
            let margin = if denominator > 0.0 { operations * roundoff / denominator * max_norm } else { f64::INFINITY };
            let loose = f64::from(self.tight.min(self.loose));
            let radius = (2.0 * max_norm - 2.0 * (loose - margin)).max(0.0).sqrt();
            for target in 0..n {
                self.query(root.as_deref(), target, radius);
                if self.exceeded {
                    return None;
                }
            }
        }
        let mut found = self.found;
        found.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        Some(found)
    }
}

fn auto_merge(conn: &mut Connection, space: &Space) -> Result<usize> {
    let partition = load_partition(conn, space, &HashSet::new(), 0.0, MAX_FACES_PER_RUN)?;
    ensure!(!partition.overflowed, "face cache overflow");
    let priors = priors(conn)?;
    let participants: HashSet<i64> = partition.rows.iter().map(|row| row.id).collect();
    let verdicts = verdicts(conn, Some(&participants))?;
    let complete: HashSet<i64> = priors
        .iter()
        .filter(|p| !p.unknown && !p.faces.is_empty() && p.faces.iter().all(|f| participants.contains(f)))
        .map(|p| p.id)
        .collect();
    let by_id: HashMap<i64, &Prior> = priors.iter().map(|p| (p.id, p)).collect();
    let mut sums: BTreeMap<i64, Vec<f32>> = BTreeMap::new();
    let mut files: HashMap<i64, HashSet<i64>> = HashMap::new();
    let mut owners: HashMap<i64, i64> = HashMap::new();
    for (row, snapshot) in partition.rows.iter().zip(&partition.snapshots) {
        let Some(owner) = snapshot.person_id.filter(|owner| complete.contains(owner)) else { continue };
        owners.insert(row.id, owner);
        if row.held {
            continue;
        }
        let vector = decode(&row.blob);
        let sum = sums.entry(owner).or_insert_with(|| vec![0.0; DIM]);
        for (index, value) in sum.iter_mut().enumerate() {
            *value += vector[index];
        }
        files.entry(owner).or_default().insert(snapshot.file_id);
    }
    let mut blockers: HashSet<Pair> = verdicts
        .iter()
        .filter(|v| !v.same)
        .filter_map(|v| match (owners.get(&v.pair.0), owners.get(&v.pair.1)) {
            (Some(a), Some(b)) if a != b => Some(Pair::new(*a, *b)),
            _ => None,
        })
        .collect();
    let mut by_file: HashMap<i64, BTreeSet<i64>> = HashMap::new();
    for snapshot in &partition.snapshots {
        if let Some(owner) = owners.get(&snapshot.id) {
            by_file.entry(snapshot.file_id).or_default().insert(*owner);
        }
    }
    for set in by_file.values().filter(|set| set.len() > 1) {
        let sorted: Vec<i64> = set.iter().copied().collect();
        for a in 0..sorted.len() {
            for b in a + 1..sorted.len() {
                blockers.insert(Pair::new(sorted[a], sorted[b]));
            }
        }
    }
    if sums.len() > 20_000 || owners.len() > 250_000 || owners.len() as i64 * 512 > 768 * 1024 * 1024 {
        return Ok(0);
    }
    if sums.len() < 2 {
        return Ok(0);
    }
    struct Centroid {
        id: i64,
        vector: Vec<f32>,
        files: usize,
        named: bool,
    }
    let mut clusters: Vec<Centroid> = Vec::new();
    for (id, mut sum) in sums {
        let norm: f32 = sum.iter().map(|x| x * x).sum();
        if !norm.is_finite() {
            continue;
        }
        let inverse = 1.0 / norm.sqrt().max(f32::from_bits(1));
        for value in &mut sum {
            *value *= inverse;
        }
        clusters.push(Centroid { id, vector: sum, files: files.get(&id).map_or(0, HashSet::len), named: by_id[&id].has_name() });
    }
    let index_of: HashMap<i64, usize> = clusters.iter().enumerate().map(|(i, c)| (c.id, i)).collect();
    let mut parent: Vec<usize> = (0..clusters.len()).collect();
    fn find(parent: &mut [usize], x: usize) -> usize {
        let mut root = x;
        while parent[root] != root {
            root = parent[root];
        }
        let mut current = x;
        while parent[current] != root {
            let next = parent[current];
            parent[current] = root;
            current = next;
        }
        root
    }
    let mut has_named: Vec<bool> = clusters.iter().map(|c| c.named).collect();
    let mut forbidden: Vec<HashSet<usize>> = vec![HashSet::new(); clusters.len()];
    for pair in &blockers {
        if let (Some(&a), Some(&b)) = (index_of.get(&pair.0), index_of.get(&pair.1)) {
            forbidden[a].insert(b);
            forbidden[b].insert(a);
        }
    }
    let mut size = vec![1usize; clusters.len()];
    let vectors: Vec<Vec<f32>> = clusters.iter().map(|c| c.vector.clone()).collect();
    let small: Vec<bool> = clusters.iter().map(|c| c.files <= 1).collect();
    let join = Join {
        vectors: &vectors,
        small: &small,
        tight: env_cos("FILEID_FACE_TIGHT_COS", 0.65),
        loose: env_cos("FILEID_FACE_SMALL_COS", 0.55),
        evaluations: 0,
        exceeded: false,
        found: Vec::new(),
    };
    let Some(edges) = join.edges() else { return Ok(0) };
    for (_, i, j) in edges {
        let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
        if ri == rj || (has_named[ri] && has_named[rj]) || forbidden[ri].contains(&rj) || forbidden[rj].contains(&ri) {
            continue;
        }
        let (keep, drop) = if forbidden[ri].len() > forbidden[rj].len() || (forbidden[ri].len() == forbidden[rj].len() && size[ri] >= size[rj]) {
            (ri, rj)
        } else {
            (rj, ri)
        };
        parent[drop] = keep;
        size[keep] += size[drop];
        has_named[keep] = has_named[keep] || has_named[drop];
        let moved = std::mem::take(&mut forbidden[drop]);
        for neighbor in moved {
            let root = find(&mut parent, neighbor);
            if root == keep {
                continue;
            }
            forbidden[root].remove(&drop);
            forbidden[root].insert(keep);
            forbidden[keep].insert(root);
        }
        forbidden[keep].remove(&drop);
        forbidden[keep].remove(&keep);
    }
    let preferred = |a: usize, b: usize| -> bool {
        if clusters[a].named != clusters[b].named {
            return clusters[a].named;
        }
        if clusters[a].files != clusters[b].files {
            return clusters[a].files > clusters[b].files;
        }
        clusters[a].id < clusters[b].id
    };
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for index in 0..clusters.len() {
        let root = find(&mut parent, index);
        groups.entry(root).or_default().push(index);
    }
    let mut targets: Vec<(i64, Vec<i64>)> = Vec::new();
    for members in groups.values().filter(|members| members.len() > 1) {
        let mut canon = members[0];
        for &member in &members[1..] {
            if preferred(member, canon) {
                canon = member;
            }
        }
        targets.push((clusters[canon].id, members.iter().filter(|m| **m != canon).map(|m| clusters[*m].id).collect()));
    }
    if targets.is_empty() {
        return Ok(0);
    }
    let tx = conn.transaction()?;
    require_unchanged(&tx, &partition, space, &HashSet::new(), 0.0)?;
    ensure!(priors == self::priors(&tx)? && verdicts == self::verdicts(&tx, Some(&participants))?, "face evidence changed");
    let fresh: HashMap<i64, Prior> = priors.into_iter().map(|p| (p.id, p)).collect();
    let mut absorbed = 0;
    for (target, sources) in &targets {
        if fresh.get(target).is_none_or(|p| p.unknown) {
            continue;
        }
        let sources: Vec<i64> = sources.iter().copied().filter(|id| fresh.get(id).is_some_and(|p| !p.has_name())).collect();
        if sources.is_empty() {
            continue;
        }
        absorbed += sources.len();
        let list = face_review::json_ids(&sources);
        tx.execute("UPDATE face_prints SET person_id=?1 WHERE person_id IN (SELECT value FROM json_each(?2))", params![target, list])?;
        tx.execute(
            "UPDATE persons SET representative_face_id=NULL,centroid=NULL,anchor_radius=NULL,file_count=0 WHERE id IN (SELECT value FROM json_each(?1))",
            [list],
        )?;
    }
    let ids: Vec<i64> = targets.iter().map(|(target, _)| *target).collect();
    tx.execute(
        "UPDATE persons SET file_count=(SELECT COUNT(DISTINCT file_id) FROM face_prints WHERE face_prints.person_id=persons.id) \
         WHERE id IN (SELECT value FROM json_each(?1))",
        [face_review::json_ids(&ids)],
    )?;
    tx.commit()?;
    Ok(absorbed)
}

// MARK: - Review (FaceReview.holdout / finish / assess)

fn holdout(conn: &Connection, space: &Space, min_quality: f64) -> rusqlite::Result<HashSet<i64>> {
    let incremental: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM face_prints WHERE person_id IS NOT NULL AND embedding_model=?1 AND processing_version=?2)",
        params![space.model, space.processing],
        |row| row.get(0),
    )?;
    let mut statement = conn.prepare(&format!(
        "SELECT fp.id, a.outcome FROM face_prints fp LEFT JOIN face_assignment_assessed a ON a.face_id=fp.id \
         JOIN catalog_revisions r ON r.file_id=fp.file_id \
         WHERE fp.person_id IS NULL AND length(fp.arcface_embedding)=512 \
           AND fp.embedding_model=?1 AND fp.processing_version=?2 AND fp.source_revision=r.revision \
           AND (a.face_id IS NULL OR a.outcome IN {PENDING}) AND (?3 <= 0 OR COALESCE(fp.face_quality,-1) >= ?3)"
    ))?;
    let rows = statement.query_map(params![space.model, space.processing, min_quality], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?))
    })?;
    let mut out = HashSet::new();
    for row in rows {
        let (id, outcome) = row?;
        if incremental || outcome.is_some() {
            out.insert(id);
        }
    }
    Ok(out)
}

fn unit(values: Vec<f32>) -> Option<Vec<f32>> {
    let mut sum = 0.0f32;
    for value in &values {
        if !value.is_finite() {
            return None;
        }
        sum += value * value;
    }
    let norm = sum.sqrt();
    if !norm.is_finite() || norm <= f32::MIN_POSITIVE {
        return None;
    }
    Some(values.into_iter().map(|value| value / norm).collect())
}

struct Scored {
    person: i64,
    score: f32,
    unknown: bool,
}

struct Decision {
    outcome: &'static str,
    person: Option<i64>,
    score: Option<f32>,
    runner: Option<(i64, f32)>,
}

fn decide(scored: &[Scored], barred: &HashSet<i64>, held: bool) -> Decision {
    let unmatched = Decision { outcome: "unmatched", person: None, score: None, runner: None };
    let (suggest, minimum, margin_floor, auto) = (
        env_cos("FILEID_FACE_REVIEW_SUGGEST_COS", 0.55),
        env_cos("FILEID_FACE_REVIEW_MIN_COS", 0.40),
        env_cos("FILEID_FACE_REVIEW_MARGIN", 0.05),
        env_bool("FILEID_FACE_REVIEW_AUTO_ASSIGN", false),
    );
    let Some(top) = scored.first() else { return unmatched };
    if top.unknown {
        return unmatched;
    }
    let Some(chosen) = scored.iter().position(|s| !s.unknown && !barred.contains(&s.person)) else { return unmatched };
    let best = &scored[chosen];
    let runner = (0..scored.len()).find(|index| *index != chosen).map(|index| (scored[index].person, scored[index].score));
    let margin = runner.map_or(f32::INFINITY, |(_, score)| best.score - score);
    if best.score < minimum {
        return unmatched;
    }
    let mut outcome = if margin < margin_floor {
        "ambiguous"
    } else if best.score >= suggest {
        if auto { "assigned" } else { "suggested" }
    } else {
        "uncertain"
    };
    if held && (outcome == "suggested" || outcome == "assigned") {
        outcome = "uncertain";
    }
    Decision { outcome, person: Some(best.person), score: Some(best.score), runner }
}

fn persons_at(conn: &Connection, file: i64, bbox: &str) -> rusqlite::Result<Vec<i64>> {
    let mut statement = conn.prepare_cached("SELECT person_id FROM face_prints WHERE file_id=?1 AND bbox=?2 AND person_id IS NOT NULL")?;
    statement.query_map(params![file, bbox], |row| row.get(0))?.collect()
}

fn bars(conn: &Connection, face: i64, file: i64, bbox: &str) -> rusqlite::Result<HashSet<i64>> {
    let mut barred: HashSet<i64> = {
        let mut statement = conn.prepare_cached("SELECT person_id FROM face_prints WHERE file_id=?1 AND id<>?2 AND person_id IS NOT NULL")?;
        statement.query_map(params![file, face], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?
    };
    let anchors: Vec<(i64, String)> = {
        let mut statement = conn.prepare_cached("SELECT anchor_file_id, anchor_bbox FROM face_identity_constraints WHERE file_id=?1 AND bbox=?2")?;
        statement.query_map(params![file, bbox], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    for (anchor_file, anchor_bbox) in anchors {
        barred.extend(persons_at(conn, anchor_file, &anchor_bbox)?);
    }
    type Side = (Option<i64>, Option<i64>, Option<String>);
    let rows: Vec<(Side, Side)> = {
        let mut statement = conn.prepare_cached(
            "SELECT face_a, file_a, bbox_a, face_b, file_b, bbox_b FROM face_verifications WHERE same_person=0 \
               AND (face_a=?1 OR face_b=?1 OR (file_a=?2 AND bbox_a=?3) OR (file_b=?2 AND bbox_b=?3))",
        )?;
        statement
            .query_map(params![face, file, bbox], |row| Ok(((row.get(0)?, row.get(1)?, row.get(2)?), (row.get(3)?, row.get(4)?, row.get(5)?))))?
            .collect::<rusqlite::Result<_>>()?
    };
    for (a, b) in rows {
        let is_a = a.0 == Some(face) || (a.1 == Some(file) && a.2.as_deref() == Some(bbox));
        let other = if is_a { b } else { a };
        if let (Some(other_file), Some(other_bbox)) = (other.1, other.2.as_deref()) {
            let exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM face_prints WHERE file_id=?1 AND bbox=?2)",
                params![other_file, other_bbox],
                |row| row.get(0),
            )?;
            if exists {
                barred.extend(persons_at(conn, other_file, other_bbox)?);
                continue;
            }
        }
        if let Some(legacy) = other.0 {
            let mut statement = conn.prepare_cached("SELECT person_id FROM face_prints WHERE id=?1 AND person_id IS NOT NULL")?;
            barred.extend(statement.query_map([legacy], |row| row.get::<_, i64>(0))?.collect::<rusqlite::Result<Vec<_>>>()?);
        }
    }
    Ok(barred)
}

fn assess(conn: &Connection, held_ids: &HashSet<i64>, space: &Space, now: f64) -> rusqlite::Result<()> {
    if held_ids.is_empty() {
        return Ok(());
    }
    struct Face {
        id: i64,
        file: i64,
        bbox: String,
        vector: Vec<f32>,
        held: bool,
    }
    let faces: Vec<Face> = {
        let mut statement = conn.prepare(
            "SELECT id, file_id, bbox, arcface_embedding, COALESCE(excluded,0) FROM face_prints \
             WHERE id IN (SELECT value FROM json_each(?1)) AND person_id IS NULL \
               AND length(arcface_embedding)=512 AND embedding_model=?2 AND processing_version=?3 ORDER BY id",
        )?;
        let rows = statement.query_map(params![face_review::json_ids(held_ids), space.model, space.processing], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?, row.get::<_, Vec<u8>>(3)?, row.get::<_, i64>(4)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, file, bbox, blob, held) = row?;
            if let Some(vector) = unit(decode(&blob)) {
                out.push(Face { id, file, bbox, vector, held: held != 0 });
            }
        }
        out
    };
    if faces.is_empty() {
        return Ok(());
    }
    let receivers: Vec<(i64, bool, Vec<f32>)> = {
        let mut statement = conn.prepare(
            "SELECT fp.person_id, fp.arcface_embedding, COALESCE(p.is_unknown,0) FROM face_prints fp JOIN persons p ON p.id=fp.person_id \
             WHERE fp.person_id IS NOT NULL AND COALESCE(fp.excluded,0)=0 AND length(fp.arcface_embedding)=512 \
               AND fp.embedding_model=?1 AND fp.processing_version=?2 ORDER BY fp.id",
        )?;
        let rows = statement.query_map(params![space.model, space.processing], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?, row.get::<_, i64>(2)?))
        })?;
        let mut sums: BTreeMap<i64, Vec<f32>> = BTreeMap::new();
        let mut unknown: HashMap<i64, bool> = HashMap::new();
        for row in rows {
            let (person, blob, flag) = row?;
            let Some(vector) = unit(decode(&blob)) else { continue };
            unknown.insert(person, flag != 0);
            match sums.get_mut(&person) {
                Some(sum) if sum.len() == vector.len() => {
                    for (total, value) in sum.iter_mut().zip(&vector) {
                        *total += value;
                    }
                }
                Some(_) => {}
                None => {
                    sums.insert(person, vector);
                }
            }
        }
        sums.into_iter()
            .filter_map(|(person, sum)| unit(sum).map(|center| (person, unknown.get(&person).copied().unwrap_or(false), center)))
            .collect()
    };
    let namespace = format!("{}|{}", space.model, space.processing);
    let mut prepared: Vec<(Face, Vec<Scored>, HashSet<i64>, Option<f32>)> = Vec::new();
    for face in faces {
        let mut scored: Vec<Scored> = receivers
            .iter()
            .map(|(person, unknown, center)| Scored { person: *person, score: dot(&face.vector, center), unknown: *unknown })
            .collect();
        scored.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.person.cmp(&b.person)));
        let barred = bars(conn, face.id, face.file, &face.bbox)?;
        let best = scored.iter().find(|s| !s.unknown && !barred.contains(&s.person)).map(|s| s.score);
        prepared.push((face, scored, barred, best));
    }
    prepared.sort_by(|a, b| match (a.3, b.3) {
        (Some(x), Some(y)) => y.total_cmp(&x).then(a.0.id.cmp(&b.0.id)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.0.id.cmp(&b.0.id),
    });
    let mut claimed: HashMap<i64, HashSet<i64>> = HashMap::new();
    let mut received: BTreeSet<i64> = BTreeSet::new();
    for (face, scored, mut barred, _) in prepared {
        if let Some(taken) = claimed.get(&face.file) {
            barred.extend(taken);
        }
        let decision = decide(&scored, &barred, face.held);
        if let Some(person) = decision.person
            && (decision.outcome == "suggested" || decision.outcome == "assigned")
        {
            claimed.entry(face.file).or_default().insert(person);
        }
        let previous: Option<(Option<i64>, Option<f64>)> = conn
            .query_row("SELECT person_id, created_at FROM face_review_candidates WHERE face_id=?1", [face.id], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()?;
        let previous_outcome: Option<String> = conn
            .query_row("SELECT outcome FROM face_assignment_assessed WHERE face_id=?1", [face.id], |row| row.get(0))
            .optional()?
            .flatten();
        conn.execute("DELETE FROM face_review_candidates WHERE face_id=?1", [face.id])?;
        let mut outcome = decision.outcome;
        match (decision.person, decision.score) {
            (Some(person), Some(score)) if REVIEW_REASONS.contains(&outcome) => {
                let created = match previous {
                    Some((Some(owner), created)) if owner == person => created.unwrap_or(now),
                    _ => now,
                };
                conn.execute(
                    "INSERT INTO face_review_candidates(face_id,person_id,score,reason,runner_up_person_id,runner_up_score,namespace,created_at) \
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                    params![
                        face.id,
                        person,
                        face_review::round4(f64::from(score)),
                        outcome,
                        decision.runner.map(|(id, _)| id),
                        decision.runner.map(|(_, value)| face_review::round4(f64::from(value))),
                        namespace,
                        created
                    ],
                )?;
            }
            (Some(person), _) if outcome == "assigned" => {
                conn.execute("UPDATE face_prints SET person_id=?1 WHERE id=?2", params![person, face.id])?;
                conn.execute("UPDATE persons SET representative_face_id=COALESCE(representative_face_id, ?1) WHERE id=?2", params![face.id, person])?;
                received.insert(person);
            }
            _ if outcome == "unmatched" && previous_outcome.as_deref() == Some("rejected") => outcome = "rejected",
            _ => {}
        }
        face_review::upsert_outcome(conn, face.id, &namespace, outcome, now)?;
    }
    for person in received {
        conn.execute(
            "UPDATE persons SET file_count=(SELECT COUNT(DISTINCT file_id) FROM face_prints WHERE person_id=?1) WHERE id=?1",
            [person],
        )?;
    }
    Ok(())
}

fn finish(conn: &mut Connection, pool: &[i64], held: &HashSet<i64>, space: &Space) -> Result<i64> {
    let tx = conn.transaction()?;
    let now = face_review::now();
    if !pool.is_empty() {
        face_review::mark_pool(&tx, pool, now)?;
    }
    assess(&tx, held, space, now)?;
    let (merges, splits) = super::face_queues::refresh(&tx, &space.model, &space.processing, &super::face_queues::QueueSettings::from_env(), now)?;
    tracing::info!(merges, splits, "[CLUSTER] stored merge and split suggestions");
    let queued: i64 = tx.query_row("SELECT COUNT(*) FROM face_review_candidates", [], |row| row.get(0))?;
    tx.commit()?;
    Ok(queued)
}

// MARK: - Run

fn person_count(conn: &Connection) -> u32 {
    conn.query_row("SELECT COUNT(*) FROM persons", [], |row| row.get::<_, i64>(0)).map_or(0, |count| count as u32)
}

fn result(person_count: u32, face_count: usize, unmatched: usize, started: Instant) -> FaceClusteringResult {
    FaceClusteringResult {
        person_count,
        face_count: face_count as u64,
        unmatched_faces: unmatched as u64,
        duration_seconds: started.elapsed().as_secs_f64(),
        deferred_faces: None,
        review_queued: None,
    }
}

/// One clustering pass in `space`, as the Swift engine runs it: the error kind
/// it reports (if any) and the summary it always sends afterwards.
pub(crate) fn run(database: &Arc<Mutex<Connection>>, space: &Space) -> Outcome {
    let started = Instant::now();
    let min_quality = env_double("FILEID_FACE_CLUSTER_MIN_QUALITY", 0.0, 0.0..=1.0);
    let loaded = {
        let conn = database.lock();
        (|| -> Result<(HashSet<i64>, HashSet<i64>, Partition)> {
            let unknown: HashSet<i64> = priors(&conn)?.into_iter().filter(|p| p.unknown).flat_map(|p| p.faces).collect();
            let held = holdout(&conn, space, min_quality)?;
            let skip: HashSet<i64> = unknown.union(&held).copied().collect();
            let partition = load_partition(&conn, space, &skip, min_quality, MAX_FACES_PER_RUN)?;
            Ok((held, skip, partition))
        })()
    };
    let (held, skip, partition) = match loaded {
        Ok(loaded) => loaded,
        Err(error) => {
            tracing::warn!(?error, "face clustering query failed");
            let count = person_count(&database.lock());
            return Outcome { result: result(count, 0, 0, started), error: Some(("face_clustering_failed", CACHE_ERROR)) };
        }
    };
    if partition.rows.is_empty() {
        let mut conn = database.lock();
        let queued = finish(&mut conn, &[], &held, space).map_err(|error| tracing::warn!(?error, "face review failed")).ok();
        let mut summary = result(person_count(&conn), 0, 0, started);
        summary.deferred_faces = Some(partition.deferred);
        summary.review_queued = queued;
        return Outcome { result: summary, error: None };
    }
    let row_by_face: HashMap<i64, usize> = partition.rows.iter().enumerate().map(|(index, row)| (row.id, index)).collect();
    let decoded: Vec<(i64, Vec<f32>)> =
        partition.rows.iter().filter(|row| !row.held).map(|row| (row.id, decode(&row.blob))).filter(|(_, v)| !v.is_empty()).collect();
    let Some(first_dim) = decoded.first().map(|(_, v)| v.len()) else {
        let count = person_count(&database.lock());
        return Outcome { result: result(count, partition.rows.len(), partition.rows.len(), started), error: None };
    };
    let decoded: Vec<(i64, Vec<f32>)> = decoded.into_iter().filter(|(_, v)| v.len() == first_dim).collect();
    let file_by_face: HashMap<i64, i64> = partition.snapshots.iter().map(|s| (s.id, s.file_id)).collect();
    let files: Vec<i64> = decoded.iter().map(|(id, _)| file_by_face.get(id).copied().unwrap_or(-id)).collect();
    let vectors: Vec<Vec<f32>> = decoded.iter().map(|(_, v)| v.clone()).collect();
    let _lease = crate::resource_scheduler::ResourceScheduler::shared().reserve(
        crate::resource_scheduler::Demand { memory_mb: 64 + (vectors.len() as u64).saturating_mul(4096).div_ceil(1_048_576), cpu: 1, io: 1 },
        crate::resource_scheduler::Priority::Background,
        || false,
    );
    let mut raw: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (cluster, members) in cluster(&vectors, &files) {
        let rows: Vec<usize> = members.iter().filter_map(|dense| row_by_face.get(&decoded[*dense].0).copied()).collect();
        raw.insert(cluster, rows);
    }
    let min_size = env_int("FILEID_FACE_MIN_CLUSTER_SIZE", 3, 1..=10_000);
    let solo_floor = env_double("FILEID_FACE_SOLO_QUALITY", 0.50, 0.0..=1.0);
    let mut conn = database.lock();
    let stats = (|| -> Result<Stats> {
        let tx = conn.transaction()?;
        let stats = persist(&tx, &partition, space, &raw, &skip, min_quality, min_size, solo_floor)?;
        tx.commit()?;
        Ok(stats)
    })();
    let stats = match stats {
        Ok(stats) => stats,
        Err(error) => {
            tracing::warn!(?error, "face cluster persist failed");
            let mut summary = result(person_count(&conn), decoded.len(), 0, started);
            summary.deferred_faces = Some(partition.deferred);
            return Outcome { result: summary, error: Some(("face_cluster_persist_failed", PERSIST_ERROR)) };
        }
    };
    let merged = auto_merge(&mut conn, space).unwrap_or_else(|error| {
        tracing::warn!(?error, "face auto-merge skipped");
        0
    });
    let pool: Vec<i64> = partition.rows.iter().map(|row| row.id).collect();
    let queued = finish(&mut conn, &pool, &held, space).map_err(|error| tracing::warn!(?error, "face review failed")).ok();
    tracing::info!(persons = stats.person_count, merged, faces = decoded.len(), "[CLUSTER] macOS face space clustered");
    let mut summary = result(person_count(&conn), decoded.len(), stats.suppressed_faces + stats.truncated_faces, started);
    summary.deferred_faces = Some(stats.deferred);
    summary.review_queued = queued;
    Outcome { result: summary, error: None }
}

#[cfg(test)]
#[path = "face_clustering_mac_tests.rs"]
mod tests;
