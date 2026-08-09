// CLIP zero-shot scene-tagging vocabulary + scorer.
//
// macOS produces scan-time scene tags from Apple's Vision classifier — a
// *scene* taxonomy ("beach", "kitchen", "document"). Windows has no
// OS-native classifier of comparable quality, so the port previously used a
// MobileNetV3 ImageNet-1k classifier: an *object* classifier whose argmax
// labels ("breakwater", "radio telescope") are the wrong taxonomy for
// "what's in this photo" tags, and which the user reported as "horrible".
//
// This module replaces it with CLIP zero-shot classification. The scan
// pipeline already computes a MobileCLIP-S2 image embedding per file (for
// dedup + semantic search); we score that embedding against a curated
// vocabulary of scene/content labels embedded with the *matched* MobileCLIP-S2
// text encoder (the vision + text towers ship from the same Xenova repo, so
// they share a 512-d space). No new model download — the text encoder is
// already installed for search, and the vocabulary is a `static` in the
// binary (so the CI binary-string telemetry scan sees no new network host).
//
// Cost: the per-file marginal work is a tiny [N×512] mat-vec + softmax, much
// cheaper than the ONNX classifier inference it replaces. The label matrix is
// built once per engine launch (text-encoding every label×template, batched)
// and cached process-static.

#[allow(unused_imports)]
use std::sync::{Arc, OnceLock};

#[allow(unused_imports)]
use anyhow::{Context, Result};

#[allow(unused_imports)]
use super::clip_text::ClipText;
#[allow(unused_imports)]
use super::ClipTokenizer;

// Generated CLIP scene-vocab embeddings (one f32×512 row per SCENE_LABELS
// entry, precomputed offline). Floats are kept at the full PyTorch print
// width so the matrix is byte-faithful with the source notebook — clippy's
// excessive_precision lint would have us truncate them for "readability"
// but precision is the point here.
#[allow(clippy::excessive_precision)]
mod scene_embeddings {
    include!("../../scene_embeddings_precomputed.rs");
}
pub use scene_embeddings::SCENE_EMBEDDINGS;


/// Curated 1-2 word scene/content labels scored by CLIP zero-shot. Chosen to
/// cover the common contents of a personal file library — outdoor/indoor
/// scenes, people, animals, food, documents, and graphic types — and to read
/// well as a one-word chip after `TagChip.FormatTag` title-cases them. This
/// is the primary accuracy lever: edit here, force a re-scan, inspect the
/// persisted `tags.score`, and adjust. Keep entries lowercase and natural so
/// the prompt templates ("a photo of a {}") read like real captions.
pub static SCENE_LABELS: &[&str] = &[
    // ── Outdoor scenes
    "beach", "ocean", "lake", "river", "waterfall", "mountain", "hill",
    "forest", "jungle", "desert", "field", "meadow", "garden", "park",
    "snow", "glacier", "canyon", "cave", "volcano", "island", "harbor",
    "sunset", "sunrise", "night sky", "rainbow", "storm", "fog",
    "city street", "city skyline", "bridge", "road", "highway", "alley",
    "countryside", "farm", "vineyard", "ruins", "cemetery",
    // ── Indoor scenes
    "kitchen", "bedroom", "living room", "bathroom", "dining room",
    "office", "classroom", "library", "restaurant", "cafe", "bar",
    "store", "shopping mall", "gym", "hospital", "church", "temple",
    "museum", "theater", "stage", "concert", "warehouse", "garage",
    "hallway", "staircase", "rooftop", "balcony", "swimming pool",
    // ── People
    "portrait", "selfie", "group photo", "crowd", "baby", "child",
    "wedding", "graduation", "birthday party", "dancing", "sports game",
    "running", "hiking", "skiing", "surfing", "fishing",
    // ── Animals
    "dog", "cat", "bird", "horse", "fish", "wildlife", "insect",
    "butterfly", "farm animal", "zoo animal", "reptile", "pet",
    // ── Food & drink
    "food", "meal", "dessert", "cake", "pizza", "fruit", "vegetables",
    "coffee", "cocktail", "barbecue", "breakfast",
    // ── Plants
    "flower", "tree", "houseplant", "leaves", "mushroom",
    // ── Vehicles & objects
    "car", "motorcycle", "bicycle", "boat", "ship", "airplane", "train",
    "bus", "truck", "furniture", "jewelry", "clothing", "shoes",
    "electronics", "machinery", "tools", "books", "toys", "artwork",
    "statue", "fireworks",
    // ── Documents & text
    "document", "handwriting", "screenshot", "receipt", "invoice",
    "spreadsheet", "presentation slide", "menu", "business card", "form",
    "certificate", "book page", "newspaper", "sign", "poster", "ticket",
    "map", "chart", "diagram", "user interface",
    // ── Graphic / art types
    "painting", "drawing", "sketch", "illustration", "comic", "logo",
    "meme", "infographic", "pattern", "texture",
    // ── Built environment
    "architecture", "building interior", "construction site",
];

/// CLIP zero-shot prompt ensembling: each label is embedded with every
/// template and the embeddings are averaged + renormalized. A few templates
/// recover a few percent of accuracy over a single one at no per-file cost
/// (the averaging happens once at matrix-build time). `{}` is the label.
pub static PROMPT_TEMPLATES: &[&str] = &[
    "a photo of a {}.",
    "a photo of the {}.",
    "a picture of a {}.",
    "an image of a {}.",
    "a {}.",
];

/// Minimum **cosine similarity** (NOT a softmax probability) for a label to be
/// emitted as a tag. Both the image embedding and each label embedding are
/// L2-normalized, so their dot product is the cosine in [-1, 1]. Thresholding
/// the raw cosine is the correct CLIP zero-shot deployment: it emits only
/// genuine matches and emits NOTHING when the image matches no label.
///
/// The prior code softmaxed cosine×100 over the whole vocabulary and
/// thresholded the *probability*, which is razor-peaky — the single top label
/// scored ~0.99 even when its true cosine was mediocre, so every file got a
/// confident WRONG tag. That was the "10% accurate / worthless" report.
///
/// 0.15 is the tuned floor for MobileCLIP-S2; this is the primary accuracy
/// lever. History: 0.24 filtered out almost everything → 0.18 surfaced some
/// chips but many images still came back with no scene tag (year-only).
/// 0.15 biases harder toward recall — CLIP scene tags are now the canonical
/// auto-tagger (no VLM auto-tag pass behind them), so an approximate chip
/// beats a blank chip. Force a re-tag and inspect persisted `tags.score`
/// (the raw cosine) to tune: raise to drop weak/wrong tags, lower for more
/// recall.
pub const SCENE_COSINE_THRESHOLD: f32 = 0.15;

/// Max scene tags emitted per file. macOS Vision surfaces a handful of
/// labels per image; the Library card shows the top 2, the rest are
/// searchable.
pub const SCENE_TOP_K: usize = 4;

/// Master switch for CLIP (MobileCLIP-S2). When true, the scan computes a
/// per-file image embedding (stored in `clip_embeddings`) that powers both
/// the Library's free-text semantic search ("a dog at the beach") AND the
/// zero-shot scene tags below. When false, no embedding is computed and
/// search degrades to FTS5 keyword/tag matching over filenames + OCR.
pub const ENABLE_CLIP: bool = true;

/// Whether the scan emits CLIP zero-shot scene tags (`source='auto'`).
/// ON by default — these chips are the canonical auto-tagger for image and
/// video files. Deep Analyze (Qwen / Gemma, `source='vlm'`) is opt-in and
/// supersedes when present. Requires ENABLE_CLIP.
pub const ENABLE_CLIP_SCENE_TAGS: bool = true;

/// Prompts text-encoded per ONNX batch when building the label matrix.
#[allow(dead_code)]
const BUILD_BATCH: usize = 64;

/// On-disk cache of the prompt-ensembled label matrix. Building it text-encodes
/// every label×template through the CLIP text ONNX session, which on a slow EP
/// (DirectML / CPU) takes 20+ s — long enough to blow the scan's 30 s model-load
/// budget on its FIRST run, and wasteful to repeat every launch. The matrix is
/// deterministic given (SCENE_LABELS, PROMPT_TEMPLATES, the CLIP-text weights),
/// so we serialize it once and reload it (~instant, and skips loading the
/// 253 MB text session entirely) on every subsequent launch.
#[allow(dead_code)]
const SCENE_CACHE_MAGIC: u32 = 0x53_43_4E_31; // "SCN1"
#[allow(dead_code)]
const SCENE_CACHE_VERSION: u32 = 1;
/// Fixed header size: magic(4) + version(4) + key(8) + n_labels(4) + dim(4).
#[allow(dead_code)]
const SCENE_CACHE_HEADER: usize = 24;

/// The label-embedding matrix used for zero-shot scoring. Holds one
/// L2-normalized, prompt-ensembled embedding per `SCENE_LABELS` entry.
pub struct SceneLabeler {
    matrix: Vec<Vec<f32>>,
}

impl SceneLabeler {
    /// Resolve a label index (returned by [`SceneLabeler::score`]) to its
    /// human-readable label string. Method (not assoc fn) for call-site
    /// ergonomics: `labeler.label(idx)` reads better than `SceneLabeler::label`.
    #[allow(clippy::unused_self)]
    pub fn label(&self, idx: usize) -> &'static str {
        SCENE_LABELS[idx]
    }

    /// Read-only view of the prompt-ensembled label matrix (one L2-normalized
    /// row per `SCENE_LABELS` entry). Used by the offline matrix-regeneration
    /// harness (`examples/gen_scene_matrix.rs`); not referenced at runtime.
    #[allow(dead_code)]
    pub fn matrix(&self) -> &[Vec<f32>] {
        &self.matrix
    }

    /// Build the label matrix from an already-loaded CLIP text encoder.
    /// Embeds every label×template prompt (batched), averages each label's
    /// template embeddings, and L2-renormalizes.
    #[allow(dead_code)]
    pub fn build(model: &mut ClipText) -> Result<Self> {
        let started = std::time::Instant::now();
        let templates = PROMPT_TEMPLATES.len();
        let mut prompts: Vec<String> = Vec::with_capacity(SCENE_LABELS.len() * templates);
        for label in SCENE_LABELS {
            for tmpl in PROMPT_TEMPLATES {
                prompts.push(tmpl.replace("{}", label));
            }
        }

        let mut embeddings: Vec<Vec<f32>> = Vec::with_capacity(prompts.len());
        // Fast path: one ONNX run per BUILD_BATCH prompts, assuming the text
        // export has a dynamic batch axis. If it instead pins batch=1 (some
        // Xenova exports do), the batched run errors on a shape mismatch. Don't
        // let that bubble up and disable scene tagging entirely (labeler =
        // None → every file silently gets zero scene tags) — fall back to a
        // per-prompt embed(), which uses a (1,77) input and works against a
        // batch-pinned model. Once batched fails we stay sequential for the
        // remaining chunks rather than re-failing (and re-logging) each batch.
        let mut use_sequential = false;
        for chunk in prompts.chunks(BUILD_BATCH) {
            if !use_sequential {
                match model.embed_batch(chunk) {
                    Ok(embs) => {
                        embeddings.extend(embs);
                        continue;
                    }
                    Err(err) => {
                        tracing::warn!(
                            ?err,
                            "[TAGGING] embed_batch failed; falling back to per-prompt embed (batch-pinned text export?)"
                        );
                        use_sequential = true;
                    }
                }
            }
            for prompt in chunk {
                embeddings.push(
                    model
                        .embed(prompt)
                        .context("embed scene-label prompt (sequential fallback)")?,
                );
            }
        }

        let mut matrix = Vec::with_capacity(SCENE_LABELS.len());
        for group in embeddings.chunks(templates) {
            if group.is_empty() {
                continue;
            }
            let dim = group[0].len();
            let mut acc = vec![0f32; dim];
            for emb in group {
                for (a, v) in acc.iter_mut().zip(emb.iter()) {
                    *a += *v;
                }
            }
            l2_normalize(&mut acc);
            matrix.push(acc);
        }

        tracing::info!(
            n_labels = matrix.len(),
            n_templates = templates,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "[TAGGING] scene-label embeddings built"
        );
        Ok(Self { matrix })
    }

    /// Build from the installed CLIP text model on disk — or load the prebuilt
    /// matrix from the on-disk cache when it's current (the fast path that skips
    /// loading the 253 MB text session AND the 20+ s text-encode entirely). On a
    /// cache miss it loads the encoder + tokenizer, builds the matrix, drops the
    /// encoder, and writes the cache for next launch.
    pub fn build_from_default_model() -> Result<Self> {
        let matrix = SCENE_EMBEDDINGS.iter().map(|row| row.to_vec()).collect();
        Ok(Self { matrix })
    }


    /// Score an image embedding against the vocabulary. Returns up to
    /// `top_k` `(label_index, cosine_similarity)` pairs whose cosine clears
    /// `threshold`, highest first. Empty when nothing matches.
    pub fn score(&self, image_emb: &[f32], threshold: f32, top_k: usize) -> Vec<(usize, f32)> {
        score_labels(image_emb, &self.matrix, threshold, top_k)
    }
}

/// Stable cache key over the inputs that determine the matrix: the label set,
/// the prompt templates, and (as a proxy for "same text model") the CLIP-text
/// weights file length. Editing the vocabulary or swapping the model changes the
/// key, so a stale cache is ignored and rebuilt.
#[allow(dead_code)]
fn scene_cache_key(weights_len: u64) -> u64 {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for label in SCENE_LABELS {
        h.update(label.as_bytes());
        h.update(b"\n");
    }
    h.update(b"\0");
    for tmpl in PROMPT_TEMPLATES {
        h.update(tmpl.as_bytes());
        h.update(b"\n");
    }
    h.update(b"\0");
    h.update(weights_len.to_le_bytes());
    let digest = h.finalize();
    let mut k = [0u8; 8];
    k.copy_from_slice(&digest[..8]);
    u64::from_le_bytes(k)
}

#[allow(dead_code)]
fn scene_cache_path() -> Result<std::path::PathBuf> {
    Ok(crate::paths::models_dir()?
        .join("clip_scene_cache")
        .join("scene_matrix.bin"))
}

/// Load the cached matrix iff the file is present, well-formed, and current
/// (magic + version + key + vocabulary length all match). Any mismatch or read
/// error returns None → the caller rebuilds.
#[allow(dead_code)]
fn try_load_scene_cache(key: u64) -> Option<SceneLabeler> {
    let path = scene_cache_path().ok()?;
    let bytes = std::fs::read(&path).ok()?;
    if bytes.len() < SCENE_CACHE_HEADER {
        return None;
    }
    let magic = u32::from_le_bytes(bytes[0..4].try_into().ok()?);
    let version = u32::from_le_bytes(bytes[4..8].try_into().ok()?);
    let stored_key = u64::from_le_bytes(bytes[8..16].try_into().ok()?);
    let n_labels = u32::from_le_bytes(bytes[16..20].try_into().ok()?) as usize;
    let dim = u32::from_le_bytes(bytes[20..24].try_into().ok()?) as usize;
    if magic != SCENE_CACHE_MAGIC
        || version != SCENE_CACHE_VERSION
        || stored_key != key
        || n_labels != SCENE_LABELS.len()
        || dim == 0
    {
        return None;
    }
    let body = &bytes[SCENE_CACHE_HEADER..];
    if body.len() != n_labels * dim * 4 {
        return None;
    }
    let floats: Vec<f32> = body
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    let matrix: Vec<Vec<f32>> = floats.chunks_exact(dim).map(|r| r.to_vec()).collect();
    Some(SceneLabeler { matrix })
}

/// Best-effort cache write (temp + rename). A failure just means we rebuild next
/// launch — never fatal, so log + swallow.
#[allow(dead_code)]
fn write_scene_cache(key: u64, matrix: &[Vec<f32>]) {
    if let Err(err) = write_scene_cache_inner(key, matrix) {
        tracing::warn!(?err, "[TAGGING] failed to persist scene-label cache (will rebuild next launch)");
    }
}

#[allow(dead_code)]
fn write_scene_cache_inner(key: u64, matrix: &[Vec<f32>]) -> Result<()> {
    let path = scene_cache_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("create scene-cache dir")?;
    }
    let n_labels = matrix.len() as u32;
    let dim = matrix.first().map_or(0, Vec::len) as u32;
    let mut buf = Vec::with_capacity(SCENE_CACHE_HEADER + matrix.len() * dim as usize * 4);
    buf.extend_from_slice(&SCENE_CACHE_MAGIC.to_le_bytes());
    buf.extend_from_slice(&SCENE_CACHE_VERSION.to_le_bytes());
    buf.extend_from_slice(&key.to_le_bytes());
    buf.extend_from_slice(&n_labels.to_le_bytes());
    buf.extend_from_slice(&dim.to_le_bytes());
    for row in matrix {
        for v in row {
            buf.extend_from_slice(&v.to_le_bytes());
        }
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &buf).context("write scene-cache temp")?;
    std::fs::rename(&tmp, &path).context("rename scene-cache into place")?;
    Ok(())
}

/// Process-static labeler, built once per engine launch on first use.
/// Returns `None` if the CLIP text model isn't installed — scene tags then
/// no-op (enriched extras still populate), matching the pre-existing
/// "model not installed → skip stage" contract. Subsequent scans in the same
/// launch reuse the cached matrix.
pub fn shared_scene_labeler() -> Option<Arc<SceneLabeler>> {
    static LABELER: OnceLock<Option<Arc<SceneLabeler>>> = OnceLock::new();
    LABELER
        .get_or_init(|| match SceneLabeler::build_from_default_model() {
            Ok(labeler) => Some(Arc::new(labeler)),
            Err(err) => {
                tracing::warn!(?err, "[TAGGING] scene labeler unavailable; scene tags empty this run");
                None
            }
        })
        .clone()
}

/// Pure zero-shot scorer: cosine similarity (a dot product, since both sides
/// are L2-normalized), thresholded directly, and truncated to the top `top_k`
/// by cosine. NO softmax — emitting the argmax of a peaky softmax tagged every
/// image with a confident wrong label; thresholding the raw cosine emits only
/// genuine matches (and nothing when the image matches no label). Kept a free
/// function with no I/O so it's deterministic and unit-testable with synthetic
/// embeddings.
pub(crate) fn score_labels(
    image_emb: &[f32],
    matrix: &[Vec<f32>],
    threshold: f32,
    top_k: usize,
) -> Vec<(usize, f32)> {
    if matrix.is_empty() || image_emb.is_empty() || top_k == 0 {
        return Vec::new();
    }

    let mut scored: Vec<(usize, f32)> = matrix
        .iter()
        .enumerate()
        .map(|(i, label)| (i, dot(image_emb, label)))
        .filter(|&(_, cos)| cos >= threshold)
        .collect();
    // Highest cosine first; deterministic index tie-break.
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    scored.truncate(top_k);
    scored
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let mut s = 0f32;
    for i in 0..n {
        s += a[i] * b[i];
    }
    s
}

fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-8);
    for x in v.iter_mut() {
        *x /= norm;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn basis(dim: usize, hot: usize) -> Vec<f32> {
        let mut v = vec![0f32; dim];
        v[hot] = 1.0;
        v
    }

    #[test]
    fn exact_match_clears_threshold_with_cosine_one() {
        let dim = 8;
        let matrix: Vec<Vec<f32>> = (0..4).map(|i| basis(dim, i)).collect();
        let img = basis(dim, 2); // identical to matrix[2] → cosine 1.0
        let out = score_labels(&img, &matrix, 0.5, 4);
        assert_eq!(out.len(), 1, "only the matching label clears 0.5: {out:?}");
        assert_eq!(out[0].0, 2, "the matching label must rank first");
        assert!((out[0].1 - 1.0).abs() < 1e-5, "score is the cosine (~1.0): {}", out[0].1);
    }

    #[test]
    fn orthogonal_below_threshold_yields_no_tags() {
        let dim = 8;
        let matrix: Vec<Vec<f32>> = (0..4).map(|i| basis(dim, i)).collect();
        // Orthogonal to every label → all cosines 0 → nothing clears a positive
        // threshold (the key property: a no-match image gets NO tag, not a
        // confident wrong one).
        let img = basis(dim, 7);
        let out = score_labels(&img, &matrix, 0.24, 4);
        assert!(out.is_empty(), "no-match image must emit nothing: {out:?}");
    }

    #[test]
    fn respects_top_k_and_orders_by_cosine() {
        let dim = 8;
        let matrix: Vec<Vec<f32>> = (0..4).map(|i| basis(dim, i)).collect();
        let mut img = vec![0f32; dim];
        img[0] = 0.8; // strongest toward label 0
        img[1] = 0.6; // then label 1
        l2_normalize(&mut img); // already unit, but keep parity with real path
        let out = score_labels(&img, &matrix, 0.1, 2);
        assert_eq!(out.len(), 2, "top_k must cap the result count");
        assert_eq!(out[0].0, 0);
        assert_eq!(out[1].0, 1);
        assert!((out[0].1 - 0.8).abs() < 1e-5, "score 0 is cosine 0.8: {}", out[0].1);
        assert!(out[0].1 >= out[1].1, "results must be sorted by cosine descending");
    }

    #[test]
    fn degenerate_inputs_are_safe() {
        assert!(score_labels(&[], &[vec![1.0]], 0.0, 4).is_empty());
        assert!(score_labels(&[1.0], &[], 0.0, 4).is_empty());
        assert!(score_labels(&[1.0], &[vec![1.0]], 0.0, 0).is_empty());
    }

    #[test]
    fn vocabulary_is_nonempty_and_lowercase() {
        assert!(!SCENE_LABELS.is_empty());
        assert!(!PROMPT_TEMPLATES.is_empty());
        for label in SCENE_LABELS {
            assert_eq!(*label, label.to_ascii_lowercase(), "labels must be lowercase: {label}");
            assert!(!label.is_empty());
        }
        for tmpl in PROMPT_TEMPLATES {
            assert!(tmpl.contains("{}"), "template must contain the label placeholder: {tmpl}");
        }
    }


}
