//! Golden-vector parity guard for the ONNX wrappers: a fixed synthetic input through the real model is compared with
//! `shared/test-corpus/model-golden/<name>.json` (shape, L2 norm, 16 leading and 16 spread values). Tests run only when
//! `FILEID_TEST_MODELS_DIR` points at a models folder in the engine layout; otherwise they print a skip line and pass.
//!
//! The goldens came from ONNX Runtime 1.22.0 (CPU), recorded in each file's `generator`. `FILEID_GOLDEN_WRITE=1`
//! creates a missing one; overwriting also needs `FILEID_GOLDEN_REBASE=1`, which marks it rebased onto tract and so no
//! longer an independent check: do that only for a deliberate model or runtime change.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const SAMPLES: usize = 16;

pub struct Probe<'a> {
    pub name: &'a str,
    pub shape: Vec<usize>,
    pub values: &'a [f32],
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tolerance {
    pub max_abs: f32,
    pub rel_norm: f32,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoldenOutput {
    name: String,
    shape: Vec<usize>,
    norm: f64,
    head: Vec<f32>,
    strided: Vec<f32>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Golden {
    model: String,
    generator: String,
    input: String,
    tolerance: Tolerance,
    outputs: Vec<GoldenOutput>,
}

pub fn model_file(relative: &str) -> Option<PathBuf> {
    let Some(root) = std::env::var_os("FILEID_TEST_MODELS_DIR").filter(|value| !value.is_empty()) else {
        eprintln!("skip {relative}: FILEID_TEST_MODELS_DIR is not set");
        return None;
    };
    let path = PathBuf::from(root).join(relative);
    if !path.is_file() {
        eprintln!("skip {relative}: not in FILEID_TEST_MODELS_DIR");
        return None;
    }
    Some(path)
}

pub fn synthetic_rgb(width: usize, height: usize) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(width * height * 3);
    for y in 0..height {
        for x in 0..width {
            for channel in 0..3usize {
                let blocks = (x / 8 * 37) ^ (y / 8 * 59);
                rgb.push(((blocks + channel * 71 + (x + y) / 2) % 256) as u8);
            }
        }
    }
    rgb
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../shared/test-corpus/model-golden")
        .join(format!("{name}.json"))
}

fn summarize(probe: &Probe) -> GoldenOutput {
    let values = probe.values;
    let strided = (0..SAMPLES).map(|i| values[i * values.len() / SAMPLES]).collect();
    GoldenOutput {
        name: probe.name.to_owned(),
        shape: probe.shape.clone(),
        norm: values.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>().sqrt(),
        head: values.iter().take(SAMPLES).copied().collect(),
        strided,
    }
}

pub fn check(name: &str, model: &str, input: &str, tolerance: Tolerance, probes: &[Probe]) {
    for probe in probes {
        assert_eq!(probe.shape.iter().product::<usize>(), probe.values.len(), "{name}/{} shape vs data", probe.name);
        assert!(probe.values.iter().all(|v| v.is_finite()), "{name}/{} has a non-finite value", probe.name);
    }
    let path = golden_path(name);
    let actual: Vec<GoldenOutput> = probes.iter().map(summarize).collect();
    if std::env::var_os("FILEID_GOLDEN_WRITE").is_some_and(|value| value == "1") {
        let rebase = std::env::var_os("FILEID_GOLDEN_REBASE").is_some_and(|value| value == "1");
        assert!(
            rebase || !path.exists(),
            "{} already exists; FILEID_GOLDEN_WRITE=1 will not overwrite it without FILEID_GOLDEN_REBASE=1",
            path.display()
        );
        let generator = if rebase { "tract-onnx 0.23.10 cpu (rebased)" } else { "tract-onnx 0.23.10 cpu" };
        let golden = Golden { model: model.to_owned(), generator: generator.to_owned(), input: input.to_owned(), tolerance, outputs: actual };
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_string_pretty(&golden).unwrap() + "\n").unwrap();
        eprintln!("wrote {}", path.display());
        return;
    }
    let golden: Golden = serde_json::from_slice(&std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert!(!golden.generator.is_empty(), "{name}: golden does not say which runtime generated it");
    assert_eq!(golden.outputs.len(), actual.len(), "{name}: output count");
    let tolerance = golden.tolerance;
    let mut worst = 0f32;
    for (want, got) in golden.outputs.iter().zip(&actual) {
        assert_eq!(want.name, got.name, "{name}: output name");
        assert_eq!(want.shape, got.shape, "{name}/{}: shape", want.name);
        let norm_gap = ((want.norm - got.norm) / want.norm.max(1e-12)).abs() as f32;
        assert!(norm_gap <= tolerance.rel_norm, "{name}/{}: norm {} vs golden {} (relative gap {norm_gap})", want.name, got.norm, want.norm);
        for (kind, want_values, got_values) in [("head", &want.head, &got.head), ("strided", &want.strided, &got.strided)] {
            for (index, (w, g)) in want_values.iter().zip(got_values).enumerate() {
                let gap = (w - g).abs();
                worst = worst.max(gap);
                assert!(
                    gap <= tolerance.max_abs + tolerance.rel_norm * w.abs(),
                    "{name}/{} {kind}[{index}]: {g} vs golden {w}",
                    want.name
                );
            }
        }
    }
    eprintln!("{name}: matches the golden generated by {} (worst sampled gap {worst:e})", golden.generator);
}
