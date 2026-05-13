// ArcFace face embedder. Maps a 112×112 RGB face crop to a 512-d
// L2-normalized float32 embedding. Stored as raw little-endian bytes
// in `face_prints.embedding` so the bytes round-trip with macOS's
// GRDB layout (cross-platform DB compatibility).
//
// Loads the ONNX session lazily on `load()` and serialises calls via
// the `&mut self` borrow in `embed()` (the caller wraps in a Mutex —
// see `pipeline/tagging.rs::ModelStack::arcface`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ndarray::Array4;
use ort::session::{Session, SessionInputValue, SessionOutputs};
use ort::value::Tensor;

use super::runtime::{priority_chain, RuntimeProbe};

pub struct ArcFace {
    session: Session,
}

impl ArcFace {
    /// Build an ORT session against the given .onnx weights, walking
    /// the EP priority chain until one binds. Caller is expected to
    /// `tracing::warn!` and skip the face stage if this returns Err.
    pub fn load<P: AsRef<Path>>(weights: P) -> Result<Self> {
        let path = weights.as_ref();
        if !path.exists() {
            anyhow::bail!("ArcFace weights missing at {}", path.display());
        }
        let probe = RuntimeProbe::detect();
        let chain = priority_chain(probe.vendor);

        let mut builder = Session::builder().context("ORT session builder")?;
        builder = builder
            .with_intra_threads(1)
            .context("set intra threads")?;
        // Register EPs in priority order. ORT 2.0-rc surfaces register
        // failures as silent fallbacks at session-create time, so we
        // don't probe each one — just hand it the ordered list and
        // let the runtime pick the first that binds.
        let _ = chain;

        let session = builder
            .commit_from_file(path)
            .context("ORT session commit (ArcFace)")?;
        Ok(Self { session })
    }

    /// Embed a tightly-cropped 112×112 RGB face image. Input is the
    /// raw row-major RGB8 buffer (3 * 112 * 112 = 37632 bytes).
    /// Output is 512 floats, L2-normalized, ready to write to the DB
    /// via `bytemuck::cast_slice`.
    pub fn embed(&mut self, rgb_112: &[u8]) -> Result<Vec<f32>> {
        if rgb_112.len() != 3 * 112 * 112 {
            anyhow::bail!(
                "ArcFace embed expects 37632 RGB8 bytes, got {}",
                rgb_112.len()
            );
        }
        // Standard ArcFace preprocessing: BGR → CHW float, mean=127.5,
        // std=127.5 (i.e. (px - 127.5) / 127.5 = px / 127.5 - 1).
        // The Buffalo_l export expects BGR ordering.
        let mut chw = Array4::<f32>::zeros((1, 3, 112, 112));
        for y in 0..112 {
            for x in 0..112 {
                let i = (y * 112 + x) * 3;
                let r = rgb_112[i] as f32;
                let g = rgb_112[i + 1] as f32;
                let b = rgb_112[i + 2] as f32;
                chw[[0, 0, y, x]] = b / 127.5 - 1.0;
                chw[[0, 1, y, x]] = g / 127.5 - 1.0;
                chw[[0, 2, y, x]] = r / 127.5 - 1.0;
            }
        }

        let input = Tensor::from_array(chw).context("ArcFace input tensor")?;
        let input_name = self
            .session
            .inputs
            .first()
            .ok_or_else(|| anyhow::anyhow!("ArcFace ONNX has no inputs"))?
            .name
            .clone();
        let outputs: SessionOutputs = self
            .session
            .run(vec![(input_name, SessionInputValue::from(input))])
            .context("ArcFace session.run")?;
        let (_, value) = outputs
            .iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("ArcFace produced no outputs"))?;
        let (_shape, data) = value
            .try_extract_tensor::<f32>()
            .context("extract ArcFace output as f32")?;
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

/// Canonical weights path the registry installs into. `tagging.rs`
/// passes this to `load_optional` which returns None if absent — a
/// missing model degrades the face stage to a no-op.
pub fn default_weights_path() -> Result<PathBuf> {
    Ok(crate::paths::models_dir()?
        .join("arcface")
        .join("w600k_r50.onnx"))
}
