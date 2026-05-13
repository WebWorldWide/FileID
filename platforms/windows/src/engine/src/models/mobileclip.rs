// MobileCLIP-S2 image encoder. Maps a 256×256 RGB image to a 512-d
// L2-normalized float32 embedding for scan-time clustering and
// query-time semantic search.
//
// Inference order: resize-and-letterbox → ImageNet mean/std normalize
// → CHW float32 → ORT session.run → L2 normalize. Persisted as raw
// little-endian bytes in `clip_embeddings.embedding` (cross-platform
// DB compatibility with macOS).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ndarray::Array4;
use ort::session::{Session, SessionInputValue, SessionOutputs};
use ort::value::Tensor;

use super::runtime::{priority_chain, RuntimeProbe};

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
        let _chain = priority_chain(probe.vendor);
        let session = Session::builder()
            .context("ORT session builder")?
            .commit_from_file(path)
            .context("ORT session commit (MobileCLIP image)")?;
        Ok(Self { session, input_size: 256 })
    }

    /// Embed a 256×256 RGB8 image. Caller pre-resizes to 256×256 via
    /// `tagging::resize_rgb_nearest` (or bilinear when we wire that).
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
            .context("MobileCLIP session.run")?;
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
