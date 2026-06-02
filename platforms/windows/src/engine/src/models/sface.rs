// SFace face embedder (OpenCV Zoo `face_recognition_sface_2021dec`, Apache-2.0).
// Maps an ALIGNED 112×112 RGB face crop to a 128-d L2-normalized embedding.
// Replaces the non-commercial ArcFace (InsightFace) for a commercial-clean
// face-recognition path.
//
// Input contract (verified against OpenCV's cv2.FaceRecognizerSF): the network
// is fed the face crop in RGB channel order with RAW [0,255] float values — the
// ONNX bakes its own (x-127.5)/128 normalization as the first graph ops, so we
// do NOT pre-normalize (unlike ArcFace's (px/127.5 - 1)). The model output is
// NOT L2-normalized, so we normalize here for cosine comparison.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ndarray::Array4;
use ort::session::{Session, SessionInputValue, SessionOutputs};
use ort::value::Tensor;

use super::runtime::{
    classify_inference_error, configure_session_builder, execution_providers_for_chain,
    priority_chain, RuntimeProbe,
};

pub struct SFace {
    session: Session,
}

impl SFace {
    pub fn load<P: AsRef<Path>>(weights: P) -> Result<Self> {
        let path = weights.as_ref();
        if !path.exists() {
            anyhow::bail!("SFace weights missing at {}", path.display());
        }
        let probe = RuntimeProbe::detect();
        let chain = priority_chain(probe.vendor);
        let builder = Session::builder().context("ORT session builder")?;
        let mut builder =
            configure_session_builder(builder).context("configure session (SFace)")?;
        let chain_labels: Vec<&'static str> = chain.iter().map(|e| e.as_str()).collect();
        let providers = execution_providers_for_chain(&chain, probe.adapter_index);
        if !providers.is_empty() {
            builder = builder
                .with_execution_providers(providers)
                .context("register execution providers (SFace)")?;
        }
        tracing::info!(model = "SFace", chain = ?chain_labels, "EP priority chain registered");
        let session = builder
            .commit_from_file(path)
            .context("ORT session commit (SFace)")?;

        let mut model = Self { session };
        let warmup_started = std::time::Instant::now();
        let _ = model.embed(&[0u8; 3 * 112 * 112])?;
        tracing::info!(
            model = "SFace",
            warmup_ms = warmup_started.elapsed().as_millis() as u64,
            "warmup complete"
        );
        Ok(model)
    }

    /// Embed an aligned 112×112 RGB8 face crop (3 * 112 * 112 = 37632 bytes).
    /// Returns 128 floats, L2-normalized.
    pub fn embed(&mut self, rgb_112: &[u8]) -> Result<Vec<f32>> {
        if rgb_112.len() != 3 * 112 * 112 {
            anyhow::bail!("SFace embed expects 37632 RGB8 bytes, got {}", rgb_112.len());
        }
        // RGB channel order, RAW [0,255] floats — the ONNX normalizes internally.
        let mut chw = Array4::<f32>::zeros((1, 3, 112, 112));
        for y in 0..112 {
            for x in 0..112 {
                let i = (y * 112 + x) * 3;
                chw[[0, 0, y, x]] = f32::from(rgb_112[i]);
                chw[[0, 1, y, x]] = f32::from(rgb_112[i + 1]);
                chw[[0, 2, y, x]] = f32::from(rgb_112[i + 2]);
            }
        }

        let input = Tensor::from_array(chw).context("SFace input tensor")?;
        let input_name = self
            .session
            .inputs
            .first()
            .ok_or_else(|| anyhow::anyhow!("SFace ONNX has no inputs"))?
            .name
            .clone();
        let outputs: SessionOutputs = self
            .session
            .run(vec![(input_name, SessionInputValue::from(input))])
            .context("SFace session.run")
            .map_err(classify_inference_error)?;
        let (_, value) = outputs
            .iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("SFace produced no outputs"))?;
        let (_shape, data) = value
            .try_extract_tensor::<f32>()
            .context("extract SFace output as f32")?;
        let mut emb: Vec<f32> = data.to_vec();
        // SFace is a 128-d embedder; a wrong/quantized export with a different
        // output width would silently mis-cluster and (worse) diverge from the
        // cross-platform face DB, which is keyed on 128-d / 512-byte blobs. Fail
        // loudly rather than persist an off-dim vector. (ENG-69)
        if emb.len() != 128 {
            anyhow::bail!(
                "SFace produced a {}-d embedding, expected 128 (wrong or quantized model?)",
                emb.len()
            );
        }
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

/// Per-EP variant-aware weights path: `sface/face_recognition_sface_2021dec.onnx`
/// (or an `_int8`/`_qnn` sibling on accelerated EPs when present).
pub fn default_weights_path() -> Result<PathBuf> {
    Ok(super::variants::resolve_model_path(
        &crate::paths::models_dir()?.join("sface"),
        "face_recognition_sface_2021dec",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l2_normalize_gives_unit_norm() {
        let mut v = vec![3.0_f32, 4.0]; // norm 5 → 0.6, 0.8
        l2_normalize(&mut v);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "norm = {norm}");
        assert!((v[0] - 0.6).abs() < 1e-5 && (v[1] - 0.8).abs() < 1e-5);
    }

    #[test]
    fn l2_normalize_zero_vector_is_finite() {
        let mut v = vec![0.0_f32; 4];
        l2_normalize(&mut v); // guarded by max(1e-8) — must not produce NaN/Inf
        assert!(v.iter().all(|x| x.is_finite()));
    }
}
