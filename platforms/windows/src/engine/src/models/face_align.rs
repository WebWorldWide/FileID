// 5-point similarity-transform face alignment to the 112x112 ArcFace template (SFace); matches OpenCV
// FaceRecognizerSF::alignCrop so embeddings line up with cv2.

const OUT: usize = 112;

/// Reject the crop when more than this fraction of pixels sample outside the source: edge-clamp replication smears
/// corrupt the SFace embedding, and the caller falls back to the bbox crop. Env-overridable.
fn max_out_of_bounds_fraction() -> f32 {
    std::env::var("FILEID_FACE_ALIGN_MAX_OOB")
        .ok()
        .and_then(|v| v.trim().parse::<f32>().ok())
        .filter(|v| v.is_finite())
        .map(|v| v.clamp(0.0, 1.0))
        .unwrap_or(0.20)
}

/// FileID landmark order [left_eye, right_eye, nose, mouth_left, mouth_right]; OpenCV's native order is [re, le, nt,
/// rcm, lcm].
const TEMPLATE: [[f32; 2]; 5] = [
    [73.5318, 51.5014],
    [38.2946, 51.6963],
    [56.0252, 71.7366],
    [70.7299, 92.2041],
    [41.5493, 92.3655],
];

/// Returns 112*112*3 RGB8 bytes, or `None` if the fit is degenerate.
pub fn align_112(rgb: &[u8], width: u32, height: u32, landmarks: &[[f32; 2]; 5]) -> Option<Vec<u8>> {
    let expected = (width as usize).checked_mul(height as usize)?.checked_mul(3)?;
    if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 || rgb.len() != expected {
        return None;
    }
    let (a, b, tx, ty) = fit_similarity(landmarks, &TEMPLATE)?;
    let det = a * a + b * b;
    if !det.is_finite() || det.abs() < 1e-9 {
        return None;
    }
    let mut out = vec![0u8; OUT * OUT * 3];
    let mut oob = 0usize;
    for oy in 0..OUT {
        for ox in 0..OUT {
            let dx = ox as f32 - tx;
            let dy = oy as f32 - ty;
            let sx = (a * dx + b * dy) / det;
            let sy = (-b * dx + a * dy) / det;
            // A sample centred outside the image is edge-clamp replication, not face pixels.
            if sx < 0.0 || sy < 0.0 || sx >= width as f32 || sy >= height as f32 {
                oob += 1;
            }
            let px = bilinear(rgb, width, height, sx, sy);
            let o = (oy * OUT + ox) * 3;
            out[o] = px[0];
            out[o + 1] = px[1];
            out[o + 2] = px[2];
        }
    }
    if (oob as f32) / ((OUT * OUT) as f32) > max_out_of_bounds_fraction() {
        return None;
    }
    Some(out)
}

/// Alignment used on Mac: RGBA8 in/out, no off-frame rejection. Its output defines the
/// `macos-landmark-overlap-v3-orientation` embedding space (`face-alignment.json` pins it), so it must not change.
pub fn align_112_rgba(rgba: &[u8], width: u32, height: u32, landmarks: &[[f32; 2]; 5]) -> Option<Vec<u8>> {
    let expected = (width as usize).checked_mul(height as usize)?.checked_mul(4)?;
    if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 || rgba.len() != expected {
        return None;
    }
    let (a, b, tx, ty) = fit_similarity(landmarks, &TEMPLATE)?;
    let det = a * a + b * b;
    if !det.is_finite() || det.abs() < 1e-9 {
        return None;
    }
    let mut out = vec![255u8; OUT * OUT * 4];
    for oy in 0..OUT {
        for ox in 0..OUT {
            let dx = ox as f32 - tx;
            let dy = oy as f32 - ty;
            let sx = (a * dx + b * dy) / det;
            let sy = (-b * dx + a * dy) / det;
            let px = bilinear_strided(rgba, 4, width, height, sx, sy);
            let o = (oy * OUT + ox) * 4;
            out[o..o + 3].copy_from_slice(&px);
        }
    }
    Some(out)
}

/// Vision's landmarks on a tiny face are sometimes upside down or sideways (mouth above the eyes), and aligning on them
/// writes a flipped crop that then groups with other flipped crops. A set is usable only when its fit to the template
/// turns the face by at most 60 degrees and puts the eyes above the nose above the mouth.
pub fn landmarks_upright(points: &[[f32; 2]; 5]) -> bool {
    let Some((a, b, _, ty)) = fit_similarity(points, &TEMPLATE) else { return false };
    let angle = f64::from(b).atan2(f64::from(a)).abs();
    if angle > std::f64::consts::FRAC_PI_3 {
        return false;
    }
    let y = |p: &[f32; 2]| b * p[0] + a * p[1] + ty;
    let eyes = (y(&points[0]) + y(&points[1])) / 2.0;
    let mouth = (y(&points[3]) + y(&points[4])) / 2.0;
    let nose = y(&points[2]);
    eyes < nose && nose < mouth
}

/// Faces smaller than this are not turned: Vision's landmarks on a small face are sometimes upside down on an upright photo.
const MIN_TURNED_FACE_PIXELS: f32 = 64.0;

/// How many clockwise quarter turns bring a face that sits sideways or upside down in its photo (no orientation tag
/// says so) upright, read from the line running from the mouth to the eyes. `None` when the face is already near
/// upright, is small, or its five points do not form a plausible face.
pub fn quarter_turns_to_upright(points: &[[f32; 2]; 5], face_pixels: f32) -> Option<u8> {
    if face_pixels < MIN_TURNED_FACE_PIXELS || points.iter().flatten().any(|value| !value.is_finite()) {
        return None;
    }
    let middle = |a: [f32; 2], b: [f32; 2]| [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0];
    let (eyes, mouth, nose) = (middle(points[0], points[1]), middle(points[3], points[4]), points[2]);
    let up = [eyes[0] - mouth[0], eyes[1] - mouth[1]];
    let length = up[0].hypot(up[1]);
    let eye_gap = (points[0][0] - points[1][0]).hypot(points[0][1] - points[1][1]);
    if eye_gap <= 0.0 || !(0.7..=1.8).contains(&(length / eye_gap)) {
        return None;
    }
    let along = |from: [f32; 2], to: [f32; 2]| ((to[0] - from[0]) * up[0] + (to[1] - from[1]) * up[1]) / (length * length);
    let nose_between_mouth_and_eyes = (0.1..=0.9).contains(&along(mouth, nose));
    let eye_line_crosses_up = ((points[1][0] - points[0][0]) * up[0] + (points[1][1] - points[0][1]) * up[1]).abs() <= 0.5 * eye_gap * length;
    if !nose_between_mouth_and_eyes || !eye_line_crosses_up {
        return None;
    }
    let clockwise_from_up = f64::from(up[0]).atan2(f64::from(-up[1])).to_degrees();
    let quarters = (clockwise_from_up / 90.0).round();
    if (clockwise_from_up - quarters * 90.0).abs() > 30.0 {
        return None;
    }
    match (-quarters as i32).rem_euclid(4) {
        0 => None,
        turns => Some(turns as u8),
    }
}

/// A square RGBA image turned clockwise by `turns` quarter turns.
pub fn rotate_square_rgba(rgba: &[u8], side: usize, turns: u8) -> Vec<u8> {
    let mut out = vec![0u8; rgba.len()];
    for y in 0..side {
        for x in 0..side {
            let (to_x, to_y) = match turns % 4 {
                0 => (x, y),
                1 => (side - 1 - y, x),
                2 => (side - 1 - x, side - 1 - y),
                _ => (y, side - 1 - x),
            };
            let from = (y * side + x) * 4;
            let to = (to_y * side + to_x) * 4;
            out[to..to + 4].copy_from_slice(&rgba[from..from + 4]);
        }
    }
    out
}

pub fn fit_to_template(src: &[[f32; 2]; 5]) -> Option<(f32, f32, f32, f32)> {
    fit_similarity(src, &TEMPLATE)
}

/// Least-squares (a, b, tx, ty) via the 4×4 normal equations.
fn fit_similarity(src: &[[f32; 2]; 5], dst: &[[f32; 2]; 5]) -> Option<(f32, f32, f32, f32)> {
    if src.iter().chain(dst).flatten().any(|value| !value.is_finite()) { return None; }
    let n = src.len() as f64;
    let (mut sxx, mut sx, mut sy) = (0f64, 0f64, 0f64);
    let (mut ba, mut bb, mut bxx, mut byy) = (0f64, 0f64, 0f64, 0f64);
    for k in 0..src.len() {
        let (x, y) = (f64::from(src[k][0]), f64::from(src[k][1]));
        let (cx, cy) = (f64::from(dst[k][0]), f64::from(dst[k][1]));
        sxx += x * x + y * y;
        sx += x;
        sy += y;
        ba += x * cx + y * cy;
        bb += x * cy - y * cx;
        bxx += cx;
        byy += cy;
    }
    // [ sxx   0    sx   sy ] [a ]   [ ba  ]
    // [ 0    sxx  -sy   sx ] [b ] = [ bb  ]
    // [ sx   -sy   n    0  ] [tx]   [ bxx ]
    // [ sy    sx   0    n  ] [ty]   [ byy ]
    let m = [
        [sxx, 0.0, sx, sy],
        [0.0, sxx, -sy, sx],
        [sx, -sy, n, 0.0],
        [sy, sx, 0.0, n],
    ];
    let p = solve4(m, [ba, bb, bxx, byy])?;
    if p.iter().any(|value| !(*value as f32).is_finite()) { return None; }
    Some((p[0] as f32, p[1] as f32, p[2] as f32, p[3] as f32))
}

fn solve4(mut m: [[f64; 4]; 4], mut r: [f64; 4]) -> Option<[f64; 4]> {
    for col in 0..4 {
        let mut piv = col;
        for row in (col + 1)..4 {
            if m[row][col].abs() > m[piv][col].abs() {
                piv = row;
            }
        }
        if m[piv][col].abs() < 1e-12 {
            return None;
        }
        m.swap(col, piv);
        r.swap(col, piv);
        for row in (col + 1)..4 {
            let f = m[row][col] / m[col][col];
            for k in col..4 {
                m[row][k] -= f * m[col][k];
            }
            r[row] -= f * r[col];
        }
    }
    let mut x = [0f64; 4];
    for col in (0..4).rev() {
        let mut s = r[col];
        for k in (col + 1)..4 {
            s -= m[col][k] * x[k];
        }
        x[col] = s / m[col][col];
    }
    Some(x)
}

fn bilinear(rgb: &[u8], width: u32, height: u32, x: f32, y: f32) -> [u8; 3] {
    bilinear_strided(rgb, 3, width, height, x, y)
}

/// `stride` is 3 for RGB, 4 for RGBA (alpha ignored).
fn bilinear_strided(pixels: &[u8], stride: usize, width: u32, height: u32, x: f32, y: f32) -> [u8; 3] {
    let x = x.clamp(0.0, (width - 1) as f32);
    let y = y.clamp(0.0, (height - 1) as f32);
    let (w, h) = (width as i32, height as i32);
    let x0 = x.floor() as i32;
    let y0 = y.floor() as i32;
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;
    let sample = |xi: i32, yi: i32| -> [f32; 3] {
        let xc = xi.clamp(0, w - 1) as usize;
        let yc = yi.clamp(0, h - 1) as usize;
        let o = (yc * (width as usize) + xc) * stride;
        [f32::from(pixels[o]), f32::from(pixels[o + 1]), f32::from(pixels[o + 2])]
    };
    let p00 = sample(x0, y0);
    let p10 = sample(x0 + 1, y0);
    let p01 = sample(x0, y0 + 1);
    let p11 = sample(x0 + 1, y0 + 1);
    let mut out = [0u8; 3];
    for ch in 0..3 {
        let top = p00[ch] * (1.0 - fx) + p10[ch] * fx;
        let bot = p01[ch] * (1.0 - fx) + p11[ch] * fx;
        out[ch] = (top * (1.0 - fy) + bot * fy).round().clamp(0.0, 255.0) as u8;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placed(angle_degrees: f32, scale: f32) -> [[f32; 2]; 5] {
        let (sin, cos) = angle_degrees.to_radians().sin_cos();
        TEMPLATE.map(|[x, y]| [300.0 + scale * (cos * x - sin * y), 200.0 + scale * (sin * x + cos * y)])
    }

    #[test]
    fn a_sideways_or_upside_down_face_is_turned_upright_by_the_quarter_turns_that_fix_it() {
        assert_eq!(quarter_turns_to_upright(&placed(0.0, 1.0), 100.0), None);
        assert_eq!(quarter_turns_to_upright(&placed(35.0, 1.0), 100.0), None);
        assert_eq!(quarter_turns_to_upright(&placed(90.0, 1.0), 100.0), Some(3));
        assert_eq!(quarter_turns_to_upright(&placed(180.0, 1.0), 100.0), Some(2));
        assert_eq!(quarter_turns_to_upright(&placed(-90.0, 1.0), 100.0), Some(1));
        assert_eq!(quarter_turns_to_upright(&placed(75.0, 1.0), 100.0), Some(3));
    }

    #[test]
    fn a_small_or_implausible_face_is_never_turned() {
        assert_eq!(quarter_turns_to_upright(&placed(90.0, 1.0), 40.0), None);
        let flipped_by_vision = [[1038.777, 417.5533], [1018.9976, 418.2268], [1024.962, 403.8286], [1043.3632, 393.6493], [1022.975, 395.21118]];
        assert_eq!(quarter_turns_to_upright(&flipped_by_vision, 20.0), None);
        let mut mouth_beside_eyes = placed(0.0, 1.0);
        mouth_beside_eyes[3] = [mouth_beside_eyes[0][0] + 5.0, mouth_beside_eyes[0][1]];
        mouth_beside_eyes[4] = [mouth_beside_eyes[1][0] - 5.0, mouth_beside_eyes[1][1]];
        assert_eq!(quarter_turns_to_upright(&mouth_beside_eyes, 100.0), None);
        let mut with_nan = placed(90.0, 1.0);
        with_nan[2][0] = f32::NAN;
        assert_eq!(quarter_turns_to_upright(&with_nan, 100.0), None);
    }

    #[test]
    fn turning_a_square_clockwise_moves_its_top_left_pixel_to_the_top_right() {
        let pixel = [9, 8, 7, 255];
        let at = |image: &[u8], x: usize, y: usize| image[(y * 3 + x) * 4..(y * 3 + x) * 4 + 4].to_vec();
        let mut image = vec![0u8; 3 * 3 * 4];
        image[0..4].copy_from_slice(&pixel);
        assert_eq!(at(&rotate_square_rgba(&image, 3, 1), 2, 0), pixel);
        assert_eq!(at(&rotate_square_rgba(&image, 3, 2), 2, 2), pixel);
        assert_eq!(at(&rotate_square_rgba(&image, 3, 3), 0, 2), pixel);
        assert_eq!(rotate_square_rgba(&rotate_square_rgba(&image, 3, 1), 3, 3), image);
    }

    #[test]
    fn upside_down_or_sideways_landmarks_are_not_aligned_on() {
        assert!(landmarks_upright(&placed(0.0, 1.0)));
        assert!(landmarks_upright(&placed(35.0, 0.6)));
        assert!(landmarks_upright(&placed(-35.0, 0.6)));
        assert!(!landmarks_upright(&placed(90.0, 1.0)));
        assert!(!landmarks_upright(&placed(180.0, 1.0)));
        let flipped_by_vision = [[1038.777, 417.5533], [1018.9976, 418.2268], [1024.962, 403.8286], [1043.3632, 393.6493], [1022.975, 395.21118]];
        assert!(!landmarks_upright(&flipped_by_vision));
        let mirrored = TEMPLATE.map(|[x, y]| [x, 112.0 - y]);
        assert!(!landmarks_upright(&mirrored));
    }

    #[test]
    fn shared_alignment_fixtures() {
        #[derive(serde::Deserialize)]
        struct Fixture { template: [[f32; 2]; 5], cases: Vec<Case> }
        #[derive(serde::Deserialize)]
        struct Case { source: [[f32; 2]; 5], expected: Option<[f32; 4]> }
        let fixture: Fixture = serde_json::from_str(include_str!("../../../../../../shared/test-corpus/face-alignment.json")).unwrap();
        assert_eq!(fixture.template, TEMPLATE);
        for case in fixture.cases {
            let fit = fit_similarity(&case.source, &TEMPLATE);
            if let Some(expected) = case.expected {
                let (a,b,x,y) = fit.unwrap();
                for (actual, target) in [a,b,x,y].into_iter().zip(expected) { assert!((actual-target).abs() < 0.001); }
            } else { assert!(fit.is_none()); }
        }
    }

    fn pattern(width: u32, height: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                out.extend_from_slice(&[((x * 7 + y * 13) % 256) as u8, ((x * x + 3 * y) % 256) as u8, (((x ^ y) * 5) % 256) as u8, 255]);
            }
        }
        out
    }

    #[derive(serde::Deserialize)]
    struct Aligned {
        name: String,
        width: u32,
        height: u32,
        landmarks: [[f32; 2]; 5],
        #[serde(rename = "fitBits")]
        fit_bits: Option<[String; 4]>,
        sha256: Option<String>,
    }

    fn aligned_fixtures() -> Vec<Aligned> {
        #[derive(serde::Deserialize)]
        struct Fixture { aligned: Vec<Aligned> }
        let fixture: Fixture = serde_json::from_str(include_str!("../../../../../../shared/test-corpus/face-alignment.json")).unwrap();
        fixture.aligned
    }

    fn aligned_actual(case: &Aligned) -> (Option<[String; 4]>, Option<String>) {
        use sha2::Digest;
        let fit = fit_to_template(&case.landmarks).map(|(a, b, x, y)| [a, b, x, y].map(|v| format!("{:08x}", v.to_bits())));
        let crop = align_112_rgba(&pattern(case.width, case.height), case.width, case.height, &case.landmarks);
        (fit, crop.map(|bytes| hex::encode(sha2::Sha256::digest(&bytes))))
    }

    /// Bit for bit with the pinned fixture.
    #[test]
    fn shared_aligned_crops_match_bit_for_bit() {
        let cases = aligned_fixtures();
        assert!(cases.len() >= 4);
        for case in &cases {
            let (fit, digest) = aligned_actual(case);
            assert_eq!(fit, case.fit_bits, "{} fit", case.name);
            assert_eq!(digest, case.sha256, "{} crop", case.name);
        }
    }

    #[test]
    #[ignore = "prints fixture values"]
    fn print_aligned_fixture() {
        for case in aligned_fixtures() {
            println!("{} {:?}", case.name, aligned_actual(&case));
        }
    }

    #[test]
    fn rgba_alignment_keeps_alpha_and_needs_a_full_buffer() {
        let mut lm = TEMPLATE;
        for p in &mut lm {
            p[0] += 24.0;
            p[1] += 24.0;
        }
        let crop = align_112_rgba(&pattern(160, 160), 160, 160, &lm).unwrap();
        assert_eq!(crop.len(), 112 * 112 * 4);
        assert!(crop.chunks(4).all(|px| px[3] == 255));
        assert!(align_112_rgba(&pattern(160, 160)[4..], 160, 160, &lm).is_none());
        let mut far = TEMPLATE;
        for p in &mut far {
            p[0] += 90.0;
            p[1] += 90.0;
        }
        assert!(align_112_rgba(&pattern(60, 60), 60, 60, &far).is_some(), "the Mac pipeline keeps off-frame crops");
    }

    #[test]
    fn rejects_invalid_geometry_and_empty_images() {
        assert!(align_112(&[],0,0,&TEMPLATE).is_none());
        assert!(align_112(&[],u32::MAX,u32::MAX,&TEMPLATE).is_none());
        let mut invalid = TEMPLATE;
        invalid[0][0] = f32::NAN;
        assert!(fit_similarity(&invalid,&TEMPLATE).is_none());
        assert!(fit_similarity(&TEMPLATE,&invalid).is_none());
        assert!(align_112(&vec![0;112*112*3],112,112,&invalid).is_none());
    }

    #[test]
    fn identity_fit() {
        let (a, b, tx, ty) = fit_similarity(&TEMPLATE, &TEMPLATE).unwrap();
        assert!((a - 1.0).abs() < 1e-3, "a={a}");
        assert!(b.abs() < 1e-3, "b={b}");
        assert!(tx.abs() < 1e-2 && ty.abs() < 1e-2, "tx={tx} ty={ty}");
    }

    #[test]
    fn translation_recovered() {
        let mut shifted = TEMPLATE;
        for p in &mut shifted {
            p[0] += 10.0;
            p[1] -= 5.0;
        }
        let (a, b, tx, ty) = fit_similarity(&shifted, &TEMPLATE).unwrap();
        assert!((a - 1.0).abs() < 1e-3 && b.abs() < 1e-3);
        assert!((tx + 10.0).abs() < 1e-2 && (ty - 5.0).abs() < 1e-2, "tx={tx} ty={ty}");
    }

    #[test]
    fn in_frame_face_aligns() {
        let (w, h) = (160u32, 160u32);
        let rgb = vec![128u8; (w * h * 3) as usize];
        // TEMPLATE is in the 112×112 output frame; offset it inside the 160×160 source so no sample is out of bounds.
        let mut lm = TEMPLATE;
        for p in &mut lm {
            p[0] += 24.0;
            p[1] += 24.0;
        }
        assert!(align_112(&rgb, w, h, &lm).is_some());
    }

/// Most of the sampling grid falls off-frame, so the caller falls back to the plain bbox crop.
    #[test]
    fn mostly_out_of_frame_face_is_rejected() {
        let (w, h) = (60u32, 60u32);
        let rgb = vec![90u8; (w * h * 3) as usize];
        let mut lm = TEMPLATE;
        for p in &mut lm {
            p[0] += 90.0;
            p[1] += 90.0;
        }
        assert!(align_112(&rgb, w, h, &lm).is_none());
    }
}
