// 5-point similarity-transform face alignment to the canonical 112x112 ArcFace
// reference template (used by SFace). Least-squares 2D similarity (scale +
// rotation + translation, no shear) from the detected 5 landmarks to the
// template, then bilinear-sample the source into a 112x112 RGB crop. Matches
// OpenCV FaceRecognizerSF::alignCrop so SFace embeddings line up with cv2.

const OUT: usize = 112;

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
    if rgb.len() != (width as usize) * (height as usize) * 3 {
        return None;
    }
    // Fit similarity src->dst: dst = [[a,-b],[b,a]]·src + [tx,ty].
    let (a, b, tx, ty) = fit_similarity(landmarks, &TEMPLATE)?;
    let det = a * a + b * b;
    if det.abs() < 1e-9 {
        return None;
    }
    let mut out = vec![0u8; OUT * OUT * 3];
    for oy in 0..OUT {
        for ox in 0..OUT {
            // Inverse map (dst->src): src = L^-1·(dst - t), L = [[a,-b],[b,a]].
            let dx = ox as f32 - tx;
            let dy = oy as f32 - ty;
            let sx = (a * dx + b * dy) / det;
            let sy = (-b * dx + a * dy) / det;
            let px = bilinear(rgb, width, height, sx, sy);
            let o = (oy * OUT + ox) * 3;
            out[o] = px[0];
            out[o + 1] = px[1];
            out[o + 2] = px[2];
        }
    }
    Some(out)
}

/// Least-squares 2D similarity fit: solve (a, b, tx, ty) minimizing
/// Σ ‖[[a,-b],[b,a]]·src_i + [tx,ty] - dst_i‖² via the 4×4 normal equations.
fn fit_similarity(src: &[[f32; 2]; 5], dst: &[[f32; 2]; 5]) -> Option<(f32, f32, f32, f32)> {
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
    let (w, h) = (width as i32, height as i32);
    let x0 = x.floor() as i32;
    let y0 = y.floor() as i32;
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;
    let sample = |xi: i32, yi: i32| -> [f32; 3] {
        let xc = xi.clamp(0, w - 1) as usize;
        let yc = yi.clamp(0, h - 1) as usize;
        let o = (yc * (width as usize) + xc) * 3;
        [f32::from(rgb[o]), f32::from(rgb[o + 1]), f32::from(rgb[o + 2])]
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
}
