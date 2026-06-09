// ArcFace face embedder. Maps a 112×112 RGB face crop to a 512-d
// L2-normalized float32 embedding. Stored as raw little-endian bytes
// in `face_prints.embedding` for cross-platform DB compatibility.
//
// Loads the ONNX session on `load()` and serialises calls via the
// `&mut self` borrow in `embed()` (the caller wraps in a Mutex).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ndarray::Array4;
use ort::session::{Session, SessionInputValue, SessionOutputs};
use ort::value::Tensor;

use super::runtime::{classify_inference_error, configure_session_builder, execution_providers_for_chain, priority_chain, RuntimeProbe};

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

        let builder = Session::builder().context("ORT session builder")?;
        let mut builder = configure_session_builder(builder)
            .context("configure session (ArcFace)")?;
        let chain_labels: Vec<&'static str> = chain.iter().map(|e| e.as_str()).collect();
        let providers = execution_providers_for_chain(&chain, probe.adapter_index);
        if !providers.is_empty() {
            builder = builder
                .with_execution_providers(providers)
                .context("register execution providers (ArcFace)")?;
        }
        tracing::info!(model = "ArcFace", chain = ?chain_labels, "EP priority chain registered");

        let session = builder
            .commit_from_file(path)
            .context("ORT session commit (ArcFace)")?;
        // Warmup with a zero buffer so DirectML kernel compilation + first
        // GPU allocation happen here, not on the first real file. If TDR
        // triggers during warmup, the marker propagates and load_pool bails
        // the whole stack instead of letting workers see a half-dead GPU.
        let mut model = Self { session };
        let warmup_started = std::time::Instant::now();
        let _ = model.embed(&[0u8; 3 * 112 * 112])?;
        tracing::info!(
            model = "ArcFace",
            warmup_ms = warmup_started.elapsed().as_millis() as u64,
            "warmup complete"
        );
        Ok(model)
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
        // ArcFace preprocessing: RGB → CHW float, mean=127.5, std=127.5
        // (i.e. (px - 127.5) / 127.5 = px / 127.5 - 1). The Buffalo_l
        // (w600k_r50) export — the SAME model the macOS reference uses — is
        // trained with insightface's swapRB=True convention, i.e. it expects
        // RGB. The previous code wrote B into channel 0 and R into channel 2,
        // producing channel-swapped embeddings that were both lower quality
        // and byte-INCOMPATIBLE with the macOS embeddings stored in the shared
        // DB schema. (Recompute existing arcface_embeddings after this fix.)
        let mut chw = Array4::<f32>::zeros((1, 3, 112, 112));
        for y in 0..112 {
            for x in 0..112 {
                let i = (y * 112 + x) * 3;
                let r = rgb_112[i] as f32;
                let g = rgb_112[i + 1] as f32;
                let b = rgb_112[i + 2] as f32;
                chw[[0, 0, y, x]] = r / 127.5 - 1.0;
                chw[[0, 1, y, x]] = g / 127.5 - 1.0;
                chw[[0, 2, y, x]] = b / 127.5 - 1.0;
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
            .context("ArcFace session.run")
            .map_err(classify_inference_error)?;
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
