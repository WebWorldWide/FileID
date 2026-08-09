// YuNet face detector (OpenCV Zoo `face_detection_yunet_2023mar`, MIT). Replaces
// the non-commercial SCRFD (InsightFace) for a commercial-clean detection path.
// Produces the SAME `scrfd::Detection` (corner bbox + 5 landmarks in FileID
// order [left_eye, right_eye, nose, mouth_left, mouth_right] + score) so the
// downstream face pipeline (estimate_pose, validate_face_geometry, alignment)
// is unchanged.
//
// The HF ONNX emits RAW per-stride anchor tensors (cls/obj/bbox/kps at strides
// 8/16/32, one anchor per cell, fixed 640x640 input). Decode + NMS are done on
// the CPU after the GPU forward pass, mirroring OpenCV's FaceDetectorYN:
//   score = sqrt(cls * obj); center/exp box; landmark = (cell + delta)*stride.
// Input is BGR, raw [0,255] float, no normalization (matches OpenCV's blob).
// A wrong-variant ONNX → scores under threshold → empty Vec (no panic), the
// same fail-soft posture as scrfd.rs.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ndarray::Array4;
use ort::session::{Session, SessionInputValue, SessionOutputs};
use ort::value::Tensor;

use super::runtime::{
    classify_inference_error, configure_session_builder, execution_providers_for_chain,
    priority_chain, RuntimeProbe,
};
use super::scrfd::{nms, resize_nearest, Detection};

const INPUT: u32 = 640;
const STRIDES: [u32; 3] = [8, 16, 32];
const SCORE_THRESHOLD: f32 = 0.6;
const NMS_IOU: f32 = 0.3;

pub struct YuNet {
    session: Session,
    /// The ONNX's single input tensor name, read once at load and reused on
    /// every forward instead of re-walking `session.inputs.first()`.
    input_name: String,
}

impl YuNet {
    pub fn load<P: AsRef<Path>>(weights: P) -> Result<Self> {
        let path = weights.as_ref();
        if !path.exists() {
            anyhow::bail!("YuNet weights missing at {}", path.display());
        }
        let probe = RuntimeProbe::shared();
        let chain = priority_chain(probe.vendor);
        let builder = Session::builder().context("ORT session builder")?;
        let mut builder =
            configure_session_builder(builder).context("configure session (YuNet)")?;
        let chain_labels: Vec<&'static str> = chain.iter().map(|e| e.as_str()).collect();
        let providers = execution_providers_for_chain(&chain, probe.adapter_index);
        if !providers.is_empty() {
            builder = builder
                .with_execution_providers(providers)
                .context("register execution providers (YuNet)")?;
        }
        tracing::info!(model = "YuNet", chain = ?chain_labels, "EP priority chain registered");
        let session = builder
            .commit_from_file(path)
            .context("ORT session commit (YuNet)")?;
        let input_name = session
            .inputs
            .first()
            .ok_or_else(|| anyhow::anyhow!("YuNet ONNX has no inputs"))?
            .name
            .clone();

        // Validate the output-name contract at LOAD. detect() matches outputs by
        // literal name (cls_/obj_/bbox_/kps_ × strides 8/16/32); a mismatch makes
        // every stride hit the skip arm and detect() return an empty Vec for the
        // ENTIRE library — silent "zero faces". The shipped opencv
        // face_detection_yunet_2023mar export emits exactly these 12 names, so
        // assert them here: a future renamed re-export then fails LOUDLY at load
        // (→ scan aborts with a reinstall message, scan.rs) instead of silently
        // producing no faces. (YuNet's cls/obj are both 1-channel, so SCRFD-style
        // shape-classification cannot disambiguate them — a name check is the
        // robust guard here.)
        {
            let have: std::collections::HashSet<&str> =
                session.outputs.iter().map(|o| o.name.as_str()).collect();
            let missing: Vec<String> = STRIDES
                .iter()
                .flat_map(|s| {
                    ["cls", "obj", "bbox", "kps"]
                        .iter()
                        .map(move |p| format!("{p}_{s}"))
                })
                .filter(|key| !have.contains(key.as_str()))
                .collect();
            if !missing.is_empty() {
                let got: Vec<&str> = session.outputs.iter().map(|o| o.name.as_str()).collect();
                anyhow::bail!(
                    "YuNet ONNX output names don't match the decode contract \
                     (missing {missing:?}; got {got:?}). The model is likely the \
                     wrong variant or a renamed re-export — reinstall faces from \
                     Settings → Local AI."
                );
            }
        }

        let mut model = Self { session, input_name };
        let warmup_started = std::time::Instant::now();
        let _ = model.detect(&vec![0u8; 3 * (INPUT as usize) * (INPUT as usize)], INPUT, INPUT)?;
        tracing::info!(
            model = "YuNet",
            warmup_ms = warmup_started.elapsed().as_millis() as u64,
            "warmup complete"
        );
        Ok(model)
    }

    /// Detect faces in an RGB8 image. Returns bboxes/landmarks in the source
    /// image's pixel coordinates (letterbox remap applied).
    pub fn detect(&mut self, rgb: &[u8], width: u32, height: u32) -> Result<Vec<Detection>> {
        if rgb.len() != (width as usize) * (height as usize) * 3 {
            anyhow::bail!("YuNet detect: rgb buffer/size mismatch");
        }
        // Letterbox to a fixed 640x640 (top-left, zero-pad remainder) — the
        // export's only input shape, and a fixed shape keeps DirectML happy.
        let scale = (INPUT as f32 / width as f32).min(INPUT as f32 / height as f32);
        let new_w = ((width as f32 * scale) as u32).max(1);
        let new_h = ((height as f32 * scale) as u32).max(1);
        let resized = resize_nearest(rgb, width, height, new_w, new_h);
        let n = INPUT as usize;
        let mut chw = Array4::<f32>::zeros((1, 3, n, n));
        for y in 0..new_h as usize {
            for x in 0..new_w as usize {
                let i = (y * new_w as usize + x) * 3;
                // YuNet expects BGR, raw [0,255] float (no normalization).
                chw[[0, 0, y, x]] = f32::from(resized[i + 2]); // B
                chw[[0, 1, y, x]] = f32::from(resized[i + 1]); // G
                chw[[0, 2, y, x]] = f32::from(resized[i]); // R
            }
        }

        let input = Tensor::from_array(chw).context("YuNet input tensor")?;
        let input_name = self.input_name.clone();
        let outputs: SessionOutputs = self
            .session
            .run(vec![(input_name, SessionInputValue::from(input))])
            .context("YuNet session.run")
            .map_err(classify_inference_error)?;

        // Outputs are named cls_{s}/obj_{s}/bbox_{s}/kps_{s}; collect by name.
        let named: Vec<(String, Vec<f32>)> = outputs
            .iter()
            .filter_map(|(name, val)| {
                val.try_extract_tensor::<f32>()
                    .ok()
                    .map(|(_, d)| (name.to_string(), d.to_vec()))
            })
            .collect();
        let find = |prefix: &str, s: u32| -> Option<&[f32]> {
            let key = format!("{prefix}_{s}");
            named.iter().find(|(n, _)| *n == key).map(|(_, d)| d.as_slice())
        };

        let mut candidates: Vec<Detection> = Vec::new();
        for &s in &STRIDES {
            match (find("cls", s), find("obj", s), find("bbox", s), find("kps", s)) {
                (Some(cls), Some(obj), Some(bbox), Some(kps)) => {
                    decode_stride(s, INPUT, cls, obj, bbox, kps, &mut candidates);
                }
                _ => {
                    tracing::warn!(model = "YuNet", stride = s, "missing cls/obj/bbox/kps output — skipping stride");
                }
            }
        }

        // Letterbox → source remap (top-left placement, no pad offset).
        if scale > 0.0 {
            for d in &mut candidates {
                for v in &mut d.bbox {
                    *v /= scale;
                }
                for lm in &mut d.landmarks {
                    lm[0] /= scale;
                    lm[1] /= scale;
                }
            }
        }
        let (iw, ih) = (width as f32, height as f32);
        for d in &mut candidates {
            d.bbox[0] = d.bbox[0].clamp(0.0, iw);
            d.bbox[1] = d.bbox[1].clamp(0.0, ih);
            d.bbox[2] = d.bbox[2].clamp(0.0, iw);
            d.bbox[3] = d.bbox[3].clamp(0.0, ih);
            // Do NOT clamp landmarks: the 5-point similarity transform in
            // align_112 needs their true positions to fit the crop correctly,
            // and align_112's bilinear sampler already edge-clamps pixel reads.
            // Clamping a slightly-out-of-frame eye/mouth point skews the
            // transform and warps the aligned face (#8). validate_face_geometry
            // already tolerates landmarks up to 10% outside the bbox, and
            // DetectedFace.landmarks is metadata-only.
        }

        Ok(nms(candidates, NMS_IOU))
    }
}

/// Decode one stride's cls/obj/bbox/kps tensors into Detections. Pure function
/// for unit-testing the decode math without an ORT session.
fn decode_stride(
    stride: u32,
    input: u32,
    cls: &[f32],
    obj: &[f32],
    bbox: &[f32],
    kps: &[f32],
    out: &mut Vec<Detection>,
) {
    let grid = (input / stride) as usize; // square (640/stride)
    let cells = grid * grid;
    if cls.len() < cells || obj.len() < cells || bbox.len() < cells * 4 || kps.len() < cells * 10 {
        tracing::warn!(stride, "YuNet stride tensor undersized — skipping");
        return;
    }
    let s = stride as f32;
    for r in 0..grid {
        for c in 0..grid {
            let i = r * grid + c;
            let score = (cls[i].clamp(0.0, 1.0) * obj[i].clamp(0.0, 1.0)).sqrt();
            if score < SCORE_THRESHOLD {
                continue;
            }
            let (cf, rf) = (c as f32, r as f32);
            let cx = (cf + bbox[i * 4]) * s;
            let cy = (rf + bbox[i * 4 + 1]) * s;
            let w = bbox[i * 4 + 2].exp() * s;
            let h = bbox[i * 4 + 3].exp() * s;
            // Native landmark order: [right_eye, left_eye, nose, right_mouth, left_mouth].
            let mut native = [[0.0_f32; 2]; 5];
            for (k, lm) in native.iter_mut().enumerate() {
                lm[0] = (cf + kps[i * 10 + 2 * k]) * s;
                lm[1] = (rf + kps[i * 10 + 2 * k + 1]) * s;
            }
            // Remap to FileID order [left_eye, right_eye, nose, mouth_left, mouth_right].
            let landmarks = [native[1], native[0], native[2], native[4], native[3]];
            out.push(Detection {
                bbox: [cx - w * 0.5, cy - h * 0.5, cx + w * 0.5, cy + h * 0.5],
                landmarks,
                score,
            });
        }
    }
}

/// Per-EP variant-aware weights path: `yunet/face_detection_yunet_2023mar.onnx`.
pub fn default_weights_path() -> Result<PathBuf> {
    Ok(super::variants::resolve_model_path(
        &crate::paths::models_dir()?.join("yunet"),
        "face_detection_yunet_2023mar",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_scores_emit_nothing() {
        let cells = (INPUT as usize / 8) * (INPUT as usize / 8);
        let cls = vec![0.0_f32; cells];
        let obj = vec![0.0_f32; cells];
        let bbox = vec![0.0_f32; cells * 4];
        let kps = vec![0.0_f32; cells * 10];
        let mut out = Vec::new();
        decode_stride(8, INPUT, &cls, &obj, &bbox, &kps, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn high_score_cell_decodes_with_remapped_landmarks() {
        let grid = INPUT as usize / 32;
        let cells = grid * grid;
        let mut cls = vec![0.0_f32; cells];
        let mut obj = vec![0.0_f32; cells];
        let bbox = vec![0.0_f32; cells * 4];
        let mut kps = vec![0.0_f32; cells * 10];
        let i = 0; // cell (0,0)
        cls[i] = 1.0;
        obj[i] = 1.0;
        // distinct landmark deltas so the remap is observable
        for k in 0..5 {
            kps[i * 10 + 2 * k] = k as f32 * 0.1;
            kps[i * 10 + 2 * k + 1] = k as f32 * 0.2;
        }
        let mut out = Vec::new();
        decode_stride(32, INPUT, &cls, &obj, &bbox, &kps, &mut out);
        assert_eq!(out.len(), 1);
        // FileID[0]=left_eye should equal native[1] (kps index 1).
        let s = 32.0_f32;
        assert!((out[0].landmarks[0][0] - (0.0 + 0.1) * s).abs() < 1e-3);
        assert!((out[0].score - 1.0).abs() < 1e-4);
    }
}
