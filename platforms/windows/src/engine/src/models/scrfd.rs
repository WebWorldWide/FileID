//! Shared face-geometry helpers for the face pipeline.
//!
//! After the commercial-clean swap, YuNet (`models/yunet.rs`) is the active
//! face detector. This module retains the detector-agnostic types + CPU
//! post-processing the rest of the pipeline shares: the `Detection` (corner
//! bbox + 5 landmarks) and `Pose` types, plus `estimate_pose`,
//! `validate_face_geometry`, `nms`, `iou`, and `resize_nearest`. YuNet and
//! `pipeline::tagging` consume these directly.

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

/// Greedy NMS by descending score. O(n²) is fine: the detector emits at
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

pub(crate) fn resize_nearest(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    if dw == 0 || dh == 0 {
        return Vec::new();
    }
    let sx_map: Vec<u32> = (0..dw)
        .map(|x| ((x as u64 * sw as u64) / dw as u64) as u32)
        .collect();
    let mut out = vec![0u8; (dw as usize) * (dh as usize) * 3];
    for y in 0..dh {
        let sy = ((y as u64 * sh as u64) / dh as u64) as u32;
        for x in 0..dw {
            let sx = sx_map[x as usize];
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
}
