//! Hand-rolled software rasterizer: a Wavefront `.obj` → a small shaded PNG the Deep
//! Analyze VLM can recognize, so a generically-named model (`mesh_001.obj`) still gets a
//! descriptive name ("a wooden chair"). No new dependency — it parses the text `.obj` /
//! `.mtl` itself, projects with a fixed 3/4 (isometric-ish) camera, z-buffers flat-shaded
//! triangles (per-face `.mtl` Kd colour + one Lambert light), and encodes the PNG with the
//! `image` crate we already ship. The target is "the VLM can tell it's a chair / car /
//! character", not photoreal — flat shading from a 3/4 view is enough, so this needs no
//! GPU and isn't lockstep-bound to macOS (which renders the same `.obj` via QuickLook).
//!
//! macOS counterpart: `DeepAnalyze.quickLookThumbnail` (the OS 3D QuickLook generator).

use std::path::Path;

use anyhow::{bail, Context, Result};

/// Output is a square RGB image; 512 is plenty for VLM recognition and keeps the
/// blocking rasterize cheap (a few ms even for a ~100k-triangle model).
const SIZE: u32 = 512;
/// Fraction of the frame left as a border so the model never touches the edge.
const MARGIN: f32 = 0.10;
/// Neutral light background so the shaded model reads clearly to the VLM.
const BG: [u8; 3] = [236, 236, 239];
/// Default surface colour when a face has no `.mtl` material (mid neutral grey).
const DEFAULT_KD: [f32; 3] = [0.60, 0.60, 0.63];

/// A triangle ready to rasterize: the three positions and its flat surface colour.
struct Tri {
    p: [[f32; 3]; 3],
    kd: [f32; 3],
}

/// Render `obj_path` to a 512×512 PNG at `out_png`. Err on an unreadable / geometry-less
/// `.obj` (the caller falls back to embedded-name metadata). Blocking (CPU-bound) — the
/// caller runs it on a blocking thread.
pub(crate) fn render_obj_to_png(obj_path: &Path, out_png: &Path) -> Result<()> {
    let (verts, tris) = parse_obj(obj_path)?;
    if tris.is_empty() || verts.len() < 3 {
        bail!("obj has no renderable geometry");
    }
    let pixels = rasterize(&verts, &tris);
    let img = image::RgbImage::from_raw(SIZE, SIZE, pixels)
        .context("assemble render buffer")?;
    let p = crate::util::path_safety::to_extended_length(out_png);
    img.save_with_format(&p, image::ImageFormat::Png)
        .with_context(|| format!("encode png {}", out_png.display()))?;
    Ok(())
}

/// Parse vertices + triangulated faces (with per-face Kd colour) from a `.obj`, reading
/// any `mtllib` for material colours. Tolerant: skips lines it doesn't understand, fans
/// polygons into triangles, resolves negative (relative) indices.
fn parse_obj(obj_path: &Path) -> Result<(Vec<[f32; 3]>, Vec<Tri>)> {
    let text = read_text(obj_path)?;
    let mut materials: std::collections::HashMap<String, [f32; 3]> = std::collections::HashMap::new();
    let mut verts: Vec<[f32; 3]> = Vec::new();
    let mut tris: Vec<Tri> = Vec::new();
    let mut cur_kd = DEFAULT_KD;

    for line in text.lines() {
        let line = line.trim();
        let mut it = line.split_whitespace();
        match it.next() {
            Some("v") => {
                let c: Vec<f32> = it.take(3).filter_map(|s| s.parse().ok()).collect();
                if c.len() == 3 {
                    verts.push([c[0], c[1], c[2]]);
                }
            }
            Some("mtllib") => {
                if let Some(name) = it.next() {
                    load_mtl(obj_path, name, &mut materials);
                }
            }
            Some("usemtl") => {
                cur_kd = it.next().and_then(|n| materials.get(n).copied()).unwrap_or(DEFAULT_KD);
            }
            Some("f") => {
                // Each token is `pos[/tex[/norm]]`; we only need the position index,
                // which may be negative (relative to the end). Fan-triangulate.
                let idx: Vec<usize> = it
                    .filter_map(|tok| tok.split('/').next())
                    .filter_map(|s| s.parse::<i64>().ok())
                    .filter_map(|i| resolve_index(i, verts.len()))
                    .collect();
                // Fan-triangulate: (v0, vk, vk+1) for k in 1..=n-2.
                for k in 1..idx.len().saturating_sub(1) {
                    tris.push(Tri {
                        p: [verts[idx[0]], verts[idx[k]], verts[idx[k + 1]]],
                        kd: cur_kd,
                    });
                }
            }
            _ => {}
        }
    }
    Ok((verts, tris))
}

/// Resolve a 1-based (or negative relative) `.obj` index against the current vertex count
/// to a 0-based index, or None if out of range.
fn resolve_index(i: i64, count: usize) -> Option<usize> {
    let zero = match i.cmp(&0) {
        std::cmp::Ordering::Greater => (i - 1) as usize,
        std::cmp::Ordering::Less => (count as i64 + i) as usize, // relative; bounds-checked below
        std::cmp::Ordering::Equal => return None,
    };
    (zero < count).then_some(zero)
}

/// Parse `Kd r g b` lines from the `.obj`'s sibling `.mtl` into `out` (material → colour).
/// Best-effort: a missing/garbled `.mtl` just leaves faces at the default colour.
fn load_mtl(obj_path: &Path, mtl_name: &str, out: &mut std::collections::HashMap<String, [f32; 3]>) {
    let mtl_path = match obj_path.parent() {
        Some(dir) => dir.join(mtl_name),
        None => return,
    };
    let Ok(text) = read_text(&mtl_path) else { return };
    let mut cur: Option<String> = None;
    for line in text.lines() {
        let mut it = line.split_whitespace();
        match it.next() {
            Some("newmtl") => cur = it.next().map(|s| s.to_string()),
            Some("Kd") => {
                let c: Vec<f32> = it.take(3).filter_map(|s| s.parse().ok()).collect();
                if c.len() == 3 {
                    if let Some(name) = &cur {
                        out.insert(name.clone(), [c[0], c[1], c[2]]);
                    }
                }
            }
            _ => {}
        }
    }
}

fn read_text(path: &Path) -> Result<String> {
    let p = crate::util::path_safety::to_extended_length(path);
    std::fs::read_to_string(&p).with_context(|| format!("read {}", path.display()))
}

/// Project, shade, and z-buffer the triangles into a `SIZE×SIZE` RGB buffer.
fn rasterize(verts: &[[f32; 3]], tris: &[Tri]) -> Vec<u8> {
    // Fixed 3/4 view: yaw(30°) then pitch(22°) so we see front + top + one side (reads
    // well for most assets). Precompute the rotation's sin/cos once, then rotate every
    // vertex and fit the rotated XY bounds to the frame.
    let (sy, cy) = 30.0_f32.to_radians().sin_cos();
    let (sp, cp) = 22.0_f32.to_radians().sin_cos();
    let rotate = |v: [f32; 3]| -> [f32; 3] {
        let x1 = v[0] * cy + v[2] * sy; // yaw about Y
        let z1 = -v[0] * sy + v[2] * cy;
        let y2 = v[1] * cp - z1 * sp; // pitch about X
        let z2 = v[1] * sp + z1 * cp;
        [x1, y2, z2]
    };
    let view: Vec<[f32; 3]> = verts.iter().map(|v| rotate(*v)).collect();
    let (mut min, mut max) = ([f32::MAX; 3], [f32::MIN; 3]);
    for v in &view {
        for a in 0..3 {
            min[a] = min[a].min(v[a]);
            max[a] = max[a].max(v[a]);
        }
    }
    let extent = (max[0] - min[0]).max(max[1] - min[1]).max(1e-6);
    let usable = SIZE as f32 * (1.0 - 2.0 * MARGIN);
    let scale = usable / extent;
    let cx = (min[0] + max[0]) * 0.5;
    let cy = (min[1] + max[1]) * 0.5;
    // Map view-space (x,y) → screen pixels (y flipped so +y is up).
    let to_screen = |p: [f32; 3]| -> [f32; 3] {
        [
            SIZE as f32 * 0.5 + (p[0] - cx) * scale,
            SIZE as f32 * 0.5 - (p[1] - cy) * scale,
            p[2],
        ]
    };

    let light = normalize([0.35, 0.55, 0.75]); // upper-front-right, toward the viewer
    let mut color = vec![0u8; (SIZE * SIZE) as usize * 3];
    for i in 0..(SIZE * SIZE) as usize {
        color[i * 3] = BG[0];
        color[i * 3 + 1] = BG[1];
        color[i * 3 + 2] = BG[2];
    }
    let mut depth = vec![f32::MIN; (SIZE * SIZE) as usize];

    for t in tris {
        let r0 = rotate(t.p[0]);
        let r1 = rotate(t.p[1]);
        let r2 = rotate(t.p[2]);
        let a = to_screen(r0);
        let b = to_screen(r1);
        let c = to_screen(r2);
        // Flat shade from the view-space face normal. Two-sided (winding is unknown),
        // so light whichever face we can see.
        let n = normalize(cross(sub(r1, r0), sub(r2, r0)));
        let lambert = dot(n, light).abs();
        let shade = (0.25 + 0.75 * lambert).clamp(0.0, 1.0);
        let px = [
            (t.kd[0] * shade * 255.0).clamp(0.0, 255.0) as u8,
            (t.kd[1] * shade * 255.0).clamp(0.0, 255.0) as u8,
            (t.kd[2] * shade * 255.0).clamp(0.0, 255.0) as u8,
        ];
        fill_triangle(a, b, c, px, &mut color, &mut depth);
    }
    color
}

/// Barycentric triangle fill with a depth test (nearer view-space z wins).
fn fill_triangle(a: [f32; 3], b: [f32; 3], c: [f32; 3], px: [u8; 3], color: &mut [u8], depth: &mut [f32]) {
    let min_x = a[0].min(b[0]).min(c[0]).floor().max(0.0) as i32;
    let max_x = a[0].max(b[0]).max(c[0]).ceil().min(SIZE as f32 - 1.0) as i32;
    let min_y = a[1].min(b[1]).min(c[1]).floor().max(0.0) as i32;
    let max_y = a[1].max(b[1]).max(c[1]).ceil().min(SIZE as f32 - 1.0) as i32;
    let area = edge(a, b, c);
    if area.abs() < 1e-6 {
        return; // degenerate
    }
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let p = [x as f32 + 0.5, y as f32 + 0.5, 0.0];
            let w0 = edge(b, c, p) / area;
            let w1 = edge(c, a, p) / area;
            let w2 = edge(a, b, p) / area;
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                continue;
            }
            let z = w0 * a[2] + w1 * b[2] + w2 * c[2];
            let idx = (y as u32 * SIZE + x as u32) as usize;
            if z > depth[idx] {
                depth[idx] = z;
                color[idx * 3] = px[0];
                color[idx * 3 + 1] = px[1];
                color[idx * 3 + 2] = px[2];
            }
        }
    }
}

/// Signed area of the triangle (a, b, p) in screen space (the edge function).
fn edge(a: [f32; 3], b: [f32; 3], p: [f32; 3]) -> f32 {
    (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn normalize(v: [f32; 3]) -> [f32; 3] {
    let m = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if m < 1e-9 {
        [0.0, 0.0, 1.0]
    } else {
        [v[0] / m, v[1] / m, v[2] / m]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_tmp(name: &str, body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("fileid-obj-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        p
    }

    #[test]
    fn parse_obj_reads_verts_and_triangulates_quads() {
        // A unit quad → two triangles; negative-index face resolves too.
        let obj = "v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nf 1 2 3 4\nf -4 -3 -2\n";
        let p = write_tmp("quad.obj", obj);
        let (verts, tris) = parse_obj(&p).unwrap();
        assert_eq!(verts.len(), 4);
        assert_eq!(tris.len(), 3); // quad → 2, plus the 3-vert relative face → 1
    }

    #[test]
    fn render_produces_non_empty_png_with_shaded_pixels() {
        // A simple tetrahedron should leave the background somewhere AND paint some
        // shaded (non-background) pixels — proves projection + fill actually drew.
        let obj = "v 0 0 0\nv 1 0 0\nv 0 1 0\nv 0 0 1\nf 1 2 3\nf 1 2 4\nf 1 3 4\nf 2 3 4\n";
        let p = write_tmp("tetra.obj", obj);
        let out = p.with_extension("png");
        render_obj_to_png(&p, &out).unwrap();
        let decoded = image::open(&out).unwrap().to_rgb8();
        assert_eq!(decoded.dimensions(), (SIZE, SIZE));
        let painted = decoded.pixels().filter(|px| px.0 != BG).count();
        assert!(painted > 100, "expected the model to cover some pixels, got {painted}");
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn empty_geometry_is_an_error() {
        let p = write_tmp("empty.obj", "# just a comment\nvn 0 0 1\n");
        assert!(render_obj_to_png(&p, &p.with_extension("png")).is_err());
    }
}
