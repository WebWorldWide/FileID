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
/// Cap RAM++'s own emissions a little below the scan pipeline's 16-tag total
/// cap, so Year/camera-family/OCR-doc extras always keep a few slots in the
/// combined per-file set (content tags are pushed first, then the extras).
const DEFAULT_MAX_TAGS: usize = 12;

pub struct RamPlusTagger {
    session: Session,
    /// Index-aligned with the model's logits; `tags[i]` is the label for output i.
    tags: Vec<String>,
    threshold: f32,
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

        let probe = RuntimeProbe::detect();
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

        let threshold = std::env::var("FILEID_RAMPLUS_THRESHOLD")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .filter(|t| (0.0..=1.0).contains(t))
            .unwrap_or(DEFAULT_THRESHOLD);

        let mut model = Self {
            session,
            tags,
            threshold,
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
        let chw = Self::preprocess(rgb, width, height)?;
        let input = Tensor::from_array(chw).context("RAM++ input tensor")?;
        let input_name = self
            .session
            .inputs
            .first()
            .ok_or_else(|| anyhow::anyhow!("RAM++ ONNX has no inputs"))?
            .name
            .clone();
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
        let mut hits: Vec<(usize, f32)> = logits
            .iter()
            .enumerate()
            .filter_map(|(i, &z)| {
                let p = sigmoid(z);
                (p >= self.threshold).then_some((i, p))
            })
            .collect();
        // Highest confidence first; truncate to the per-file cap.
        hits.sort_by(|a, b| b.1.total_cmp(&a.1));
        hits.truncate(self.max_tags);
        Ok(hits
            .into_iter()
            .map(|(i, p)| (self.tags[i].clone(), p))
            .collect())
    }

    /// Bilinear resize to 384×384 + ImageNet normalize into a (1,3,384,384)
    /// tensor. Bilinear (not nearest) because tag quality is sensitive to
    /// resampling; matches the export script's PIL BILINEAR.
    fn preprocess(rgb: &[u8], width: u32, height: u32) -> Result<Array4<f32>> {
        let src = image::RgbImage::from_raw(width, height, rgb.to_vec())
            .ok_or_else(|| anyhow::anyhow!("RAM++ preprocess: bad RGB buffer"))?;
        let resized = if width == INPUT_SIZE && height == INPUT_SIZE {
            src
        } else {
            image::imageops::resize(
                &src,
                INPUT_SIZE,
                INPUT_SIZE,
                image::imageops::FilterType::Triangle,
            )
        };
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
}
