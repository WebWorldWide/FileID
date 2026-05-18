// MobileNetV3-Large ImageNet-1k scene classifier.
//
// Provides per-file semantic tags (e.g. "dog", "beach", "kitchen") so
// Library cards have meaningful chips beyond CLIP embeddings + face/OCR
// signals. ~22 MB on disk, ~10-15 ms per inference on DirectML/RTX 2060
// at 224×224. Designed to slot into the scan pipeline alongside the
// existing MobileCLIP image embed.
//
// Input: NCHW float32, 3×224×224, ImageNet mean/std normalization
//        (same as MobileCLIP image encoder, so we reuse the decoded RGB
//        from the worker — see tagging.rs).
// Output: 1000-class logits → softmax → top-K filtered by threshold.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ndarray::Array4;
use ort::session::{Session, SessionInputValue, SessionOutputs};
use ort::value::Tensor;

use super::runtime::{classify_inference_error, execution_providers_for_chain, priority_chain, RuntimeProbe};

const IMAGENET_MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const IMAGENET_STD: [f32; 3] = [0.229, 0.224, 0.225];
pub const INPUT_SIZE: usize = 224;

pub struct ClassifierSession {
    session: Session,
    labels: Vec<String>,
}

impl ClassifierSession {
    pub fn load<P: AsRef<Path>>(model_path: P, labels_path: P) -> Result<Self> {
        let model_path = model_path.as_ref();
        let labels_path = labels_path.as_ref();
        if !model_path.exists() {
            anyhow::bail!("classifier model missing at {}", model_path.display());
        }
        if !labels_path.exists() {
            anyhow::bail!("classifier labels missing at {}", labels_path.display());
        }
        let labels = parse_labels(labels_path)?;
        if labels.is_empty() {
            anyhow::bail!("classifier labels file is empty");
        }
        let probe = RuntimeProbe::detect();
        let chain = priority_chain(probe.vendor);
        let mut builder = Session::builder()
            .context("ORT session builder (classifier)")?
            .with_intra_threads(1)
            .context("set intra threads (classifier)")?;
        let providers = execution_providers_for_chain(&chain);
        if !providers.is_empty() {
            builder = builder
                .with_execution_providers(providers)
                .context("register EPs (classifier)")?;
        }
        tracing::info!(model = "classifier", "EP chain registered");
        let session = builder
            .commit_from_file(model_path)
            .context("ORT session commit (classifier)")?;
        let mut model = Self { session, labels };
        // Warmup the session with a zero-frame so the first user-driven
        // classify doesn't pay the JIT/kernel-compile cost.
        let warmup_started = std::time::Instant::now();
        let _ = model.classify_batch(&[vec![0u8; 3 * INPUT_SIZE * INPUT_SIZE]], 1, 0.0);
        tracing::info!(
            model = "classifier",
            warmup_ms = warmup_started.elapsed().as_millis() as u64,
            label_count = model.labels.len(),
            "warmup complete"
        );
        Ok(model)
    }

    /// Batched classify. Inputs: N pre-resized 224×224 RGB8 buffers,
    /// each `3 * INPUT_SIZE * INPUT_SIZE` bytes long (interleaved RGB).
    /// Returns top-K (label, confidence) per input, sorted by confidence
    /// descending. Confidences below `threshold` are dropped.
    pub fn classify_batch(
        &mut self,
        images: &[Vec<u8>],
        top_k: usize,
        threshold: f32,
    ) -> Result<Vec<Vec<(String, f32)>>> {
        let n = INPUT_SIZE;
        let batch = images.len();
        if batch == 0 {
            return Ok(Vec::new());
        }
        for (i, buf) in images.iter().enumerate() {
            if buf.len() != 3 * n * n {
                anyhow::bail!(
                    "classifier image[{}] expects {} bytes, got {}",
                    i, 3 * n * n, buf.len()
                );
            }
        }
        let mut chw = Array4::<f32>::zeros((batch, 3, n, n));
        for (b, rgb) in images.iter().enumerate() {
            for y in 0..n {
                for x in 0..n {
                    let i = (y * n + x) * 3;
                    let r = rgb[i] as f32 / 255.0;
                    let g = rgb[i + 1] as f32 / 255.0;
                    let b_ch = rgb[i + 2] as f32 / 255.0;
                    chw[[b, 0, y, x]] = (r - IMAGENET_MEAN[0]) / IMAGENET_STD[0];
                    chw[[b, 1, y, x]] = (g - IMAGENET_MEAN[1]) / IMAGENET_STD[1];
                    chw[[b, 2, y, x]] = (b_ch - IMAGENET_MEAN[2]) / IMAGENET_STD[2];
                }
            }
        }
        let input = Tensor::from_array(chw).context("classifier input tensor")?;
        let input_name = self
            .session
            .inputs
            .first()
            .ok_or_else(|| anyhow::anyhow!("classifier ONNX has no inputs"))?
            .name
            .clone();
        let outputs: SessionOutputs = self
            .session
            .run(vec![(input_name, SessionInputValue::from(input))])
            .context("classifier session.run")
            .map_err(classify_inference_error)?;
        let (_, value) = outputs
            .iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("classifier produced no outputs"))?;
        let (shape, data) = value
            .try_extract_tensor::<f32>()
            .context("extract classifier output as f32")?;
        let total: usize = shape.iter().map(|d| *d as usize).product();
        if total != data.len() {
            anyhow::bail!(
                "classifier output shape product {} != data len {}",
                total,
                data.len()
            );
        }
        let n_classes = total / batch;
        // Some MobileNet exports include a 1000-d head with no background
        // class; others add one for 1001 logits. Accept anything whose
        // first N matches our label count, else fail with a clear msg.
        let label_count = self.labels.len();
        if n_classes != label_count && n_classes != label_count + 1 {
            anyhow::bail!(
                "classifier output dim {} != label count {} (or {}+1 with background)",
                n_classes, label_count, label_count
            );
        }
        let label_offset = usize::from(n_classes == label_count + 1);
        let mut out = Vec::with_capacity(batch);
        for b in 0..batch {
            let start = b * n_classes + label_offset;
            let logits = &data[start..start + label_count];
            let probs = softmax(logits);
            let mut indexed: Vec<(usize, f32)> = probs.iter().copied().enumerate().collect();
            indexed.sort_by(|a, c| c.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            let top: Vec<(String, f32)> = indexed
                .into_iter()
                .take(top_k)
                .filter(|(_, p)| *p >= threshold)
                .map(|(i, p)| (self.labels[i].clone(), p))
                .collect();
            out.push(top);
        }
        Ok(out)
    }
}

fn softmax(logits: &[f32]) -> Vec<f32> {
    let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let exps: Vec<f32> = logits.iter().map(|x| (x - max).exp()).collect();
    let sum: f32 = exps.iter().sum::<f32>().max(1e-12);
    exps.into_iter().map(|x| x / sum).collect()
}

/// Accept either ImageNet synset format (`"n01440764 tench, Tinca tinca"`)
/// or one-label-per-line plain text. For synset lines, strip the wnid and
/// take the first comma-delimited synonym so display tags read naturally.
fn parse_labels(path: &Path) -> Result<Vec<String>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading labels at {}", path.display()))?;
    let labels: Vec<String> = raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            if let Some((first_token, rest)) = l.split_once(char::is_whitespace) {
                let is_wnid = first_token.starts_with('n')
                    && first_token.len() >= 7
                    && first_token[1..].chars().all(|c| c.is_ascii_digit());
                if is_wnid {
                    return rest
                        .split(',')
                        .next()
                        .unwrap_or(rest)
                        .trim()
                        .to_string();
                }
            }
            l.to_string()
        })
        .collect();
    Ok(labels)
}

pub fn default_model_path() -> Result<PathBuf> {
    Ok(crate::paths::models_dir()?
        .join("classifier")
        .join("mobilenetv3_large.onnx"))
}

pub fn default_labels_path() -> Result<PathBuf> {
    Ok(crate::paths::models_dir()?
        .join("classifier")
        .join("imagenet_classes.txt"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn softmax_sums_to_one() {
        let probs = softmax(&[1.0, 2.0, 3.0, 4.0]);
        let sum: f32 = probs.iter().sum();
        assert!((sum - 1.0).abs() < 1e-5, "softmax sum = {sum}");
    }

    #[test]
    fn softmax_picks_max() {
        let probs = softmax(&[0.0, 0.0, 10.0, 0.0]);
        let max_idx = probs
            .iter()
            .copied()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
            .unwrap()
            .0;
        assert_eq!(max_idx, 2);
        assert!(probs[2] > 0.99);
    }

    #[test]
    fn parse_labels_plain() {
        let dir = std::env::temp_dir().join("fileid-classifier-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("plain.txt");
        std::fs::write(&p, "dog\ncat\n# a comment\n\nfish\n").unwrap();
        let got = parse_labels(&p).unwrap();
        assert_eq!(got, vec!["dog", "cat", "fish"]);
    }

    #[test]
    fn parse_labels_synset_strips_wnid() {
        let dir = std::env::temp_dir().join("fileid-classifier-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("synset.txt");
        std::fs::write(
            &p,
            "n01440764 tench, Tinca tinca\nn01443537 goldfish, Carassius auratus\n",
        )
        .unwrap();
        let got = parse_labels(&p).unwrap();
        assert_eq!(got, vec!["tench", "goldfish"]);
    }
}
