// MobileCLIP-S2 image encoder. Maps a 256×256 RGB image to a 512-d
// L2-normalized float32 embedding for scan-time clustering and
// query-time semantic search.
//
// Inference order: resize-and-letterbox → ImageNet mean/std normalize
// → CHW float32 → ORT session.run → L2 normalize. Persisted as raw
// little-endian bytes in `clip_embeddings.embedding`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ndarray::Array4;
use ort::session::{Session, SessionInputValue, SessionOutputs};
use ort::value::Tensor;

use super::runtime::{classify_inference_error, execution_providers_for_chain, priority_chain, RuntimeProbe};

const IMAGENET_MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const IMAGENET_STD: [f32; 3] = [0.229, 0.224, 0.225];

pub struct MobileClipImage {
    session: Session,
    input_size: u32,
}

impl MobileClipImage {
    pub fn load<P: AsRef<Path>>(weights: P) -> Result<Self> {
        let path = weights.as_ref();
        if !path.exists() {
            anyhow::bail!("MobileCLIP weights missing at {}", path.display());
        }
        let probe = RuntimeProbe::detect();
        let chain = priority_chain(probe.vendor);
        let mut builder = Session::builder()
            .context("ORT session builder")?
            .with_intra_threads(1)
            .context("set intra threads (MobileCLIP image)")?;
        let chain_labels: Vec<&'static str> = chain.iter().map(|e| e.as_str()).collect();
        let providers = execution_providers_for_chain(&chain);
        if !providers.is_empty() {
            builder = builder
                .with_execution_providers(providers)
                .context("register execution providers (MobileCLIP image)")?;
        }
        tracing::info!(model = "MobileCLIP image", chain = ?chain_labels, "EP priority chain registered");
        let session = builder
            .commit_from_file(path)
            .context("ORT session commit (MobileCLIP image)")?;
        // Warmup with a zero 256×256 frame so first-call kernel compile
        // happens during load.
        let mut model = Self { session, input_size: 256 };
        let warmup_started = std::time::Instant::now();
        let _ = model.embed(&[0u8; 3 * 256 * 256])?;
        tracing::info!(
            model = "MobileCLIP image",
            warmup_ms = warmup_started.elapsed().as_millis() as u64,
            "warmup complete"
        );
        Ok(model)
    }

    /// Embed a 256×256 RGB8 image. Caller pre-resizes to 256×256 via
    /// `tagging::resize_rgb_nearest` (or bilinear when we wire that).
    /// Single-image embed. Kept for non-batched callers (e.g. interactive
    /// semantic-search query embedding) — main scan pipeline goes through
    /// `embed_batch` via `pipeline::batch_clip::ClipBatchCoordinator`.
    #[allow(dead_code)]
    pub fn embed(&mut self, rgb_256: &[u8]) -> Result<Vec<f32>> {
        let n = self.input_size as usize;
        if rgb_256.len() != 3 * n * n {
            anyhow::bail!(
                "MobileCLIP embed expects {} RGB8 bytes, got {}",
                3 * n * n,
                rgb_256.len()
            );
        }
        let mut chw = Array4::<f32>::zeros((1, 3, n, n));
        for y in 0..n {
            for x in 0..n {
                let i = (y * n + x) * 3;
                let r = rgb_256[i] as f32 / 255.0;
                let g = rgb_256[i + 1] as f32 / 255.0;
                let b = rgb_256[i + 2] as f32 / 255.0;
                chw[[0, 0, y, x]] = (r - IMAGENET_MEAN[0]) / IMAGENET_STD[0];
                chw[[0, 1, y, x]] = (g - IMAGENET_MEAN[1]) / IMAGENET_STD[1];
                chw[[0, 2, y, x]] = (b - IMAGENET_MEAN[2]) / IMAGENET_STD[2];
            }
        }

        let input = Tensor::from_array(chw).context("MobileCLIP input tensor")?;
        let input_name = self
            .session
            .inputs
            .first()
            .ok_or_else(|| anyhow::anyhow!("MobileCLIP ONNX has no inputs"))?
            .name
            .clone();
        let outputs: SessionOutputs = self
            .session
            .run(vec![(input_name, SessionInputValue::from(input))])
            .context("MobileCLIP session.run")
            .map_err(classify_inference_error)?;
        let (_, value) = outputs
            .iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("MobileCLIP produced no outputs"))?;
        let (_shape, data) = value
            .try_extract_tensor::<f32>()
            .context("extract MobileCLIP output as f32")?;
        let mut emb: Vec<f32> = data.to_vec();
        l2_normalize(&mut emb);
        Ok(emb)
    }

    /// Batched inference. Takes N pre-resized 256×256 RGB8 buffers, packs
    /// them into a single (N, 3, 256, 256) tensor, calls `session.run` ONCE,
    /// and returns N L2-normalized embeddings.
    ///
    /// Per-call dispatch overhead through DirectML is sizable (kernel queue
    /// submission, fence wait, GPU↔CPU sync). Doing 4 images in one call ≈
    /// 2× the wall time of one image, so throughput is ~2× per Session
    /// without growing VRAM.
    pub fn embed_batch(
        &mut self,
        rgb_256_images: &[Vec<u8>],
    ) -> Result<Vec<Vec<f32>>> {
        let n = self.input_size as usize;
        let batch = rgb_256_images.len();
        if batch == 0 {
            return Ok(Vec::new());
        }
        for (i, buf) in rgb_256_images.iter().enumerate() {
            if buf.len() != 3 * n * n {
                anyhow::bail!(
                    "MobileCLIP embed_batch[{}] expects {} RGB8 bytes, got {}",
                    i,
                    3 * n * n,
                    buf.len()
                );
            }
        }
        let mut chw = Array4::<f32>::zeros((batch, 3, n, n));
        for (b, rgb) in rgb_256_images.iter().enumerate() {
            for y in 0..n {
                for x in 0..n {
                    let i = (y * n + x) * 3;
                    let r = rgb[i] as f32 / 255.0;
                    let g = rgb[i + 1] as f32 / 255.0;
                    let bch = rgb[i + 2] as f32 / 255.0;
                    chw[[b, 0, y, x]] = (r - IMAGENET_MEAN[0]) / IMAGENET_STD[0];
                    chw[[b, 1, y, x]] = (g - IMAGENET_MEAN[1]) / IMAGENET_STD[1];
                    chw[[b, 2, y, x]] = (bch - IMAGENET_MEAN[2]) / IMAGENET_STD[2];
                }
            }
        }
        let input = Tensor::from_array(chw).context("MobileCLIP batch input tensor")?;
        let input_name = self
            .session
            .inputs
            .first()
            .ok_or_else(|| anyhow::anyhow!("MobileCLIP ONNX has no inputs"))?
            .name
            .clone();
        let outputs: SessionOutputs = self
            .session
            .run(vec![(input_name, SessionInputValue::from(input))])
            .context("MobileCLIP session.run (batch)")
            .map_err(classify_inference_error)?;
        let (_, value) = outputs
            .iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("MobileCLIP produced no outputs"))?;
        let (shape, data) = value
            .try_extract_tensor::<f32>()
            .context("extract MobileCLIP batch output as f32")?;
        let total: usize = shape.iter().map(|d| *d as usize).product();
        if total != data.len() {
            anyhow::bail!(
                "MobileCLIP output shape product {} != data len {}",
                total,
                data.len()
            );
        }
        let embed_dim = total / batch;
        let mut out = Vec::with_capacity(batch);
        for b in 0..batch {
            let start = b * embed_dim;
            let mut emb: Vec<f32> = data[start..start + embed_dim].to_vec();
            l2_normalize(&mut emb);
            out.push(emb);
        }
        Ok(out)
    }
}

fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-8);
    for x in v.iter_mut() {
        *x /= norm;
    }
}

pub fn default_weights_path() -> Result<PathBuf> {
    Ok(crate::paths::models_dir()?
        .join("mobileclip")
        .join("mobileclip_s2_image.onnx"))
}
