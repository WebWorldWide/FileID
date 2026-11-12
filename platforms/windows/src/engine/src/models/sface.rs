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
    classify_inference_error, commit_chain_session, ensure_gpu_inference_alive,
};

pub struct SFace {
    session: Session,
    /// The ONNX's single input tensor name, read once at load and reused on
    /// every forward instead of re-walking `session.inputs.first()`.
    input_name: String,
    model_version: String,
}

impl SFace {
    pub fn load<P: AsRef<Path>>(weights: P) -> Result<Self> {
        Self::load_with(weights, Provider::Chain)
    }

    /// Load with an explicit provider: the shared EP chain, ORT's CPU EP alone,
    /// or (macOS) the CoreML EP configured as the Swift engine's
    /// `ArcFaceService` configures it.
    pub fn load_with<P: AsRef<Path>>(weights: P, provider: Provider) -> Result<Self> {
        let path = weights.as_ref();
        if !path.exists() {
            anyhow::bail!("SFace weights missing at {}", path.display());
        }
        let before = std::fs::metadata(path)?;
        let size = before.len();
        anyhow::ensure!(size > 0 && size <= 128 * 1024 * 1024, "SFace weight size outside supported bounds");
        let digest = crate::util::content_hash::exact_file_sha256(path, size)?;
        let model_version = format!("sface128-sha256:{}", hex::encode(digest));
        let (session, input_name) = match provider {
            Provider::Chain => commit_chain_session("SFace", path)?,
            Provider::Cpu => plain_session(path, None, None)?,
            Provider::CoreMl { cache_dir, intra_threads } => plain_session(path, Some(cache_dir), Some(intra_threads))?,
        };
        let after = std::fs::metadata(path)?;
        anyhow::ensure!(before.len() == after.len() && before.modified()? == after.modified()?, "SFace weights changed during load");

        let mut model = Self { session, input_name, model_version };
        let warmup_started = std::time::Instant::now();
        let _ = model.embed(&[0u8; 3 * 112 * 112])?;
        tracing::info!(
            model = "SFace",
            warmup_ms = warmup_started.elapsed().as_millis() as u64,
            "warmup complete"
        );
        Ok(model)
    }

    pub fn model_version(&self) -> &str {
        &self.model_version
    }

    /// Embed a 112×112 RGBA8 face crop (alpha ignored), the buffer the macOS
    /// pipeline aligns into; same contract as [`Self::embed`].
    pub fn embed_rgba(&mut self, rgba_112: &[u8]) -> Result<Vec<f32>> {
        if rgba_112.len() != 4 * 112 * 112 {
            anyhow::bail!("SFace embed_rgba expects 50176 RGBA8 bytes, got {}", rgba_112.len());
        }
        let rgb: Vec<u8> = rgba_112.as_chunks::<4>().0.iter().flat_map(|px| [px[0], px[1], px[2]]).collect();
        self.embed(&rgb)
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
        let input_name = self.input_name.clone();
        ensure_gpu_inference_alive()?;
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
        // l2_normalize's max(1e-8) floor passes NaN through; never persist one.
        anyhow::ensure!(emb.iter().all(|v| v.is_finite()), "SFace produced a non-finite embedding");
        Ok(emb)
    }
}

/// Where the SFace session runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Provider {
    /// The process's execution-provider priority chain.
    Chain,
    /// ORT's CPU execution provider with default session options.
    Cpu,
    /// The CoreML execution provider as an ML program, also on subgraphs, with
    /// all compute units and its compiled model cached in `cache_dir`; ORT's
    /// CPU provider takes what CoreML declines, on `intra_threads` threads.
    CoreMl { cache_dir: PathBuf, intra_threads: usize },
}

/// A session with ORT's default options, as the Swift engine builds its
/// `ORTSession`s, plus the CoreML EP when a cache folder is given.
fn plain_session(path: &Path, coreml_cache: Option<PathBuf>, intra_threads: Option<usize>) -> Result<(Session, String)> {
    crate::ort_runtime::commit_environment().context("ORT environment")?;
    let mut builder = Session::builder().context("ORT session builder")?;
    if let Some(threads) = intra_threads {
        builder = builder.with_intra_threads(threads.max(1))?;
    }
    if let Some(cache) = coreml_cache {
        use ort::execution_providers::coreml::{CoreMLComputeUnits, CoreMLExecutionProvider, CoreMLModelFormat};
        std::fs::create_dir_all(&cache).with_context(|| format!("CoreML cache folder {}", cache.display()))?;
        let provider = CoreMLExecutionProvider::default()
            .with_model_format(CoreMLModelFormat::MLProgram)
            .with_subgraphs(true)
            .with_compute_units(CoreMLComputeUnits::All)
            .with_model_cache_dir(cache.display())
            .build()
            .error_on_failure();
        builder = builder.with_execution_providers([provider]).context("register the CoreML execution provider (SFace)")?;
    }
    let session = builder.commit_from_file(path).context("ORT session commit (SFace)")?;
    let input_name = session.inputs.first().ok_or_else(|| anyhow::anyhow!("SFace ONNX has no inputs"))?.name.clone();
    Ok((session, input_name))
}

/// ONNX Runtime's release as its build string names it (`rel-1.30.0` → `1.30.0`).
pub fn runtime_release() -> String {
    let info = ort::info();
    info.split("git-branch=rel-")
        .nth(1)
        .and_then(|rest| rest.split([',', ' ']).next())
        .filter(|release| !release.is_empty())
        .unwrap_or("unknown")
        .to_string()
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
    Ok(crate::paths::installed_model_file(&super::variants::resolve_model_path(
        &crate::paths::models_dir()?.join("sface"),
        "face_recognition_sface_2021dec",
    )))
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
