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

/// A fusion-weight + confidence-threshold profile. The image pass keeps the
/// calibrated CLIP values; the non-image pass (filename+tag bag-of-words
/// representative — a PDF/video has no CLIP image vector) runs a separate,
/// deliberately tighter profile so a sparser signature can't over-move. Stays
/// byte-faithful with the Swift engine. (RESTRUCTURE.md §2/R1)
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
}

/// Image pass — representative is the L2-normalized 512-d CLIP image embedding.
/// Thresholds calibrated 2026-06-17 against a real ~3.3k-image personal-photo library
/// (the "Adlon" corpus), kept byte-faithful with the Swift engine's `imageProfile`.
/// Finding: CLIP cosines for personal photos compress into a HIGH band (intra-folder
/// cohesion median ≈ 0.80, inter-folder centroid p90 ≈ 0.84), so the original
/// folder_match_cos 0.55 / auto_folder_cos 0.72 sat BELOW the whole distribution and
/// auto-routed every photo into the nearest catch-all folder. Env-overridable for owner
/// tuning, mirroring the non-image knobs. (RESTRUCTURE.md R3 calibration)
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
    }
}

/// Cap the tag vocabulary to the most common tags. Frequent tags carry the
/// grouping signal; rare ones are noise and would bloat the fused vector.
const TAG_VOCAB_CAP: usize = 256;

/// Filenames tokenize into many one-off terms, so the non-image bag-of-words
/// needs a wider vocab than the image tag block.
const NON_IMAGE_VOCAB_CAP: usize = 512;

/// Owner kill-switch for the non-image semantic pass
/// (`FILEID_RESTRUCTURE_NONIMAGE=0` → off, falls back to the rule cascade).
pub fn non_image_enabled() -> bool {
    std::env::var("FILEID_RESTRUCTURE_NONIMAGE")
        .map(|v| v != "0")
        .unwrap_or(true)
}

/// Non-image pass profile — representative is a filename+tag bag-of-words
/// (sparser than CLIP), so demand a cleaner cluster + a tighter folder match.
/// `w_tags` is 0: tags already live inside the representative and naming reads
/// them directly, so re-adding the block would double-count. Thresholds are
/// env-overridable (`FILEID_RESTRUCTURE_NI_*`) for owner calibration on a real
/// library before the defaults are promoted. (RESTRUCTURE.md R1)
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
    }
}

fn env_f32(key: &str, dflt: f32) -> f32 {
    std::env::var(key).ok().and_then(|s| s.parse().ok()).unwrap_or(dflt)
}

/// Folder basenames that must never act as a learn-your-style prototype —
/// generic dumping grounds the butler should organize files *out of*.
const JUNK_FOLDER_NAMES: &[&str] = &[
    "downloads", "downloaded", "desktop", "new folder", "untitled", "temp", "tmp",
    "misc", "other", "stuff", "things", "files", "unsorted", "inbox",
];

/// Filename tokens that carry no grouping signal: camera/scan boilerplate, common
/// English connectors (so "boys at the zoo" doesn't name a "Boys The" folder), and
/// file-extension tokens that leak in on double-extension names like `E14.jpg.lps`
/// → `E14.jpg` → spurious "jpg". Lowercase.
const FILENAME_STOPWORDS: &[&str] = &[
    // camera / scan / boilerplate
    "img", "image", "dsc", "dscn", "dscf", "photo", "pic", "picture", "screenshot",
    "screen", "shot", "untitled", "new", "copy", "final", "draft", "version", "scan",
    "document", "file", "video", "vid", "clip",
    // English connectors (no grouping signal)
    "the", "and", "for", "with", "from", "was", "are", "this", "that", "your", "our",
    "his", "her", "its", "out", "all", "has",
    // extension tokens that leak in on double-extension names
    "jpg", "jpeg", "png", "gif", "bmp", "heic", "heif", "tiff", "webp", "pdf", "doc",
    "docx", "xls", "xlsx", "ppt", "pptx", "txt", "rtf", "mov", "mp4", "avi", "mkv",
    "mp3", "wav", "zip", "rar", "lps",
];

/// Density-clustering hyperparameters for *files* (looser than faces: a
/// semantic group is broader than one identity).
///
/// SOTA single-knob (HDBSCAN `min_cluster_size` philosophy, deep-research 2026-06-16):
/// one `FILEID_RESTRUCTURE_GRANULARITY` ∈ {loose, normal, tight} shifts the cluster
/// cosines so the owner tunes folder count with ONE lever instead of many opaque
/// thresholds. Loose = lower cosine bar = broader / fewer folders; tight = higher
/// bar = narrower / more folders. Applied identically on both engines so the
/// chosen granularity round-trips cross-platform.
pub fn granularity_delta() -> f32 {
    match std::env::var("FILEID_RESTRUCTURE_GRANULARITY").as_deref() {
        Ok("loose") => -0.05,
        Ok("tight") => 0.05,
        _ => 0.0, // "normal" / unset
    }
}

fn file_hyperparams() -> Hyperparameters {
    // Cluster-merge cosines calibrated 2026-06-17 on the real ~3.3k-image Adlon corpus,
    // byte-faithful with the Swift engine. The original 0.50/0.40/0.42 were tuned for
    // DIVERSE images; CLIP cosines for a coherent personal library compress high (typical
    // pair ≈ 0.71+, within-event ≈ 0.80), so those low bars merged the ENTIRE photo set
    // into one cluster that routed to a single catch-all folder. The new bars sit at the
    // within-event cohesion so a cluster ≈ one event. Env-overridable; the single-knob
    // GRANULARITY delta still shifts all three together.
    let d = granularity_delta();
    Hyperparameters {
        pass1_cosine: env_f32("FILEID_RESTRUCTURE_CLUSTER_P1", 0.84) + d,
        pass2_cosine: env_f32("FILEID_RESTRUCTURE_CLUSTER_P2", 0.76) + d,
        pass2_margin: 0.08,
        pass3_variance_threshold: 0.06,
        pass3_min_mean_cosine: env_f32("FILEID_RESTRUCTURE_CLUSTER_P3", 0.76) + d,
        pass3_max_splits: 5,
        k_nn: 12,
    }
}

/// An existing folder learned from the current tree: its path + the mean
/// (L2-normalized) CLIP embedding of the files currently in it.
pub struct FolderPrototype {
    pub path: PathBuf,
    pub centroid: Vec<f32>,
    /// Distinctive filename/folder-name tokens of this folder + its current
    /// contents — the Dropbox "Smart Move" signal that names route as well as (or
    /// better than) content. Used ADDITIVELY: strong name agreement can upgrade a
    /// thin-margin content match's confidence, never override the content routing.
    pub name_tokens: std::collections::HashSet<String>,
}

/// Name-routing thresholds, overlap coefficient = |a∩b| / min(|a|,|b|). At/above
/// AUTO, the cluster's filenames agree strongly enough with the target folder to
/// upgrade a thin-margin content match to Auto; at/above REASON the "filenames fit"
/// note is added. Tuned against the labeled name-routing scenarios in the tests.
const NAME_AGREE_AUTO: f32 = 0.30;
const NAME_AGREE_REASON: f32 = 0.20;

/// Overlap coefficient of two token sets: |a∩b| / min(|a|,|b|), 0 when either is
/// empty. Less penalized by union size than Jaccard — a cluster that shares a
/// folder's few distinctive tokens scores high even when each side has extra terms.
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

/// Build prototypes from the files' *current* locations: each parent folder
/// with ≥ `min_files` becomes a class whose centroid is the mean CLIP vector of
/// its contents (Nearest-Class-Mean / Dropbox "Smart Move"). Zero user effort —
/// the existing tree is the labeled ground truth.
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
            // Folder's own name tokens + every sibling filename's tokens.
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
    semantic_classify_profiled(files, prototypes, library_root, image_profile(), file_hyperparams())
}

fn semantic_classify_profiled(
    files: &[SemanticFile],
    prototypes: &[FolderPrototype],
    library_root: &Path,
    profile: Profile,
    hp: Hyperparameters,
) -> Vec<ProposedMove> {
    if files.is_empty() {
        return Vec::new();
    }
    let global_freq = tag_frequencies(files);
    let vocab = vocab_from_freq(&global_freq, TAG_VOCAB_CAP);
    let fused: Vec<Vec<f32>> = files.iter().map(|f| fuse(f, &vocab, profile)).collect();
    let cluster_ids = cluster(&fused, hp);

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
        // Distinctive filename tokens shared by this cluster — the name-routing
        // signal matched against each candidate folder's tokens below.
        let cluster_name_tokens: std::collections::HashSet<String> = members
            .iter()
            .flat_map(|&i| filename_tokens(&files[i].source))
            .collect();

        let (dest_dir, category, confidence, reason) =
            match nearest_two_folders(&centroid, prototypes) {
                // Learn-your-style: route to the nearest confident existing
                // folder. Auto-file only when the match is strong *and*
                // unambiguous (clear margin over the runner-up) on a tight
                // cluster; otherwise surface for one-click review.
                // Containment guard: only an in-root prototype is a valid
                // destination — routing to a folder outside library_root would
                // be silently rejected by the apply layer (canonicalizes
                // outside root), so such a match falls through to a new in-root
                // group instead. (audit E12)
                Some((proto, sim, runner_up))
                    if sim >= profile.folder_match_cos && proto.path.starts_with(library_root) =>
                {
                    let name = folder_display_name(&proto.path);
                    // Name-routing (Dropbox Smart Move): how much this cluster's
                    // filenames overlap the folder's name/sibling tokens. Additive —
                    // strong agreement upgrades a thin-margin content match to Auto,
                    // but never overrides the content routing decision itself.
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
                    let safe_base = crate::util::path_safety::safe_filename_component(&pretty);
                    let mut safe = safe_base.clone();
                    // Disambiguate in the SANITIZED namespace that actually backs
                    // the folder. Two pretty names that differ only in chars
                    // safe_filename_component maps to '_' (e.g. "16:9" and "16/9" →
                    // "16_9") would otherwise collide into one physical directory.
                    // Prefer the next distinctive term first.
                    if used_group_names.contains(&safe) {
                        if let Some(extra) = terms.get(2) {
                            pretty = format!("{} {}", base, title_case(extra));
                            safe = crate::util::path_safety::safe_filename_component(&pretty);
                        }
                    }
                    // Numeric-suffix fallback. Build each candidate so the suffix
                    // ALWAYS survives the 200-scalar filename cap: a base that
                    // already sanitizes to ~200 chars would otherwise truncate every
                    // "{base} {n}" to the SAME string, so the uniqueness check never
                    // clears and the loop spins forever (hanging plan generation).
                    // Reserve room on the (already-sanitized) base and append the
                    // suffix directly — distinct n ⇒ distinct candidate ⇒ guaranteed
                    // termination within |used_group_names|+1 iterations.
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
                    // Path-safe directory name (mirrors the person route):
                    // illegal/separator chars in tag-derived names ("16:9",
                    // "dog/cat") would mis-route or fail the move and break
                    // cross-platform name parity (#2). `safe` was computed during
                    // dedup above so the uniqueness check ran in this same folder
                    // namespace. Keep `pretty` for the human-facing category/reason.
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

// ── Non-image semantic pass (RESTRUCTURE.md R1) ─────────────────────────────

/// Cluster non-image files (documents, video, audio — anything without a CLIP
/// image embedding) by a filename-token + content-tag bag-of-words signature, so
/// a mixed library groups invoices/manuals/clips by *content* instead of dumping
/// them all into `Documents/<Year>`. Additive: the image pass claims its files
/// first, this handles the remainder, and the rule cascade still catches whatever
/// neither clusters. The bag-of-words IS the representative vector (there is no
/// image embedding), so the same density clusterer + learn-your-style folder
/// matching apply unchanged under the tighter non-image profile.
pub fn classify_non_image(files: &[SemanticFile], library_root: &Path) -> Vec<ProposedMove> {
    let sigs = non_image_signatures(files);
    if sigs.len() < 2 {
        return Vec::new();
    }
    // Learn-your-style targets, but NEVER generic dumping grounds (Downloads,
    // Desktop, Temp, …): the whole point is to organize files OUT of those, so
    // they must not become a prototype that routes everything back where it
    // already is. Real user folders ("Taxes", "Invoices") still anchor.
    let protos: Vec<FolderPrototype> = folder_prototypes(&sigs, 4)
        .into_iter()
        .filter(|p| !is_junk_prototype_folder(&p.path))
        .collect();
    semantic_classify_profiled(&sigs, &protos, library_root, non_image_profile(), file_hyperparams())
}

/// Document-content pass — cluster documents by their BGE text embedding (read from
/// `text_embeddings`, stored in each file's `clip`), which reads the *content* rather
/// than the filename. Far stronger than the filename-token bag-of-words fallback (an
/// owner A/B on a real 533-doc corpus: nearest-neighbour-same-folder 49%→57%). Each
/// `file.clip` MUST be the BGE vector; docs without an extractable-text embedding are
/// excluded by the caller and fall through to `classify_non_image`. Uses doc-specific
/// thresholds because BGE cosines sit lower than CLIP-image cosines. (RESTRUCTURE.md R3)
pub fn classify_documents(files: &[SemanticFile], library_root: &Path) -> Vec<ProposedMove> {
    if files.len() < 2 {
        return Vec::new();
    }
    let protos: Vec<FolderPrototype> = folder_prototypes(files, 4)
        .into_iter()
        .filter(|p| !is_junk_prototype_folder(&p.path))
        .collect();
    semantic_classify_profiled(files, &protos, library_root, doc_profile(), doc_hyperparams())
}

/// Document content-embedding profile. The representative IS the 384-d BGE vector (so
/// `w_clip` dominates; `w_tags`/`w_time` are tiny — a document has no meaningful capture
/// time). Thresholds CALIBRATED 2026-06-17 on the owner's real ~1.4k-doc corpus: the
/// engine MEAN-pools BGE, whose cosines compress high (within-folder cohesion ≈ 0.786,
/// inter-folder centroid p90 ≈ 0.80), so the bars sit there — NOT at the lower CLS-pooled
/// A/B values, which collapsed every doc into one folder. Validated: doc folder-agreement
/// 46% (filenames) → 53%. Env-overridable. (RESTRUCTURE.md R3)
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
    }
}

/// Cluster-merge cosines for the MEAN-pooled BGE document space (compresses high, like the
/// image space — within-folder ≈ 0.786). Env-overridable; GRANULARITY still shifts all three.
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

/// A folder that must never act as a learn-your-style prototype — a generic
/// dumping ground the butler should organize files *out of*, not route them back
/// into. Matches the exact junk names AND any folder whose first word is a
/// dumping-ground word, so versioned/suffixed variants ("Desktop 1.0",
/// "Downloads (2)", "Temp files") are caught too.
fn is_junk_prototype_folder(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();
    if JUNK_FOLDER_NAMES.contains(&name.as_str()) {
        return true;
    }
    let first_word = name.split(|c: char| !c.is_alphabetic()).find(|w| !w.is_empty());
    matches!(
        first_word,
        Some("desktop" | "downloads" | "download" | "downloaded" | "temp" | "tmp"
            | "unsorted" | "inbox" | "misc")
    )
}

/// Build bag-of-words representatives for the non-image pass. Each file's `clip`
/// slot becomes an L2-normalized multi-hot over a shared, frequency-capped vocab
/// of (filename tokens ∪ content tags); `tags` keeps the same token set so the
/// distinctive-term namer can still label the group. The input `clip` is ignored
/// (non-image files have none). A file with no in-vocab token is dropped (no
/// grouping signal) and falls through to the rule cascade.
fn non_image_signatures(files: &[SemanticFile]) -> Vec<SemanticFile> {
    // BTreeSet → deterministic sorted token order across runs.
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
        // A file whose every token is unique to it (each token freq 1) shares NO
        // signal with any other file, so it's orthogonal to all of them and can
        // never cluster — leave it for the rule cascade. Excluding it up front
        // (instead of trusting the density clusterer to noise-reject an orthogonal
        // point) keeps the result deterministic across architectures: with
        // k_nn >= n and all-tied zero similarities, the clusterer's kNN tie order
        // is arch-sensitive, which made a degenerate lone file group-or-not by
        // luck. (CI determinism / lockstep with the Swift engine)
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

/// Lowercase alphanumeric filename tokens, extension dropped, split on any
/// non-alphanumeric. Drops pure-numeric, very short, and generic camera/scan
/// tokens — so `IMG_4821.heic` yields nothing while `acme_invoice_2023.pdf`
/// yields `acme` + `invoice`.
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
fn fuse(file: &SemanticFile, vocab: &HashMap<String, usize>, profile: Profile) -> Vec<f32> {
    let mut out = Vec::with_capacity(file.clip.len() + vocab.len() + 2);

    // CLIP block (already unit; re-normalize defensively).
    let clip = l2_normalized(&file.clip);
    out.extend(clip.iter().map(|x| x * profile.w_clip));

    // Tag multi-hot block.
    let mut tags = vec![0f32; vocab.len()];
    for t in &file.tags {
        if let Some(&idx) = vocab.get(t) {
            tags[idx] = 1.0;
        }
    }
    let tags = l2_normalized(&tags);
    out.extend(tags.iter().map(|x| x * profile.w_tags));

    // Time block: cyclical day-of-year (captures seasonality without raw epoch).
    let (s, c) = day_of_year_cyclical(file.time_unix);
    out.push(s * profile.w_time);
    out.push(c * profile.w_time);

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
fn cluster(fused: &[Vec<f32>], params: Hyperparameters) -> Vec<usize> {
    const HNSW_MIN: usize = 5_000;
    let n = fused.len();
    // Can't request more neighbors than other points exist; k_nn >= n made the kNN
    // over an all-tied set arch-sensitive (see non_image_signatures). (lockstep)
    let k = params.k_nn.min(n.saturating_sub(1).max(1));

    let hnsw = (n >= HNSW_MIN).then(|| {
        let points: Vec<(Vec<f32>, usize)> =
            fused.iter().enumerate().map(|(i, e)| (e.clone(), i)).collect();
        crate::util::hnsw_index::build(points)
    });

    let mut knn_search = crate::util::hnsw_index::Searcher::default();
    let result = identity_clustering::cluster(
        fused,
        |i| {
            let mut hits: Vec<Neighbor> = if let Some(idx) = &hnsw {
                // Reuse one Search scratch across the sweep (a fresh one re-zeros
                // an n-byte visited set per query — an O(n²) term over the pass).
                knn_search
                    .top_k(idx, &fused[i], k + 1)
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
            // Bounded top-k: O(n) partition instead of an O(n log n) full sort of
            // all n-1 neighbors when only k are used; sort just the k kept.
            let cmp = |a: &Neighbor, b: &Neighbor| {
                b.similarity.partial_cmp(&a.similarity).unwrap_or(std::cmp::Ordering::Equal)
            };
            if hits.len() > k {
                hits.select_nth_unstable_by(k, cmp);
                hits.truncate(k);
            }
            hits.sort_by(cmp);
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
            // Compute the idf's ln in f64 then narrow — f32 `logf` is libm-dependent
            // (can differ Windows↔macOS by ~1 ULP), which could flip the tie order of
            // two near-equal-score terms and pick a DIFFERENT group folder name across
            // platforms. f64 `ln` is consistently correctly-rounded, matching the macOS
            // engine's `Float(log(Double(...)))`. (audit — lockstep)
            (t, tf * (((total / df) as f64).ln().max(0.0) as f32))
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
        let f = fuse(&file(1, "a.jpg", vec![1.0, 0.0, 0.0], &["beach"]), &vocab, image_profile());
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
    fn sanitization_colliding_group_names_get_distinct_folders() {
        // Two distinct content clusters whose distinctive tags differ ONLY in
        // chars safe_filename_component maps to '_' ("16:9" vs "16/9" → "16_9").
        // The dedup must back them with DISTINCT physical directories (E4), not
        // collapse both into one folder — and the numeric-suffix loop must
        // terminate.
        let mut files = Vec::new();
        for i in 0..6 {
            files.push(file(i, &format!("a/r{i}.jpg"), vec![1.0, 0.0, 0.0, 0.0], &["16:9"]));
        }
        for i in 0..6 {
            files.push(file(100 + i, &format!("a/s{i}.jpg"), vec![0.0, 1.0, 0.0, 0.0], &["16/9"]));
        }
        let moves = semantic_classify(&files, &[], Path::new("/lib"));
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
        // A cluster of "dog" photos; an existing folder whose prototype matches.
        let files: Vec<SemanticFile> = (0..5)
            .map(|i| file(i, &format!("inbox/d{i}.jpg"), vec![1.0, 0.0, 0.0], &["dog"]))
            .collect();
        let protos = vec![FolderPrototype {
            path: PathBuf::from("/lib/Dogs"),
            centroid: unit(vec![1.0, 0.0, 0.0]),
            name_tokens: std::collections::HashSet::default(),
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
            name_tokens: std::collections::HashSet::default(),
        }];
        let moves = semantic_classify(&files, &protos, Path::new("/lib"));
        assert!(!moves.is_empty());
        assert!(moves.iter().all(|m| m.confidence == Confidence::Auto), "exact match should auto-file");
        assert!(moves.iter().all(|m| m.reason.as_deref().unwrap_or("").contains("Dogs")));
    }

    // ── Name-based routing (Dropbox Smart Move; deep-research 2026-06-16) ───
    // Labeled ground truth: filename agreement with a target folder is a strong
    // routing signal that can decide a thin-margin content match. These encode the
    // expert call on what SHOULD happen and pin the additive behavior.

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
        // CONTENT only weakly matches (cosine ~0.82, between the calibrated 0.80 match bar
        // and the 0.86 content-auto bar) but the FILENAMES clearly belong in the folder →
        // auto-file on the name evidence.
        let files: Vec<SemanticFile> = (0..3)
            .map(|i| file(i, &format!("inbox/acme_invoice_part{i}.pdf"), vec![0.82, 0.5724, 0.0], &[]))
            .collect();
        let protos = vec![FolderPrototype {
            path: PathBuf::from("/lib/Acme Invoices"),
            centroid: unit(vec![1.0, 0.0, 0.0]), // ~0.6 cosine from the cluster
            name_tokens: std::collections::HashSet::from(["acme".to_string(), "invoice".to_string()]),
        }];
        let moves = semantic_classify(&files, &protos, Path::new("/lib"));
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
        // The control: same thin content match, filenames carry NO signal → the
        // confidence must stay Review. Name-routing is additive, never fabricated.
        let files: Vec<SemanticFile> = (0..3)
            .map(|i| file(i, &format!("inbox/d{i}.jpg"), vec![0.6, 0.8, 0.0], &[]))
            .collect();
        let protos = vec![FolderPrototype {
            path: PathBuf::from("/lib/Dogs"),
            centroid: unit(vec![1.0, 0.0, 0.0]),
            name_tokens: std::collections::HashSet::from(["dog".to_string(), "puppy".to_string()]),
        }];
        let moves = semantic_classify(&files, &protos, Path::new("/lib"));
        assert!(!moves.is_empty());
        assert!(
            moves.iter().all(|m| m.confidence == Confidence::Review),
            "no filename signal → thin content match stays Review"
        );
    }

    // ── Non-image pass (RESTRUCTURE.md R1) ─────────────────────────────────

    #[test]
    fn filename_tokens_keep_content_drop_generic() {
        assert_eq!(
            filename_tokens(Path::new("/a/acme_invoice_2023.pdf")),
            vec!["acme".to_string(), "invoice".to_string()]
        );
        assert!(filename_tokens(Path::new("/a/IMG_4821.heic")).is_empty());
        assert!(filename_tokens(Path::new("/a/Screenshot 2024-01-02.png")).is_empty());
    }

    /// The R1 fix: non-image files (no CLIP embedding — `clip` is empty) cluster
    /// by their filename+tag bag-of-words, so a mixed download dir groups invoices
    /// and trip clips into two content folders instead of one Documents/<Year>
    /// dump. A filename sharing no token (singleton) is left for the rule cascade.
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

        let moves = classify_non_image(&files, Path::new("/lib"));
        assert_eq!(moves.len(), 10, "the singleton is excluded: {moves:?}");
        let cats: std::collections::HashSet<_> = moves.iter().map(|m| m.category.clone()).collect();
        assert_eq!(cats.len(), 2, "got {cats:?}");
        assert!(!moves.iter().any(|m| m.file_id == 999));
        let dirs: std::collections::HashSet<_> =
            moves.iter().filter_map(|m| m.destination.parent().map(|p| p.to_path_buf())).collect();
        assert_eq!(dirs.len(), 2, "two content groups → two folders: {dirs:?}");
    }
}
