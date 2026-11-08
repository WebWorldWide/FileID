//! Eye aspect ratio from the face mesh for labelled faces.
//!
//!     cargo run --release --example eye_state_eval -- <face_landmarks.onnx> <jobs.jsonl>
//!
//! Each job line is `{"id":…,"image":<upright PNG/JPEG>,"bbox":[x1,y1,x2,y2]}` in that
//! image's pixels. Prints `{"id":…,"ratio":…}` per face (`null` when the mesh finds no
//! face). The first crop is levelled from the box alone; the refinement pass then
//! follows the mesh's own eye line, as it does after a YuNet detection.

use fileid_engine::models::face_geometry::Detection;
use fileid_engine::models::face_mesh::FaceMesh;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    anyhow::ensure!(args.len() == 2, "usage: eye_state_eval <face_landmarks.onnx> <jobs.jsonl>");
    let mut mesh = FaceMesh::load(&args[0])?;
    let mut loaded: Option<(String, image::RgbImage)> = None;
    for line in std::fs::read_to_string(&args[1])?.lines().filter(|l| !l.trim().is_empty()) {
        let job: serde_json::Value = serde_json::from_str(line)?;
        let path = job["image"].as_str().unwrap_or_default().to_owned();
        if loaded.as_ref().is_none_or(|(p, _)| *p != path) {
            loaded = Some((path.clone(), image::open(&path)?.to_rgb8()));
        }
        let (_, rgb) = loaded.as_ref().unwrap();
        let b: Vec<f32> = job["bbox"].as_array().into_iter().flatten().filter_map(|v| v.as_f64()).map(|v| v as f32).collect();
        anyhow::ensure!(b.len() == 4, "bbox needs four numbers");
        let (w, h) = (b[2] - b[0], b[3] - b[1]);
        let face = Detection {
            bbox: [b[0], b[1], b[2], b[3]],
            landmarks: [
                [b[0] + 0.3 * w, b[1] + 0.4 * h],
                [b[0] + 0.7 * w, b[1] + 0.4 * h],
                [b[0] + 0.5 * w, b[1] + 0.6 * h],
                [b[0] + 0.35 * w, b[1] + 0.8 * h],
                [b[0] + 0.65 * w, b[1] + 0.8 * h],
            ],
            score: 1.0,
        };
        let started = std::time::Instant::now();
        let ratio = mesh.eye_ratio(rgb.as_raw(), rgb.width(), rgb.height(), &face)?;
        println!("{}", serde_json::json!({"id": job["id"], "ratio": ratio, "ms": started.elapsed().as_secs_f64() * 1000.0}));
    }
    Ok(())
}
