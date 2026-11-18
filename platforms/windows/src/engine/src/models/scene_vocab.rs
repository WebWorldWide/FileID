// CLIP zero-shot scene tags: the image embedding (already computed for dedup and search) is scored against a label
// matrix precomputed by `examples/gen_scene_matrix.rs`, so scene tagging needs no text model at scan time.

use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};

use super::clip_text::ClipText;

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


/// Keep entries lowercase and natural: they fill the "a photo of a {}" templates and are title-cased by
/// `TagChip.FormatTag`.
pub static SCENE_LABELS: &[&str] = &[
    "beach", "ocean", "lake", "river", "waterfall", "mountain", "hill",
    "forest", "jungle", "desert", "field", "meadow", "garden", "park",
    "snow", "glacier", "canyon", "cave", "volcano", "island", "harbor",
    "sunset", "sunrise", "night sky", "rainbow", "storm", "fog",
    "city street", "city skyline", "bridge", "road", "highway", "alley",
    "countryside", "farm", "vineyard", "ruins", "cemetery",
    "kitchen", "bedroom", "living room", "bathroom", "dining room",
    "office", "classroom", "library", "restaurant", "cafe", "bar",
    "store", "shopping mall", "gym", "hospital", "church", "temple",
    "museum", "theater", "stage", "concert", "warehouse", "garage",
    "hallway", "staircase", "rooftop", "balcony", "swimming pool",
    "portrait", "selfie", "group photo", "crowd", "baby", "child",
    "wedding", "graduation", "birthday party", "dancing", "sports game",
    "running", "hiking", "skiing", "surfing", "fishing",
    "dog", "cat", "bird", "horse", "fish", "wildlife", "insect",
    "butterfly", "farm animal", "zoo animal", "reptile", "pet",
    "food", "meal", "dessert", "cake", "pizza", "fruit", "vegetables",
    "coffee", "cocktail", "barbecue", "breakfast",
    "flower", "tree", "houseplant", "leaves", "mushroom",
    "car", "motorcycle", "bicycle", "boat", "ship", "airplane", "train",
    "bus", "truck", "furniture", "jewelry", "clothing", "shoes",
    "electronics", "machinery", "tools", "books", "toys", "artwork",
    "statue", "fireworks",
    "document", "handwriting", "screenshot", "receipt", "invoice",
    "spreadsheet", "presentation slide", "menu", "business card", "form",
    "certificate", "book page", "newspaper", "sign", "poster", "ticket",
    "map", "chart", "diagram", "user interface",
    "painting", "drawing", "sketch", "illustration", "comic", "logo",
    "meme", "infographic", "pattern", "texture",
    "architecture", "building interior", "construction site",
];

/// Each label is embedded with every template, averaged and renormalized (done once at matrix-build time); `{}` is the
/// label.
pub static PROMPT_TEMPLATES: &[&str] = &[
    "a photo of a {}.",
    "a photo of the {}.",
    "a picture of a {}.",
    "an image of a {}.",
    "a {}.",
];

/// Minimum cosine (not a softmax probability) to emit a label, so an image matching nothing emits nothing; a softmax
/// over cosine×100 made the top label look confident at a mediocre cosine. Tune against the persisted `tags.score`.
pub const SCENE_COSINE_THRESHOLD: f32 = 0.15;

/// The Library card shows the top 2; the rest are searchable.
pub const SCENE_TOP_K: usize = 4;

const BUILD_BATCH: usize = 64;

pub struct SceneLabeler {
    matrix: Vec<Vec<f32>>,
}

impl SceneLabeler {
    #[allow(clippy::unused_self)]
    pub fn label(&self, idx: usize) -> &'static str {
        SCENE_LABELS[idx]
    }

    /// Used only by the offline `examples/gen_scene_matrix.rs`.
    pub fn matrix(&self) -> &[Vec<f32>] {
        &self.matrix
    }

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
        // One batched ONNX run per BUILD_BATCH prompts; exports that pin batch=1 fall back to per-prompt embed()
        // instead of disabling scene tagging.
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

    pub fn precomputed() -> Self {
        Self { matrix: SCENE_EMBEDDINGS.iter().map(|row| row.to_vec()).collect() }
    }

    pub fn score(&self, image_emb: &[f32], threshold: f32, top_k: usize) -> Vec<(usize, f32)> {
        score_labels(image_emb, &self.matrix, threshold, top_k)
    }
}

pub fn shared_scene_labeler() -> Arc<SceneLabeler> {
    static LABELER: OnceLock<Arc<SceneLabeler>> = OnceLock::new();
    LABELER.get_or_init(|| Arc::new(SceneLabeler::precomputed())).clone()
}

/// Free function with no I/O so it is unit-testable. No softmax: the argmax of a peaky softmax tagged every image with
/// a confident wrong label; thresholding the raw cosine emits only genuine matches.
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
        let img = basis(dim, 2);
        let out = score_labels(&img, &matrix, 0.5, 4);
        assert_eq!(out.len(), 1, "only the matching label clears 0.5: {out:?}");
        assert_eq!(out[0].0, 2, "the matching label must rank first");
        assert!((out[0].1 - 1.0).abs() < 1e-5, "score is the cosine (~1.0): {}", out[0].1);
    }

    #[test]
    fn orthogonal_below_threshold_yields_no_tags() {
        let dim = 8;
        let matrix: Vec<Vec<f32>> = (0..4).map(|i| basis(dim, i)).collect();
        // Orthogonal to every label: a no-match image must get NO tag.
        let img = basis(dim, 7);
        let out = score_labels(&img, &matrix, 0.24, 4);
        assert!(out.is_empty(), "no-match image must emit nothing: {out:?}");
    }

    #[test]
    fn respects_top_k_and_orders_by_cosine() {
        let dim = 8;
        let matrix: Vec<Vec<f32>> = (0..4).map(|i| basis(dim, i)).collect();
        let mut img = vec![0f32; dim];
        img[0] = 0.8;
        img[1] = 0.6;
        l2_normalize(&mut img);
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
