// RAM++ (Recognize Anything Plus) image tagger — the universal FileID tagger.
//
// WHY this and not a VLM: tagging must run on every Windows GPU/iGPU/NPU and be
// license-clean. RAM++ (Apache-2.0, Swin-Large @ 384px, 4585-tag English
// vocabulary with frozen tag embeddings) is a single forward pass — exactly the
// shape ONNX Runtime's GPU/NPU EPs (DirectML / CUDA / OpenVINO / QNN) accelerate
// — so it rides the SAME EP chain as MobileCLIP/faces (see runtime.rs). The VLM
// is reserved for opt-in Deep Analyze.
//
// Inference order: decode → bilinear resize to 384×384 → ImageNet mean/std
// normalize → CHW f32 → ORT session.run → sigmoid → threshold + top-k → map tag
// indices to strings. The ONNX is produced by shared/scripts/export_ram_plus_onnx.py;
// the contract (input "image" [1,3,384,384] f32, output "logits" [1,4585]) and
// the ImageNet constants below MUST stay in sync with that script.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ndarray::Array4;
use ort::session::{Session, SessionInputValue, SessionOutputs};
use ort::value::Tensor;

use super::runtime::{
    classify_inference_error, configure_session_builder, execution_providers_for_chain,
    priority_chain, RuntimeProbe,
};

const IMAGENET_MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const IMAGENET_STD: [f32; 3] = [0.229, 0.224, 0.225];
const INPUT_SIZE: u32 = 384;

/// Default sigmoid-probability cutoff. RAM++ is calibrated with per-class
/// thresholds (~0.68 mean); a single global cutoff is the v1 approximation,
/// overridable via FILEID_RAMPLUS_THRESHOLD. WS4 adds a per-class
/// `ram_plus_thresholds.txt` sidecar (model.class_threshold) that supersedes
/// this when present.
const DEFAULT_THRESHOLD: f32 = 0.68;
/// Cap RAM++'s own emissions well below the scan pipeline's 16-tag total cap.
/// Biased to precision (8, not 12): only the most-confident content tags survive,
/// so Library cards read clean and Year/camera/OCR extras keep slots. Lowered
/// from 12 — users reported tag sets feeling "loose" (too many weak labels).
const DEFAULT_MAX_TAGS: usize = 8;

/// Hard precision floor under the per-class thresholds. RAM++'s exported
/// `class_threshold`s are F1-balanced; a few common classes calibrate quite low,
/// which surfaces weak tags. Clamping the effective cutoff up to this floor
/// trades a little recall for noticeably cleaner, higher-confidence tags.
/// Raised from 0.5 → users reported generic/wrong tags ("catch", dog→bear);
/// overridable per-run via FILEID_RAMPLUS_PRECISION_FLOOR for threshold sweeps.
const DEFAULT_PRECISION_FLOOR: f32 = 0.62;

/// RAM++ vocab tags that read as noise on a Library card and get filtered from
/// the emitted set. Three families:
///   - medium words (`image`/`photo`/…): the file already *is* a photo.
///   - `face`: faces are surfaced by the People tab (the underlying signal still
///     lives in `has_faces`).
///   - `catch` + posture/clothing-state fillers (`stand`/`sit`/`lay`/`pose`/
///     `wear`): content-free labels that fire on nearly every human photo and
///     add no search/organization value. "catch" was also a frequent
///     false-positive (it fired on dogs, bears, and sports shots alike); the
///     posture fillers were the dominant "tags feel too generic" offenders on a
///     real 100-photo family-library sample (stand 47×, pose 20×, …).
///
/// Extend WITHOUT a rebuild via the `ram_plus_suppress.txt` sidecar (one tag per
/// line, next to the tag list), merged case-insensitively.
const SUPPRESSED_TAGS: &[&str] = &[
    "image", "photo", "photograph", "photography", "picture", "face", "catch",
    "stand", "sit", "lay", "pose", "wear",
];

/// Built-in suppress check (case-insensitive). [`is_suppressed`] also folds in
/// the per-instance sidecar set.
fn is_suppressed_builtin(tag: &str) -> bool {
    SUPPRESSED_TAGS.iter().any(|s| s.eq_ignore_ascii_case(tag))
}

/// Suppressed = built-in set OR the sidecar set, both case-insensitive. Free
/// (not a method) so the hot tag() closure can borrow `suppress_extra` as a
/// disjoint field while `self.session` is mutably borrowed by `outputs`.
fn is_suppressed(tag: &str, extra: &HashSet<String>) -> bool {
    // Short-circuit the common empty case (no ram_plus_suppress.txt sidecar): an
    // empty set can never contain anything, so skip the per-tag
    // to_ascii_lowercase() heap String allocation for the ~4573 tags not in the
    // 12-entry built-in set. Saves ~4573 alloc/free pairs per scanned image on
    // the primary tagger path when no sidecar is configured (the default).
    is_suppressed_builtin(tag) || (!extra.is_empty() && extra.contains(&tag.to_ascii_lowercase()))
}

/// Read the optional `ram_plus_suppress.txt` sidecar next to the tag list: one
/// tag per line, lowercased; blank lines and `#` comments ignored. Missing file
/// → empty set (built-in suppression only). No rebuild needed to edit it.
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
    session: Session,
    /// The ONNX's single input tensor name, read once at load. Reused on every
    /// forward instead of re-walking `session.inputs.first()` per inference.
    input_name: String,
    /// Index-aligned with the model's logits; `tags[i]` is the label for output i.
    tags: Vec<String>,
    /// Global fallback cutoff (DEFAULT_THRESHOLD or the FILEID_RAMPLUS_THRESHOLD
    /// env override). Used for any class lacking a per-class threshold.
    threshold: f32,
    /// Optional per-class cutoffs (index-aligned with `tags`), loaded from the
    /// `ram_plus_thresholds.txt` sidecar. `None` → use the global `threshold`
    /// for every class.
    per_class_threshold: Option<Vec<f32>>,
    /// Effective floor under the per-class/global cutoffs (DEFAULT_PRECISION_FLOOR
    /// or the FILEID_RAMPLUS_PRECISION_FLOOR env override).
    precision_floor: f32,
    /// Extra suppressed tags from the `ram_plus_suppress.txt` sidecar (lowercased),
    /// merged with the built-in SUPPRESSED_TAGS at tag time.
    suppress_extra: HashSet<String>,
    max_tags: usize,
}

impl RamPlusTagger {
    /// Load the exported RAM++ ONNX + its index-aligned tag list. Registers the
    /// same EP priority chain the rest of the ONNX stack uses (via
    /// `runtime::priority_chain` + `configure_session_builder`) and warms up so
    /// first-call kernel compile happens at load.
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

        let probe = RuntimeProbe::shared();
        let chain = priority_chain(probe.vendor);
        let chain_labels: Vec<&'static str> = chain.iter().map(|e| e.as_str()).collect();
        let builder = Session::builder().context("ORT session builder")?;
        let mut builder =
            configure_session_builder(builder).context("configure session (RAM++)")?;
        let providers = execution_providers_for_chain(&chain, probe.adapter_index);
        if !providers.is_empty() {
            builder = builder
                .with_execution_providers(providers)
                .context("register execution providers (RAM++)")?;
        }
        tracing::info!(model = "RAM++", tags = tags.len(), chain = ?chain_labels, "EP priority chain registered");
        let session = builder
            .commit_from_file(onnx)
            .context("ORT session commit (RAM++)")?;
        let input_name = session
            .inputs
            .first()
            .ok_or_else(|| anyhow::anyhow!("RAM++ ONNX has no inputs"))?
            .name
            .clone();

        // A set FILEID_RAMPLUS_THRESHOLD forces a single global cutoff (per-class
        // disabled — handy for threshold sweeps). Otherwise load the per-class
        // sidecar (`ram_plus_thresholds.txt`, written next to the tag list by the
        // export) when present + length-matched; missing/mismatched → global.
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

        // `select_tags` clamps the global cut UP to the precision floor, so a
        // FILEID_RAMPLUS_THRESHOLD sweep below the floor (default 0.62) is a
        // silent no-op. Warn once so the operator lowers the floor in lockstep
        // rather than chasing a knob that does nothing (#14).
        if let Some(t) = env_threshold {
            if t < precision_floor {
                tracing::warn!(
                    model = "RAM++",
                    env_threshold = t,
                    precision_floor,
                    "FILEID_RAMPLUS_THRESHOLD is below the precision floor; the effective \
                     cut is clamped up to the floor — lower FILEID_RAMPLUS_PRECISION_FLOOR \
                     in lockstep to sweep below it."
                );
            }
        }

        let suppress_extra = load_suppress_sidecar(tag_list);

        let mut model = Self {
            session,
            input_name,
            tags,
            threshold,
            per_class_threshold,
            precision_floor,
            suppress_extra,
            max_tags: DEFAULT_MAX_TAGS,
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

    /// Tag one decoded RGB8 image (any size — resized internally). Returns
    /// `(tag, confidence)` pairs above the threshold, highest-confidence first,
    /// capped at `max_tags`. Confidence is the sigmoid probability (0..1) and is
    /// what the pipeline persists in `tags.score`.
    pub fn tag(&mut self, rgb: &[u8], width: u32, height: u32) -> Result<Vec<(String, f32)>> {
        let chw = Self::preprocess_tensor(rgb, width, height)?;
        self.tag_prepared(chw)
    }

    /// CPU-only preprocess (resize + ImageNet-normalize) into the model input
    /// tensor. Split out of [`tag`] so a caller can run it OUTSIDE the per-session
    /// Mutex + GPU permit — the GPU forward in [`tag_prepared`] is the only part
    /// that needs the exclusive session, so one worker can prep while another's
    /// forward pass runs (shrinks the per-session serial CPU gap). Tag output is
    /// byte-identical to the old single-call `tag`.
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

    /// Run the GPU forward on a pre-normalized input tensor + select tags. The
    /// ONLY part that touches `&mut self.session`, so it is what the caller
    /// serializes under the pool Mutex.
    pub fn tag_prepared(&mut self, chw: Array4<f32>) -> Result<Vec<(String, f32)>> {
        let input = Tensor::from_array(chw).context("RAM++ input tensor")?;
        let input_name = self.input_name.clone();
        // Extract the logits inside a block so the `outputs` borrow of
        // `self.session` is released BEFORE the `&self` select_tags call below.
        // `SessionOutputs` has a Drop impl, so its borrow lives to end-of-scope
        // — calling select_tags in the same scope is the E0502 the old
        // closure-locals workaround sidestepped; the block drops it first.
        let logits_vec: Vec<f32> = {
            let outputs: SessionOutputs = self
                .session
                .run(vec![(input_name, SessionInputValue::from(input))])
                .context("RAM++ session.run")
                .map_err(classify_inference_error)?;
            let (_, value) = outputs
                .iter()
                .next()
                .ok_or_else(|| anyhow::anyhow!("RAM++ produced no outputs"))?;
            let (_shape, logits) = value
                .try_extract_tensor::<f32>()
                .context("extract RAM++ logits as f32")?;
            if logits.len() != self.tags.len() {
                anyhow::bail!(
                    "RAM++ output dim {} != tag list len {} — the ONNX and tag list are out of sync",
                    logits.len(),
                    self.tags.len()
                );
            }
            logits.to_vec()
        };
        Ok(self.select_tags(&logits_vec))
    }

    /// Batched [`tag`]: preprocess N images into ONE (N,3,384,384) tensor and run
    /// a single forward. A lone 384² image leaves a ~52-TFLOPS GPU <1% utilized —
    /// RAM++ at batch=1 is launch/latency-bound, not compute-bound (measured
    /// 670 ms/img on an RTX 2060; adding concurrent sessions regressed) — so
    /// batching fills the kernels and is the throughput fix (HW-4). REQUIRES an
    /// ONNX exported with a dynamic batch axis (export_ram_plus_onnx.py
    /// --dynamic-batch); a fixed-batch=1 model errors at run. Returns one tag
    /// list per input, in order.
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
        let input = Tensor::from_array(batch).context("RAM++ batch input tensor")?;
        let input_name = self.input_name.clone();
        let num_tags = self.tags.len();
        let logits_vec: Vec<f32> = {
            let outputs: SessionOutputs = self
                .session
                .run(vec![(input_name, SessionInputValue::from(input))])
                .context("RAM++ batch session.run")
                .map_err(classify_inference_error)?;
            let (_, value) = outputs
                .iter()
                .next()
                .ok_or_else(|| anyhow::anyhow!("RAM++ produced no outputs"))?;
            let (_shape, logits) = value
                .try_extract_tensor::<f32>()
                .context("extract RAM++ batch logits as f32")?;
            if logits.len() != bs * num_tags {
                anyhow::bail!(
                    "RAM++ batch output len {} != {bs} images x {num_tags} tags",
                    logits.len()
                );
            }
            logits.to_vec()
        };
        Ok(logits_vec
            .chunks_exact(num_tags)
            .map(|row| self.select_tags(row))
            .collect())
    }

    /// Apply per-class thresholds + suppress-list + top-k cap to one logits row →
    /// `(tag, confidence)` pairs, highest first. Shared by [`tag`] + [`tag_batch`]
    /// so both paths emit identical tags.
    fn select_tags(&self, logits: &[f32]) -> Vec<(String, f32)> {
        let mut hits: Vec<(usize, f32)> = logits
            .iter()
            .enumerate()
            .filter_map(|(i, &z)| {
                if is_suppressed(&self.tags[i], &self.suppress_extra) {
                    return None;
                }
                let p = sigmoid(z);
                let cut = self
                    .per_class_threshold
                    .as_ref()
                    .map(|t| t[i])
                    .unwrap_or(self.threshold)
                    .max(self.precision_floor);
                (p >= cut).then_some((i, p))
            })
            .collect();
        hits.sort_by(|a, b| b.1.total_cmp(&a.1));
        hits.truncate(self.max_tags);
        hits.into_iter()
            .map(|(i, p)| (self.tags[i].clone(), p))
            .collect()
    }

    /// Bilinear resize to 384×384 + ImageNet normalize into a (1,3,384,384)
    /// tensor. Bilinear (not nearest) because tag quality is sensitive to
    /// resampling; matches the export script's PIL BILINEAR.
    fn preprocess(rgb: &[u8], width: u32, height: u32) -> Result<Array4<f32>> {
        // Borrow the caller's buffer instead of cloning it (caller already
        // validated rgb.len() == w*h*3). resize early-outs to a plain copy at
        // equal dimensions, so calling it unconditionally is byte-identical to
        // the old width==height==INPUT_SIZE short-circuit.
        let src = image::ImageBuffer::<image::Rgb<u8>, &[u8]>::from_raw(width, height, rgb)
            .ok_or_else(|| anyhow::anyhow!("RAM++ preprocess: bad RGB buffer"))?;
        let resized = image::imageops::resize(
            &src,
            INPUT_SIZE,
            INPUT_SIZE,
            image::imageops::FilterType::Triangle,
        );
        let n = INPUT_SIZE as usize;
        let mut chw = Array4::<f32>::zeros((1, 3, n, n));
        for y in 0..n {
            for x in 0..n {
                let px = resized.get_pixel(x as u32, y as u32);
                chw[[0, 0, y, x]] = (px[0] as f32 / 255.0 - IMAGENET_MEAN[0]) / IMAGENET_STD[0];
                chw[[0, 1, y, x]] = (px[1] as f32 / 255.0 - IMAGENET_MEAN[1]) / IMAGENET_STD[1];
                chw[[0, 2, y, x]] = (px[2] as f32 / 255.0 - IMAGENET_MEAN[2]) / IMAGENET_STD[2];
            }
        }
        Ok(chw)
    }
}

fn sigmoid(z: f32) -> f32 {
    1.0 / (1.0 + (-z).exp())
}

/// Per-EP variant-aware ONNX path: `ram_plus/ram_plus.onnx` (or
/// `_int8.onnx` / `_qnn.bin` on accelerated EPs when a variant is dropped in).
pub fn default_onnx_path() -> Result<PathBuf> {
    Ok(super::variants::resolve_model_path(
        &crate::paths::models_dir()?.join("ram_plus"),
        "ram_plus",
    ))
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
    fn sigmoid_monotone() {
        assert!(sigmoid(-10.0) < 0.01);
        assert!((sigmoid(0.0) - 0.5).abs() < 1e-6);
        assert!(sigmoid(10.0) > 0.99);
    }

    #[test]
    fn generic_medium_tags_are_suppressed() {
        // Image-medium words + the People-redundant "face" + the noisy "catch"
        // are filtered (case-insensitively); real content tags pass through.
        assert!(is_suppressed_builtin("photo"));
        assert!(is_suppressed_builtin("image"));
        assert!(is_suppressed_builtin("face"));
        assert!(is_suppressed_builtin("catch"));
        assert!(is_suppressed_builtin("Photo"));
        assert!(is_suppressed_builtin("CATCH"));
        // Posture / clothing-state fillers (the "too generic" offenders).
        assert!(is_suppressed_builtin("stand"));
        assert!(is_suppressed_builtin("sit"));
        assert!(is_suppressed_builtin("lay"));
        assert!(is_suppressed_builtin("pose"));
        assert!(is_suppressed_builtin("wear"));
        // Content words + emotion/activity tags survive.
        assert!(!is_suppressed_builtin("graduation"));
        assert!(!is_suppressed_builtin("mountain"));
        assert!(!is_suppressed_builtin("person"));
        assert!(!is_suppressed_builtin("smile"));
        assert!(!is_suppressed_builtin("play"));
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
