// 5-point similarity-transform face alignment to the canonical 112x112 ArcFace
// reference template (used by SFace). Least-squares 2D similarity (scale +
// rotation + translation, no shear) from the detected 5 landmarks to the
// template, then bilinear-sample the source into a 112x112 RGB crop. Matches
// OpenCV FaceRecognizerSF::alignCrop so SFace embeddings line up with cv2.

const OUT: usize = 112;

/// Reject the aligned crop when more than this fraction of its pixels sample
/// OUTSIDE the source image. Those pixels are edge-clamp replication — long
/// constant-color streaks that corrupt the SFace embedding (and look like a
/// smear in the People card). A face this far off-frame, or one whose
/// landmarks fit so badly the warp leaves the image, is better served by the
/// bounds-clamped plain bbox crop the caller falls back to. Env-overridable.
/// (audit 2026-07-15: found by inspecting real People-tab representative crops.)
fn max_out_of_bounds_fraction() -> f32 {
    std::env::var("FILEID_FACE_ALIGN_MAX_OOB")
        .ok()
        .and_then(|v| v.trim().parse::<f32>().ok())
        .filter(|v| v.is_finite())
        .map(|v| v.clamp(0.0, 1.0))
        .unwrap_or(0.20)
}

/// Template in FileID landmark order [left_eye, right_eye, nose, mouth_left,
/// mouth_right] (the standard ArcFace 5-point template, reordered from
/// OpenCV's native [re, le, nt, rcm, lcm]).
const TEMPLATE: [[f32; 2]; 5] = [
    [73.5318, 51.5014], // left_eye
    [38.2946, 51.6963], // right_eye
    [56.0252, 71.7366], // nose
    [70.7299, 92.2041], // mouth_left
    [41.5493, 92.3655], // mouth_right
];

/// Align a face to 112x112 RGB from its 5 landmarks (FileID order) in the
/// source image's pixel coordinates. Returns 112*112*3 RGB8 bytes, or `None`
/// if the fit is degenerate.
pub fn align_112(rgb: &[u8], width: u32, height: u32, landmarks: &[[f32; 2]; 5]) -> Option<Vec<u8>> {
    let expected = (width as usize).checked_mul(height as usize)?.checked_mul(3)?;
    if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 || rgb.len() != expected {
        return None;
    }
    // Fit similarity src->dst: dst = [[a,-b],[b,a]]·src + [tx,ty].
    let (a, b, tx, ty) = fit_similarity(landmarks, &TEMPLATE)?;
    let det = a * a + b * b;
    if !det.is_finite() || det.abs() < 1e-9 {
        return None;
    }
    let mut out = vec![0u8; OUT * OUT * 3];
    let mut oob = 0usize;
    for oy in 0..OUT {
        for ox in 0..OUT {
            // Inverse map (dst->src): src = L^-1·(dst - t), L = [[a,-b],[b,a]].
            let dx = ox as f32 - tx;
            let dy = oy as f32 - ty;
            let sx = (a * dx + b * dy) / det;
            let sy = (-b * dx + a * dy) / det;
            // A sample whose source center is outside the image is edge-clamp
            // replication, not real face pixels. Count it so an over-warped or
            // off-frame face can be rejected below.
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
    // Too much of the crop is replicated border smear — reject so the caller
    // falls back to the bounds-clamped bbox crop, which is a real (if
    // off-center) face rather than a streak that corrupts the SFace embedding.
    if (oob as f32) / ((OUT * OUT) as f32) > max_out_of_bounds_fraction() {
        return None;
    }
    Some(out)
}

/// The macOS engine's alignment (`FaceAlign.align112` over one rendered
/// buffer): RGBA8 in (tightly packed, top-left origin), 112×112 RGBA8 out with
/// alpha 255, and no off-frame rejection. Kept bit-identical to the Swift code
/// so a face embedded here lands in the `macos-landmark-overlap-v3-orientation`
/// space next to the Swift engine's vectors (`face-alignment.json` pins it).
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

/// The similarity fit `align_112_rgba` uses, exposed for the shared fixtures.
pub fn fit_to_template(src: &[[f32; 2]; 5]) -> Option<(f32, f32, f32, f32)> {
    fit_similarity(src, &TEMPLATE)
}

/// Least-squares 2D similarity fit: solve (a, b, tx, ty) minimizing
/// Σ ‖[[a,-b],[b,a]]·src_i + [tx,ty] - dst_i‖² via the 4×4 normal equations.
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
        ba += x * cx + y * cy; // Σ(x·X + y·Y)
        bb += x * cy - y * cx; // Σ(x·Y - y·X)
        bxx += cx; // ΣX
        byy += cy; // ΣY
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

/// 4×4 linear solve via Gaussian elimination with partial pivoting.
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

/// Bilinear RGB sample with edge clamping.
fn bilinear(rgb: &[u8], width: u32, height: u32, x: f32, y: f32) -> [u8; 3] {
    bilinear_strided(rgb, 3, width, height, x, y)
}

/// Bilinear RGB sample with edge clamping from pixels `stride` bytes apart
/// (3 for RGB, 4 for RGBA with alpha ignored).
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

    /// The source image the shared fixture describes in `pattern`.
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

    /// Bit for bit with the Swift engine's `FaceAlign` (FaceLandmarkMatchTests
    /// checks the same fixture).
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

    /// Prints the fixture's expected values: `cargo test print_aligned_fixture -- --ignored --nocapture`.
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

    /// A well-placed face (landmarks near the ArcFace template inside a large
    /// image) aligns without hitting the border, so it is accepted.
    #[test]
    fn in_frame_face_aligns() {
        let (w, h) = (160u32, 160u32);
        let rgb = vec![128u8; (w * h * 3) as usize];
        // TEMPLATE is authored in the 112×112 output frame; offset it well
        // inside the 160×160 source so no sample lands out of bounds.
        let mut lm = TEMPLATE;
        for p in &mut lm {
            p[0] += 24.0;
            p[1] += 24.0;
        }
        assert!(align_112(&rgb, w, h, &lm).is_some());
    }

    /// Landmarks in the far corner of a tiny image force most of the 112×112
    /// sampling grid off-frame; that crop is edge-clamp smear and must be
    /// rejected so the caller falls back to the plain bbox crop. (audit)
    #[test]
    fn mostly_out_of_frame_face_is_rejected() {
        let (w, h) = (60u32, 60u32);
        let rgb = vec![90u8; (w * h * 3) as usize];
        // Push the template landmarks past the bottom-right corner so the
        // inverse-mapped source grid mostly falls outside [0,60)².
        let mut lm = TEMPLATE;
        for p in &mut lm {
            p[0] += 90.0;
            p[1] += 90.0;
        }
        assert!(align_112(&rgb, w, h, &lm).is_none());
    }
}
