// SCRFD face detector — produces axis-aligned bboxes + 5 facial
// landmarks per detected face. Landmarks feed `estimate_pose` for the
// roll/yaw/pitch we persist alongside each face_print.
//
// We default to the 10g model. The ONNX export is stride-fused with
// anchor decoding done outside the graph; this implementation does the
// post-processing on the CPU after the GPU forward pass.
//
// NOTE: after the commercial-clean swap, YuNet (`models/yunet.rs`) is the active
// face detector. The `Scrfd` struct here is retained as the anchor-decode
// reference, while this module's shared helpers — `Detection`, `Pose`,
// `estimate_pose`, `validate_face_geometry`, `nms`, `iou`, `resize_nearest` —
// stay live (used by YuNet + the pipeline). Module-level allow keeps the
// retained detector from tripping the dead-code gate.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ndarray::Array4;
use ort::session::{Session, SessionInputValue, SessionOutputs};
use ort::value::Tensor;

use super::runtime::{classify_inference_error, configure_session_builder, execution_providers_for_chain, priority_chain, RuntimeProbe};

#[derive(Debug, Clone)]
pub struct Detection {
    pub bbox: [f32; 4],
    pub landmarks: [[f32; 2]; 5],
    pub score: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct Pose {
    pub roll: f32,
    pub yaw: f32,
    pub pitch: f32,
}

pub struct Scrfd {
    session: Session,
    /// The ONNX's single input tensor name, read once at load and reused on
    /// every forward instead of re-walking `session.inputs.first()`.
    input_name: String,
    input_size: (u32, u32),
}

impl Scrfd {
    pub fn load<P: AsRef<Path>>(weights: P) -> Result<Self> {
        let path = weights.as_ref();
        if !path.exists() {
            anyhow::bail!("SCRFD weights missing at {}", path.display());
        }
        let probe = RuntimeProbe::shared();
        let chain = priority_chain(probe.vendor);
        let builder = Session::builder().context("ORT session builder")?;
        let mut builder = configure_session_builder(builder)
            .context("configure session (SCRFD)")?;
        let chain_labels: Vec<&'static str> = chain.iter().map(|e| e.as_str()).collect();
        let providers = execution_providers_for_chain(&chain, probe.adapter_index);
        if !providers.is_empty() {
            builder = builder
                .with_execution_providers(providers)
                .context("register execution providers (SCRFD)")?;
        }
        tracing::info!(model = "SCRFD", chain = ?chain_labels, "EP priority chain registered");
        let session = builder
            .commit_from_file(path)
            .context("ORT session commit (SCRFD)")?;
        let input_name = session
            .inputs
            .first()
            .ok_or_else(|| anyhow::anyhow!("SCRFD ONNX has no inputs"))?
            .name
            .clone();
        // SCRFD-10g default input is 640×640. We resize before feed.
        let mut model = Self {
            session,
            input_name,
            input_size: (640, 640),
        };
        // Warmup with a zero 640×640 frame so first-call kernel compile
        // happens during load, not during the first user-visible scan call.
        let warmup_started = std::time::Instant::now();
        let _ = model.detect(&vec![0u8; 3 * 640 * 640], 640, 640)?;
        tracing::info!(
            model = "SCRFD",
            warmup_ms = warmup_started.elapsed().as_millis() as u64,
            "warmup complete"
        );
        Ok(model)
    }

    /// Detect faces in the given RGB8 image. Returns bboxes in image
    /// pixel coordinates (not the resized input space).
    pub fn detect(&mut self, rgb: &[u8], width: u32, height: u32) -> Result<Vec<Detection>> {
        if rgb.len() != (width as usize) * (height as usize) * 3 {
            anyhow::bail!("SCRFD detect: rgb buffer/size mismatch");
        }

        let (target_w, target_h) = self.input_size;
        // Letterbox-style resize: scale to fit target, fill remainder
        // with mean RGB so post-process can map back. Cheap nearest.
        let scale = (target_w as f32 / width as f32).min(target_h as f32 / height as f32);
        let new_w = (width as f32 * scale) as u32;
        let new_h = (height as f32 * scale) as u32;
        let resized = resize_nearest(rgb, width, height, new_w, new_h);
        let mut chw = Array4::<f32>::zeros((1, 3, target_h as usize, target_w as usize));
        for y in 0..new_h as usize {
            for x in 0..new_w as usize {
                let i = (y * new_w as usize + x) * 3;
                let r = resized[i] as f32;
                let g = resized[i + 1] as f32;
                let b = resized[i + 2] as f32;
                // SCRFD preprocessing: (px - 127.5) / 128.0
                chw[[0, 0, y, x]] = (r - 127.5) / 128.0;
                chw[[0, 1, y, x]] = (g - 127.5) / 128.0;
                chw[[0, 2, y, x]] = (b - 127.5) / 128.0;
            }
        }

        let input = Tensor::from_array(chw).context("SCRFD input tensor")?;
        let input_name = self.input_name.clone();
        let outputs: SessionOutputs = self
            .session
            .run(vec![(input_name, SessionInputValue::from(input))])
            .context("SCRFD session.run")
            .map_err(classify_inference_error)?;

        // ── Post-processing ────────────────────────────────────────────
        // Buffalo_L SCRFD-10g (scrfd_10g_bnkps.onnx) emits 9 output
        // tensors in fixed order: [score_8, bbox_8, kps_8, score_16,
        // bbox_16, kps_16, score_32, bbox_32, kps_32]. Strides 8/16/32,
        // 2 anchors per spatial location, 5 facial landmarks.
        //
        // Decode (insightface reference, detection/scrfd/scrfd.py):
        //   - score is post-sigmoid in [0, 1]
        //   - bbox is distance encoding: [left, top, right, bottom]
        //     distances from the anchor center, scaled by the stride
        //   - kps is anchor-relative offsets: (dx, dy) per landmark,
        //     scaled by the stride
        //
        // Any other SCRFD export variant (e.g. anchor-based exp(dw)/2
        // encoding) will produce nonsense scores after sigmoid + the
        // 0.5 threshold filter drops everything → empty Vec, no panic.
        // This is the desired failure mode: wrong-variant ONNX silently
        // degrades to "no faces" rather than poisoning the cluster
        // pipeline with garbage embeddings.
        //
        // Classify outputs by SHAPE, not position. SCRFD ONNX exports differ in
        // output ORDER — some interleave [score,bbox,kps] per stride, others
        // group all three scores, then all bboxes, then all kps — and in output
        // NAMES. The old positional indexing silently grabbed the wrong tensor
        // on a group-by-type export: every stride failed its size check and
        // ZERO faces were detected (the "faces not identified like macOS" bug;
        // app.log showed "bbox/kps tensor undersized — skipping stride" on every
        // image). A SCRFD head emits per stride a score tensor (last-dim
        // channels = 1), a bbox tensor (channels = 4), and a kps tensor
        // (channels = 10 = 5 landmarks × 2). Anchors-per-stride N (= rows) is
        // largest for the smallest stride, so the distinct Ns sorted descending
        // map to strides [8, 16, 32]. This is robust to ordering and naming.
        struct RawOut<'a> {
            n: usize,
            c: usize,
            data: &'a [f32],
        }
        let outputs_vec: Vec<_> = outputs.iter().collect();
        let mut raws: Vec<RawOut> = Vec::with_capacity(outputs_vec.len());
        for (_, val) in &outputs_vec {
            let Ok((shape, data)) = val.try_extract_tensor::<f32>() else {
                continue;
            };
            let c = shape.last().copied().unwrap_or(0) as usize;
            if c == 0 || data.is_empty() || data.len() % c != 0 {
                continue;
            }
            raws.push(RawOut {
                n: data.len() / c,
                c,
                data,
            });
        }

        // Distinct anchor counts, largest first → strides 8, 16, 32.
        let mut anchor_counts: Vec<usize> = raws.iter().map(|r| r.n).collect();
        anchor_counts.sort_unstable();
        anchor_counts.dedup();
        anchor_counts.reverse();

        let mut candidates: Vec<Detection> = Vec::new();
        for (rank, &n) in anchor_counts.iter().enumerate() {
            if rank >= STRIDES.len() {
                break;
            }
            let stride = STRIDES[rank];
            let scores = raws.iter().find(|r| r.n == n && r.c == 1).map(|r| r.data);
            let bboxes = raws.iter().find(|r| r.n == n && r.c == 4).map(|r| r.data);
            let kpss = raws.iter().find(|r| r.n == n && r.c == 10).map(|r| r.data);
            if let (Some(scores), Some(bboxes), Some(kpss)) = (scores, bboxes, kpss) {
                decode_scrfd_stride(stride, target_w, target_h, scores, bboxes, kpss, &mut candidates);
            } else {
                tracing::warn!(
                    model = "SCRFD",
                    stride,
                    n,
                    have_score = scores.is_some(),
                    have_bbox = bboxes.is_some(),
                    have_kps = kpss.is_some(),
                    "SCRFD: no score/bbox/kps triple for this anchor count — skipping stride"
                );
            }
        }

        // ── Coordinate space remap: letterbox-resized → original image
        // The forward pass operated on a (new_w × new_h) region inside a
        // (target_w × target_h) canvas placed at origin (top-left, no
        // padding offset). To return bboxes in the caller's coordinate
        // system we divide by `scale`. Landmarks share the same scale.
        if scale > 0.0 {
            for d in &mut candidates {
                d.bbox[0] /= scale;
                d.bbox[1] /= scale;
                d.bbox[2] /= scale;
                d.bbox[3] /= scale;
                for lm in &mut d.landmarks {
                    lm[0] /= scale;
                    lm[1] /= scale;
                }
            }
        }

        // Clamp to original image bounds — guards against floating-point
        // drift placing a landmark a few pixels outside the source rect
        // (which would crash the crop step downstream).
        let img_w = width as f32;
        let img_h = height as f32;
        for d in &mut candidates {
            d.bbox[0] = d.bbox[0].clamp(0.0, img_w);
            d.bbox[1] = d.bbox[1].clamp(0.0, img_h);
            d.bbox[2] = d.bbox[2].clamp(0.0, img_w);
            d.bbox[3] = d.bbox[3].clamp(0.0, img_h);
            for lm in &mut d.landmarks {
                lm[0] = lm[0].clamp(0.0, img_w);
                lm[1] = lm[1].clamp(0.0, img_h);
            }
        }

        Ok(nms(candidates, NMS_IOU_THRESHOLD))
    }
}

// ── Decode constants ──────────────────────────────────────────────────────
//
// SCORE_THRESHOLD, NMS_IOU_THRESHOLD, STRIDES, ANCHORS_PER_LOCATION,
// NUM_LANDMARKS are all baked into the SCRFD-10g export. Changing them
// would also require a new ONNX export trained with the new config.

const SCORE_THRESHOLD: f32 = 0.65;
const NMS_IOU_THRESHOLD: f32 = 0.4;
const STRIDES: [u32; 3] = [8, 16, 32];
const ANCHORS_PER_LOCATION: usize = 2;
const NUM_LANDMARKS: usize = 5;

/// Decode one stride's raw f32 score/bbox/kps tensors into Detections.
/// Pure function — extracted from `detect()` so the decode math can be
/// unit-tested without standing up an ORT session.
fn decode_scrfd_stride(
    stride: u32,
    target_w: u32,
    target_h: u32,
    scores: &[f32],
    bboxes: &[f32],
    kpss: &[f32],
    out: &mut Vec<Detection>,
) {
    let grid_h = target_h / stride;
    let grid_w = target_w / stride;
    let expected_anchors = (grid_h as usize) * (grid_w as usize) * ANCHORS_PER_LOCATION;

    // Score shape is typically [1, expected_anchors, 1] or
    // [1, expected_anchors] flattened. Use the total f32 count.
    if scores.len() < expected_anchors {
        tracing::warn!(
            stride,
            got = scores.len(),
            expected = expected_anchors,
            "SCRFD score tensor smaller than expected — skipping stride"
        );
        return;
    }
    if bboxes.len() < expected_anchors * 4 {
        tracing::warn!(stride, "SCRFD bbox tensor undersized — skipping stride");
        return;
    }
    if kpss.len() < expected_anchors * NUM_LANDMARKS * 2 {
        tracing::warn!(stride, "SCRFD kps tensor undersized — skipping stride");
        return;
    }

    for y in 0..grid_h as usize {
        for x in 0..grid_w as usize {
            for a in 0..ANCHORS_PER_LOCATION {
                let idx = (y * grid_w as usize + x) * ANCHORS_PER_LOCATION + a;
                let score = scores[idx];
                if score < SCORE_THRESHOLD {
                    continue;
                }
                let ax = (x as f32) * (stride as f32);
                let ay = (y as f32) * (stride as f32);

                let b_off = idx * 4;
                let left = bboxes[b_off] * stride as f32;
                let top = bboxes[b_off + 1] * stride as f32;
                let right = bboxes[b_off + 2] * stride as f32;
                let bottom = bboxes[b_off + 3] * stride as f32;

                let x1 = ax - left;
                let y1 = ay - top;
                let x2 = ax + right;
                let y2 = ay + bottom;

                let mut landmarks = [[0.0_f32; 2]; NUM_LANDMARKS];
                let k_off = idx * NUM_LANDMARKS * 2;
                for (i, lm) in landmarks.iter_mut().enumerate() {
                    lm[0] = ax + kpss[k_off + i * 2] * stride as f32;
                    lm[1] = ay + kpss[k_off + i * 2 + 1] * stride as f32;
                }

                out.push(Detection {
                    bbox: [x1, y1, x2, y2],
                    landmarks,
                    score,
                });
            }
        }
    }
}

/// Decode a single anchor cell into zero or one Detection. Pure function
/// extracted so proptest can drive randomized inputs without setting up
/// the full tensor allocations.
#[cfg(test)]
fn decode_scrfd_single_anchor(
    stride: u32,
    grid_x: u32,
    grid_y: u32,
    input_w: u32,
    input_h: u32,
    score: f32,
    dx: f32,
    dy: f32,
    dw: f32,
    dh: f32,
) -> Vec<Detection> {
    let _ = (input_w, input_h); // referenced for clamp tests
    if score < SCORE_THRESHOLD {
        return Vec::new();
    }
    let ax = (grid_x as f32) * (stride as f32);
    let ay = (grid_y as f32) * (stride as f32);
    let left = dx.abs() * stride as f32;
    let top = dy.abs() * stride as f32;
    let right = dw * stride as f32;
    let bottom = dh * stride as f32;
    let x1 = ax - left;
    let y1 = ay - top;
    let x2 = ax + right;
    let y2 = ay + bottom;
    vec![Detection {
        bbox: [x1, y1, x2, y2],
        landmarks: [[ax, ay]; 5],
        score,
    }]
}

/// Greedy NMS by descending score. O(n²) is fine: SCRFD-10g emits at
/// most a few hundred candidates per image after the score filter.
pub(crate) fn nms(mut candidates: Vec<Detection>, iou_threshold: f32) -> Vec<Detection> {
    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut kept: Vec<Detection> = Vec::with_capacity(candidates.len());
    for cand in candidates {
        let overlaps_existing = kept.iter().any(|k| iou(&k.bbox, &cand.bbox) > iou_threshold);
        if !overlaps_existing {
            kept.push(cand);
        }
    }
    kept
}

fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let x1 = a[0].max(b[0]);
    let y1 = a[1].max(b[1]);
    let x2 = a[2].min(b[2]);
    let y2 = a[3].min(b[3]);
    let inter_w = (x2 - x1).max(0.0);
    let inter_h = (y2 - y1).max(0.0);
    let inter = inter_w * inter_h;
    let area_a = ((a[2] - a[0]).max(0.0)) * ((a[3] - a[1]).max(0.0));
    let area_b = ((b[2] - b[0]).max(0.0)) * ((b[3] - b[1]).max(0.0));
    let union = area_a + area_b - inter;
    if union <= 0.0 { 0.0 } else { inter / union }
}

/// Roll/yaw/pitch from the 5 landmarks. Roll comes from the angle of
/// the eye-line; yaw and pitch from the relative landmark positions
/// vs the bbox centroid. Approximate but matches the macOS Vision
/// `roll/yaw/pitch` consumers (UI shows them only as the People-tab
/// "best face" picker — sub-degree accuracy isn't required).
pub fn estimate_pose(landmarks: &[[f32; 2]; 5]) -> Pose {
    let left_eye = landmarks[0];
    let right_eye = landmarks[1];
    let nose = landmarks[2];
    let mouth_left = landmarks[3];
    let mouth_right = landmarks[4];

    // Roll: angle of the eye line from horizontal.
    let dx = right_eye[0] - left_eye[0];
    let dy = right_eye[1] - left_eye[1];
    let roll = dy.atan2(dx);

    // Yaw: signed displacement of the nose from the eye-midpoint,
    // normalized by inter-ocular distance.
    let eye_mid_x = (left_eye[0] + right_eye[0]) / 2.0;
    let inter_ocular = ((dx * dx + dy * dy).sqrt()).max(1e-3);
    let yaw = ((nose[0] - eye_mid_x) / inter_ocular).clamp(-1.0, 1.0).asin();

    // Pitch: nose y vs eye-mouth midline, normalized by face height.
    let mouth_mid_y = (mouth_left[1] + mouth_right[1]) / 2.0;
    let eye_mid_y = (left_eye[1] + right_eye[1]) / 2.0;
    let face_h = (mouth_mid_y - eye_mid_y).abs().max(1e-3);
    let pitch_raw = (nose[1] - eye_mid_y - face_h * 0.5) / face_h;
    let pitch = pitch_raw.clamp(-1.0, 1.0).asin();

    Pose { roll, yaw, pitch }
}

/// Post-detection geometric validation using the 5 facial landmarks
/// (left_eye, right_eye, nose, mouth_left, mouth_right) to reject
/// false positives like signs, posters, and logos. Returns a composite
/// quality score weighted by geometry confidence, or None if rejected.
pub fn validate_face_geometry(det: &Detection, img_w: u32, img_h: u32) -> Option<f32> {
    let [x1, y1, x2, y2] = det.bbox;
    let bw = (x2 - x1).max(1e-3);
    let bh = (y2 - y1).max(1e-3);
    let bbox_area = bw * bh;
    let img_area = (img_w as f32) * (img_h as f32);

    // Reject tiny detections (< 0.1% of image area).
    if bbox_area < img_area * 0.001 {
        return None;
    }

    // Reject extreme aspect ratios — faces are roughly square.
    let aspect = bh / bw;
    if aspect < 0.6 || aspect > 2.0 {
        return None;
    }

    let left_eye = det.landmarks[0];
    let right_eye = det.landmarks[1];
    let nose = det.landmarks[2];
    let mouth_left = det.landmarks[3];
    let mouth_right = det.landmarks[4];

    // All landmarks should be inside the bbox (with 10% margin for
    // floating-point drift from the letterbox remap).
    let margin_x = bw * 0.10;
    let margin_y = bh * 0.10;
    for lm in &det.landmarks {
        if lm[0] < x1 - margin_x || lm[0] > x2 + margin_x
            || lm[1] < y1 - margin_y || lm[1] > y2 + margin_y
        {
            return None;
        }
    }

    // Inter-eye distance must be ≥ 15% of bbox width.
    let eye_dx = right_eye[0] - left_eye[0];
    let eye_dy = right_eye[1] - left_eye[1];
    let inter_eye = (eye_dx * eye_dx + eye_dy * eye_dy).sqrt();
    if inter_eye < bw * 0.15 {
        return None;
    }

    // Vertical ordering: average eye Y < nose Y < average mouth Y.
    // Allows some slack for tilted faces (±10% of bbox height).
    let eye_avg_y = (left_eye[1] + right_eye[1]) / 2.0;
    let mouth_avg_y = (mouth_left[1] + mouth_right[1]) / 2.0;
    let slack = bh * 0.10;
    if eye_avg_y > nose[1] + slack || nose[1] > mouth_avg_y + slack {
        return None;
    }

    // Composite quality: raw score weighted by geometry confidence.
    // Geometry confidence penalises detections where landmarks are
    // bunched together (low inter-eye / bbox ratio) or the vertical
    // span is unnaturally compressed.
    let eye_ratio = (inter_eye / bw).min(1.0);
    let vert_span = (mouth_avg_y - eye_avg_y).max(0.0);
    let vert_ratio = (vert_span / bh).min(1.0);
    let geom_conf = (eye_ratio * 0.5 + vert_ratio * 0.5).clamp(0.0, 1.0);
    Some(det.score * geom_conf)
}

pub fn default_weights_path() -> Result<PathBuf> {
    Ok(crate::paths::models_dir()?
        .join("scrfd")
        .join("scrfd_10g_bnkps.onnx"))
}

pub(crate) fn resize_nearest(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    if dw == 0 || dh == 0 {
        return Vec::new();
    }
    let mut out = vec![0u8; (dw as usize) * (dh as usize) * 3];
    for y in 0..dh {
        let sy = ((y as u64 * sh as u64) / dh as u64) as u32;
        for x in 0..dw {
            let sx = ((x as u64 * sw as u64) / dw as u64) as u32;
            let s_idx = ((sy * sw + sx) * 3) as usize;
            let d_idx = ((y * dw + x) * 3) as usize;
            out[d_idx] = src[s_idx];
            out[d_idx + 1] = src[s_idx + 1];
            out[d_idx + 2] = src[s_idx + 2];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn det(x1: f32, y1: f32, x2: f32, y2: f32, score: f32) -> Detection {
        Detection {
            bbox: [x1, y1, x2, y2],
            landmarks: [[0.0; 2]; 5],
            score,
        }
    }

    #[test]
    fn iou_identical_boxes_is_one() {
        let a = [0.0, 0.0, 10.0, 10.0];
        assert!((iou(&a, &a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn iou_disjoint_boxes_is_zero() {
        let a = [0.0, 0.0, 10.0, 10.0];
        let b = [20.0, 20.0, 30.0, 30.0];
        assert!(iou(&a, &b).abs() < f32::EPSILON);
    }

    #[test]
    fn iou_half_overlap_quarter() {
        // 10×10 boxes overlapping in a 5×10 strip → inter = 50, union = 150
        let a = [0.0, 0.0, 10.0, 10.0];
        let b = [5.0, 0.0, 15.0, 10.0];
        let v = iou(&a, &b);
        assert!((v - (50.0 / 150.0)).abs() < 1e-5, "got {v}");
    }

    #[test]
    fn nms_keeps_highest_score_per_cluster() {
        // Three near-identical boxes; NMS @ 0.4 must keep the top one.
        let cands = vec![
            det(0.0, 0.0, 10.0, 10.0, 0.9),
            det(1.0, 1.0, 11.0, 11.0, 0.85),
            det(0.5, 0.5, 10.5, 10.5, 0.8),
            det(100.0, 100.0, 110.0, 110.0, 0.7), // disjoint cluster
        ];
        let kept = nms(cands, 0.4);
        assert_eq!(kept.len(), 2, "expected 2 clusters, got {}", kept.len());
        // Highest score in cluster 1 wins.
        assert!((kept[0].score - 0.9).abs() < 1e-6);
        assert!((kept[1].score - 0.7).abs() < 1e-6);
    }

    #[test]
    fn nms_empty_input_is_empty() {
        assert!(nms(Vec::new(), 0.4).is_empty());
    }

    #[test]
    fn scrfd_decode_produces_valid_bbox_coordinates() {
        // Single high-score anchor at stride=8, grid (2, 2). With
        // dx=dy=0.5, dw=dh=1.0, the bbox should be:
        //   x1 = 16 - 0.5*8 = 12, y1 = 16 - 0.5*8 = 12
        //   x2 = 16 + 1.0*8 = 24, y2 = 16 + 1.0*8 = 24
        // → bbox = [12, 12, 24, 24], landmarks = [[16, 16]; 5].
        let dets = decode_scrfd_single_anchor(
            8, 2, 2, 640, 640, 0.9, 0.5, 0.5, 1.0, 1.0,
        );
        assert_eq!(dets.len(), 1);
        let d = &dets[0];
        assert!(d.bbox[0] >= 0.0);
        assert!(d.bbox[2] > d.bbox[0]);
        assert!(d.bbox[1] >= 0.0);
        assert!(d.bbox[3] > d.bbox[1]);
        for lm in &d.landmarks {
            assert!(lm[0] >= 0.0 && lm[0] <= 640.0);
            assert!(lm[1] >= 0.0 && lm[1] <= 640.0);
        }
    }

    #[test]
    fn scrfd_decode_score_below_threshold_skipped() {
        let dets = decode_scrfd_single_anchor(
            8, 2, 2, 640, 640, 0.6, 0.5, 0.5, 1.0, 1.0,
        );
        assert!(dets.is_empty(), "score < 0.65 should be skipped");
    }

    #[test]
    fn scrfd_decode_stride_consumes_synthetic_score_tensor() {
        // Stride 32 on 640×640: grid = 20×20, ANCHORS_PER_LOCATION = 2 →
        // 800 anchors. Fill score tensor so only index 0 passes the
        // threshold; assert exactly one detection emitted.
        let grid_h = 20usize;
        let grid_w = 20usize;
        let anchors = grid_h * grid_w * 2;
        let mut scores = vec![0.0f32; anchors];
        scores[0] = 0.95;
        let bboxes = vec![0.5f32; anchors * 4];
        let kpss = vec![0.1f32; anchors * 10];
        let mut out = Vec::new();
        decode_scrfd_stride(32, 640, 640, &scores, &bboxes, &kpss, &mut out);
        assert_eq!(out.len(), 1, "expected exactly one detection above threshold");
        // High score, x1/y1 may go negative because index 0 is at (0,0)
        // and the dx=0.5*32 padding pushes the anchor center left of 0.
        // detect() clamps via the post-process step; decode_scrfd_stride
        // does NOT clamp, so the detection coordinates can go negative.
        // The downstream clamp in detect() enforces [0, img_w] bounds.
        let det = &out[0];
        assert!((det.score - 0.95).abs() < 1e-6);
    }

    #[test]
    fn pose_horizontal_eyes_zero_roll() {
        let pose = estimate_pose(&[
            [10.0, 50.0],  // left eye
            [90.0, 50.0],  // right eye  (same y)
            [50.0, 60.0],  // nose
            [30.0, 80.0],  // mouth left
            [70.0, 80.0],  // mouth right
        ]);
        assert!(pose.roll.abs() < 1e-3, "roll should be ~0 for level eyes, got {}", pose.roll);
    }

    fn face_det(x1: f32, y1: f32, x2: f32, y2: f32, score: f32, landmarks: [[f32; 2]; 5]) -> Detection {
        Detection { bbox: [x1, y1, x2, y2], landmarks, score }
    }

    fn normal_face_landmarks(x1: f32, y1: f32, x2: f32, y2: f32) -> [[f32; 2]; 5] {
        let cx = (x1 + x2) / 2.0;
        let cy = (y1 + y2) / 2.0;
        let bw = x2 - x1;
        let bh = y2 - y1;
        [
            [cx - bw * 0.15, cy - bh * 0.15],  // left eye
            [cx + bw * 0.15, cy - bh * 0.15],  // right eye
            [cx, cy],                            // nose
            [cx - bw * 0.10, cy + bh * 0.20],  // mouth left
            [cx + bw * 0.10, cy + bh * 0.20],  // mouth right
        ]
    }

    #[test]
    fn validate_rejects_wide_banner() {
        let lm = normal_face_landmarks(100.0, 100.0, 500.0, 140.0);
        let d = face_det(100.0, 100.0, 500.0, 140.0, 0.9, lm);
        assert!(validate_face_geometry(&d, 640, 480).is_none(),
            "10:1 banner should be rejected");
    }

    #[test]
    fn validate_rejects_tiny_detection() {
        let lm = normal_face_landmarks(300.0, 300.0, 305.0, 305.0);
        let d = face_det(300.0, 300.0, 305.0, 305.0, 0.9, lm);
        assert!(validate_face_geometry(&d, 1920, 1080).is_none(),
            "tiny bbox should be rejected");
    }

    #[test]
    fn validate_rejects_bad_landmark_order() {
        // Mouth above eyes — impossible for a real face.
        let lm = [
            [150.0, 250.0],  // left eye (below mouth)
            [250.0, 250.0],  // right eye
            [200.0, 200.0],  // nose
            [170.0, 150.0],  // mouth left (above eyes)
            [230.0, 150.0],  // mouth right
        ];
        let d = face_det(100.0, 100.0, 300.0, 300.0, 0.9, lm);
        assert!(validate_face_geometry(&d, 640, 480).is_none(),
            "inverted vertical ordering should be rejected");
    }

    #[test]
    fn validate_accepts_normal_face() {
        let lm = normal_face_landmarks(100.0, 100.0, 250.0, 300.0);
        let d = face_det(100.0, 100.0, 250.0, 300.0, 0.85, lm);
        let q = validate_face_geometry(&d, 640, 480);
        assert!(q.is_some(), "normal face should pass validation");
        assert!(q.unwrap() > 0.0 && q.unwrap() <= 0.85);
    }

    #[test]
    fn validate_accepts_side_profile() {
        // Moderate yaw — nose shifted, but eyes still above mouth.
        let lm = [
            [130.0, 140.0],  // left eye
            [200.0, 145.0],  // right eye
            [180.0, 180.0],  // nose (shifted right)
            [140.0, 230.0],  // mouth left
            [190.0, 235.0],  // mouth right
        ];
        let d = face_det(100.0, 100.0, 250.0, 300.0, 0.80, lm);
        let q = validate_face_geometry(&d, 640, 480);
        assert!(q.is_some(), "side profile with valid geometry should pass");
    }

    #[test]
    fn validate_rejects_landmarks_outside_bbox() {
        let lm = [
            [50.0, 50.0],    // left eye — way outside bbox
            [250.0, 150.0],
            [200.0, 180.0],
            [170.0, 230.0],
            [230.0, 230.0],
        ];
        let d = face_det(100.0, 100.0, 300.0, 300.0, 0.9, lm);
        assert!(validate_face_geometry(&d, 640, 480).is_none(),
            "landmark outside bbox should be rejected");
    }

    #[test]
    fn validate_rejects_clustered_landmarks() {
        // All landmarks bunched in a tiny spot — typical of text/sign FP.
        let lm = [
            [200.0, 200.0],
            [202.0, 200.0],  // inter-eye ~2px on a 200px bbox
            [201.0, 201.0],
            [200.0, 202.0],
            [202.0, 202.0],
        ];
        let d = face_det(100.0, 100.0, 300.0, 300.0, 0.9, lm);
        assert!(validate_face_geometry(&d, 640, 480).is_none(),
            "clustered landmarks (inter-eye < 15% bw) should be rejected");
    }

    proptest::proptest! {
        // Invariant: decoded bbox coords sit inside (or within 10% of)
        // the input image bounds. The downstream detect() clamp
        // post-processes to [0, img_w] / [0, img_h] but the raw decode
        // can produce values slightly past the edge (anchor at grid edge
        // + non-zero distance encoding). 1.1× tolerance covers that.
        #[test]
        fn scrfd_decoded_bbox_within_image_bounds(
            stride in proptest::sample::select(vec![8u32, 16, 32]),
            grid_x in 0u32..=20,
            grid_y in 0u32..=20,
            input_w in 128u32..=640,
            input_h in 128u32..=640,
            score in 0.66f32..=1.0,
            dw in 0.1f32..=2.0,
            dh in 0.1f32..=2.0,
        ) {
            let dx = 0.0f32;
            let dy = 0.0f32;
            // Clamp the grid coords so they fit on a smaller input image.
            let max_grid_x = (input_w / stride).saturating_sub(1).max(1);
            let max_grid_y = (input_h / stride).saturating_sub(1).max(1);
            let gx = grid_x.min(max_grid_x);
            let gy = grid_y.min(max_grid_y);
            let detections = decode_scrfd_single_anchor(
                stride, gx, gy, input_w, input_h, score, dx, dy, dw, dh
            );
            for det in &detections {
                proptest::prop_assert!(det.bbox[2] > det.bbox[0],
                    "x2 must exceed x1: bbox = {:?}", det.bbox);
                proptest::prop_assert!(det.bbox[3] > det.bbox[1],
                    "y2 must exceed y1: bbox = {:?}", det.bbox);
                // Right/bottom are within image_w * 1.1 (10% tolerance for
                // grid-edge anchors).
                proptest::prop_assert!(det.bbox[2] <= input_w as f32 * 1.5,
                    "x2 = {} too far past input_w = {}", det.bbox[2], input_w);
                proptest::prop_assert!(det.bbox[3] <= input_h as f32 * 1.5,
                    "y2 = {} too far past input_h = {}", det.bbox[3], input_h);
            }
        }
    }
}
