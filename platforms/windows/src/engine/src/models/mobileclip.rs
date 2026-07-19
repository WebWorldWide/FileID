// CLIP ViT-B/32 image encoder (OpenAI/OpenCLIP, MIT). Maps a 224×224 RGB
// image to a 512-d L2-normalized float32 embedding for scan-time clustering
// and query-time semantic search.
//
// Inference order: resize → CLIP mean/std normalize → CHW float32
// → ORT session.run → L2 normalize. Persisted as raw little-endian bytes
// in `clip_embeddings.embedding`.
//
// File was originally MobileCLIP-S2 (256×256, research-only license); now
// loads the Apache-2.0 / commercial-clean ViT-B/32 export at 224×224.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ndarray::Array4;
use ort::session::{Session, SessionInputValue, SessionOutputs};
use ort::value::Tensor;

use super::runtime::{
    classify_inference_error, commit_chain_session, ensure_gpu_inference_alive,
};

/// Expected image-embedding width. ViT-B/32 emits 512-d. A model whose output
/// width differs is wrong/substituted (corrupt, re-quantized, or a future swap
/// with a mismatched registry SHA) and must be rejected, not L2-normalized and
/// persisted as an off-dimension `clip_embeddings` blob — that silently poisons
/// scene-tag dot products (`scene_vocab::dot` folds over `min(len)`) and
/// semantic search. Mirrors the SFace (`sface.rs` ENG-69), RAM++, and BGE guards.
pub(crate) const CLIP_EMBED_DIM: usize = 512;

// OpenAI CLIP normalization (ViT-B/32) — differs from ImageNet; using ImageNet
// stats on a CLIP model measurably degrades the embeddings.
#[allow(clippy::excessive_precision)]
const CLIP_MEAN: [f32; 3] = [0.48145466, 0.4578275, 0.40821073];
#[allow(clippy::excessive_precision)]
const CLIP_STD: [f32; 3] = [0.26862954, 0.26130258, 0.27577711];
const INV_255: f32 = 1.0 / 255.0;

pub struct MobileClipImage {
    session: Session,
    /// The ONNX's single input tensor name, read once at load and reused on
    /// every forward instead of re-walking `session.inputs.first()`.
    input_name: String,
    input_size: u32,
}

impl MobileClipImage {
    pub fn load<P: AsRef<Path>>(weights: P) -> Result<Self> {
        let path = weights.as_ref();
        if !path.exists() {
            anyhow::bail!("MobileCLIP weights missing at {}", path.display());
        }
        let (session, input_name) = commit_chain_session("MobileCLIP image", path)?;
        // Warmup with a zero 224x224 frame so first-call kernel compile
        // happens during load.
        let mut model = Self { session, input_name, input_size: 224 };
        let warmup_started = std::time::Instant::now();
        let _ = model.embed(&[0u8; 3 * 224 * 224])?;
        tracing::info!(
            model = "MobileCLIP image",
            warmup_ms = warmup_started.elapsed().as_millis() as u64,
            "warmup complete"
        );
        Ok(model)
    }

    /// Embed a 224x224 RGB8 image. Caller pre-resizes to 224x224 via
    /// `tagging::resize_rgb_quality`.
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
        fill_clip_chw(&mut chw, 0, rgb_256, n);

        let input = Tensor::from_array(chw).context("MobileCLIP input tensor")?;
        let input_name = self.input_name.clone();
        ensure_gpu_inference_alive()?;
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
        if data.len() != CLIP_EMBED_DIM {
            anyhow::bail!(
                "MobileCLIP produced a {}-d embedding, expected {CLIP_EMBED_DIM} (wrong or corrupt model?)",
                data.len()
            );
        }
        let mut emb: Vec<f32> = data.to_vec();
        l2_normalize(&mut emb);
        Ok(emb)
    }

    /// Batched inference. Takes N pre-resized 224x224 RGB8 buffers, packs
    /// them into a single (N, 3, 224, 224) tensor, calls `session.run` ONCE,
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
            fill_clip_chw(&mut chw, b, rgb, n);
        }
        let input = Tensor::from_array(chw).context("MobileCLIP batch input tensor")?;
        let input_name = self.input_name.clone();
        ensure_gpu_inference_alive()?;
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
        if batch == 0 || total % batch != 0 {
            anyhow::bail!(
                "MobileCLIP output total {} not divisible by batch size {}; shape {:?}",
                total, batch, shape
            );
        }
        let embed_dim = total / batch;
        if embed_dim != CLIP_EMBED_DIM {
            anyhow::bail!(
                "MobileCLIP produced {embed_dim}-d embeddings, expected {CLIP_EMBED_DIM} (wrong or corrupt model?)"
            );
        }
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

fn fill_clip_chw(chw: &mut Array4<f32>, image_index: usize, rgb: &[u8], n: usize) {
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
        out[base + p] = (r - CLIP_MEAN[0]) / CLIP_STD[0];
        out[base + plane + p] = (g - CLIP_MEAN[1]) / CLIP_STD[1];
        out[base + (2 * plane) + p] = (b - CLIP_MEAN[2]) / CLIP_STD[2];
        src += 3;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(v: u8, channel: usize) -> f32 {
        ((v as f32 * INV_255) - CLIP_MEAN[channel]) / CLIP_STD[channel]
    }

    fn assert_close(actual: f32, expected: f32) {
        assert!((actual - expected).abs() < 1e-6, "{actual} != {expected}");
    }

    #[test]
    fn fill_clip_chw_writes_contiguous_channel_planes() {
        let rgb = vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120];
        let mut chw = Array4::<f32>::zeros((1, 3, 2, 2));

        fill_clip_chw(&mut chw, 0, &rgb, 2);

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
}
