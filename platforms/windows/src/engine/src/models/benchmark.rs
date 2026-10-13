//! On-device model benchmark and launch tuning.
//!
//! Measures what matters for FileID on the user's own hardware: load time,
//! llama-server's prefill/decode rates, caption latency, peak resident memory,
//! structured-output reliability, and exact answers on synthetic probes
//! (colour, count, OCR). The probes are rendered locally, so no personal or
//! third-party imagery is involved; they check instruction-following, not
//! photo-caption quality.
//!
//! Tuning follows the magnitude approach: try a small grid of llama-server
//! flags and keep the fastest configuration whose greedy answer is identical
//! to the default configuration's.

use super::launch_tuning;
use super::vlm_server::{Timings, VlmServer};
use crate::ipc::ModelBenchmark;
use anyhow::{bail, Context, Result};
use image::{Rgb, RgbImage};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const OCR_FIXTURE: &[u8] = include_bytes!("../../../../../../shared/test-corpus/ocr/known-text.png");
const ANSWER_TOKENS: u32 = 12;
const CAPTION_TOKENS: u32 = 80;
const STRUCTURED_TOKENS: u32 = 300;
pub const TUNING_TOKENS: u32 = 64;

#[derive(Debug, Clone)]
pub struct Probe {
    pub name: &'static str,
    pub image: PathBuf,
    pub question: &'static str,
    pub expected: &'static str,
}

fn fill_circle(image: &mut RgbImage, cx: i32, cy: i32, radius: i32, color: [u8; 3]) {
    for y in (cy - radius).max(0)..(cy + radius).min(image.height() as i32) {
        for x in (cx - radius).max(0)..(cx + radius).min(image.width() as i32) {
            if (x - cx).pow(2) + (y - cy).pow(2) <= radius * radius {
                image.put_pixel(x as u32, y as u32, Rgb(color));
            }
        }
    }
}

fn fill_rect(image: &mut RgbImage, x0: u32, y0: u32, x1: u32, y1: u32, color: [u8; 3]) {
    for y in y0..y1.min(image.height()) {
        for x in x0..x1.min(image.width()) {
            image.put_pixel(x, y, Rgb(color));
        }
    }
}

const RED: [u8; 3] = [220, 30, 30];
const BLUE: [u8; 3] = [30, 60, 220];
const GREEN: [u8; 3] = [30, 170, 60];
const YELLOW: [u8; 3] = [240, 210, 20];
const WHITE: [u8; 3] = [255, 255, 255];
const GRAY: [u8; 3] = [200, 200, 200];

/// Renders the deterministic probe set into `directory`.
pub fn write_probes(directory: &Path) -> Result<Vec<Probe>> {
    std::fs::create_dir_all(directory)?;
    let canvas = |background: [u8; 3]| RgbImage::from_pixel(448, 448, Rgb(background));
    let mut renders: Vec<(&'static str, RgbImage, &'static str, &'static str)> = Vec::new();

    let mut three_red = canvas(WHITE);
    for (x, y) in [(110, 120), (330, 140), (220, 330)] {
        fill_circle(&mut three_red, x, y, 50, RED);
    }
    renders.push(("count-three", three_red, "How many red circles are in this image? Answer with a single number.", "3"));

    let mut blue_square = canvas(WHITE);
    fill_rect(&mut blue_square, 104, 104, 344, 344, BLUE);
    renders.push(("colour-square", blue_square, "What color is the large square? Answer with one word.", "blue"));

    let mut five_yellow = canvas(GRAY);
    for (x, y) in [(90, 90), (350, 90), (220, 220), (90, 350), (350, 350)] {
        fill_circle(&mut five_yellow, x, y, 45, YELLOW);
    }
    renders.push(("count-five", five_yellow, "How many yellow circles are in this image? Answer with a single number.", "5"));

    let mut green_circle = canvas(WHITE);
    fill_circle(&mut green_circle, 224, 224, 150, GREEN);
    renders.push(("colour-circle", green_circle, "What color is the large circle? Answer with one word.", "green"));

    let mut probes = Vec::new();
    for (name, image, question, expected) in renders {
        let path = directory.join(format!("{name}.png"));
        image.save(&path)?;
        probes.push(Probe { name, image: path, question, expected });
    }
    let ocr = directory.join("ocr-digits.png");
    std::fs::write(&ocr, OCR_FIXTURE)?;
    probes.push(Probe { name: "ocr-digits", image: ocr, question: "What number is written in the image? Answer with digits only.", expected: "12345" });
    Ok(probes)
}

/// Lower-cased first word, digits kept, number words mapped to digits.
pub fn normalize_answer(raw: &str) -> String {
    let word = raw
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | '.' | ':' | ';' | '!' | '"' | '\''))
        .map(str::trim)
        .find(|w| !w.is_empty() && !matches!(w.to_ascii_lowercase().as_str(), "the" | "a" | "an" | "answer" | "it" | "is" | "there" | "are"))
        .unwrap_or_default()
        .to_ascii_lowercase();
    match word.as_str() {
        "one" => "1", "two" => "2", "three" => "3", "four" => "4", "five" => "5",
        "six" => "6", "seven" => "7", "eight" => "8", "nine" => "9", "ten" => "10",
        other => return other.trim_matches(|c: char| !c.is_alphanumeric()).to_owned(),
    }
    .to_owned()
}

#[derive(Debug, Default, Clone)]
pub struct Measurement {
    pub load_seconds: f64,
    pub prompt_rates: Vec<f64>,
    pub generated_rates: Vec<f64>,
    pub caption_seconds: Vec<f64>,
    pub correct: usize,
    pub probes: usize,
    pub json_valid: usize,
    pub json_total: usize,
    pub peak_resident_bytes: u64,
}

pub fn median(values: &[f64]) -> Option<f64> {
    let mut sorted: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if sorted.is_empty() {
        return None;
    }
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    Some(if sorted.len() % 2 == 0 { (sorted[middle - 1] + sorted[middle]) / 2.0 } else { sorted[middle] })
}

fn record(measurement: &mut Measurement, timings: Option<Timings>) {
    if let Some(t) = timings {
        if t.prompt_tokens > 0 {
            measurement.prompt_rates.push(t.prompt_per_second);
        }
        if t.predicted_tokens > 0 {
            measurement.generated_rates.push(t.predicted_per_second);
        }
    }
}

/// Samples a process's resident memory until dropped.
struct PeakSampler {
    peak: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
}

impl PeakSampler {
    fn start(pid: Option<u32>) -> Self {
        let peak = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        if let Some(pid) = pid {
            let (peak, stop) = (peak.clone(), stop.clone());
            std::thread::spawn(move || {
                use sysinfo::{Pid, System};
                let mut system = System::new();
                let pid = Pid::from_u32(pid);
                while !stop.load(Ordering::Relaxed) {
                    if system.refresh_process(pid) {
                        if let Some(process) = system.process(pid) {
                            peak.fetch_max(process.memory(), Ordering::Relaxed);
                        }
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
            });
        }
        Self { peak, stop }
    }

    fn finish(&self) -> u64 {
        self.stop.store(true, Ordering::Relaxed);
        self.peak.load(Ordering::Relaxed)
    }
}

/// Runs every probe, the caption timing pass and the structured-output pass
/// against a loaded server. `progress(done, total)` is called per request.
pub async fn measure(server: &VlmServer, probes: &[Probe], cancel: &Arc<AtomicBool>, mut progress: impl FnMut(usize, usize)) -> Result<Measurement> {
    let sampler = PeakSampler::start(server.pid());
    let mut measurement = Measurement { probes: probes.len(), ..Measurement::default() };
    let caption_prompt = super::vlm::caption_prompt_with_faces(&[]);
    let structured: Vec<&Probe> = probes.iter().filter(|p| p.name != "ocr-digits").collect();
    let total = probes.len() * 2 + 2;
    let mut done = 0;
    let result = async {
        for probe in probes {
            let answer = server.complete_with_cancel(&probe.image, probe.question, ANSWER_TOKENS, cancel.clone()).await?;
            record(&mut measurement, server.last_timings());
            if normalize_answer(&answer) == probe.expected {
                measurement.correct += 1;
            }
            done += 1;
            progress(done, total);
        }
        for probe in probes {
            let started = Instant::now();
            server.complete_with_cancel(&probe.image, &caption_prompt, CAPTION_TOKENS, cancel.clone()).await?;
            measurement.caption_seconds.push(started.elapsed().as_secs_f64());
            record(&mut measurement, server.last_timings());
            done += 1;
            progress(done, total);
        }
        for window in [&structured[..structured.len().min(3)], &structured[structured.len().saturating_sub(3)..]] {
            let images: Vec<&Path> = window.iter().map(|p| p.image.as_path()).collect();
            let times: Vec<f64> = (0..images.len()).map(|i| i as f64).collect();
            let raw = server.complete_sequence_with_cancel(&images, &crate::pipeline::timeline_analysis::moment_prompt(&times), STRUCTURED_TOKENS, cancel.clone()).await?;
            record(&mut measurement, server.last_timings());
            measurement.json_total += 1;
            if crate::pipeline::timeline_analysis::parse_moments(&raw, images.len()).is_ok() {
                measurement.json_valid += 1;
            }
            done += 1;
            progress(done, total);
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;
    measurement.peak_resident_bytes = sampler.finish();
    result?;
    Ok(measurement)
}

pub fn summarize(measurement: &Measurement, hardware: &str, tuned: Option<Vec<String>>) -> ModelBenchmark {
    let round = |v: f64, places: i32| (v * 10f64.powi(places)).round() / 10f64.powi(places);
    ModelBenchmark {
        measured_at: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        hardware: hardware.chars().take(200).collect(),
        load_seconds: round(measurement.load_seconds, 2),
        prompt_tokens_per_second: median(&measurement.prompt_rates).map(|v| round(v, 1)),
        generated_tokens_per_second: median(&measurement.generated_rates).map(|v| round(v, 1)),
        seconds_per_image: median(&measurement.caption_seconds).map(|v| round(v, 2)),
        peak_resident_gb: (measurement.peak_resident_bytes > 0).then(|| round(measurement.peak_resident_bytes as f64 / 1e9, 2)),
        json_valid_rate: (measurement.json_total > 0).then(|| round(measurement.json_valid as f64 / measurement.json_total as f64, 2)),
        probe_accuracy: (measurement.probes > 0).then(|| round(measurement.correct as f64 / measurement.probes as f64, 2)),
        tuned_arguments: tuned,
    }
}

/// Greedy answer and decode rate for one fixed tuning prompt.
pub async fn tuning_sample(server: &VlmServer, probe: &Probe, cancel: &Arc<AtomicBool>) -> Result<(String, f64)> {
    let prompt = super::vlm::caption_prompt_with_faces(&[]);
    let started = Instant::now();
    let output = server.complete_with_cancel(&probe.image, &prompt, TUNING_TOKENS, cancel.clone()).await?;
    let rate = server
        .last_timings()
        .filter(|t| t.predicted_tokens > 0)
        .map_or_else(|| TUNING_TOKENS as f64 / started.elapsed().as_secs_f64().max(1e-3), |t| t.predicted_per_second);
    Ok((output, rate))
}

/// Tries each candidate through `launch` and returns the fastest one whose
/// greedy output equals `baseline`, if it beats the baseline rate by 5%.
pub async fn tune<L, F>(baseline: &(String, f64), candidates: Vec<Vec<String>>, mut launch: L) -> Option<(Vec<String>, f64)>
where
    L: FnMut(Vec<String>) -> F,
    F: Future<Output = Result<(String, f64)>>,
{
    let mut best: Option<(Vec<String>, f64)> = None;
    for candidate in candidates {
        match launch(candidate.clone()).await {
            Ok((output, rate)) if output == baseline.0 && best.as_ref().is_none_or(|(_, b)| rate > *b) => best = Some((candidate, rate)),
            Ok(_) => {}
            Err(error) => tracing::info!(?error, ?candidate, "[BENCH] tuning candidate failed"),
        }
    }
    best.filter(|(_, rate)| *rate > baseline.1 * 1.05)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StoredBenchmark {
    pub model: String,
    pub hardware: String,
    pub benchmark: ModelBenchmark,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct Store {
    results: Vec<StoredBenchmark>,
}

fn store_path() -> Result<PathBuf> {
    Ok(crate::paths::root()?.join("model-benchmarks.json"))
}

pub fn load_in(path: &Path) -> Vec<StoredBenchmark> {
    std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice::<Store>(&bytes).ok()).map(|s| s.results).unwrap_or_default()
}

pub fn save_in(path: &Path, result: StoredBenchmark) -> Result<()> {
    let mut results = load_in(path);
    results.retain(|r| !(r.model == result.model && r.hardware == result.hardware));
    results.push(result);
    let staged = path.with_extension("json.tmp");
    std::fs::write(&staged, serde_json::to_vec_pretty(&Store { results })?)?;
    std::fs::rename(staged, path)?;
    Ok(())
}

/// Benchmarks recorded on this machine, keyed by model id.
pub fn local_results() -> std::collections::HashMap<String, ModelBenchmark> {
    let hardware = launch_tuning::hardware_fingerprint();
    store_path().map(|p| load_in(&p)).unwrap_or_default().into_iter().filter(|r| r.hardware == hardware).map(|r| (r.model, r.benchmark)).collect()
}

/// Full benchmark of one installed model: verify, load, measure and
/// optionally tune; persists the result for `listModels`.
pub async fn run(model_kind: &str, tuning: bool, cancel: Arc<AtomicBool>, mut progress: impl FnMut(&str, usize, usize)) -> Result<ModelBenchmark> {
    let verified = super::model_provenance::verify_installed(model_kind, cancel.clone()).await.context("The model is not installed or failed verification")?;
    let (gguf, mmproj) = verified.paths();
    let (gguf, mmproj) = (gguf.to_path_buf(), mmproj.to_path_buf());
    let directory = std::env::temp_dir().join(format!("fileid-bench-{}", uuid::Uuid::new_v4()));
    let probes = write_probes(&directory)?;
    let hardware = launch_tuning::hardware_fingerprint();
    let outcome = async {
        progress("loading", 0, 1);
        let started = Instant::now();
        let server = VlmServer::start(&gguf, &mmproj, &cancel).await?;
        let load_seconds = started.elapsed().as_secs_f64();
        let measured = measure(&server, &probes, &cancel, |done, total| progress("measuring", done, total)).await;
        let baseline = if tuning && measured.is_ok() { Some(tuning_sample(&server, &probes[0], &cancel).await) } else { None };
        server.shutdown().await?;
        let mut measurement = measured?;
        measurement.load_seconds = load_seconds;
        let mut tuned = launch_tuning::lookup(&gguf);
        if let Some(baseline) = baseline {
            let baseline = baseline?;
            let info = crate::commands::hardware::build_hardware_info();
            let model_bytes = std::fs::metadata(&gguf)?.len();
            let candidates = launch_tuning::candidates(info.physical_cpu_cores, crate::platform::dedicated_vram_mb().unwrap_or(0), model_bytes, 48);
            let total = candidates.len();
            let mut tried = 0;
            let winner = tune(&baseline, candidates, |args| {
                tried += 1;
                progress("tuning", tried, total);
                let (gguf, mmproj, cancel, probe) = (gguf.clone(), mmproj.clone(), cancel.clone(), probes[0].clone());
                async move {
                    if cancel.load(Ordering::Relaxed) { bail!("Benchmark cancelled") }
                    let server = VlmServer::start_with_arguments(&gguf, &mmproj, &cancel, Some(&args)).await?;
                    let sample = tuning_sample(&server, &probe, &cancel).await;
                    server.shutdown().await?;
                    sample
                }
            })
            .await;
            if let (Some((arguments, rate)), Some((model, bytes))) = (winner, launch_tuning::model_key(&gguf)) {
                launch_tuning::save(launch_tuning::Profile {
                    model, model_bytes: bytes, hardware: hardware.clone(), arguments: arguments.clone(),
                    measured_at: chrono::Utc::now().to_rfc3339(), generated_tokens_per_second: rate, baseline_tokens_per_second: baseline.1,
                })?;
                tuned = Some(arguments);
            }
        }
        let benchmark = summarize(&measurement, &hardware, tuned);
        save_in(&store_path()?, StoredBenchmark { model: model_kind.to_owned(), hardware: hardware.clone(), benchmark: benchmark.clone() })?;
        Ok(benchmark)
    }
    .await;
    let _ = std::fs::remove_dir_all(&directory);
    outcome
}

#[cfg(test)]
#[path = "benchmark_tests.rs"]
mod tests;
