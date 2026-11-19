//! Butler restructure, semantic pass: fuses the CLIP embedding, content tags and capture time into one vector, clusters files by content, and routes each cluster to the nearest existing folder when the match is confident. Pure logic; the DB load lives in `commands/restructure.rs`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::identity_clustering::{self, Hyperparameters, Neighbor};
use super::restructure::{Confidence, ProposedMove};

/// Per-file signals. `clip` is the L2-normalized representative vector for the pass: 512-d CLIP image embedding (image pass), 384-d BGE text embedding (document pass), or empty for the non-image pass, which fills it with a bag-of-words signature in `non_image_signatures`.
#[derive(Clone)]
pub struct SemanticFile {
    pub file_id: i64,
    pub source: PathBuf,
    pub clip: Vec<f32>,
    pub tags: Vec<String>,
    pub time_unix: f64,
}

/// Fusion-weight + confidence-threshold profile. The image pass keeps the calibrated CLIP values; the non-image pass (filename+tag bag-of-words, since a PDF/video has no CLIP vector) runs a deliberately tighter profile so a sparser signature can't over-move. Byte-faithful with the Swift engine (RESTRUCTURE.md §2/R1).
#[derive(Clone, Copy)]
pub struct Profile {
    pub w_clip: f32,
    pub w_tags: f32,
    pub w_time: f32,
    pub folder_match_cos: f32,
    pub auto_folder_cos: f32,
    pub auto_cohesion: f32,
    pub review_cohesion: f32,
    pub min_margin: f32,
    pub auto_min_members: usize,
    /// Multi-hot bag-of-words ties defeat HNSW pruning (37 s build vs 0.9 s exact on 5k files), so these use exact search, which matches the Swift result.
    pub quantized_vectors: bool,
}

/// Representative is the L2-normalized 512-d CLIP image embedding. Thresholds are calibrated for CLIP cosines, which compress high on personal photos (intra-folder cohesion ~0.80); the original 0.55/0.72 routed every photo into one catch-all folder. Byte-faithful with the Swift `imageProfile`; env-overridable.
pub fn image_profile() -> Profile {
    Profile {
        w_clip: 0.70,
        w_tags: 0.22,
        w_time: 0.08,
        folder_match_cos: env_f32("FILEID_RESTRUCTURE_IMG_FOLDER_COS", 0.80),
        auto_folder_cos: env_f32("FILEID_RESTRUCTURE_IMG_AUTO_FOLDER_COS", 0.86),
        auto_cohesion: env_f32("FILEID_RESTRUCTURE_IMG_AUTO_COH", 0.78),
        review_cohesion: env_f32("FILEID_RESTRUCTURE_IMG_REVIEW_COH", 0.70),
        min_margin: 0.05,
        auto_min_members: 4,
        quantized_vectors: false,
    }
}

/// Frequent tags carry the grouping signal; rare ones are noise and would bloat the fused vector.
const TAG_VOCAB_CAP: usize = 256;

/// Filenames tokenize into many one-off terms, so the non-image bag-of-words needs a wider vocab than the image tag block.
const NON_IMAGE_VOCAB_CAP: usize = 512;

/// Owner kill-switch: `FILEID_RESTRUCTURE_NONIMAGE=0` turns the non-image semantic pass off (falls back to the rule cascade).
pub fn non_image_enabled() -> bool {
    std::env::var("FILEID_RESTRUCTURE_NONIMAGE")
        .map(|v| v != "0")
        .unwrap_or(true)
}

/// Representative is a filename+tag bag-of-words (sparser than CLIP), so demand a cleaner cluster and a tighter folder match. `w_tags` is 0: tags already live inside the representative and naming reads them directly. Thresholds are env-overridable (`FILEID_RESTRUCTURE_NI_*`) for calibration on a real library (RESTRUCTURE.md R1).
fn non_image_profile() -> Profile {
    Profile {
        w_clip: 0.74,
        w_tags: 0.0,
        w_time: 0.08,
        folder_match_cos: env_f32("FILEID_RESTRUCTURE_NI_FOLDER_COS", 0.60),
        auto_folder_cos: env_f32("FILEID_RESTRUCTURE_NI_AUTO_FOLDER_COS", 0.80),
        auto_cohesion: env_f32("FILEID_RESTRUCTURE_NI_AUTO_COH", 0.70),
        review_cohesion: env_f32("FILEID_RESTRUCTURE_NI_REVIEW_COH", 0.55),
        min_margin: 0.08,
        auto_min_members: 4,
        quantized_vectors: true,
    }
}

fn env_f32(key: &str, dflt: f32) -> f32 {
    std::env::var(key).ok().and_then(|s| s.parse().ok()).unwrap_or(dflt)
}

/// Folder basenames that must never act as a learn-your-style prototype: dumping grounds the butler should organize files out of.
const JUNK_FOLDER_NAMES: &[&str] = &[
    "downloads", "downloaded", "desktop", "new folder", "untitled", "temp", "tmp",
    "misc", "other", "stuff", "things", "files", "unsorted", "inbox",
];

/// Filename tokens with no grouping signal: camera/scan boilerplate, English connectors (so "boys at the zoo" doesn't name a "Boys The" folder), and extension tokens leaking from double-extension names like `E14.jpg.lps`. Lowercase.
const FILENAME_STOPWORDS: &[&str] = &[
    "img", "image", "dsc", "dscn", "dscf", "photo", "pic", "picture", "screenshot",
    "screen", "shot", "untitled", "new", "copy", "final", "draft", "version", "scan",
    "document", "file", "video", "vid", "clip",
    "the", "and", "for", "with", "from", "was", "are", "this", "that", "your", "our",
    "his", "her", "its", "out", "all", "has",
    "jpg", "jpeg", "png", "gif", "bmp", "heic", "heif", "tiff", "webp", "pdf", "doc",
    "docx", "xls", "xlsx", "ppt", "pptx", "txt", "rtf", "mov", "mp4", "avi", "mkv",
    "mp3", "wav", "zip", "rar", "lps",
];

/// Density-clustering hyperparameters for files (looser than faces: a semantic group is broader than one identity). One `FILEID_RESTRUCTURE_GRANULARITY` ∈ {loose, normal, tight} shifts the cluster cosines so the owner tunes folder count with one lever: loose = fewer, broader folders; tight = more, narrower. Applied identically on both engines.
pub fn granularity_delta() -> f32 {
    match std::env::var("FILEID_RESTRUCTURE_GRANULARITY").as_deref() {
        Ok("loose") => -0.05,
        Ok("tight") => 0.05,
        _ => 0.0,
    }
}

fn file_hyperparams() -> Hyperparameters {
    // Cluster-merge cosines sit in CLIP's compressed high band so a cluster ≈ one event. Env-overridable; GRANULARITY shifts all three together.
    let d = granularity_delta();
    Hyperparameters {
        pass1_cosine: env_f32("FILEID_RESTRUCTURE_CLUSTER_P1", 0.84) + d,
        pass2_cosine: env_f32("FILEID_RESTRUCTURE_CLUSTER_P2", 0.76) + d,
        pass2_margin: 0.08 + d * 0.5,
        pass3_variance_threshold: 0.06,
        pass3_min_mean_cosine: env_f32("FILEID_RESTRUCTURE_CLUSTER_P3", 0.76) + d,
        pass3_max_splits: 5,
        k_nn: 12,
    }
}

/// An existing folder learned from the current tree: its path + the mean (L2-normalized) CLIP embedding of the files currently in it.
pub struct FolderPrototype {
    pub path: PathBuf,
    pub centroid: Vec<f32>,
    /// Distinctive filename/folder-name tokens of this folder and its contents (the Dropbox "Smart Move" signal). Used additively: strong name agreement can upgrade a thin-margin content match, never override the content routing.
    pub name_tokens: std::collections::HashSet<String>,
}

/// Name-routing thresholds, overlap coefficient = |a∩b| / min(|a|,|b|). At/above AUTO the cluster's filenames agree strongly enough with the target folder to upgrade a thin-margin content match to Auto; at/above REASON the "filenames fit" note is added.
const NAME_AGREE_AUTO: f32 = 0.30;
const NAME_AGREE_REASON: f32 = 0.20;

/// Overlap coefficient |a∩b| / min(|a|,|b|), 0 when either is empty. Less penalized by union size than Jaccard, so a cluster sharing a folder's few distinctive tokens scores high.
fn overlap_coefficient(
    a: &std::collections::HashSet<String>,
    b: &std::collections::HashSet<String>,
) -> f32 {
    let m = a.len().min(b.len());
    if m == 0 {
        return 0.0;
    }
    let inter = a.iter().filter(|t| b.contains(*t)).count();
    inter as f32 / m as f32
}

/// Prototypes from the files' current locations: each parent folder with ≥ `min_files` becomes a class whose centroid is the mean CLIP vector of its contents (Nearest-Class-Mean / Dropbox "Smart Move"); the existing tree is the labeled ground truth.
pub fn folder_prototypes(files: &[SemanticFile], min_files: usize) -> Vec<FolderPrototype> {
    let mut by_folder: HashMap<PathBuf, Vec<&SemanticFile>> = HashMap::new();
    for f in files {
        if let Some(parent) = f.source.parent() {
            by_folder.entry(parent.to_path_buf()).or_default().push(f);
        }
    }
    let mut out = Vec::new();
    for (path, fs) in by_folder {
        if fs.len() < min_files {
            continue;
        }
        let clips: Vec<&[f32]> = fs.iter().map(|f| f.clip.as_slice()).collect();
        if let Some(centroid) = mean_unit(&clips) {
            let mut name_tokens: std::collections::HashSet<String> =
                filename_tokens(&path).into_iter().collect();
            for f in &fs {
                for t in filename_tokens(&f.source) {
                    name_tokens.insert(t);
                }
            }
            out.push(FolderPrototype { path, centroid, name_tokens });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// 2 hours between consecutive photos signals a new event; configurable via `FILEID_RESTRUCTURE_TIME_GAP` (seconds).
fn time_gap_secs() -> f64 {
    std::env::var("FILEID_RESTRUCTURE_TIME_GAP")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(7_200.0)
}

/// Pre-segments photos by capture-time gap into index groups clustered independently, so events separated by more than `time_gap_secs()` never compete in one cluster. Files without a valid timestamp (time_unix <= 0) form a trailing group (RESTRUCTURE.md §2).
fn time_segment_indices(files: &[SemanticFile]) -> Vec<Vec<usize>> {
    let gap = time_gap_secs();
    let (mut timed, untimed): (Vec<usize>, Vec<usize>) =
        (0..files.len()).partition(|&i| files[i].time_unix > 0.0);
    timed.sort_unstable_by(|&a, &b| {
        files[a]
            .time_unix
            .partial_cmp(&files[b].time_unix)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut segments: Vec<Vec<usize>> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    for &i in &timed {
        if let Some(&last) = cur.last()
            && files[i].time_unix - files[last].time_unix > gap {
                segments.push(std::mem::take(&mut cur));
            }
        cur.push(i);
    }
    if !cur.is_empty() {
        segments.push(cur);
    }
    if !untimed.is_empty() {
        segments.push(untimed);
    }
    segments
}

/// Classifies `files` into proposed moves: each cluster extends the nearest confident existing folder or becomes a new tag-named group under `library_root`. Noise/singleton files are not returned; the caller's rule cascade handles them. The image pass pre-segments by capture-time gap before clustering.
/// `used_group_names` is a caller-owned new-folder-name registry shared by the image, document and non-image passes (all target `library_root`) so two passes can't mint the SAME new folder and silently merge unrelated content.
pub(crate) fn semantic_classify(
    files: &[SemanticFile],
    prototypes: &[FolderPrototype],
    library_root: &Path,
    used_group_names: &mut std::collections::HashSet<String>,
    settled: &mut std::collections::HashSet<i64>,
) -> Vec<ProposedMove> {
    let profile = image_profile();
    let hp = file_hyperparams();
    let segments = time_segment_indices(files);
    if segments.len() <= 1 {
        return semantic_classify_profiled(&files.iter().collect::<Vec<_>>(), prototypes, library_root, profile, hp, used_group_names, settled);
    }
    let mut all_moves = Vec::new();
    for idxs in &segments {
        let seg: Vec<&SemanticFile> = idxs.iter().map(|&i| &files[i]).collect();
        let moves = semantic_classify_profiled(&seg, prototypes, library_root, profile, hp, used_group_names, settled);
        all_moves.extend(moves);
    }
    all_moves
}

fn semantic_classify_profiled(
    files: &[&SemanticFile],
    prototypes: &[FolderPrototype],
    library_root: &Path,
    profile: Profile,
    hp: Hyperparameters,
    used_group_names: &mut std::collections::HashSet<String>,
    settled: &mut std::collections::HashSet<i64>,
) -> Vec<ProposedMove> {
    if files.is_empty() {
        return Vec::new();
    }
    let global_freq = tag_frequencies(files);
    let vocab = vocab_from_freq(&global_freq, TAG_VOCAB_CAP);
    let fused: Vec<Vec<f32>> = files.iter().map(|f| fuse(f, &vocab, profile)).collect();
    let cluster_ids = cluster(&fused, hp, profile.quantized_vectors);

    let mut clusters: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, &cid) in cluster_ids.iter().enumerate() {
        clusters.entry(cid).or_default().push(i);
    }

    let mut moves = Vec::new();

    // Prototype lookup by folder path for the stay-put guard: a file whose CURRENT folder already fits at the routing bar is well-placed, and a butler moves misplaced files, it doesn't reshuffle good ones.
    let proto_by_path: HashMap<&Path, &FolderPrototype> =
        prototypes.iter().map(|p| (p.path.as_path(), p)).collect();

    // Group names already claimed by a different new-group cluster this run (shared across time-segments so two segments producing "Beach" get distinct names). Consulted ONLY by the new-group branch; the existing-folder branch legitimately routes many clusters into one folder.

    // Smallest cluster id first, so collision disambiguation is deterministic.
    let mut ids: Vec<usize> = clusters.keys().copied().collect();
    ids.sort_unstable();
    for cid in ids {
        let members = &clusters[&cid];
        // Singletons (the clusterer's outliers) have no group signal: the rule cascade places them.
        if members.len() < 2 {
            continue;
        }
        let member_clip: Vec<&[f32]> = members.iter().map(|&i| files[i].clip.as_slice()).collect();
        let centroid = match mean_unit(&member_clip) {
            Some(c) => c,
            None => continue,
        };
        let coh = cohesion(&member_clip, &centroid);
        let cluster_name_tokens: std::collections::HashSet<String> = members
            .iter()
            .flat_map(|&i| filename_tokens(&files[i].source))
            .collect();

        let (dest_dir, category, confidence, reason, route_sim) =
            match nearest_two_folders(&centroid, prototypes) {
                // Learn-your-style: route to the nearest confident existing folder. Auto-file only when the match is strong and unambiguous (clear margin over the runner-up) on a tight cluster; otherwise surface for review. Only an in-root prototype is a valid destination: a folder outside library_root would be silently rejected by the apply layer, so such a match falls through to a new in-root group.
                Some((proto, sim, runner_up))
                    if sim >= profile.folder_match_cos && proto.path.starts_with(library_root) =>
                {
                    let name = folder_display_name(&proto.path);
                    // Name-routing (Dropbox Smart Move): additive, so strong filename overlap upgrades a thin-margin content match to Auto but never overrides the content decision.
                    let name_sim = overlap_coefficient(&cluster_name_tokens, &proto.name_tokens);
                    let content_auto = sim >= profile.auto_folder_cos
                        && coh >= profile.review_cohesion
                        && (sim - runner_up) >= profile.min_margin;
                    let name_auto = name_sim >= NAME_AGREE_AUTO
                        && sim >= profile.folder_match_cos
                        && coh >= profile.review_cohesion;
                    let confidence = if content_auto || name_auto {
                        Confidence::Auto
                    } else {
                        Confidence::Review
                    };
                    let reason = if name_sim >= NAME_AGREE_REASON {
                        format!(
                            "Matches your '{name}' folder ({:.0}% alike; the filenames fit too)",
                            sim * 100.0
                        )
                    } else {
                        format!("Matches your '{name}' folder ({:.0}% alike)", sim * 100.0)
                    };
                    (proto.path.clone(), name, confidence, reason, Some(sim))
                }
                _ => {
                    let terms = distinctive_terms(members, files, &global_freq);
                    let mut base = group_name_from_terms(&terms);
                    // No distinctive tags: name the group by when it happened ("June 2014") instead of minting "Unsorted N" folders. The median capture time keeps one outlier from mislabeling the event; undated clusters keep the generic name.
                    if terms.is_empty() {
                        let mut times: Vec<f64> = members
                            .iter()
                            .map(|&i| files[i].time_unix)
                            .filter(|t| *t > 0.0)
                            .collect();
                        if !times.is_empty() {
                            times.sort_by(|a, b| a.total_cmp(b));
                            let (year, month) =
                                super::restructure::year_month(times[times.len() / 2]);
                            base = format!("{} {}", super::restructure::month_name(month), year);
                        }
                    }
                    let mut pretty = base.clone();
                    let safe_base = crate::util::path_safety::safe_filename_component(&pretty);
                    let mut safe = safe_base.clone();
                    // Disambiguate in the SANITIZED namespace that backs the folder: names differing only in chars safe_filename_component maps to '_' ("16:9" vs "16/9") would otherwise collide into one directory. Prefer the next distinctive term first.
                    if used_group_names.contains(&safe)
                        && let Some(extra) = terms.get(2) {
                            pretty = format!("{} {}", base, title_case(extra));
                            safe = crate::util::path_safety::safe_filename_component(&pretty);
                        }
                    // Numeric-suffix fallback: build each candidate so the suffix ALWAYS survives the 200-scalar filename cap. A base that already sanitizes to ~200 chars would otherwise truncate every "{base} {n}" to the same string, the uniqueness check would never clear and plan generation would hang. Distinct n ⇒ distinct candidate ⇒ termination within
                    // used_group_names
                    // +1 iterations.
                    const SAFE_NAME_MAX: usize = 200; // mirrors safe_filename_component MAX_LEN
                    let mut n = 2usize;
                    while used_group_names.contains(&safe) {
                        let suffix = format!(" {n}");
                        let room = SAFE_NAME_MAX.saturating_sub(suffix.chars().count());
                        let prefix: String = safe_base.chars().take(room).collect();
                        safe = format!("{prefix}{suffix}");
                        pretty = format!("{} {}", base, n);
                        n += 1;
                    }
                    used_group_names.insert(safe.clone());
                    let confidence = if coh >= profile.auto_cohesion
                        && members.len() >= profile.auto_min_members
                    {
                        Confidence::Auto
                    } else if coh >= profile.review_cohesion {
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
                    // Path-safe directory name (mirrors the person route): illegal/separator chars in tag-derived names ("16:9", "dog/cat") would mis-route or fail the move. `safe` was computed during dedup so uniqueness ran in the same namespace; `pretty` stays for the human-facing category/reason.
                    (library_root.join(&safe), pretty, confidence, reason, None)
                }
            };

        for &i in members {
            let file = &files[i];
            let Some(name) = file.source.file_name() else { continue };
            let parent = file.source.parent();
            // Already home: a move into the file's current folder is pure churn. Record it as settled so the rule cascade can't re-propose a date-tree move for a file the semantic pass deliberately left in place.
            if parent == Some(dest_dir.as_path()) {
                settled.insert(file.file_id);
                continue;
            }
            // Stay-put guard: if the file's current folder is itself a valid prototype and the file fits it at the routing bar, moving it is reshuffling. Only a destination that clears the routing margin may pull it out; a freshly minted group never outranks a good existing home.
            if !file.clip.is_empty()
                && let Some(cur) = parent.and_then(|p| proto_by_path.get(p)) {
                    let fit = dot(&file.clip, &cur.centroid);
                    let pull = route_sim.unwrap_or(profile.folder_match_cos);
                    if fit >= profile.folder_match_cos && fit + profile.min_margin >= pull {
                        settled.insert(file.file_id);
                        continue;
                    }
                }
            let dest = dest_dir.join(name);
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


/// Clusters non-image files (documents, video, audio: no CLIP image embedding) by a filename-token + content-tag bag-of-words signature, so a mixed library groups by content instead of dumping into `Documents/<Year>`. Additive: the image pass claims its files first and the rule cascade catches the rest. The bag-of-words IS the representative vector, so the same clusterer and folder matching apply under the tighter non-image profile.
/// `used_group_names` is the registry shared with the image and document passes.
pub(crate) fn classify_non_image(
    files: &[SemanticFile],
    library_root: &Path,
    used_group_names: &mut std::collections::HashSet<String>,
    settled: &mut std::collections::HashSet<i64>,
) -> Vec<ProposedMove> {
    let sigs = non_image_signatures(files);
    if sigs.len() < 2 {
        return Vec::new();
    }
    // Learn-your-style targets, but NEVER generic dumping grounds (Downloads, Desktop, Temp, …): the point is to organize files out of those. Real user folders ("Taxes", "Invoices") still anchor.
    let protos: Vec<FolderPrototype> = folder_prototypes(&sigs, 4)
        .into_iter()
        .filter(|p| !is_junk_prototype_folder(&p.path))
        .collect();
    semantic_classify_profiled(&sigs.iter().collect::<Vec<_>>(), &protos, library_root, non_image_profile(), file_hyperparams(), used_group_names, settled)
}

/// Document-content pass: clusters documents by their BGE text embedding (from `text_embeddings`, stored in each file's `clip`), which reads content rather than filename (owner A/B on 533 real docs: nearest-neighbour-same-folder 49%→57%). Each `file.clip` MUST be the BGE vector; docs without one are excluded by the caller and fall through to `classify_non_image`. Doc-specific thresholds because BGE cosines sit lower than CLIP's (RESTRUCTURE.md R3).
/// `used_group_names` is the registry shared with the image and non-image passes.
pub(crate) fn classify_documents(
    files: &[SemanticFile],
    library_root: &Path,
    used_group_names: &mut std::collections::HashSet<String>,
    settled: &mut std::collections::HashSet<i64>,
) -> Vec<ProposedMove> {
    if files.len() < 2 {
        return Vec::new();
    }
    let protos: Vec<FolderPrototype> = folder_prototypes(files, 4)
        .into_iter()
        .filter(|p| !is_junk_prototype_folder(&p.path))
        .collect();
    semantic_classify_profiled(&files.iter().collect::<Vec<_>>(), &protos, library_root, doc_profile(), doc_hyperparams(), used_group_names, settled)
}

/// Document content-embedding profile. The representative IS the 384-d BGE vector, so `w_clip` dominates and `w_tags`/`w_time` are tiny (a document has no meaningful capture time). Calibrated on the owner's ~1.4k-doc corpus: the engine MEAN-pools BGE, whose cosines compress high (within-folder cohesion ≈ 0.786, inter-folder centroid p90 ≈ 0.80), so the bars sit there, not at the lower CLS-pooled values that collapsed every doc into one folder. Env-overridable (RESTRUCTURE.md R3).
fn doc_profile() -> Profile {
    Profile {
        w_clip: 0.92,
        w_tags: 0.06,
        w_time: 0.02,
        folder_match_cos: env_f32("FILEID_RESTRUCTURE_DOC_FOLDER_COS", 0.78),
        auto_folder_cos: env_f32("FILEID_RESTRUCTURE_DOC_AUTO_FOLDER_COS", 0.84),
        auto_cohesion: env_f32("FILEID_RESTRUCTURE_DOC_AUTO_COH", 0.78),
        review_cohesion: env_f32("FILEID_RESTRUCTURE_DOC_REVIEW_COH", 0.70),
        min_margin: 0.05,
        auto_min_members: 4,
        quantized_vectors: false,
    }
}

/// Cluster-merge cosines for the MEAN-pooled BGE document space (compresses high, within-folder ≈ 0.786). Env-overridable; GRANULARITY still shifts all three.
fn doc_hyperparams() -> Hyperparameters {
    let d = granularity_delta();
    Hyperparameters {
        pass1_cosine: env_f32("FILEID_RESTRUCTURE_DOC_CLUSTER_P1", 0.82) + d,
        pass2_cosine: env_f32("FILEID_RESTRUCTURE_DOC_CLUSTER_P2", 0.74) + d,
        pass2_margin: 0.06,
        pass3_variance_threshold: 0.06,
        pass3_min_mean_cosine: env_f32("FILEID_RESTRUCTURE_DOC_CLUSTER_P3", 0.74) + d,
        pass3_max_splits: 5,
        k_nn: 12,
    }
}

/// A folder that must never act as a learn-your-style prototype: matches the exact junk names AND any folder whose first word is a dumping-ground word ("Desktop 1.0", "Downloads (2)", "Temp files"). All-digit names that aren't a plausible year (export/DB artifacts like "332699") are machine noise; year folders ("2019") stay valid targets.
pub(crate) fn is_junk_prototype_folder(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();
    if JUNK_FOLDER_NAMES.contains(&name.as_str()) {
        return true;
    }
    if !name.is_empty() && name.chars().all(|c| c.is_ascii_digit()) {
        let plausible_year =
            name.len() == 4 && matches!(name.parse::<u32>(), Ok(y) if (1900..=2099).contains(&y));
        if !plausible_year {
            return true;
        }
    }
    let first_word = name.split(|c: char| !c.is_alphabetic()).find(|w| !w.is_empty());
    matches!(
        first_word,
        Some("desktop" | "downloads" | "download" | "downloaded" | "temp" | "tmp"
            | "unsorted" | "inbox" | "misc")
    )
}

/// Bag-of-words representatives for the non-image pass: each file's `clip` becomes an L2-normalized multi-hot over a shared, frequency-capped vocab of (filename tokens ∪ content tags); `tags` keeps the token set so the namer can label the group. A file with no in-vocab token is dropped and falls through to the rule cascade.
fn non_image_signatures(files: &[SemanticFile]) -> Vec<SemanticFile> {
    let token_sets: Vec<Vec<String>> = files
        .iter()
        .map(|f| {
            let mut set: std::collections::BTreeSet<String> =
                filename_tokens(&f.source).into_iter().collect();
            for t in &f.tags {
                let lt = t.to_lowercase();
                if !lt.is_empty() {
                    set.insert(lt);
                }
            }
            set.into_iter().collect()
        })
        .collect();

    let mut freq: HashMap<String, usize> = HashMap::new();
    for toks in &token_sets {
        for t in toks {
            *freq.entry(t.clone()).or_insert(0) += 1;
        }
    }
    let vocab = vocab_from_freq(&freq, NON_IMAGE_VOCAB_CAP);
    if vocab.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::with_capacity(files.len());
    for (i, f) in files.iter().enumerate() {
        // A file whose every token is unique to it shares NO signal with any other file and can never cluster: exclude it up front rather than trusting the clusterer to noise-reject an orthogonal point, because with k_nn >= n and all-tied zero similarities the kNN tie order is arch-sensitive and a lone file would group-or-not by luck.
        if !token_sets[i].iter().any(|t| freq.get(t).copied().unwrap_or(0) >= 2) {
            continue;
        }
        let mut vec = vec![0f32; vocab.len()];
        let mut any = false;
        for t in &token_sets[i] {
            if let Some(&idx) = vocab.get(t) {
                vec[idx] = 1.0;
                any = true;
            }
        }
        if !any {
            continue;
        }
        out.push(SemanticFile {
            file_id: f.file_id,
            source: f.source.clone(),
            clip: l2_normalized(&vec),
            tags: token_sets[i].clone(),
            time_unix: f.time_unix,
        });
    }
    out
}

/// Lowercase alphanumeric filename tokens, extension dropped; drops pure-numeric, very short and generic camera/scan tokens, so `IMG_4821.heic` yields nothing while `acme_invoice_2023.pdf` yields `acme` + `invoice`.
pub(crate) fn filename_tokens(path: &Path) -> Vec<String> {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();
    stem.split(|c: char| !c.is_alphanumeric())
        .filter(|t| {
            t.chars().count() >= 3
                && t.chars().any(|c| c.is_alphabetic())
                && !FILENAME_STOPWORDS.contains(t)
        })
        .map(|t| t.to_string())
        .collect()
}


/// Global tag frequency across all files: drives the vocab cap and the c-TF-IDF weighting in [`distinctive_terms`].
fn tag_frequencies(files: &[&SemanticFile]) -> HashMap<String, usize> {
    let mut freq: HashMap<String, usize> = HashMap::new();
    for f in files {
        for t in &f.tags {
            *freq.entry(t.clone()).or_insert(0) += 1;
        }
    }
    freq
}

fn vocab_from_freq(freq: &HashMap<String, usize>, cap: usize) -> HashMap<String, usize> {
    let mut ranked: Vec<(&String, &usize)> = freq.iter().collect();
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
    vocab_from_freq(&tag_frequencies(&files.iter().collect::<Vec<_>>()), cap)
}

/// Per-block L2-normalize, scale by weight, concatenate, then L2-normalize the whole so the clusterer's cosine is meaningful.
fn fuse(file: &SemanticFile, vocab: &HashMap<String, usize>, profile: Profile) -> Vec<f32> {
    let mut out = Vec::with_capacity(file.clip.len() + vocab.len() + 5);

    let clip = l2_normalized(&file.clip);
    out.extend(clip.iter().map(|x| x * profile.w_clip));

    let mut tags = vec![0f32; vocab.len()];
    for t in &file.tags {
        if let Some(&idx) = vocab.get(t) {
            tags[idx] = 1.0;
        }
    }
    let tags = l2_normalized(&tags);
    out.extend(tags.iter().map(|x| x * profile.w_tags));

    for v in time_features(file.time_unix) {
        out.push(v * profile.w_time);
    }

    l2_normalized(&out)
}

/// Five time features: day-of-year cyclical (2), time-of-day cyclical (2), log-compressed absolute year (1). They separate seasons, morning vs evening, and the same calendar day in different years. Byte-faithful with the Swift engine (RESTRUCTURE.md §2).
fn time_features(time_unix: f64) -> [f32; 5] {
    if time_unix <= 0.0 {
        return [0.0; 5];
    }
    let secs = time_unix as i64;
    let day = (secs / 86_400) % 365;
    let day_angle = std::f64::consts::TAU * (day as f64) / 365.0;
    let second_of_day = secs % 86_400;
    let tod_angle = std::f64::consts::TAU * (second_of_day as f64) / 86_400.0;
    let years = (time_unix / (365.25 * 86_400.0)).clamp(0.0, 100.0);
    let log_year = years.ln_1p() / 100.0_f64.ln_1p();
    [
        day_angle.sin() as f32,
        day_angle.cos() as f32,
        tod_angle.sin() as f32,
        tod_angle.cos() as f32,
        log_year as f32,
    ]
}


/// Two-pass density clustering: brute-force cosine kNN below `HNSW_MIN`, HNSW above (mirrors `face_clustering::cluster`).
fn cluster(fused: &[Vec<f32>], params: Hyperparameters, exact_only: bool) -> Vec<usize> {
    const HNSW_MIN: usize = 5_000;
    let n = fused.len();
    // Can't request more neighbors than other points exist; k_nn >= n made the kNN over an all-tied set arch-sensitive (see non_image_signatures).
    let k = params.k_nn.min(n.saturating_sub(1).max(1));

    let hnsw = (n >= HNSW_MIN && !exact_only).then(|| crate::util::hnsw_index::DedupIndex::build(fused));

    // Bounded top-k: O(n) partition instead of an O(n log n) sort of all n-1 neighbors when only k are used.
    let top_k = |mut hits: Vec<Neighbor>| {
        let cmp = |a: &Neighbor, b: &Neighbor| {
            b.similarity
                .total_cmp(&a.similarity)
                .then_with(|| a.idx.cmp(&b.idx))
        };
        if hits.len() > k {
            hits.select_nth_unstable_by(k, cmp);
            hits.truncate(k);
        }
        hits.sort_by(cmp);
        hits
    };
    // Below HNSW_MIN every list is computed once, in parallel, up front: the clusterer asks for each file's neighbours up to three times, and the per-query scalar scan was most of a plan's time (1.1 s of 1.9 s on 3,400 photos).
    let exact: Option<Vec<Vec<Neighbor>>> = hnsw.is_none().then(|| {
        use rayon::prelude::*;
        (0..n)
            .into_par_iter()
            .map(|i| {
                top_k(
                    (0..n)
                        .filter(|&j| j != i)
                        .map(|j| Neighbor { idx: j, similarity: dot_lanes(&fused[i], &fused[j]) })
                        .collect(),
                )
            })
            .collect()
    });

    let mut knn_search = crate::util::hnsw_index::Searcher::default();
    let result = identity_clustering::cluster(
        fused,
        |i| match (&exact, &hnsw) {
            (Some(lists), _) => lists[i].clone(),
            // Reuse one Search scratch across the sweep: a fresh one re-zeros an n-byte visited set per query, an O(n²) term over the pass.
            (None, Some(idx)) => top_k(
                idx.neighbours(&mut knn_search, fused, i, k)
                    .into_iter()
                    .map(|(j, d)| Neighbor { idx: j, similarity: 1.0 - d / 2.0 })
                    .collect(),
            ),
            (None, None) => Vec::new(),
        },
        params,
    );
    result.cluster_ids
}


/// Mean cosine of a cluster's members to its centroid (unit vectors, so dot == cosine).
fn cohesion(member_clip: &[&[f32]], centroid: &[f32]) -> f32 {
    if member_clip.is_empty() {
        return 0.0;
    }
    let sum: f32 = member_clip.iter().map(|c| dot(c, centroid)).sum();
    sum / member_clip.len() as f32
}

/// Nearest folder prototype to `centroid` plus the runner-up similarity, so the caller can gate on the top-1−top-2 margin and abstain when two folders fit almost equally (RESTRUCTURE.md §4).
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

/// A cluster's most distinctive tags by c-TF-IDF (frequent inside the cluster, rare across the library), so a name is specific ("Beach Trip") not bland ("Photos"). Ubiquitous tags (idf 0) drop out on their own.
fn distinctive_terms<'a>(
    members: &[usize],
    files: &'a [&SemanticFile],
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
            // Compute idf's ln in f64 then narrow: f32 `logf` is libm-dependent (can differ Windows↔macOS by ~1 ULP), which could flip the tie order of near-equal terms and pick a different group folder name across platforms. f64 `ln` is consistently correctly rounded, matching the macOS `Float(log(Double(...)))`.
            (t, tf * (((total / df) as f64).ln().max(0.0) as f32))
        })
        .collect();
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

fn group_name_from_terms(terms: &[&str]) -> String {
    let parts: Vec<String> = terms.iter().take(2).map(|t| title_case(t)).collect();
    if parts.is_empty() {
        "Unsorted".to_string()
    } else {
        parts.join(" ")
    }
}


fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

use crate::util::vector_math::dot as dot_lanes;

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

    fn lcg(state: &mut u64) -> f32 {
        *state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((*state >> 33) as f32 / (1u64 << 31) as f32) - 0.5
    }

    #[test]
    fn identical_vectors_past_the_hnsw_threshold_share_a_cluster() {
        let _counting = crate::util::hnsw_index::tests::COUNTING.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = 7u64;
        let mut fused: Vec<Vec<f32>> = (0..3_500).map(|_| unit((0..37).map(|_| lcg(&mut state)).collect())).collect();
        for i in 0..1_700 {
            fused.push(fused[i % 40].clone());
        }
        let ids = cluster(&fused, file_hyperparams(), false);
        assert_eq!(ids.len(), fused.len());
        assert!(ids[..40].iter().zip(&ids[3_500..3_540]).all(|(a, b)| a == b), "identical vectors share a cluster");
    }

    /// Bag-of-words vectors (the non-image pass) are multi-hot, so many pairs tie. Past the HNSW threshold they must not build a graph at all: counts HNSW distance evaluations (not time, so load can't flip it); building the graph for this shape took about 7 evaluations per pair.
    #[test]
    fn quantized_vectors_past_the_hnsw_threshold_build_no_graph() {
        use crate::util::hnsw_index::tests::{COUNTED_DIM, COUNTING, EVALUATIONS};
        let _counting = COUNTING.lock().unwrap_or_else(|e| e.into_inner());
        assert!(non_image_profile().quantized_vectors && !image_profile().quantized_vectors && !doc_profile().quantized_vectors);
        let mut state = 3u64;
        let n = 5_200usize;
        let fused: Vec<Vec<f32>> = (0..n)
            .map(|_| {
                let mut v = vec![0f32; COUNTED_DIM];
                v[0] = 1.0; // every file shares one token
                for _ in 0..4 {
                    v[1 + ((lcg(&mut state) + 0.5) * 36.0) as usize % 36] = 1.0;
                }
                unit(v)
            })
            .collect();
        EVALUATIONS.store(0, std::sync::atomic::Ordering::Relaxed);
        let ids = cluster(&fused, file_hyperparams(), true);
        assert_eq!(ids.len(), n);
        assert_eq!(EVALUATIONS.load(std::sync::atomic::Ordering::Relaxed), 0, "the exact search builds no HNSW graph");
        EVALUATIONS.store(0, std::sync::atomic::Ordering::Relaxed);
        let _ = cluster(&fused, file_hyperparams(), false);
        let graph = EVALUATIONS.load(std::sync::atomic::Ordering::Relaxed);
        eprintln!("QUANTIZED graph evaluations {graph} for n={n} (n*n={})", (n * n));
        assert!(graph > (n * n) as u64 / 2, "this shape is the one that degenerates the graph ({graph} evaluations)");
    }

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
    fn all_digit_folders_are_junk_prototypes_but_years_are_not() {
        assert!(is_junk_prototype_folder(Path::new("/x/332699")));
        assert!(is_junk_prototype_folder(Path::new("/x/47138")));
        assert!(is_junk_prototype_folder(Path::new("/x/0")));
        assert!(!is_junk_prototype_folder(Path::new("/x/2019")));
        assert!(!is_junk_prototype_folder(Path::new("/x/1987")));
        assert!(is_junk_prototype_folder(Path::new("/x/2985")));
    }

    #[test]
    fn well_placed_members_stay_put_instead_of_reshuffling() {
        // Two folders that are equally good homes for the same content: every file already fits where it is, so the butler proposes NOTHING (the stay-put guard).
        let mut files = Vec::new();
        for i in 0..4 {
            files.push(file(i, &format!("/lib/A/a{i}.jpg"), vec![1.0, 0.0, 0.0, 0.0], &["dog"]));
        }
        for i in 0..4 {
            files.push(file(10 + i, &format!("/lib/B/b{i}.jpg"), vec![1.0, 0.0, 0.0, 0.0], &["dog"]));
        }
        let protos = folder_prototypes(&files, 4);
        assert_eq!(protos.len(), 2, "both folders should be prototypes");
        let moves = semantic_classify(
            &files,
            &protos,
            Path::new("/lib"),
            &mut std::collections::HashSet::new(),
            &mut std::collections::HashSet::new(),
        );
        assert!(moves.is_empty(), "well-placed files must not be reshuffled: {moves:?}");
    }

    #[test]
    fn misplaced_members_still_route_to_the_matching_folder() {
        // Four files establish the "Beach" prototype; two identical strays sit in an un-prototyped folder. Only the strays move, and no move targets the folder a file already sits in.
        let mut files = Vec::new();
        for i in 0..4 {
            files.push(file(i, &format!("/lib/Beach/a{i}.jpg"), vec![1.0, 0.0, 0.0, 0.0], &["beach"]));
        }
        for i in 0..2 {
            files.push(file(10 + i, &format!("/lib/New Folder/s{i}.jpg"), vec![1.0, 0.0, 0.0, 0.0], &["beach"]));
        }
        let protos = folder_prototypes(&files, 4);
        assert_eq!(protos.len(), 1);
        let moves = semantic_classify(
            &files,
            &protos,
            Path::new("/lib"),
            &mut std::collections::HashSet::new(),
            &mut std::collections::HashSet::new(),
        );
        assert_eq!(moves.len(), 2, "only the strays move: {moves:?}");
        for m in &moves {
            assert!(m.source.starts_with("/lib/New Folder"), "{m:?}");
            assert_eq!(m.destination.parent(), Some(Path::new("/lib/Beach")), "{m:?}");
        }
    }

    #[test]
    fn labelled_tree_moves_only_the_strays() {
        // Labelled check shared with Swift: 44 well-placed files (four 8-file topic folders plus two identical-content twin folders of 6) and 6 labelled strays. A plan is better when it moves fewer well-placed files yet routes the strays.
        let vec = |cls: usize, i: usize| {
            let mut v = vec![0.0f32; 8];
            v[cls] = 1.0;
            v[5] = 0.04 * (i % 3) as f32;
            v[6] = 0.03 * (i % 2) as f32;
            v
        };
        let mut files = Vec::new();
        let add = |files: &mut Vec<SemanticFile>, path: String, cls: usize, i: usize| {
            let id = files.len() as i64 + 1;
            files.push(file(id, &path, vec(cls, i), &[&format!("topic{cls}")]));
        };
        for c in 0..4 {
            for i in 0..8 {
                add(&mut files, format!("/lib/Topic{c}/f{i}.jpg"), c, i);
            }
        }
        for i in 0..6 {
            add(&mut files, format!("/lib/TwinA/f{i}.jpg"), 4, i);
        }
        for i in 0..6 {
            add(&mut files, format!("/lib/TwinB/f{i}.jpg"), 4, i);
        }
        let stray_start = files.len() as i64 + 1;
        for i in 0..3 {
            add(&mut files, format!("/lib/Inbox/z{i}.jpg"), 0, i);
        }
        for i in 0..3 {
            add(&mut files, format!("/lib/Inbox/y{i}.jpg"), 2, i);
        }
        let protos: Vec<_> = folder_prototypes(&files, 4)
            .into_iter()
            .filter(|p| !is_junk_prototype_folder(&p.path))
            .collect();
        let moves = semantic_classify(
            &files,
            &protos,
            Path::new("/lib"),
            &mut std::collections::HashSet::new(),
            &mut std::collections::HashSet::new(),
        );
        assert!(moves.iter().all(|m| m.file_id >= stray_start), "{moves:?}");
        let strays: Vec<_> = moves.iter().filter(|m| m.file_id >= stray_start).collect();
        assert_eq!(strays.len(), 6, "{moves:?}");
        for m in strays {
            let want = if m.source.to_string_lossy().contains("/z") { "/lib/Topic0" } else { "/lib/Topic2" };
            assert_eq!(m.destination.parent(), Some(Path::new(want)), "{m:?}");
        }
    }

    #[test]
    fn tagless_dated_cluster_is_named_by_month_year_not_unsorted() {
        let june_2014 = 1_402_444_800.0;
        let mut files = Vec::new();
        for i in 0..6 {
            let mut f = file(i, &format!("/lib/dump/p{i}.jpg"), vec![1.0, 0.0, 0.0, 0.0], &[]);
            f.time_unix = june_2014 + i as f64;
            files.push(f);
        }
        let moves = semantic_classify(
            &files,
            &[],
            Path::new("/lib"),
            &mut std::collections::HashSet::new(),
            &mut std::collections::HashSet::new(),
        );
        assert!(!moves.is_empty());
        for m in &moves {
            assert_eq!(m.category, "June 2014", "{m:?}");
            assert_eq!(m.destination.parent(), Some(Path::new("/lib/June 2014")), "{m:?}");
        }
    }

    #[test]
    fn fuse_is_unit_norm() {
        let vocab = build_tag_vocab(&[file(1, "a.jpg", vec![1.0, 0.0, 0.0], &["beach"])], 16);
        let f = fuse(&file(1, "a.jpg", vec![1.0, 0.0, 0.0], &["beach"]), &vocab, image_profile());
        let n: f32 = f.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((n - 1.0).abs() < 1e-4, "fused norm = {n}");
    }

    #[test]
    fn two_content_groups_separate_and_get_distinct_categories() {
        let mut files = Vec::new();
        for i in 0..6 {
            files.push(file(i, &format!("src/dog{i}.jpg"), vec![1.0, 0.0, 0.0, 0.0], &["dog", "park"]));
        }
        for i in 0..6 {
            files.push(file(100 + i, &format!("src/boat{i}.jpg"), vec![0.0, 1.0, 0.0, 0.0], &["boat", "lake"]));
        }
        let moves = semantic_classify(&files, &[], Path::new("/lib"), &mut std::collections::HashSet::new(), &mut std::collections::HashSet::new());
        let cats: std::collections::HashSet<_> = moves.iter().map(|m| m.category.clone()).collect();
        assert_eq!(cats.len(), 2, "expected 2 groups, got {cats:?}");
    }

    #[test]
    fn sanitization_colliding_group_names_get_distinct_folders() {
        // Two clusters whose distinctive tags differ ONLY in chars safe_filename_component maps to '_' ("16:9" vs "16/9"): dedup must back them with DISTINCT directories and the numeric-suffix loop must terminate.
        let mut files = Vec::new();
        for i in 0..6 {
            files.push(file(i, &format!("a/r{i}.jpg"), vec![1.0, 0.0, 0.0, 0.0], &["16:9"]));
        }
        for i in 0..6 {
            files.push(file(100 + i, &format!("a/s{i}.jpg"), vec![0.0, 1.0, 0.0, 0.0], &["16/9"]));
        }
        let moves = semantic_classify(&files, &[], Path::new("/lib"), &mut std::collections::HashSet::new(), &mut std::collections::HashSet::new());
        assert_eq!(moves.len(), 12, "all files placed: {moves:?}");
        let parents: std::collections::HashSet<_> = moves
            .iter()
            .filter_map(|m| m.destination.parent().map(|p| p.to_path_buf()))
            .collect();
        assert_eq!(
            parents.len(),
            2,
            "sanitization-colliding group names must get 2 distinct folders, got {parents:?}"
        );
    }

    #[test]
    fn cluster_routes_to_matching_existing_folder() {
        let files: Vec<SemanticFile> = (0..5)
            .map(|i| file(i, &format!("inbox/d{i}.jpg"), vec![1.0, 0.0, 0.0], &["dog"]))
            .collect();
        let protos = vec![FolderPrototype {
            path: PathBuf::from("/lib/Dogs"),
            centroid: unit(vec![1.0, 0.0, 0.0]),
            name_tokens: std::collections::HashSet::default(),
        }];
        let moves = semantic_classify(&files, &protos, Path::new("/lib"), &mut std::collections::HashSet::new(), &mut std::collections::HashSet::new());
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
        let mut files = Vec::new();
        for i in 0..6 {
            files.push(file(i, &format!("a/t{i}.jpg"), vec![1.0, 0.0, 0.0], &["photo", "tree"]));
        }
        for i in 0..4 {
            files.push(file(100 + i, &format!("a/s{i}.jpg"), vec![0.0, 1.0, 0.0], &["photo", "sunset", "beach"]));
        }
        let cats: std::collections::HashSet<_> = semantic_classify(&files, &[], Path::new("/lib"), &mut std::collections::HashSet::new(), &mut std::collections::HashSet::new())
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
            name_tokens: std::collections::HashSet::default(),
        }];
        let moves = semantic_classify(&files, &protos, Path::new("/lib"), &mut std::collections::HashSet::new(), &mut std::collections::HashSet::new());
        assert!(!moves.is_empty());
        assert!(moves.iter().all(|m| m.confidence == Confidence::Auto), "exact match should auto-file");
        assert!(moves.iter().all(|m| m.reason.as_deref().unwrap_or("").contains("Dogs")));
    }

    // Labeled ground truth: filename agreement with a target folder is a strong routing signal that can decide a thin-margin content match; these pin the additive behavior.

    #[test]
    fn folder_prototypes_collect_name_tokens() {
        let files = vec![
            file(1, "/lib/Taxes/return_2022.pdf", vec![1.0, 0.0], &[]),
            file(2, "/lib/Taxes/return_2023.pdf", vec![1.0, 0.0], &[]),
        ];
        let protos = folder_prototypes(&files, 2);
        assert_eq!(protos.len(), 1);
        assert!(protos[0].name_tokens.contains("taxes"), "folder name token");
        assert!(protos[0].name_tokens.contains("return"), "sibling filename token");
    }

    #[test]
    fn name_agreement_upgrades_a_thin_content_match_to_auto() {
        // CONTENT only weakly matches (cosine ~0.82, between the 0.80 match bar and the 0.86 content-auto bar) but the FILENAMES clearly belong in the folder: auto-file on the name evidence.
        let files: Vec<SemanticFile> = (0..3)
            .map(|i| file(i, &format!("inbox/acme_invoice_part{i}.pdf"), vec![0.82, 0.5724, 0.0], &[]))
            .collect();
        let protos = vec![FolderPrototype {
            path: PathBuf::from("/lib/Acme Invoices"),
            centroid: unit(vec![1.0, 0.0, 0.0]), // ~0.6 cosine from the cluster
            name_tokens: std::collections::HashSet::from(["acme".to_string(), "invoice".to_string()]),
        }];
        let moves = semantic_classify(&files, &protos, Path::new("/lib"), &mut std::collections::HashSet::new(), &mut std::collections::HashSet::new());
        assert!(!moves.is_empty());
        assert!(
            moves.iter().all(|m| m.destination.starts_with("/lib/Acme Invoices")),
            "should route to the name-matching folder"
        );
        assert!(
            moves.iter().all(|m| m.confidence == Confidence::Auto),
            "strong filename agreement should upgrade the thin content match to Auto"
        );
        assert!(
            moves.iter().any(|m| m.reason.as_deref().unwrap_or("").contains("filenames fit")),
            "the reason should note the filename agreement"
        );
    }

    #[test]
    fn thin_content_match_without_name_agreement_stays_review() {
        // Control: the same thin content match with filenames carrying NO signal stays Review; name-routing is additive, never fabricated.
        let files: Vec<SemanticFile> = (0..3)
            .map(|i| file(i, &format!("inbox/d{i}.jpg"), vec![0.6, 0.8, 0.0], &[]))
            .collect();
        let protos = vec![FolderPrototype {
            path: PathBuf::from("/lib/Dogs"),
            centroid: unit(vec![1.0, 0.0, 0.0]),
            name_tokens: std::collections::HashSet::from(["dog".to_string(), "puppy".to_string()]),
        }];
        let moves = semantic_classify(&files, &protos, Path::new("/lib"), &mut std::collections::HashSet::new(), &mut std::collections::HashSet::new());
        assert!(!moves.is_empty());
        assert!(
            moves.iter().all(|m| m.confidence == Confidence::Review),
            "no filename signal → thin content match stays Review"
        );
    }


    #[test]
    fn filename_tokens_keep_content_drop_generic() {
        assert_eq!(
            filename_tokens(Path::new("/a/acme_invoice_2023.pdf")),
            vec!["acme".to_string(), "invoice".to_string()]
        );
        assert!(filename_tokens(Path::new("/a/IMG_4821.heic")).is_empty());
        assert!(filename_tokens(Path::new("/a/Screenshot 2024-01-02.png")).is_empty());
    }

    /// Non-image files (empty `clip`) cluster by filename+tag bag-of-words, so a mixed download dir groups invoices and trip clips into two content folders instead of one Documents/<Year> dump; a filename sharing no token (singleton) is left for the rule cascade.
    #[test]
    fn non_image_pass_groups_by_filename_content() {
        let mut files = Vec::new();
        for i in 0..5 {
            files.push(file(i, &format!("/lib/downloads/acme_invoice_{i}.pdf"), vec![], &[]));
        }
        for i in 0..5 {
            files.push(file(100 + i, &format!("/lib/downloads/trip_hawaii_{i}.mp4"), vec![], &[]));
        }
        files.push(file(999, "/lib/downloads/zzqq_widget.txt", vec![], &[]));

        let moves = classify_non_image(&files, Path::new("/lib"), &mut std::collections::HashSet::new(), &mut std::collections::HashSet::new());
        assert_eq!(moves.len(), 10, "the singleton is excluded: {moves:?}");
        let cats: std::collections::HashSet<_> = moves.iter().map(|m| m.category.clone()).collect();
        assert_eq!(cats.len(), 2, "got {cats:?}");
        assert!(!moves.iter().any(|m| m.file_id == 999));
        let dirs: std::collections::HashSet<_> =
            moves.iter().filter_map(|m| m.destination.parent().map(|p| p.to_path_buf())).collect();
        assert_eq!(dirs.len(), 2, "two content groups → two folders: {dirs:?}");
    }
}
