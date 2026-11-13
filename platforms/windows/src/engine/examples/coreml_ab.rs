//! CoreML EP vs CPU A/B for SFace and CLIP text (macOS).
//!
//!     coreml_ab <sface|cliptext> <cpu|coreml> <models_dir> <out_vectors.f32> <cache_dir> [iters]
//!
//! Prints one JSON line: session build ms, first-run ms, and per-inference
//! latency percentiles. Writes the L2-normalised vectors (little-endian f32).

use std::time::Instant;

use anyhow::{Context, Result};
use fileid_engine::models::clip_tokenizer::ClipTokenizer;
use fileid_engine::models::sface::{Provider, SFace};
use ndarray::Array2;
use ort::execution_providers::coreml::{CoreMLComputeUnits, CoreMLExecutionProvider, CoreMLModelFormat};
use ort::session::{Session, SessionInputValue};
use ort::value::Tensor;

fn crop(seed: u32) -> Vec<u8> {
    let mut state = seed.wrapping_mul(2654435761).wrapping_add(12345);
    let mut out = vec![0u8; 3 * 112 * 112];
    for y in 0..112usize {
        for x in 0..112usize {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            let noise = ((state >> 24) & 0x1f) as i32;
            let cx = (x as i32 - 56).abs() + (y as i32 - 56).abs();
            for c in 0..3usize {
                let base = 200 - cx * 2 + (seed as i32 * 7 + c as i32 * 23) % 40;
                out[(y * 112 + x) * 3 + c] = (base + noise).clamp(0, 255) as u8;
            }
        }
    }
    out
}

const QUERIES: [&str; 12] = [
    "a dog running on the beach", "birthday cake with candles", "mountain lake at sunrise", "a person holding a baby",
    "red car parked on a street", "baseball game at night", "scanned receipt on a table", "family dinner",
    "snow covered pine trees", "a man playing guitar", "close up of a flower", "city skyline at dusk",
];

fn pct(sorted: &[f64], p: f64) -> f64 {
    sorted[((sorted.len() as f64 - 1.0) * p).round() as usize]
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (model, ep, models, out, cache) = (&args[1], &args[2], &args[3], &args[4], &args[5]);
    let iters: usize = args.get(6).and_then(|v| v.parse().ok()).unwrap_or(200);
    let coreml = ep == "coreml";
    let mut times = Vec::new();
    let mut vectors: Vec<f32> = Vec::new();
    let (build_ms, first_ms);
    match model.as_str() {
        "sface" => {
            let path = format!("{models}/face_recognition_sface_2021dec.onnx");
            let provider = if coreml { Provider::CoreMl { cache_dir: cache.clone().into(), intra_threads: 4 } } else { Provider::Cpu };
            let started = Instant::now();
            let mut m = SFace::load_with(&path, provider)?;
            build_ms = started.elapsed().as_secs_f64() * 1e3;
            let started = Instant::now();
            let _ = m.embed(&crop(0))?;
            first_ms = started.elapsed().as_secs_f64() * 1e3;
            let crops: Vec<Vec<u8>> = (0..32).map(crop).collect();
            for i in 0..iters {
                let started = Instant::now();
                let v = m.embed(&crops[i % 32])?;
                times.push(started.elapsed().as_secs_f64() * 1e3);
                if i < 32 {
                    vectors.extend(v);
                }
            }
        }
        "cliptext" => {
            let dir = format!("{models}/clip_text");
            let tokenizer = ClipTokenizer::new(&std::fs::read_to_string(format!("{dir}/vocab.json"))?, &std::fs::read_to_string(format!("{dir}/merges.txt"))?)?;
            fileid_engine::ort_runtime::commit_environment().context("ORT environment")?;
            let started = Instant::now();
            let mut builder = Session::builder()?
                .with_log_level(ort::logging::LogLevel::Error)?
                .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)?;
            if coreml {
                std::fs::create_dir_all(cache)?;
                let p = CoreMLExecutionProvider::default()
                    .with_model_format(CoreMLModelFormat::MLProgram)
                    .with_subgraphs(true)
                    .with_compute_units(CoreMLComputeUnits::All)
                    .with_model_cache_dir(cache.as_str())
                    .build()
                    .error_on_failure();
                builder = builder.with_execution_providers([p])?;
            }
            let mut session = builder.commit_from_file(format!("{dir}/clip_text.onnx"))?;
            build_ms = started.elapsed().as_secs_f64() * 1e3;
            let name = session.inputs.first().context("input")?.name.clone();
            let mut run = |q: &str| -> Result<Vec<f32>> {
                let mut padded = vec![0i64; 77];
                for (i, t) in tokenizer.encode(q).iter().take(77).enumerate() {
                    padded[i] = i64::from(*t);
                }
                let tensor = Tensor::from_array(Array2::<i64>::from_shape_vec((1, 77), padded)?)?;
                let outputs = session.run(vec![(name.clone(), SessionInputValue::from(tensor))])?;
                let (_, value) = outputs.iter().next().context("no output")?;
                let (_, data) = value.try_extract_tensor::<f32>()?;
                let mut v = data.to_vec();
                let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-8);
                for x in &mut v { *x /= n; }
                Ok(v)
            };
            let started = Instant::now();
            let _ = run(QUERIES[0])?;
            first_ms = started.elapsed().as_secs_f64() * 1e3;
            for i in 0..iters {
                let started = Instant::now();
                let v = run(QUERIES[i % QUERIES.len()])?;
                times.push(started.elapsed().as_secs_f64() * 1e3);
                if i < QUERIES.len() {
                    vectors.extend(v);
                }
            }
        }
        other => anyhow::bail!("unknown model {other}"),
    }
    let mut sorted = times.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = times.iter().sum::<f64>() / times.len() as f64;
    std::fs::write(out, vectors.iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>())?;
    println!(
        "{}",
        serde_json::json!({"model": model, "ep": ep, "build_ms": build_ms, "first_ms": first_ms, "iters": iters,
            "mean_ms": mean, "p50_ms": pct(&sorted, 0.5), "p95_ms": pct(&sorted, 0.95), "ort": fileid_engine::models::sface::runtime_release()})
    );
    Ok(())
}
