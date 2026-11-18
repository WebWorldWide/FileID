//! Per-model inference latency on tract, for tuning the matmul thread count.
//!
//!     FILEID_TRACT_THREADS=1 cargo run --release --example tract_bench -- <models-dir> [iterations] [sessions]
//!
//! `<models-dir>` is a models directory (`clip_text/`, `mobileclip/`, `sface/`,
//! `yunet/`, `ram_plus/`, `bge_text/`). Each model runs `iterations` times on
//! `sessions` concurrent threads (one session per thread, sharing the compiled
//! graph); one JSON line per model reports the mean milliseconds per run and the
//! wall time for the whole batch of runs. Inputs are synthetic.

use std::path::PathBuf;
use std::time::Instant;

use fileid_engine::models::bge_text::BgeText;
use fileid_engine::models::clip_text::ClipText;
use fileid_engine::models::inference::matmul_threads;
use fileid_engine::models::mobileclip::MobileClipImage;
use fileid_engine::models::ram_plus::RamPlusTagger;
use fileid_engine::models::sface::SFace;
use fileid_engine::models::yunet::YuNet;
use fileid_engine::models::ClipTokenizer;

fn rgb(width: usize, height: usize) -> Vec<u8> {
    (0..width * height * 3).map(|i| ((i * 31 + (i / width) * 7) % 251) as u8).collect()
}

fn run<S: Send>(name: &str, iterations: usize, sessions: usize, make: impl Fn() -> S + Sync, step: impl Fn(&mut S) + Sync) {
    let mut states: Vec<S> = (0..sessions).map(|_| make()).collect();
    for state in &mut states {
        step(state);
    }
    let started = Instant::now();
    std::thread::scope(|scope| {
        for state in &mut states {
            let step = &step;
            scope.spawn(move || {
                for _ in 0..iterations {
                    step(state);
                }
            });
        }
    });
    let wall = started.elapsed().as_secs_f64();
    println!(
        "{}",
        serde_json::json!({
            "model": name,
            "threads": matmul_threads(),
            "sessions": sessions,
            "iterations": iterations,
            "ms_per_run": wall * 1000.0 * sessions as f64 / (iterations * sessions) as f64,
            "runs_per_second": (iterations * sessions) as f64 / wall,
        })
    );
}

fn main() {
    let mut args = std::env::args().skip(1);
    let models = PathBuf::from(args.next().expect("usage: tract_bench <models-dir> [iterations] [sessions]"));
    let iterations: usize = args.next().and_then(|v| v.parse().ok()).unwrap_or(10);
    let sessions: usize = args.next().and_then(|v| v.parse().ok()).unwrap_or(1);
    let only = std::env::var("FILEID_BENCH_ONLY").ok();
    let wanted = |name: &str| only.as_deref().is_none_or(|only| only.split(',').any(|item| item == name));

    let clip_text = models.join("clip_text/clip_text.onnx");
    if wanted("clip_text") && clip_text.is_file() {
        let directory = clip_text.parent().unwrap().to_owned();
        run(
            "clip_text",
            iterations,
            sessions,
            || {
                let tokenizer = ClipTokenizer::new(
                    &std::fs::read_to_string(directory.join("vocab.json")).unwrap(),
                    &std::fs::read_to_string(directory.join("merges.txt")).unwrap(),
                )
                .unwrap();
                ClipText::load(&clip_text, tokenizer).unwrap()
            },
            |model| {
                model.embed("a photo of a dog running on a beach at sunset").unwrap();
            },
        );
    }
    let clip_image = models.join("mobileclip/mobileclip_s2_image.onnx");
    if wanted("clip_image") && clip_image.is_file() {
        let image = rgb(224, 224);
        run("clip_image", iterations, sessions, || MobileClipImage::load(&clip_image).unwrap(), |model| {
            model.embed(&image).unwrap();
        });
        let batch: Vec<Vec<u8>> = (0..8).map(|_| image.clone()).collect();
        run("clip_image_batch8", iterations, sessions, || MobileClipImage::load(&clip_image).unwrap(), |model| {
            model.embed_batch(&batch).unwrap();
        });
    }
    let sface = models.join("sface/face_recognition_sface_2021dec.onnx");
    if wanted("sface") && sface.is_file() {
        let face = rgb(112, 112);
        run("sface", iterations, sessions, || SFace::load(&sface).unwrap(), |model| {
            model.embed(&face).unwrap();
        });
    }
    let yunet = models.join("yunet/face_detection_yunet_2023mar.onnx");
    if wanted("yunet") && yunet.is_file() {
        let image = rgb(640, 640);
        run("yunet", iterations, sessions, || YuNet::load(&yunet).unwrap(), |model| {
            model.detect(&image, 640, 640).unwrap();
        });
    }
    let ram_plus = models.join("ram_plus/ram_plus.onnx");
    let tags = models.join("ram_plus/ram_plus_tags.txt");
    if wanted("ram_plus") && ram_plus.is_file() && tags.is_file() {
        let image = rgb(384, 384);
        run("ram_plus", iterations.min(5), sessions, || RamPlusTagger::load(&ram_plus, tags.clone()).unwrap(), |model| {
            model.tag(&image, 384, 384).unwrap();
        });
    }
    let bge = models.join("bge_text/bge_small.onnx");
    let vocab = models.join("bge_text/vocab.txt");
    if wanted("bge_text") && bge.is_file() && vocab.is_file() {
        run("bge_text", iterations, sessions, || BgeText::load(&bge, &vocab).unwrap(), |model| {
            model.embed("Quarterly invoice for consulting services rendered during the third quarter of the fiscal year").unwrap();
        });
    }
}
