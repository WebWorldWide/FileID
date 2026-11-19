// SFace face embedder (OpenCV Zoo `face_recognition_sface_2021dec`, Apache-2.0): an ALIGNED 112×112 RGB crop → 128-d
// L2-normalized embedding; the commercial-clean replacement for ArcFace.
// Input is RGB with RAW [0,255] floats: the ONNX bakes (x-127.5)/128 in as its first ops, so do NOT pre-normalize.
// Output is not L2-normalized, so normalize here.

use crate::util::vector_math::l2_normalize;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ndarray::Array4;

use super::inference::{load_session, Dim, InferenceSession, InputSpec};

pub struct SFace {
    session: InferenceSession,
    input_name: String,
    model_version: String,
}

impl SFace {
    pub fn load<P: AsRef<Path>>(weights: P) -> Result<Self> {
        let path = weights.as_ref();
        if !path.exists() {
            anyhow::bail!("SFace weights missing at {}", path.display());
        }
        let before = std::fs::metadata(path)?;
        let size = before.len();
        anyhow::ensure!(size > 0 && size <= 128 * 1024 * 1024, "SFace weight size outside supported bounds");
        let digest = crate::util::content_hash::exact_file_sha256(path, size)?;
        let model_version = format!("sface128-sha256:{}", hex::encode(digest));
        let (session, input_name) = load_session("SFace", path, &[InputSpec::f32(&[Dim::Fixed(1), Dim::Fixed(3), Dim::Fixed(112), Dim::Fixed(112)])])?;
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

    pub fn embed_rgba(&mut self, rgba_112: &[u8]) -> Result<Vec<f32>> {
        if rgba_112.len() != 4 * 112 * 112 {
            anyhow::bail!("SFace embed_rgba expects 50176 RGBA8 bytes, got {}", rgba_112.len());
        }
        let rgb: Vec<u8> = rgba_112.as_chunks::<4>().0.iter().flat_map(|px| [px[0], px[1], px[2]]).collect();
        self.embed(&rgb)
    }

    /// Embeds an aligned 112×112 RGB8 face crop (37632 bytes); returns 128 floats, L2-normalized.
    pub fn embed(&mut self, rgb_112: &[u8]) -> Result<Vec<f32>> {
        if rgb_112.len() != 3 * 112 * 112 {
            anyhow::bail!("SFace embed expects 37632 RGB8 bytes, got {}", rgb_112.len());
        }
        let mut chw = Array4::<f32>::zeros((1, 3, 112, 112));
        for y in 0..112 {
            for x in 0..112 {
                let i = (y * 112 + x) * 3;
                chw[[0, 0, y, x]] = f32::from(rgb_112[i]);
                chw[[0, 1, y, x]] = f32::from(rgb_112[i + 1]);
                chw[[0, 2, y, x]] = f32::from(rgb_112[i + 2]);
            }
        }

        let outputs = self.session.run(vec![(self.input_name.as_str(), chw.into())]).context("SFace session.run")?;
        let mut emb: Vec<f32> = outputs.into_iter().next().ok_or_else(|| anyhow::anyhow!("SFace produced no outputs"))?.data;
        // Another output width would silently mis-cluster and diverge from the cross-platform face DB (128-d / 512-byte
        // blobs): fail rather than persist it.
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

pub fn default_weights_path() -> Result<PathBuf> {
    Ok(crate::paths::installed_model_file(
        &crate::paths::models_dir()?.join("sface").join("face_recognition_sface_2021dec.onnx"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l2_normalize_gives_unit_norm() {
        let mut v = vec![3.0_f32, 4.0];
        l2_normalize(&mut v);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "norm = {norm}");
        assert!((v[0] - 0.6).abs() < 1e-5 && (v[1] - 0.8).abs() < 1e-5);
    }

    #[test]
    fn l2_normalize_zero_vector_is_finite() {
        let mut v = vec![0.0_f32; 4];
        l2_normalize(&mut v);
        assert!(v.iter().all(|x| x.is_finite()));
    }
}

#[cfg(test)]
mod golden_tests {
    use super::*;
    use crate::models::golden::{check, model_file, synthetic_rgb, Probe, Tolerance};

    const PATH: &str = "sface/face_recognition_sface_2021dec.onnx";

    #[test]
    fn embedding_matches_the_golden_vector() {
        let Some(path) = model_file(PATH) else { return };
        let mut model = SFace::load(&path).unwrap();
        let embedding = model.embed(&synthetic_rgb(112, 112)).unwrap();
        check(
            "sface",
            PATH,
            "synthetic_rgb(112, 112) as an aligned RGB8 face crop; L2-normalized 128-d embedding",
            Tolerance { max_abs: 1e-4, rel_norm: 1e-4 },
            &[Probe { name: "embedding", shape: vec![128], values: &embedding }],
        );
    }
}
