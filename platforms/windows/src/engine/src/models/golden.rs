//! Golden-vector parity guard for the ONNX model wrappers.
//!
//! Each wrapper's test feeds a fixed synthetic input through the real model and
//! compares the output with `shared/test-corpus/model-golden/<name>.json`: the
//! output shape, its L2 norm, its first 16 values and 16 values spread evenly
//! across it. The tests run when `FILEID_TEST_MODELS_DIR` points at a models
//! folder in the engine layout (`sface/…`, `yunet/…`, `mobileclip/…`,
//! `clip_text/…`, `bge_text/…`, `face_mesh/…`, `ram_plus/…`); without it, or
//! without the model's file, they print a skip line and pass.
//!
//! `FILEID_GOLDEN_WRITE=1` rewrites the golden files from the current runtime.

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

/// Fixed pixels with edges at every scale; integer arithmetic only, so every
/// platform builds the same bytes.
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
        let golden = Golden { model: model.to_owned(), input: input.to_owned(), tolerance, outputs: actual };
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_string_pretty(&golden).unwrap() + "\n").unwrap();
        eprintln!("wrote {}", path.display());
        return;
    }
    let golden: Golden = serde_json::from_slice(&std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
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
    eprintln!("{name}: matches the golden (worst sampled gap {worst:e})");
}
