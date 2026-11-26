// RAM++ (Apache-2.0) image tagger, the universal tagger that runs on every machine; the VLM is reserved for opt-in Deep
// Analyze. The ONNX comes from shared/scripts/export_ram_plus_onnx.py; its contract (input "image" [1,3,384,384] f32,
// output "logits" [1,4585]) and the ImageNet constants below must stay in sync with it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ndarray::Array4;

use super::inference::{load_session, Dim, InferenceSession, InputSpec};

const IMAGENET_MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const IMAGENET_STD: [f32; 3] = [0.229, 0.224, 0.225];
pub const INPUT_SIZE: u32 = 384;
const INV_255: f32 = 1.0 / 255.0;

/// Used when no per-class sidecar (`ram_plus_thresholds.txt`) is present; overridable via FILEID_RAMPLUS_THRESHOLD.
const DEFAULT_THRESHOLD: f32 = 0.68;
/// Below the pipeline's 16-tag total cap, biased to precision so Year/camera/OCR extras keep slots.
const DEFAULT_MAX_TAGS: usize = 8;

/// Floor under the per-class thresholds (F1-balanced thresholds calibrate some common classes low and surface weak
/// tags); overridable via FILEID_RAMPLUS_PRECISION_FLOOR.
const DEFAULT_PRECISION_FLOOR: f32 = 0.62;

/// Vocab tags that read as noise on a Library card: medium words, `face` (surfaced by the People tab) and
/// posture/clothing fillers. Extend via the `ram_plus_suppress.txt` sidecar.
const SUPPRESSED_TAGS: &[&str] = &[
    "image", "photo", "photograph", "photography", "picture", "face", "catch",
    "stand", "sit", "lay", "pose", "wear",
    "person", "people", "hair", "head", "hand", "play", "game", "make", "fill", "dark",
    "blue", "red", "white", "black", "green", "brown", "yellow", "pink", "gray", "grey", "purple",
];

/// At most one survives per image, and none when face detection ran and found no face (`tagging.rs`).
pub const PEOPLE_WORDS: &[&str] = &["man", "woman", "boy", "girl", "child", "couple"];

const REDUNDANT_WITH: &[(&str, &str)] = &[("goggles", "sunglasses")];

fn is_suppressed_builtin(tag: &str) -> bool {
    SUPPRESSED_TAGS.iter().any(|s| s.eq_ignore_ascii_case(tag))
}

fn is_suppressed(tag: &str, extra: &HashSet<String>) -> bool {
    is_suppressed_builtin(tag) || (!extra.is_empty() && extra.contains(&tag.to_ascii_lowercase()))
}

/// Optional `ram_plus_suppress.txt` next to the tag list: one tag per line, lowercased, `#` comments.
fn load_suppress_sidecar(tag_list: &Path) -> HashSet<String> {
    let path = tag_list.with_file_name("ram_plus_suppress.txt");
    match std::fs::read_to_string(&path) {
        Ok(s) => {
            let set: HashSet<String> = s
                .lines()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(|l| l.to_ascii_lowercase())
                .collect();
            if !set.is_empty() {
                tracing::info!(model = "RAM++", count = set.len(), "suppress sidecar loaded");
            }
            set
        }
        Err(_) => HashSet::new(),
    }
}

pub struct RamPlusTagger {
    session: InferenceSession,
    input_name: String,
    tags: Vec<String>,
    threshold: f32,
    per_class_threshold: Option<Vec<f32>>,
    precision_floor: f32,
    /// Decided once at load: comparing all 4,585 names per image under the session lock was measurable.
    suppressed: Vec<bool>,
    supports_dynamic_batch: bool,
    max_tags: usize,
}

impl RamPlusTagger {
    pub fn load<P: AsRef<Path>, Q: AsRef<Path>>(onnx: P, tag_list: Q) -> Result<Self> {
        let onnx = onnx.as_ref();
        let tag_list = tag_list.as_ref();
        if !onnx.exists() {
            anyhow::bail!("RAM++ ONNX missing at {}", onnx.display());
        }
        if !tag_list.exists() {
            anyhow::bail!("RAM++ tag list missing at {}", tag_list.display());
        }
        let tags: Vec<String> = std::fs::read_to_string(tag_list)
            .with_context(|| format!("read RAM++ tag list {}", tag_list.display()))?
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        if tags.is_empty() {
            anyhow::bail!("RAM++ tag list {} is empty", tag_list.display());
        }

        tracing::info!(model = "RAM++", tags = tags.len(), "tag list loaded");
        let n = INPUT_SIZE as usize;
        let (session, input_name) =
            load_session("RAM++", onnx, &[InputSpec::f32(&[Dim::Batch, Dim::Fixed(3), Dim::Fixed(n), Dim::Fixed(n)])])?;
        let supports_dynamic_batch = ram_plus_shape_has_dynamic_batch(&session.first_input_shape());

        // A set FILEID_RAMPLUS_THRESHOLD forces one global cutoff (handy for sweeps); otherwise use the length-matched
        // `ram_plus_thresholds.txt` sidecar if present.
        let env_threshold = std::env::var("FILEID_RAMPLUS_THRESHOLD")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .filter(|t| (0.0..=1.0).contains(t));
        let per_class_threshold = if env_threshold.is_some() {
            None
        } else {
            let thr_path = tag_list.with_file_name("ram_plus_thresholds.txt");
            match std::fs::read_to_string(&thr_path) {
                Ok(s) => {
                    let v: Vec<f32> = s
                        .lines()
                        .filter_map(|l| l.trim().parse::<f32>().ok())
                        .collect();
                    if v.len() == tags.len() {
                        tracing::info!(model = "RAM++", count = v.len(), "per-class thresholds loaded");
                        Some(v)
                    } else {
                        tracing::warn!(
                            model = "RAM++",
                            got = v.len(),
                            want = tags.len(),
                            "threshold sidecar count mismatch; using global cutoff"
                        );
                        None
                    }
                }
                Err(_) => None,
            }
        };
        let threshold = env_threshold.unwrap_or(DEFAULT_THRESHOLD);

        let precision_floor = std::env::var("FILEID_RAMPLUS_PRECISION_FLOOR")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .filter(|t| (0.0..=1.0).contains(t))
            .unwrap_or(DEFAULT_PRECISION_FLOOR);

// `select_tags` clamps the global cut UP to the precision floor, so a threshold sweep below the floor is a silent
// no-op; warn once.
        if let Some(t) = env_threshold
            && t < precision_floor {
                tracing::warn!(
                    model = "RAM++",
                    env_threshold = t,
                    precision_floor,
                    "FILEID_RAMPLUS_THRESHOLD is below the precision floor; the effective \
                     cut is clamped up to the floor — lower FILEID_RAMPLUS_PRECISION_FLOOR \
                     in lockstep to sweep below it."
                );
            }

        let suppress_extra = load_suppress_sidecar(tag_list);
        let max_tags = std::env::var("FILEID_RAMPLUS_MAX_TAGS")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|n| (1..=512).contains(n))
            .unwrap_or(DEFAULT_MAX_TAGS);

        let suppressed = suppression_mask(&tags, &suppress_extra);
        let mut model = Self {
            session,
            input_name,
            tags,
            threshold,
            per_class_threshold,
            precision_floor,
            suppressed,
            supports_dynamic_batch,
            max_tags,
        };

        let warmup_started = std::time::Instant::now();
        let zero = vec![0u8; (INPUT_SIZE * INPUT_SIZE * 3) as usize];
        let _ = model.tag(&zero, INPUT_SIZE, INPUT_SIZE)?;
        tracing::info!(
            model = "RAM++",
            warmup_ms = warmup_started.elapsed().as_millis() as u64,
            "warmup complete"
        );
        Ok(model)
    }

    pub fn supports_dynamic_batch(&self) -> bool {
        self.supports_dynamic_batch
    }

    /// Pairs are `(tag, sigmoid confidence)`, best first, capped at `max_tags`; the confidence is what the pipeline
    /// persists in `tags.score`.
    pub fn tag(&mut self, rgb: &[u8], width: u32, height: u32) -> Result<Vec<(String, f32)>> {
        let chw = Self::preprocess_tensor(rgb, width, height)?;
        self.tag_prepared(chw)
    }

    /// Resize + ImageNet-normalize, split from [`tag`] so a caller can run it outside the per-session Mutex while
    /// another worker's forward pass runs.
    pub fn preprocess_tensor(rgb: &[u8], width: u32, height: u32) -> Result<Array4<f32>> {
        let expected = (width as usize) * (height as usize) * 3;
        if rgb.len() != expected {
            anyhow::bail!(
                "RAM++ tag expects {} RGB8 bytes for {}x{}, got {}",
                expected,
                width,
                height,
                rgb.len()
            );
        }
        Self::preprocess(rgb, width, height)
    }

    /// The only part that needs `&mut self.session`, so callers serialize it under the pool Mutex.
    pub fn tag_prepared(&mut self, chw: Array4<f32>) -> Result<Vec<(String, f32)>> {
        let logits = self.logits(chw)?;
        Ok(self.select_tags(&logits))
    }

    fn logits(&mut self, chw: Array4<f32>) -> Result<Vec<f32>> {
        let input_name = self.input_name.clone();
        let logits = self
            .session
            .run(vec![(input_name.as_str(), chw.into())])
            .context("RAM++ session.run")?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("RAM++ produced no outputs"))?
            .data;
        if logits.len() != self.tags.len() {
            anyhow::bail!(
                "RAM++ output dim {} != tag list len {} — the ONNX and tag list are out of sync",
                logits.len(),
                self.tags.len()
            );
        }
        Ok(logits)
    }

/// One (N,3,384,384) forward; needs an ONNX exported with a dynamic batch axis (`export_ram_plus_onnx.py
/// --dynamic-batch`), a fixed batch=1 model errors at run.
    pub fn tag_batch(&mut self, imgs: &[(&[u8], u32, u32)]) -> Result<Vec<Vec<(String, f32)>>> {
        use ndarray::s;
        if imgs.is_empty() {
            return Ok(Vec::new());
        }
        let n = INPUT_SIZE as usize;
        let bs = imgs.len();
        let mut batch = Array4::<f32>::zeros((bs, 3, n, n));
        for (i, (rgb, w, h)) in imgs.iter().enumerate() {
            let expected = (*w as usize) * (*h as usize) * 3;
            if rgb.len() != expected {
                anyhow::bail!(
                    "RAM++ tag_batch image {i}: expected {expected} RGB8 bytes for {w}x{h}, got {}",
                    rgb.len()
                );
            }
            let chw = Self::preprocess(rgb, *w, *h)?;
            batch.slice_mut(s![i..i + 1, .., .., ..]).assign(&chw);
        }
        let input_name = self.input_name.clone();
        let num_tags = self.tags.len();
        let logits_vec = self
            .session
            .run(vec![(input_name.as_str(), batch.into())])
            .context("RAM++ batch session.run")?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("RAM++ produced no outputs"))?
            .data;
        if logits_vec.len() != bs * num_tags {
            anyhow::bail!("RAM++ batch output len {} != {bs} images x {num_tags} tags", logits_vec.len());
        }
        Ok(logits_vec
            .chunks_exact(num_tags)
            .map(|row| self.select_tags(row))
            .collect())
    }

    fn select_tags(&self, logits: &[f32]) -> Vec<(String, f32)> {
        select_tags_from(
            logits,
            &self.tags,
            &self.suppressed,
            self.per_class_threshold.as_deref(),
            self.threshold,
            self.precision_floor,
            self.max_tags,
        )
    }

    /// Bilinear, not nearest: tag quality is sensitive to resampling; matches the export script's PIL BILINEAR.
    fn preprocess(rgb: &[u8], width: u32, height: u32) -> Result<Array4<f32>> {
        let resized = Self::resize_to_input(rgb, width, height)?;
        let n = INPUT_SIZE as usize;
        let mut chw = Array4::<f32>::zeros((1, 3, n, n));
        fill_imagenet_chw(&mut chw, 0, &resized, n);
        Ok(chw)
    }

    /// `resize` is a plain copy at equal dimensions, so feeding the result back through `preprocess` yields the same
    /// tensor.
    pub fn resize_to_input(rgb: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
        let src = image::ImageBuffer::<image::Rgb<u8>, &[u8]>::from_raw(width, height, rgb)
            .ok_or_else(|| anyhow::anyhow!("RAM++ preprocess: bad RGB buffer"))?;
        Ok(image::imageops::resize(&src, INPUT_SIZE, INPUT_SIZE, image::imageops::FilterType::Triangle)
            .into_raw())
    }
}

fn suppression_mask(tags: &[String], extra: &HashSet<String>) -> Vec<bool> {
    tags.iter().map(|tag| is_suppressed(tag, extra)).collect()
}

fn select_tags_from(
    logits: &[f32],
    tags: &[String],
    suppressed: &[bool],
    per_class_threshold: Option<&[f32]>,
    threshold: f32,
    precision_floor: f32,
    max_tags: usize,
) -> Vec<(String, f32)> {
    let mut hits: Vec<(usize, f32)> = logits
        .iter()
        .enumerate()
        .filter_map(|(i, &z)| {
            if suppressed[i] {
                return None;
            }
            let p = sigmoid(z);
            let cut = per_class_threshold.map(|t| t[i]).unwrap_or(threshold).max(precision_floor);
            (p >= cut).then_some((i, p))
        })
        .collect();
    hits.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut named: Vec<(String, f32)> = hits.into_iter().map(|(i, p)| (tags[i].clone(), p)).collect();
    refine_tags(&mut named);
    named.truncate(max_tags);
    named
}

/// Collapse redundancy before the cap: a one-word tag that ends another present tag
/// ("bat" vs "baseball bat") goes, `REDUNDANT_WITH` pairs collapse, only the strongest people word survives. `hits` is
/// sorted by score.
pub fn refine_tags(hits: &mut Vec<(String, f32)>) {
    let names: HashSet<String> = hits.iter().map(|(t, _)| t.to_ascii_lowercase()).collect();
    let heads: HashSet<&str> = names
        .iter()
        .filter_map(|name| name.rsplit_once(' ').map(|(_, head)| head))
        .collect();
    let mut kept_people_word = false;
    hits.retain(|(tag, _)| {
        let name = tag.to_ascii_lowercase();
        if !name.contains(' ') && heads.contains(name.as_str()) {
            return false;
        }
        if REDUNDANT_WITH.iter().any(|(broad, partner)| *broad == name && names.contains(*partner)) {
            return false;
        }
        if PEOPLE_WORDS.contains(&name.as_str()) {
            if kept_people_word {
                return false;
            }
            kept_people_word = true;
        }
        true
    });
}

fn fill_imagenet_chw(chw: &mut Array4<f32>, image_index: usize, rgb: &[u8], n: usize) {
    let plane = n * n;
    let base = image_index * 3 * plane;
    let out = chw
        .as_slice_mut()
        .expect("fresh Array4 input tensor is contiguous");
    let mut src = 0usize;
    for p in 0..plane {
        let r = rgb[src] as f32 * INV_255;
        let g = rgb[src + 1] as f32 * INV_255;
        let b = rgb[src + 2] as f32 * INV_255;
        out[base + p] = (r - IMAGENET_MEAN[0]) / IMAGENET_STD[0];
        out[base + plane + p] = (g - IMAGENET_MEAN[1]) / IMAGENET_STD[1];
        out[base + (2 * plane) + p] = (b - IMAGENET_MEAN[2]) / IMAGENET_STD[2];
        src += 3;
    }
}

fn ram_plus_shape_has_dynamic_batch(shape: &[i64]) -> bool {
    shape.first().is_some_and(|dim| *dim < 0)
}

fn sigmoid(z: f32) -> f32 {
    1.0 / (1.0 + (-z).exp())
}

pub fn default_onnx_path() -> Result<PathBuf> {
    Ok(crate::paths::models_dir()?.join("ram_plus").join("ram_plus.onnx"))
}

pub fn default_tags_path() -> Result<PathBuf> {
    Ok(crate::paths::models_dir()?
        .join("ram_plus")
        .join("ram_plus_tags.txt"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_suppression_mask_matches_the_per_tag_check_and_drives_selection() {
        let tags: Vec<String> = ["dog", "Photo", "beach", "sidecar tag", "blue", "ball"].map(String::from).to_vec();
        let extra: HashSet<String> = ["sidecar tag".to_string()].into();
        let mask = suppression_mask(&tags, &extra);
        assert_eq!(mask, vec![false, true, false, true, true, false]);
        for (tag, masked) in tags.iter().zip(&mask) {
            assert_eq!(*masked, is_suppressed(tag, &extra));
        }
        let logits = [3.0, 9.0, 5.0, 8.0, 7.0, 4.0];
        let picked = select_tags_from(&logits, &tags, &mask, None, 0.5, 0.0, 16);
        assert_eq!(picked.iter().map(|(tag, _)| tag.as_str()).collect::<Vec<_>>(), ["beach", "ball", "dog"]);
        let capped = select_tags_from(&logits, &tags, &mask, Some(&[0.99, 0.0, 0.5, 0.0, 0.0, 0.5]), 0.5, 0.0, 1);
        assert_eq!(capped.iter().map(|(tag, _)| tag.as_str()).collect::<Vec<_>>(), ["beach"]);
    }

    fn norm(v: u8, channel: usize) -> f32 {
        ((v as f32 * INV_255) - IMAGENET_MEAN[channel]) / IMAGENET_STD[channel]
    }

    fn assert_close(actual: f32, expected: f32) {
        assert!((actual - expected).abs() < 1e-6, "{actual} != {expected}");
    }

    #[test]
    fn worker_resized_input_yields_the_same_tensor() {
        let (w, h) = (1000u32, 750u32);
        let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i * 7 % 251) as u8).collect();
        let small = RamPlusTagger::resize_to_input(&rgb, w, h).unwrap();
        assert_eq!(
            RamPlusTagger::preprocess(&small, INPUT_SIZE, INPUT_SIZE).unwrap(),
            RamPlusTagger::preprocess(&rgb, w, h).unwrap()
        );
    }

    #[test]
    fn fill_imagenet_chw_writes_contiguous_channel_planes() {
        let rgb = vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120];
        let mut chw = Array4::<f32>::zeros((1, 3, 2, 2));

        fill_imagenet_chw(&mut chw, 0, &rgb, 2);

        let out = chw.as_slice().expect("test tensor is contiguous");
        assert_eq!(out.len(), 12);
        assert_close(out[0], norm(10, 0));
        assert_close(out[1], norm(40, 0));
        assert_close(out[2], norm(70, 0));
        assert_close(out[3], norm(100, 0));
        assert_close(out[4], norm(20, 1));
        assert_close(out[5], norm(50, 1));
        assert_close(out[6], norm(80, 1));
        assert_close(out[7], norm(110, 1));
        assert_close(out[8], norm(30, 2));
        assert_close(out[9], norm(60, 2));
        assert_close(out[10], norm(90, 2));
        assert_close(out[11], norm(120, 2));
    }

    #[test]
    fn batch_guard_accepts_only_dynamic_batch_axis() {
        assert!(ram_plus_shape_has_dynamic_batch(&[-1, 3, 384, 384]));
        assert!(!ram_plus_shape_has_dynamic_batch(&[1, 3, 384, 384]));
        assert!(!ram_plus_shape_has_dynamic_batch(&[4, 3, 384, 384]));
        assert!(!ram_plus_shape_has_dynamic_batch(&[3, 384, 384]));
        assert!(!ram_plus_shape_has_dynamic_batch(&[]));
    }

    #[test]
    fn refine_drops_head_words_redundant_pairs_and_extra_people_words() {
        let mut hits = vec![
            ("baseball bat".to_owned(), 0.9),
            ("boy".to_owned(), 0.85),
            ("bat".to_owned(), 0.8),
            ("sunglasses".to_owned(), 0.75),
            ("man".to_owned(), 0.7),
            ("goggles".to_owned(), 0.65),
            ("Baseball".to_owned(), 0.64),
        ];
        refine_tags(&mut hits);
        let names: Vec<&str> = hits.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(names, vec!["baseball bat", "boy", "sunglasses", "Baseball"]);
    }

    #[test]
    fn sigmoid_monotone() {
        assert!(sigmoid(-10.0) < 0.01);
        assert!((sigmoid(0.0) - 0.5).abs() < 1e-6);
        assert!(sigmoid(10.0) > 0.99);
    }

    #[test]
    fn generic_medium_tags_are_suppressed() {
        assert!(is_suppressed_builtin("photo"));
        assert!(is_suppressed_builtin("image"));
        assert!(is_suppressed_builtin("face"));
        assert!(is_suppressed_builtin("catch"));
        assert!(is_suppressed_builtin("Photo"));
        assert!(is_suppressed_builtin("CATCH"));
        assert!(is_suppressed_builtin("stand"));
        assert!(is_suppressed_builtin("sit"));
        assert!(is_suppressed_builtin("lay"));
        assert!(is_suppressed_builtin("pose"));
        assert!(is_suppressed_builtin("wear"));
        assert!(is_suppressed_builtin("person"));
        assert!(is_suppressed_builtin("play"));
        assert!(is_suppressed_builtin("Blue"));
        assert!(!is_suppressed_builtin("graduation"));
        assert!(!is_suppressed_builtin("mountain"));
        assert!(!is_suppressed_builtin("smile"));
        assert!(!is_suppressed_builtin("boy"));
    }

    #[test]
    fn suppress_sidecar_parses_lowercases_and_skips_comments() {
        let dir = std::env::temp_dir().join(format!("fileid_rp_suppress_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tag_list = dir.join("ram_plus_tags.txt");
        std::fs::write(&tag_list, "dog\ncat\n").unwrap();
        std::fs::write(
            dir.join("ram_plus_suppress.txt"),
            "# a comment\nBokeh\n  blur  \n\nCATCH\n",
        )
        .unwrap();
        let set = load_suppress_sidecar(&tag_list);
        assert!(set.contains("bokeh"));
        assert!(set.contains("blur"));
        assert!(set.contains("catch"));
        assert_eq!(set.len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod golden_tests {
    use super::*;
    use crate::models::golden::{check, model_file, synthetic_rgb, Probe, Tolerance};

    const PATH: &str = "ram_plus/ram_plus.onnx";

    #[test]
    fn logits_match_the_golden_vectors() {
        let (Some(onnx), Some(tags)) = (model_file(PATH), model_file("ram_plus/ram_plus_tags.txt")) else { return };
        let mut model = RamPlusTagger::load(&onnx, &tags).unwrap();
        let chw = RamPlusTagger::preprocess_tensor(&synthetic_rgb(384, 384), 384, 384).unwrap();
        let logits = model.logits(chw).unwrap();
        check(
            "ram_plus",
            PATH,
            "synthetic_rgb(384, 384) through the engine's bilinear-384 + ImageNet preprocessing; the raw per-tag logits",
            Tolerance { max_abs: 0.02, rel_norm: 2e-3 },
            &[Probe { name: "logits", shape: vec![logits.len()], values: &logits }],
        );
    }
}
