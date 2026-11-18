// YuNet face detector (OpenCV Zoo `face_detection_yunet_2023mar`, MIT) → `face_geometry::Detection` (corner bbox + 5
// landmarks in FileID order + score). The ONNX emits RAW per-stride anchor tensors (cls/obj/bbox/kps at strides
// 8/16/32, one anchor per cell, fixed 640x640 input); decode + NMS run on the CPU, mirroring OpenCV's FaceDetectorYN:
//   score = sqrt(cls * obj); center/exp box; landmark = (cell + delta)*stride.
// Input is BGR, raw [0,255] float, no normalization. A wrong-variant ONNX scores under threshold → empty Vec, no panic.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ndarray::Array4;

use super::inference::{load_session, Dim, InferenceSession, InputSpec};
use super::face_geometry::{nms, resize_nearest, Detection};

const INPUT: u32 = 640;
const STRIDES: [u32; 3] = [8, 16, 32];
const SCORE_THRESHOLD: f32 = 0.6;
const NMS_IOU: f32 = 0.3;

pub struct YuNet {
    session: InferenceSession,
    input_name: String,
}

impl YuNet {
    pub fn load<P: AsRef<Path>>(weights: P) -> Result<Self> {
        let path = weights.as_ref();
        if !path.exists() {
            anyhow::bail!("YuNet weights missing at {}", path.display());
        }
        let (session, input_name) = load_session(
            "YuNet",
            path,
            &[InputSpec::f32(&[Dim::Fixed(1), Dim::Fixed(3), Dim::Fixed(INPUT as usize), Dim::Fixed(INPUT as usize)])],
        )?;

        // detect() matches outputs by literal name, so a renamed re-export would silently return zero faces for the
        // whole library; assert the 12 shipped names at load (cls/obj are both 1-channel, so shape alone can't tell
        // them apart).
        {
            let have: std::collections::HashSet<&str> =
                session.output_names().iter().map(String::as_str).collect();
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
                let got: Vec<&str> = session.output_names().iter().map(String::as_str).collect();
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

    pub fn detect(&mut self, rgb: &[u8], width: u32, height: u32) -> Result<Vec<Detection>> {
        let Some(input) = Self::prepare(rgb, width, height)? else {
            return Ok(Vec::new());
        };
        let outputs = self.forward(input.chw)?;
        Ok(decode_outputs(&outputs, input.scale, width, height))
    }

    /// Holds the session lock only for the forward pass; letterbox, tensor fill, decode and NMS run on the caller's
    /// thread.
    pub fn detect_shared(pool: &[parking_lot::Mutex<Self>], preferred: usize, rgb: &[u8], width: u32, height: u32) -> Result<Vec<Detection>> {
        let Some(input) = Self::prepare(rgb, width, height)? else {
            return Ok(Vec::new());
        };
        let outputs = super::checkout(pool, preferred).forward(input.chw)?;
        Ok(decode_outputs(&outputs, input.scale, width, height))
    }

    fn prepare(rgb: &[u8], width: u32, height: u32) -> Result<Option<PreparedInput>> {
        if rgb.len() != (width as usize) * (height as usize) * 3 {
            anyhow::bail!("YuNet detect: rgb buffer/size mismatch");
        }
        if width == 0 || height == 0 {
            return Ok(None);
        }
        // Fixed 640x640 (top-left, zero-pad): the export's only input shape, and a fixed shape lets tract compile once.
        let scale = (INPUT as f32 / width as f32).min(INPUT as f32 / height as f32);
        let new_w = ((width as f32 * scale) as u32).max(1);
        let new_h = ((height as f32 * scale) as u32).max(1);
        let resized = resize_nearest(rgb, width, height, new_w, new_h);
        let n = INPUT as usize;
        let mut chw = Array4::<f32>::zeros((1, 3, n, n));
        for y in 0..new_h as usize {
            for x in 0..new_w as usize {
                let i = (y * new_w as usize + x) * 3;
                // YuNet expects BGR, raw [0,255] float.
                chw[[0, 0, y, x]] = f32::from(resized[i + 2]);
                chw[[0, 1, y, x]] = f32::from(resized[i + 1]);
                chw[[0, 2, y, x]] = f32::from(resized[i]);
            }
        }
        Ok(Some(PreparedInput { chw, scale }))
    }

    fn forward(&mut self, chw: Array4<f32>) -> Result<Vec<(String, Vec<f32>)>> {
        let input_name = self.input_name.clone();
        let outputs = self.session.run(vec![(input_name.as_str(), chw.into())]).context("YuNet session.run")?;

        let named: Vec<(String, Vec<f32>)> = outputs.into_iter().map(|output| (output.name, output.data)).collect();
        Ok(named)
    }
}

struct PreparedInput {
    chw: Array4<f32>,
    scale: f32,
}

/// Pure, so it runs outside the session lock.
fn decode_outputs(named: &[(String, Vec<f32>)], scale: f32, width: u32, height: u32) -> Vec<Detection> {
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
        // Do not clamp landmarks: clamping an out-of-frame point skews the similarity transform; align_112 edge-clamps
        // its samples and validate_face_geometry tolerates landmarks up to 10% outside the bbox.
    }

    nms(candidates, NMS_IOU)
}

fn decode_stride(
    stride: u32,
    input: u32,
    cls: &[f32],
    obj: &[f32],
    bbox: &[f32],
    kps: &[f32],
    out: &mut Vec<Detection>,
) {
    let grid = (input / stride) as usize;
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
            // NaN (fp16 overflow, a broken export) fails `<` too; reject it explicitly.
            if score.is_nan() || score < SCORE_THRESHOLD {
                continue;
            }
            let (cf, rf) = (c as f32, r as f32);
            let cx = (cf + bbox[i * 4]) * s;
            let cy = (rf + bbox[i * 4 + 1]) * s;
            let w = bbox[i * 4 + 2].exp() * s;
            let h = bbox[i * 4 + 3].exp() * s;
            let mut native = [[0.0_f32; 2]; 5];
            for (k, lm) in native.iter_mut().enumerate() {
                lm[0] = (cf + kps[i * 10 + 2 * k]) * s;
                lm[1] = (rf + kps[i * 10 + 2 * k + 1]) * s;
            }
            let landmarks = [native[1], native[0], native[2], native[4], native[3]];
            if ![cx, cy, w, h].iter().chain(native.iter().flatten()).all(|v| v.is_finite()) {
                continue;
            }
            out.push(Detection {
                bbox: [cx - w * 0.5, cy - h * 0.5, cx + w * 0.5, cy + h * 0.5],
                landmarks,
                score,
            });
        }
    }
}

pub fn default_weights_path() -> Result<PathBuf> {
    Ok(crate::paths::models_dir()?.join("yunet").join("face_detection_yunet_2023mar.onnx"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preparation_letterboxes_to_bgr_and_rejects_bad_buffers() {
        assert!(YuNet::prepare(&[0u8; 5], 2, 1).is_err());
        assert!(YuNet::prepare(&[], 0, 0).unwrap().is_none());
        let input = YuNet::prepare(&[255, 0, 0, 0, 0, 255], 2, 1).unwrap().unwrap();
        assert!((input.scale - 320.0).abs() < 1e-3);
        assert_eq!(input.chw.dim(), (1, 3, 640, 640));
        let at = |channel: usize, y: usize, x: usize| input.chw[[0, channel, y, x]] as u8;
        assert_eq!((at(0, 0, 0), at(2, 0, 0)), (0, 255), "red lands in the R plane (BGR order)");
        assert_eq!((at(0, 0, 639), at(2, 0, 639)), (255, 0));
        assert_eq!(at(0, 400, 100), 0, "below the image is zero padding");
    }

    #[test]
    fn outputs_decode_to_source_pixels_and_clamp_to_the_frame() {
        let outputs: Vec<(String, Vec<f32>)> = STRIDES
            .iter()
            .flat_map(|&s| {
                let cells = ((INPUT / s) * (INPUT / s)) as usize;
                let mut cls = vec![0.0_f32; cells];
                if s == 32 {
                    cls[0] = 1.0;
                }
                [
                    (format!("cls_{s}"), cls),
                    (format!("obj_{s}"), vec![1.0; cells]),
                    (format!("bbox_{s}"), vec![0.0; cells * 4]),
                    (format!("kps_{s}"), vec![0.0; cells * 10]),
                ]
            })
            .collect();
        let found = decode_outputs(&outputs, 0.5, 1280, 1280);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].bbox.map(|v| v.round() as i32), [0, 0, 32, 32]);
        assert!(decode_outputs(&[], 0.5, 1280, 1280).is_empty(), "missing outputs are skipped, not a panic");
    }

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
        let i = 0;
        cls[i] = 1.0;
        obj[i] = 1.0;
        for k in 0..5 {
            kps[i * 10 + 2 * k] = k as f32 * 0.1;
            kps[i * 10 + 2 * k + 1] = k as f32 * 0.2;
        }
        let mut out = Vec::new();
        decode_stride(32, INPUT, &cls, &obj, &bbox, &kps, &mut out);
        assert_eq!(out.len(), 1);
        let s = 32.0_f32;
        assert!((out[0].landmarks[0][0] - (0.0 + 0.1) * s).abs() < 1e-3);
        assert!((out[0].score - 1.0).abs() < 1e-4);
    }
}

#[cfg(test)]
mod golden_tests {
    use super::*;
    use crate::models::golden::{check, model_file, synthetic_rgb, Probe, Tolerance};

    const PATH: &str = "yunet/face_detection_yunet_2023mar.onnx";

    #[test]
    fn raw_outputs_match_the_golden_vectors() {
        let Some(path) = model_file(PATH) else { return };
        let mut model = YuNet::load(&path).unwrap();
        let input = YuNet::prepare(&synthetic_rgb(640, 640), 640, 640).unwrap().unwrap();
        let mut outputs = model.forward(input.chw).unwrap();
        outputs.sort_by(|a, b| a.0.cmp(&b.0));
        let probes: Vec<Probe> = outputs
            .iter()
            .map(|(name, values)| Probe { name, shape: vec![values.len()], values })
            .collect();
        check(
            "yunet",
            PATH,
            "synthetic_rgb(640, 640) letterboxed (identity) to BGR 1x3x640x640; the 12 raw cls/obj/bbox/kps outputs by name",
            Tolerance { max_abs: 1e-3, rel_norm: 1e-3 },
            &probes,
        );
    }
}
