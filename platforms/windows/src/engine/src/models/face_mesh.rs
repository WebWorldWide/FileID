// MediaPipe Face Mesh V2 (Google, Apache-2.0), the 478-point face landmark
// network from the Face Landmarker bundle, as a format-only ONNX conversion
// pinned in shared/models/manifest.json. Used for eye openness in Best Takes:
// YuNet's five landmarks cannot tell an open eye from a closed one.
//
// Input `input_12`: [1, 256, 256, 3] RGB in [0, 1], a square crop around one
// face with a 25% margin on each side, rolled so the eyes are level.
// Outputs: `Identity` 478 × (x, y, z) in crop pixels, `Identity_1` the face
// presence logit.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ndarray::Array4;
use ort::session::{Session, SessionInputValue, SessionOutputs};
use ort::value::Tensor;

use super::face_geometry::Detection;
use super::runtime::{classify_inference_error, commit_chain_session, ensure_gpu_inference_alive};

const INPUT: usize = 256;
const POINTS: usize = 478;
const LANDMARKS: &str = "Identity";
const PRESENCE: &str = "Identity_1";
const MARGIN: f32 = 1.5;
const MIN_PRESENCE_LOGIT: f32 = 0.0;
const RIGHT_EYE: [usize; 6] = [33, 160, 158, 133, 153, 144];
const LEFT_EYE: [usize; 6] = [362, 385, 387, 263, 373, 380];
pub const CLOSED_RATIO: f64 = 0.14;
pub const OPEN_RATIO: f64 = 0.24;

#[derive(Debug, Clone, Copy, PartialEq)]
struct Crop {
    cx: f32,
    cy: f32,
    side: f32,
    cos: f32,
    sin: f32,
}

impl Crop {
    fn project(&self, u: f32, v: f32) -> [f32; 2] {
        let (lx, ly) = ((u / INPUT as f32 - 0.5) * self.side, (v / INPUT as f32 - 0.5) * self.side);
        [self.cx + lx * self.cos - ly * self.sin, self.cy + lx * self.sin + ly * self.cos]
    }
}

pub struct FaceMesh {
    session: Session,
    input_name: String,
}

impl FaceMesh {
    pub fn load<P: AsRef<Path>>(weights: P) -> Result<Self> {
        let path = weights.as_ref();
        anyhow::ensure!(path.exists(), "Face mesh weights missing at {}", path.display());
        let (session, input_name) = commit_chain_session("FaceMesh", path)?;
        let names: Vec<&str> = session.outputs.iter().map(|o| o.name.as_str()).collect();
        anyhow::ensure!(
            names.contains(&LANDMARKS) && names.contains(&PRESENCE),
            "Face mesh ONNX outputs {names:?} don't match the landmark contract"
        );
        let mut model = Self { session, input_name };
        let crop = Crop { cx: 128.0, cy: 128.0, side: 256.0, cos: 1.0, sin: 0.0 };
        model.run(&[0u8; INPUT * INPUT * 3], INPUT as u32, INPUT as u32, &crop)?;
        Ok(model)
    }

    /// Eye openness 0…1 for one detected face in `rgb`, or `None` when the
    /// network says the crop holds no face.
    pub fn eyes_open(&mut self, rgb: &[u8], width: u32, height: u32, face: &Detection) -> Result<Option<f64>> {
        Ok(self.eye_ratio(rgb, width, height, face)?.map(openness))
    }

    pub fn eye_ratio(&mut self, rgb: &[u8], width: u32, height: u32, face: &Detection) -> Result<Option<f64>> {
        anyhow::ensure!(rgb.len() == width as usize * height as usize * 3, "Face mesh: rgb buffer/size mismatch");
        let Some(first) = self.run(rgb, width, height, &crop_from_detection(face))? else { return Ok(None) };
        let points = match crop_from_points(&first) {
            Some(refined) => self.run(rgb, width, height, &refined)?.unwrap_or(first),
            None => first,
        };
        Ok(eye_aspect_ratio(&points))
    }

    fn run(&mut self, rgb: &[u8], width: u32, height: u32, crop: &Crop) -> Result<Option<Vec<[f32; 2]>>> {
        let input = Tensor::from_array(sample(rgb, width as usize, height as usize, crop)).context("Face mesh input tensor")?;
        let input_name = self.input_name.clone();
        ensure_gpu_inference_alive()?;
        let outputs: SessionOutputs = self
            .session
            .run(vec![(input_name, SessionInputValue::from(input))])
            .context("Face mesh session.run")
            .map_err(classify_inference_error)?;
        let (_, presence) = outputs[PRESENCE].try_extract_tensor::<f32>().context("Face mesh presence")?;
        let (_, landmarks) = outputs[LANDMARKS].try_extract_tensor::<f32>().context("Face mesh landmarks")?;
        anyhow::ensure!(landmarks.len() == POINTS * 3, "Face mesh produced {} values, expected {}", landmarks.len(), POINTS * 3);
        let logit = presence.first().copied().unwrap_or(f32::NAN);
        if logit.is_nan() || logit < MIN_PRESENCE_LOGIT || !landmarks.iter().all(|v| v.is_finite()) {
            return Ok(None);
        }
        Ok(Some(landmarks.as_chunks::<3>().0.iter().map(|p| crop.project(p[0], p[1])).collect()))
    }
}

fn sample(rgb: &[u8], width: usize, height: usize, crop: &Crop) -> Array4<f32> {
    let mut out = Array4::<f32>::zeros((1, INPUT, INPUT, 3));
    let pixel = |x: usize, y: usize, c: usize| f32::from(rgb[(y * width + x) * 3 + c]);
    for v in 0..INPUT {
        for u in 0..INPUT {
            let [x, y] = crop.project(u as f32 + 0.5, v as f32 + 0.5);
            let (x, y) = ((x - 0.5).clamp(0.0, (width - 1) as f32), (y - 0.5).clamp(0.0, (height - 1) as f32));
            let (x0, y0) = (x as usize, y as usize);
            let (x1, y1) = ((x0 + 1).min(width - 1), (y0 + 1).min(height - 1));
            let (fx, fy) = (x - x0 as f32, y - y0 as f32);
            for c in 0..3 {
                let top = pixel(x0, y0, c) * (1.0 - fx) + pixel(x1, y0, c) * fx;
                let bottom = pixel(x0, y1, c) * (1.0 - fx) + pixel(x1, y1, c) * fx;
                out[[0, v, u, c]] = (top * (1.0 - fy) + bottom * fy) / 255.0;
            }
        }
    }
    out
}

fn rolled(cx: f32, cy: f32, side: f32, from: [f32; 2], to: [f32; 2], below: [f32; 2]) -> Crop {
    let (dx, dy) = (to[0] - from[0], to[1] - from[1]);
    let flip = dx * (below[1] - from[1]) - dy * (below[0] - from[0]) < 0.0;
    let angle = dy.atan2(dx) + if flip { std::f32::consts::PI } else { 0.0 };
    Crop { cx, cy, side, cos: angle.cos(), sin: angle.sin() }
}

fn crop_from_detection(face: &Detection) -> Crop {
    let [x1, y1, x2, y2] = face.bbox;
    let side = (x2 - x1).max(y2 - y1).max(1.0) * MARGIN;
    let [a, b, nose, ..] = face.landmarks;
    let (left, right) = if a[0] <= b[0] { (a, b) } else { (b, a) };
    rolled((x1 + x2) * 0.5, (y1 + y2) * 0.5, side, left, right, nose)
}

fn crop_from_points(points: &[[f32; 2]]) -> Option<Crop> {
    if points.len() < POINTS {
        return None;
    }
    let crop = rolled(0.0, 0.0, 0.0, points[33], points[263], points[152]);
    let local = |p: &[f32; 2]| (p[0] * crop.cos + p[1] * crop.sin, -p[0] * crop.sin + p[1] * crop.cos);
    let (mut lo_x, mut lo_y, mut hi_x, mut hi_y) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for (x, y) in points[..468].iter().map(local) {
        (lo_x, lo_y, hi_x, hi_y) = (lo_x.min(x), lo_y.min(y), hi_x.max(x), hi_y.max(y));
    }
    let side = (hi_x - lo_x).max(hi_y - lo_y) * MARGIN;
    let (mx, my) = ((lo_x + hi_x) * 0.5, (lo_y + hi_y) * 0.5);
    (side.is_finite() && side >= 1.0).then_some(Crop { cx: mx * crop.cos - my * crop.sin, cy: mx * crop.sin + my * crop.cos, side, ..crop })
}

/// Mean eye aspect ratio over both eyes: eyelid gap over eye width.
pub fn eye_aspect_ratio(points: &[[f32; 2]]) -> Option<f64> {
    if points.len() < POINTS {
        return None;
    }
    let distance = |a: usize, b: usize| f64::from((points[a][0] - points[b][0]).hypot(points[a][1] - points[b][1]));
    let ratio = |eye: [usize; 6]| {
        let width = distance(eye[0], eye[3]);
        (width > 0.0).then(|| (distance(eye[1], eye[5]) + distance(eye[2], eye[4])) / (2.0 * width))
    };
    let ratios: Vec<f64> = [ratio(RIGHT_EYE), ratio(LEFT_EYE)].into_iter().flatten().collect();
    (!ratios.is_empty()).then(|| ratios.iter().sum::<f64>() / ratios.len() as f64)
}

pub fn openness(ratio: f64) -> f64 {
    ((ratio - CLOSED_RATIO) / (OPEN_RATIO - CLOSED_RATIO)).clamp(0.0, 1.0)
}

/// `face_mesh/face_landmarks.onnx` under the models folder.
pub fn default_weights_path() -> Result<PathBuf> {
    Ok(crate::paths::models_dir()?.join("face_mesh").join("face_landmarks.onnx"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eye_points(gap: f32) -> Vec<[f32; 2]> {
        let mut points = vec![[0.0_f32, 0.0]; POINTS];
        for (eye, x) in [(RIGHT_EYE, 10.0_f32), (LEFT_EYE, 60.0)] {
            points[eye[0]] = [x, 50.0];
            points[eye[3]] = [x + 30.0, 50.0];
            points[eye[1]] = [x + 10.0, 50.0 - gap / 2.0];
            points[eye[2]] = [x + 20.0, 50.0 - gap / 2.0];
            points[eye[5]] = [x + 10.0, 50.0 + gap / 2.0];
            points[eye[4]] = [x + 20.0, 50.0 + gap / 2.0];
        }
        points
    }

    #[test]
    fn eyelid_gap_over_width_scores_openness() {
        let open = eye_aspect_ratio(&eye_points(9.0)).unwrap();
        let closed = eye_aspect_ratio(&eye_points(1.5)).unwrap();
        assert!((open - 0.3).abs() < 1e-6, "{open}");
        assert!((closed - 0.05).abs() < 1e-6, "{closed}");
        assert!((openness(open) - 1.0).abs() < 1e-9);
        assert!(openness(closed).abs() < 1e-9);
        assert!((openness(0.175) - crate::pipeline::take_scoring::EYES_CLOSED_BELOW).abs() < 1e-9);
        assert!(eye_aspect_ratio(&eye_points(9.0)[..10]).is_none());
    }

    #[test]
    fn crops_level_the_eyes_and_keep_the_nose_below() {
        let face = Detection { bbox: [100.0, 100.0, 200.0, 220.0], landmarks: [[130.0, 140.0], [170.0, 140.0], [150.0, 170.0], [135.0, 195.0], [165.0, 195.0]], score: 0.9 };
        let crop = crop_from_detection(&face);
        assert!((crop.side - 180.0).abs() < 1e-3 && (crop.cos - 1.0).abs() < 1e-6 && crop.sin.abs() < 1e-6);
        let [x, y] = crop.project(128.0, 128.0);
        assert!((x - 150.0).abs() < 1e-3 && (y - 160.0).abs() < 1e-3);
        let upside_down = Detection { landmarks: [[170.0, 180.0], [130.0, 180.0], [150.0, 150.0], [0.0, 0.0], [0.0, 0.0]], ..face };
        let crop = crop_from_detection(&upside_down);
        assert!((crop.cos + 1.0).abs() < 1e-5, "{crop:?}");
        let [_, top] = crop.project(128.0, 0.0);
        assert!(top > 160.0, "the crop's top row lies below the centre when the face is upside down");
    }

    #[test]
    fn sampling_a_level_crop_reads_the_source_pixels() {
        let (w, h) = (64usize, 64usize);
        let rgb: Vec<u8> = (0..w * h).flat_map(|i| [(i % w * 4) as u8, (i / w * 4) as u8, 7]).collect();
        let crop = Crop { cx: 32.0, cy: 32.0, side: 64.0, cos: 1.0, sin: 0.0 };
        let tensor = sample(&rgb, w, h, &crop);
        assert!((tensor[[0, 128, 128, 0]] * 255.0 - 128.0).abs() < 2.5);
        assert!((tensor[[0, 0, 0, 2]] * 255.0 - 7.0).abs() < 1e-3);
    }

    #[test]
    fn shared_geometry_fixture_matches() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!("../../../../../../shared/test-corpus/face-mesh-geometry.json")).unwrap();
        let num = |v: &serde_json::Value| v.as_f64().unwrap() as f32;
        let pair = |v: &serde_json::Value| [num(&v[0]), num(&v[1])];
        let close = |a: f32, b: &serde_json::Value| (f64::from(a) - b.as_f64().unwrap()).abs() < 1e-3;
        let check_crop = |c: &Crop, e: &serde_json::Value| {
            assert!(close(c.cx, &e["cx"]) && close(c.cy, &e["cy"]) && close(c.side, &e["side"]), "{c:?} vs {e}");
            assert!(close(c.cos, &e["cos"]) && close(c.sin, &e["sin"]), "{c:?} vs {e}");
        };
        let (w, h) = (fixture["image"]["width"].as_u64().unwrap() as usize, fixture["image"]["height"].as_u64().unwrap() as usize);
        let rgb: Vec<u8> = (0..w * h)
            .flat_map(|i| {
                let (x, y) = (i % w, i / w);
                [((7 * x + 3 * y) % 256) as u8, ((x * x + 5 * y) % 256) as u8, ((x * y) % 256) as u8]
            })
            .collect();
        for case in fixture["detections"].as_array().unwrap() {
            let b = &case["bbox"];
            let face = Detection {
                bbox: [num(&b[0]), num(&b[1]), num(&b[2]), num(&b[3])],
                landmarks: [pair(&case["eyeA"]), pair(&case["eyeB"]), pair(&case["nose"]), [0.0, 0.0], [0.0, 0.0]],
                score: 1.0,
            };
            let crop = crop_from_detection(&face);
            check_crop(&crop, &case["crop"]);
            let p = &case["project"];
            let [x, y] = crop.project(num(&p["u"]), num(&p["v"]));
            assert!(close(x, &p["expect"][0]) && close(y, &p["expect"][1]));
            let tensor = sample(&rgb, w, h, &crop);
            for s in case["samples"].as_array().unwrap() {
                let (u, v) = (s["u"].as_u64().unwrap() as usize, s["v"].as_u64().unwrap() as usize);
                for c in 0..3 {
                    assert!(close(tensor[[0, v, u, c]], &s["rgb"][c]), "sample {s} channel {c}: {}", tensor[[0, v, u, c]]);
                }
            }
        }
        for mesh in fixture["meshes"].as_array().unwrap() {
            let points: Vec<[f32; 2]> = mesh["points"].as_array().unwrap().iter().map(pair).collect();
            check_crop(&crop_from_points(&points).unwrap(), &mesh["crop"]);
            let ratio = eye_aspect_ratio(&points).unwrap();
            assert!((ratio - mesh["ratio"].as_f64().unwrap()).abs() < 1e-4, "{ratio}");
            assert!((openness(ratio) - mesh["openness"].as_f64().unwrap()).abs() < 1e-3);
        }
        for case in fixture["openness"].as_array().unwrap() {
            assert!((openness(case["ratio"].as_f64().unwrap()) - case["expect"].as_f64().unwrap()).abs() < 1e-9);
        }
    }
}
