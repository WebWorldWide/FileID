//! Per-machine llama-server launch tuning.
//!
//! `fileid-bench --tune` tries a small grid of launch flags for one model and
//! keeps the fastest configuration whose greedy output matches the baseline
//! (the magnitude approach: speed only counts if the answer is unchanged).
//! The result is stored per model and hardware fingerprint; a profile from a
//! different machine, or one containing anything outside the allowlist, is
//! ignored and the server starts with its defaults.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub model: String,
    pub model_bytes: u64,
    pub hardware: String,
    pub arguments: Vec<String>,
    pub measured_at: String,
    pub generated_tokens_per_second: f64,
    pub baseline_tokens_per_second: f64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Store {
    profiles: Vec<Profile>,
}

/// Flags auto-tuning may set, each with a validator for its value.
const ALLOWED: [(&str, fn(&str) -> bool); 5] = [
    ("-t", |v| v.parse::<u32>().is_ok_and(|n| (1..=256).contains(&n))),
    ("-ngl", |v| v.parse::<u32>().is_ok_and(|n| n <= 999)),
    ("-fa", |v| matches!(v, "on" | "off")),
    ("-b", |v| v.parse::<u32>().is_ok_and(|n| (32..=8192).contains(&n))),
    ("-ub", |v| v.parse::<u32>().is_ok_and(|n| (32..=8192).contains(&n))),
];

/// True when `arguments` is a sequence of distinct allowlisted flag/value pairs.
pub fn valid(arguments: &[String]) -> bool {
    if arguments.len() % 2 != 0 || arguments.len() > 2 * ALLOWED.len() {
        return false;
    }
    let mut seen = std::collections::HashSet::new();
    arguments.chunks(2).all(|pair| {
        ALLOWED.iter().any(|(flag, check)| *flag == pair[0] && check(&pair[1])) && seen.insert(pair[0].as_str())
    })
}

/// A stable description of the machine a profile was measured on.
pub fn hardware_fingerprint() -> String {
    let info = crate::commands::hardware::build_hardware_info();
    let memory = crate::platform::physical_memory_gb().round() as u64;
    let vram = crate::platform::dedicated_vram_mb().unwrap_or(0);
    format!(
        "{}-{} · {} cores · {memory} GB · {} {vram} MB",
        std::env::consts::OS,
        std::env::consts::ARCH,
        info.physical_cpu_cores,
        info.adapter_name.as_deref().unwrap_or(&info.gpu_vendor),
    )
}

/// Model identity for tuning: its install directory name plus weight size,
/// so a different quantization or a replaced file never reuses a profile.
pub fn model_key(gguf: &Path) -> Option<(String, u64)> {
    let directory = gguf.parent()?.file_name()?.to_str()?.to_owned();
    Some((directory, std::fs::metadata(gguf).ok()?.len()))
}

fn store_path() -> Result<PathBuf> {
    Ok(crate::paths::root()?.join("model-tuning.json"))
}

fn read(path: &Path) -> Store {
    std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
}

pub fn lookup_in(path: &Path, model: &str, model_bytes: u64, hardware: &str) -> Option<Vec<String>> {
    read(path)
        .profiles
        .into_iter()
        .find(|p| p.model == model && p.model_bytes == model_bytes && p.hardware == hardware && valid(&p.arguments))
        .map(|p| p.arguments)
}

/// Tuned flags for this model on this machine, if a valid profile exists.
pub fn lookup(gguf: &Path) -> Option<Vec<String>> {
    let (model, bytes) = model_key(gguf)?;
    lookup_in(&store_path().ok()?, &model, bytes, &hardware_fingerprint())
}

pub fn save_in(path: &Path, profile: Profile) -> Result<()> {
    if !valid(&profile.arguments) {
        bail!("Refusing to store launch flags outside the tuning allowlist");
    }
    let mut store = read(path);
    store.profiles.retain(|p| !(p.model == profile.model && p.hardware == profile.hardware));
    store.profiles.push(profile);
    let staged = path.with_extension("json.tmp");
    std::fs::write(&staged, serde_json::to_vec_pretty(&store)?)?;
    std::fs::rename(staged, path)?;
    Ok(())
}

pub fn save(profile: Profile) -> Result<()> {
    save_in(&store_path()?, profile)
}

/// The candidate grid for one model. Partial GPU offload is only tried when
/// the weights do not fit in dedicated VRAM; CPU-only machines vary threads.
pub fn candidates(physical_cores: u32, vram_mb: u64, model_bytes: u64, layers: u32) -> Vec<Vec<String>> {
    let args = |pairs: &[(&str, String)]| pairs.iter().flat_map(|(f, v)| [(*f).to_owned(), v.clone()]).collect::<Vec<_>>();
    let cores = physical_cores.max(1);
    let mut grid = Vec::new();
    if vram_mb == 0 {
        for threads in [cores, (cores / 2).max(1), cores + cores / 2] {
            for batch in ["512", "2048"] {
                grid.push(args(&[("-ngl", "0".into()), ("-t", threads.to_string()), ("-b", batch.into()), ("-ub", "512".into())]));
            }
        }
    } else {
        let model_mb = model_bytes / 1_048_576;
        let offloads: Vec<u32> = if model_mb * 11 / 10 <= vram_mb {
            vec![999]
        } else {
            let fraction = vram_mb as f64 * 0.85 / model_mb.max(1) as f64;
            let fit = ((layers as f64) * fraction).floor() as u32;
            vec![fit.min(layers), (fit * 3 / 4).min(layers), (fit / 2).min(layers)]
        };
        for ngl in offloads {
            for flash in ["on", "off"] {
                grid.push(args(&[("-ngl", ngl.to_string()), ("-fa", flash.into()), ("-t", cores.to_string())]));
            }
        }
    }
    grid.dedup();
    grid.retain(|candidate| valid(candidate));
    grid
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).to_owned()).collect()
    }

    #[test]
    fn allowlist_accepts_only_known_bounded_pairs() {
        assert!(valid(&strings(&["-ngl", "20", "-fa", "on", "-t", "8"])));
        assert!(valid(&[]));
        for bad in [
            &["--api-key", "x"][..],
            &["-ngl"][..],
            &["-ngl", "-1"][..],
            &["-fa", "maybe"][..],
            &["-t", "8", "-t", "4"][..],
            &["-m", "/tmp/evil.gguf"][..],
            &["-b", "1"][..],
        ] {
            assert!(!valid(&strings(bad)), "{bad:?}");
        }
    }

    #[test]
    fn profiles_round_trip_per_model_size_and_hardware() {
        let directory = std::env::temp_dir().join(format!("fileid-tuning-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("model-tuning.json");
        let profile = Profile {
            model: "catalog-qwen".into(), model_bytes: 10, hardware: "linux-x86_64 · 4 cores".into(),
            arguments: strings(&["-ngl", "0", "-t", "4"]), measured_at: "2026-10-13".into(),
            generated_tokens_per_second: 20.0, baseline_tokens_per_second: 15.0,
        };
        save_in(&path, profile.clone()).unwrap();
        assert_eq!(lookup_in(&path, "catalog-qwen", 10, "linux-x86_64 · 4 cores"), Some(profile.arguments.clone()));
        assert_eq!(lookup_in(&path, "catalog-qwen", 11, "linux-x86_64 · 4 cores"), None, "a changed file never reuses a profile");
        assert_eq!(lookup_in(&path, "catalog-qwen", 10, "other machine"), None);
        let replacement = Profile { arguments: strings(&["-ngl", "0", "-t", "2"]), ..profile.clone() };
        save_in(&path, replacement).unwrap();
        assert_eq!(lookup_in(&path, "catalog-qwen", 10, "linux-x86_64 · 4 cores"), Some(strings(&["-ngl", "0", "-t", "2"])));
        assert!(save_in(&path, Profile { arguments: strings(&["--host", "0.0.0.0"]), ..profile }).is_err());
        std::fs::write(&path, br#"{"profiles":[{"model":"m","modelBytes":1,"hardware":"h","arguments":["--host","0.0.0.0"],"measuredAt":"","generatedTokensPerSecond":1,"baselineTokensPerSecond":1}]}"#).unwrap();
        assert_eq!(lookup_in(&path, "m", 1, "h"), None, "a tampered profile is ignored");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn candidate_grid_matches_the_machine() {
        let cpu = candidates(8, 0, 2_000_000_000, 36);
        assert!(cpu.iter().all(|c| c.windows(2).any(|w| w[0] == "-ngl" && w[1] == "0")));
        assert_eq!(cpu.len(), 6);
        let roomy = candidates(8, 24_576, 2_000_000_000, 36);
        assert!(roomy.iter().all(|c| c.contains(&"999".to_owned())));
        let tight = candidates(8, 6_144, 9_000_000_000, 40);
        let offloads: Vec<u32> = tight.iter().filter_map(|c| c.windows(2).find(|w| w[0] == "-ngl").map(|w| w[1].parse().unwrap())).collect();
        assert!(offloads.iter().all(|&n| n < 40), "partial offload when weights exceed VRAM: {offloads:?}");
        assert!(tight.iter().all(|c| valid(c)));
    }
}
