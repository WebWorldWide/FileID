//! Butler restructure — semantic + learn-your-style classification (Phase 1).
//!
//! See `shared/docs/RESTRUCTURE.md`. Where the legacy `restructure::classify`
//! cascade buckets every photo into `Photos/Year/Month`, this fuses the rich
//! signals FileID already computed — CLIP image embedding + content tags +
//! capture time — into one feature vector, clusters files by *content* (reusing
//! the proven `identity_clustering` density algorithm — no new deps), then
//! assigns each cluster to the user's nearest EXISTING folder when the match is
//! confident ("organize like I already do"), otherwise proposes a new
//! tag-named group. Density-noise files fall back to the rule cascade.
//!
//! Pure logic: the DB load lives in `commands/restructure.rs`. VLM group naming,
//! confidence-tier routing, and the learn-from-corrections loop are later phases.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::identity_clustering::{self, Hyperparameters, Neighbor};
use super::restructure::{Confidence, ProposedMove};

/// Per-file signals. `clip` is the L2-normalized 512-d CLIP image embedding;
/// callers only pass files that have one (images), so it is never empty here.
pub struct SemanticFile {
    pub file_id: i64,
    pub source: PathBuf,
    pub clip: Vec<f32>,
    pub tags: Vec<String>,
    pub time_unix: f64,
}

/// Fusion weights (RESTRUCTURE.md): content embedding dominates; tags refine;
/// time nudges. Each block is L2-normalized before weighting, so the weight —
/// not the block's dimensionality — controls its contribution.
const W_CLIP: f32 = 0.70;
const W_TAGS: f32 = 0.22;
const W_TIME: f32 = 0.08;

/// Cap the tag vocabulary to the most common tags. Frequent tags carry the
/// grouping signal; rare ones are noise and would bloat the fused vector.
const TAG_VOCAB_CAP: usize = 256;

/// Minimum cosine from a cluster's content centroid to an existing folder's
/// prototype centroid to route the cluster there (learn-your-style). Below
/// this, propose a new group. Provisional — calibrate on a labeled library.
const FOLDER_MATCH_COS: f32 = 0.55;

/// Confidence-band thresholds (RESTRUCTURE.md §6). A cluster auto-files only
/// when it matches an existing folder strongly *and* unambiguously, or forms a
/// tight, substantial new group. Provisional — calibrate to measured
/// per-category accuracy on a labeled library before promoting any category to
/// standing auto-file.
const AUTO_FOLDER_COS: f32 = 0.72;
const AUTO_COHESION: f32 = 0.62;
const REVIEW_COHESION: f32 = 0.50;
const MIN_MARGIN: f32 = 0.05;
const AUTO_MIN_MEMBERS: usize = 4;

/// Density-clustering hyperparameters for *files* (looser than faces: a
/// semantic group is broader than one identity). Provisional.
fn file_hyperparams() -> Hyperparameters {
    Hyperparameters {
        pass1_cosine: 0.50,
        pass2_cosine: 0.40,
        pass2_margin: 0.08,
        pass3_variance_threshold: 0.06,
        pass3_min_mean_cosine: 0.42,
        pass3_max_splits: 5,
        k_nn: 12,
    }
}

/// An existing folder learned from the current tree: its path + the mean
/// (L2-normalized) CLIP embedding of the files currently in it.
pub struct FolderPrototype {
    pub path: PathBuf,
    pub centroid: Vec<f32>,
}

/// Build prototypes from the files' *current* locations: each parent folder
/// with ≥ `min_files` becomes a class whose centroid is the mean CLIP vector of
/// its contents (Nearest-Class-Mean / Dropbox "Smart Move"). Zero user effort —
/// the existing tree is the labeled ground truth.
pub fn folder_prototypes(files: &[SemanticFile], min_files: usize) -> Vec<FolderPrototype> {
    let mut by_folder: HashMap<PathBuf, Vec<&[f32]>> = HashMap::new();
    for f in files {
        if let Some(parent) = f.source.parent() {
            by_folder
                .entry(parent.to_path_buf())
                .or_default()
                .push(&f.clip);
        }
    }
    let mut out = Vec::new();
    for (path, vecs) in by_folder {
        if vecs.len() < min_files {
            continue;
        }
        if let Some(centroid) = mean_unit(&vecs) {
            out.push(FolderPrototype { path, centroid });
        }
    }
    // Deterministic order (path) so proposals are stable across runs.
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Classify `files` into proposed moves: each discovered cluster either extends
/// the nearest confident existing folder or becomes a new tag-named group under
/// `library_root`. Density-noise / singleton files are simply not returned — the
/// caller routes anything left unmoved through its rule-cascade fallback.
pub fn semantic_classify(
    files: &[SemanticFile],
    prototypes: &[FolderPrototype],
    library_root: &Path,
) -> Vec<ProposedMove> {
    if files.is_empty() {
        return Vec::new();
    }
    let global_freq = tag_frequencies(files);
    let vocab = vocab_from_freq(&global_freq, TAG_VOCAB_CAP);
    let fused: Vec<Vec<f32>> = files.iter().map(|f| fuse(f, &vocab)).collect();
    let cluster_ids = cluster(&fused);

    // Group file indices by cluster id.
    let mut clusters: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, &cid) in cluster_ids.iter().enumerate() {
        clusters.entry(cid).or_default().push(i);
    }

    let mut moves = Vec::new();

    // Group names already claimed by a *different* new-group cluster this run.
    // Without this, two clusters with identical top tags collapse into one
    // folder (#9). Consulted ONLY by the new-group branch; the existing-folder
    // branch legitimately routes many clusters into one user folder.
    let mut used_group_names: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Stable cluster iteration (smallest id first) — makes the collision
    // disambiguation below deterministic across runs.
    let mut ids: Vec<usize> = clusters.keys().copied().collect();
    ids.sort_unstable();
    for cid in ids {
        let members = &clusters[&cid];
        // Singletons (the clusterer's outliers) have no group signal — let the
        // rule cascade place them.
        if members.len() < 2 {
            continue;
        }
        let member_clip: Vec<&[f32]> = members.iter().map(|&i| files[i].clip.as_slice()).collect();
        let centroid = match mean_unit(&member_clip) {
            Some(c) => c,
            None => continue,
        };
        // How tightly the cluster's members hug their centroid (mean cosine) —
        // the core "are these really alike?" confidence signal.
        let coh = cohesion(&member_clip, &centroid);

        let (dest_dir, category, confidence, reason) =
            match nearest_two_folders(&centroid, prototypes) {
                // Learn-your-style: route to the nearest confident existing
                // folder. Auto-file only when the match is strong *and*
                // unambiguous (clear margin over the runner-up) on a tight
                // cluster; otherwise surface for one-click review.
                Some((proto, sim, runner_up)) if sim >= FOLDER_MATCH_COS => {
                    let name = folder_display_name(&proto.path);
                    let confidence = if sim >= AUTO_FOLDER_COS
                        && coh >= REVIEW_COHESION
                        && (sim - runner_up) >= MIN_MARGIN
                    {
                        Confidence::Auto
                    } else {
                        Confidence::Review
                    };
                    let reason =
                        format!("Matches your '{name}' folder ({:.0}% alike)", sim * 100.0);
                    (proto.path.clone(), name, confidence, reason)
                }
                // Otherwise a new group, named from the cluster's most
                // *distinctive* tags (c-TF-IDF), tiered by how tight + large it is.
                _ => {
                    let terms = distinctive_terms(members, files, &global_freq);
                    let base = group_name_from_terms(&terms);
                    // Disambiguate a name already claimed by another new-group
                    // cluster so distinct clusters get distinct folders (#9):
                    // prefer the next distinctive term, then a numeric suffix.
                    let mut pretty = base.clone();
                    if used_group_names.contains(&pretty) {
                        if let Some(extra) = terms.get(2) {
                            pretty = format!("{} {}", base, title_case(extra));
                        }
                    }
                    let mut n = 2usize;
                    while used_group_names.contains(&pretty) {
                        pretty = format!("{} {}", base, n);
                        n += 1;
                    }
                    used_group_names.insert(pretty.clone());
                    let confidence = if coh >= AUTO_COHESION && members.len() >= AUTO_MIN_MEMBERS {
                        Confidence::Auto
                    } else if coh >= REVIEW_COHESION {
                        Confidence::Review
                    } else {
                        Confidence::Ask
                    };
                    let reason = if terms.is_empty() {
                        format!("{} files that look alike", members.len())
                    } else {
                        let shown: Vec<String> = terms.iter().take(3).map(|t| title_case(t)).collect();
                        format!("{} files sharing {}", members.len(), shown.join(", "))
                    };
                    // Path-safe directory name (mirrors the person route):
                    // illegal/separator chars in tag-derived names ("16:9",
                    // "dog/cat") would mis-route or fail the move and break
                    // cross-platform name parity (#2). Keep `pretty` for the
                    // human-facing category/reason.
                    let safe = crate::util::path_safety::safe_filename_component(&pretty);
                    (library_root.join(&safe), pretty, confidence, reason)
                }
            };

        for &i in members {
            let file = &files[i];
            let dest = dest_dir.join(file.source.file_name().unwrap_or_default());
            moves.push(ProposedMove {
                file_id: file.file_id,
                source: file.source.clone(),
                destination: dest,
                category: category.clone(),
                confidence,
                reason: Some(reason.clone()),
            });
        }
    }

    moves
}

// ── Fusion ────────────────────────────────────────────────────────────────

/// Global tag frequency across all files — drives both the vocab cap and the
/// c-TF-IDF inverse-document weighting in [`distinctive_terms`].
fn tag_frequencies(files: &[SemanticFile]) -> HashMap<String, usize> {
    let mut freq: HashMap<String, usize> = HashMap::new();
    for f in files {
        for t in &f.tags {
            *freq.entry(t.clone()).or_insert(0) += 1;
        }
    }
    freq
}

/// Top-`cap` tags by frequency → index map. Common tags carry grouping signal.
fn vocab_from_freq(freq: &HashMap<String, usize>, cap: usize) -> HashMap<String, usize> {
    let mut ranked: Vec<(&String, &usize)> = freq.iter().collect();
    // Frequency desc, then name for determinism.
    ranked.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    ranked
        .into_iter()
        .take(cap)
        .enumerate()
        .map(|(i, (t, _))| (t.clone(), i))
        .collect()
}

#[cfg(test)]
fn build_tag_vocab(files: &[SemanticFile], cap: usize) -> HashMap<String, usize> {
    vocab_from_freq(&tag_frequencies(files), cap)
}

/// Fuse one file: per-block L2-normalize, scale by weight, concatenate, then
/// L2-normalize the whole so the clusterer's cosine is meaningful.
fn fuse(file: &SemanticFile, vocab: &HashMap<String, usize>) -> Vec<f32> {
    let mut out = Vec::with_capacity(file.clip.len() + vocab.len() + 2);

    // CLIP block (already unit; re-normalize defensively).
    let clip = l2_normalized(&file.clip);
    out.extend(clip.iter().map(|x| x * W_CLIP));

    // Tag multi-hot block.
    let mut tags = vec![0f32; vocab.len()];
    for t in &file.tags {
        if let Some(&idx) = vocab.get(t) {
            tags[idx] = 1.0;
        }
    }
    let tags = l2_normalized(&tags);
    out.extend(tags.iter().map(|x| x * W_TAGS));

    // Time block: cyclical day-of-year (captures seasonality without raw epoch).
    let (s, c) = day_of_year_cyclical(file.time_unix);
    out.push(s * W_TIME);
    out.push(c * W_TIME);

    l2_normalized(&out)
}

/// `sin`/`cos` of the day-of-year angle. Zero time → (0,0) (no contribution).
fn day_of_year_cyclical(time_unix: f64) -> (f32, f32) {
    if time_unix <= 0.0 {
        return (0.0, 0.0);
    }
    let day = ((time_unix as i64) / 86_400) % 365;
    let angle = std::f64::consts::TAU * (day as f64) / 365.0;
    (angle.sin() as f32, angle.cos() as f32)
}

// ── Clustering (reuse identity_clustering) ──────────────────────────────────

/// Cluster fused vectors via the two-pass density algorithm. Brute-force cosine
/// kNN below `HNSW_MIN`, HNSW above (mirrors `face_clustering::cluster`).
fn cluster(fused: &[Vec<f32>]) -> Vec<usize> {
    const HNSW_MIN: usize = 5_000;
    let params = file_hyperparams();
    let k = params.k_nn;
    let n = fused.len();

    let hnsw = (n >= HNSW_MIN).then(|| {
        let points: Vec<(Vec<f32>, usize)> =
            fused.iter().enumerate().map(|(i, e)| (e.clone(), i)).collect();
        crate::util::hnsw_index::build(points)
    });

    let result = identity_clustering::cluster(
        fused,
        |i| {
            let mut hits: Vec<Neighbor> = if let Some(idx) = &hnsw {
                crate::util::hnsw_index::search_top_k(idx, &fused[i], k + 1)
                    .into_iter()
                    .filter(|(j, _)| *j != i)
                    .map(|(j, d)| Neighbor { idx: j, similarity: 1.0 - d / 2.0 })
                    .collect()
            } else {
                (0..n)
                    .filter(|&j| j != i)
                    .map(|j| Neighbor { idx: j, similarity: dot(&fused[i], &fused[j]) })
                    .collect()
            };
            hits.sort_by(|a, b| b.similarity.partial_cmp(&a.similarity).unwrap_or(std::cmp::Ordering::Equal));
            hits.truncate(k);
            hits
        },
        params,
    );
    result.cluster_ids
}

// ── Learn-your-style assignment ─────────────────────────────────────────────

/// Mean cosine of a cluster's members to its centroid — cluster tightness.
/// Members and `centroid` are unit vectors, so dot == cosine.
fn cohesion(member_clip: &[&[f32]], centroid: &[f32]) -> f32 {
    if member_clip.is_empty() {
        return 0.0;
    }
    let sum: f32 = member_clip.iter().map(|c| dot(c, centroid)).sum();
    sum / member_clip.len() as f32
}

/// Nearest folder prototype to `centroid`, plus the runner-up similarity so the
/// caller can gate on the top-1−top-2 margin (abstain when two folders fit
/// almost equally — RESTRUCTURE.md §4).
fn nearest_two_folders<'a>(
    centroid: &[f32],
    prototypes: &'a [FolderPrototype],
) -> Option<(&'a FolderPrototype, f32, f32)> {
    let mut best: Option<(&FolderPrototype, f32)> = None;
    let mut runner_up = 0.0f32;
    for p in prototypes {
        let sim = dot(centroid, &p.centroid);
        match best {
            Some((_, bs)) if sim > bs => {
                runner_up = bs;
                best = Some((p, sim));
            }
            Some(_) => runner_up = runner_up.max(sim),
            None => best = Some((p, sim)),
        }
    }
    best.map(|(p, sim)| (p, sim, runner_up))
}

fn folder_display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("Folder")
        .to_string()
}

/// A cluster's most *distinctive* tags by c-TF-IDF: frequent inside the cluster
/// but rare across the whole library. This makes a name specific ("Beach Trip")
/// instead of bland ("Photos") — RESTRUCTURE.md §5 calls distinctive terms the
/// single highest-impact naming input. Ubiquitous tags (in every file → idf 0)
/// drop out on their own.
fn distinctive_terms<'a>(
    members: &[usize],
    files: &'a [SemanticFile],
    global_freq: &HashMap<String, usize>,
) -> Vec<&'a str> {
    let mut in_cluster: HashMap<&str, usize> = HashMap::new();
    for &i in members {
        for t in &files[i].tags {
            *in_cluster.entry(t.as_str()).or_insert(0) += 1;
        }
    }
    let size = members.len().max(1) as f32;
    let total = files.len().max(1) as f32;
    let mut scored: Vec<(&str, f32)> = in_cluster
        .into_iter()
        .map(|(t, c)| {
            let tf = c as f32 / size;
            let df = *global_freq.get(t).unwrap_or(&1) as f32;
            (t, tf * (total / df).ln().max(0.0))
        })
        .collect();
    // Score desc, then name for determinism; drop zero-score (ubiquitous) tags.
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(b.0))
    });
    scored
        .into_iter()
        .filter(|(_, s)| *s > 0.0)
        .map(|(t, _)| t)
        .collect()
}

/// Title-case the top 1-2 distinctive terms into a folder name.
fn group_name_from_terms(terms: &[&str]) -> String {
    let parts: Vec<String> = terms.iter().take(2).map(|t| title_case(t)).collect();
    if parts.is_empty() {
        "Unsorted".to_string()
    } else {
        parts.join(" ")
    }
}

// ── Small numeric + string helpers ──────────────────────────────────────────

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn l2_normalized(v: &[f32]) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm < 1e-8 {
        return v.to_vec();
    }
    v.iter().map(|x| x / norm).collect()
}

fn mean_unit(vecs: &[&[f32]]) -> Option<Vec<f32>> {
    let dim = vecs.first()?.len();
    if dim == 0 {
        return None;
    }
    let mut acc = vec![0f32; dim];
    for v in vecs {
        if v.len() != dim {
            return None;
        }
        for (a, x) in acc.iter_mut().zip(v.iter()) {
            *a += x;
        }
    }
    let inv = 1.0 / vecs.len() as f32;
    for a in &mut acc {
        *a *= inv;
    }
    Some(l2_normalized(&acc))
}

fn title_case(s: &str) -> String {
    s.split_whitespace()
        .map(|w| {
            let mut ch = w.chars();
            match ch.next() {
                Some(f) => f.to_uppercase().collect::<String>() + ch.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(v: Vec<f32>) -> Vec<f32> {
        l2_normalized(&v)
    }

    fn file(id: i64, path: &str, clip: Vec<f32>, tags: &[&str]) -> SemanticFile {
        SemanticFile {
            file_id: id,
            source: PathBuf::from(path),
            clip: unit(clip),
            tags: tags.iter().map(|&s| s.to_string()).collect(),
            time_unix: 0.0,
        }
    }

    #[test]
    fn fuse_is_unit_norm() {
        let vocab = build_tag_vocab(&[file(1, "a.jpg", vec![1.0, 0.0, 0.0], &["beach"])], 16);
        let f = fuse(&file(1, "a.jpg", vec![1.0, 0.0, 0.0], &["beach"]), &vocab);
        let n: f32 = f.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((n - 1.0).abs() < 1e-4, "fused norm = {n}");
    }

    #[test]
    fn two_content_groups_separate_and_get_distinct_categories() {
        // Two tight content clusters (orthogonal CLIP directions + distinct tags).
        let mut files = Vec::new();
        for i in 0..6 {
            files.push(file(i, &format!("src/dog{i}.jpg"), vec![1.0, 0.0, 0.0, 0.0], &["dog", "park"]));
        }
        for i in 0..6 {
            files.push(file(100 + i, &format!("src/boat{i}.jpg"), vec![0.0, 1.0, 0.0, 0.0], &["boat", "lake"]));
        }
        let moves = semantic_classify(&files, &[], Path::new("/lib"));
        let cats: std::collections::HashSet<_> = moves.iter().map(|m| m.category.clone()).collect();
        assert_eq!(cats.len(), 2, "expected 2 groups, got {cats:?}");
    }

    #[test]
    fn cluster_routes_to_matching_existing_folder() {
        // A cluster of "dog" photos; an existing folder whose prototype matches.
        let files: Vec<SemanticFile> = (0..5)
            .map(|i| file(i, &format!("inbox/d{i}.jpg"), vec![1.0, 0.0, 0.0], &["dog"]))
            .collect();
        let protos = vec![FolderPrototype {
            path: PathBuf::from("/lib/Dogs"),
            centroid: unit(vec![1.0, 0.0, 0.0]),
        }];
        let moves = semantic_classify(&files, &protos, Path::new("/lib"));
        assert!(!moves.is_empty());
        assert!(
            moves.iter().all(|m| m.destination.starts_with("/lib/Dogs")),
            "cluster should route into the matching existing folder"
        );
        assert!(moves.iter().all(|m| m.category == "Dogs"));
    }

    #[test]
    fn folder_prototypes_skip_sparse_folders() {
        let files = vec![
            file(1, "/lib/A/x.jpg", vec![1.0, 0.0], &[]),
            file(2, "/lib/A/y.jpg", vec![1.0, 0.0], &[]),
            file(3, "/lib/B/z.jpg", vec![0.0, 1.0], &[]),
        ];
        let protos = folder_prototypes(&files, 2);
        assert_eq!(protos.len(), 1, "only folder A has >= 2 files");
        assert!(protos[0].path.ends_with("A"));
    }

    #[test]
    fn distinctive_naming_drops_ubiquitous_tags() {
        // "photo" tags every file (idf → 0, dropped); the rarer tags name groups.
        let mut files = Vec::new();
        for i in 0..6 {
            files.push(file(i, &format!("a/t{i}.jpg"), vec![1.0, 0.0, 0.0], &["photo", "tree"]));
        }
        for i in 0..4 {
            files.push(file(100 + i, &format!("a/s{i}.jpg"), vec![0.0, 1.0, 0.0], &["photo", "sunset", "beach"]));
        }
        let cats: std::collections::HashSet<_> = semantic_classify(&files, &[], Path::new("/lib"))
            .into_iter()
            .map(|m| m.category)
            .collect();
        assert!(cats.iter().any(|c| c.contains("Beach") || c.contains("Sunset")), "got {cats:?}");
        assert!(!cats.iter().any(|c| c == "Photo"), "ubiquitous 'photo' must not name a group: {cats:?}");
    }

    #[test]
    fn tight_match_to_existing_folder_auto_files_with_reason() {
        let files: Vec<SemanticFile> = (0..5)
            .map(|i| file(i, &format!("inbox/d{i}.jpg"), vec![1.0, 0.0, 0.0], &["dog"]))
            .collect();
        let protos = vec![FolderPrototype {
            path: PathBuf::from("/lib/Dogs"),
            centroid: unit(vec![1.0, 0.0, 0.0]),
        }];
        let moves = semantic_classify(&files, &protos, Path::new("/lib"));
        assert!(!moves.is_empty());
        assert!(moves.iter().all(|m| m.confidence == Confidence::Auto), "exact match should auto-file");
        assert!(moves.iter().all(|m| m.reason.as_deref().unwrap_or("").contains("Dogs")));
    }
}
