//! Subprocess wrapper around whisper.cpp's `whisper-cli` for on-device speech
//! transcription (Deep Analyze audio naming). Same shape as `vlm.rs` (llama.cpp): the
//! binary is a downloaded runtime pack under `%LOCALAPPDATA%\FileID\Models\whisper.cpp\`
//! and the ggml model under `…\Models\whisper\`. Both MIT (OpenAI Whisper + whisper.cpp
//! port) — commercial-clean. The engine feeds it the 16 kHz mono WAV that
//! `pipeline::audio_decode` produces; the transcript becomes the file's descriptive name.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

pub struct WhisperRunner {
    binary: PathBuf,
}

impl WhisperRunner {
    /// Locate `whisper-cli` (or the legacy `main`) under the whisper.cpp pack. Returns
    /// Err if the runtime pack isn't installed — callers fall back to metadata naming.
    pub fn find() -> Result<Self> {
        let root = crate::paths::models_dir().context("resolving Models dir")?;
        let dir = root.join("whisper.cpp");
        // The official release zip lays the CLI out under `Release\` (and recent builds
        // renamed `main` → `whisper-cli`); accept both names and the common subdirs.
        for name in ["whisper-cli", "main"] {
            let file = format!("{name}{BIN_EXT}");
            for sub in ["", "Release", "bin"] {
                let cand = if sub.is_empty() {
                    dir.join(&file)
                } else {
                    dir.join(sub).join(&file)
                };
                if cand.exists() {
                    return Ok(WhisperRunner { binary: cand });
                }
            }
        }
        bail!("whisper.cpp runtime not found under {}", dir.display())
    }

    /// The installed ggml whisper model — the largest `.bin` under `Models\whisper\`
    /// (a smaller quantized model and a larger one can coexist; prefer the larger /
    /// higher-quality). None when no model is installed.
    pub fn find_model() -> Option<PathBuf> {
        let root = crate::paths::models_dir().ok()?;
        let dir = root.join("whisper");
        let mut best: Option<(u64, PathBuf)> = None;
        for entry in std::fs::read_dir(&dir).ok()?.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some("bin") {
                let sz = entry.metadata().map(|m| m.len()).unwrap_or(0);
                if best.as_ref().map(|(b, _)| sz > *b).unwrap_or(true) {
                    best = Some((sz, p));
                }
            }
        }
        best.map(|(_, p)| p)
    }

    /// Transcribe a 16 kHz mono WAV (what `audio_decode::decode_to_wav16_mono` writes)
    /// to plain text — no timestamps, language auto-detected. Returns the collapsed
    /// transcript; Err on spawn/non-zero exit. Blocking (caller runs it on a blocking
    /// thread, like the VLM subprocess).
    pub fn transcribe(&self, model: &Path, wav: &Path) -> Result<String> {
        let out = Command::new(&self.binary)
            .arg("-m")
            .arg(model)
            .arg("-f")
            .arg(wav)
            .arg("-nt") // no timestamps — just the text
            .arg("-np") // no progress prints
            .arg("-l")
            .arg("auto") // auto-detect language
            .stdin(std::process::Stdio::null())
            .output()
            .with_context(|| format!("spawn {}", self.binary.display()))?;
        if !out.status.success() {
            bail!("whisper-cli exited with status {}", out.status);
        }
        Ok(collapse_transcript(&String::from_utf8_lossy(&out.stdout)))
    }
}

#[cfg(windows)]
const BIN_EXT: &str = ".exe";
#[cfg(not(windows))]
const BIN_EXT: &str = "";

/// Join whisper-cli's `-nt` stdout lines into one whitespace-collapsed string. Pure +
/// testable (the transcript → name path is unit-tested without the binary).
pub(crate) fn collapse_transcript(raw: &str) -> String {
    raw.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapse_transcript_joins_and_collapses_whitespace() {
        assert_eq!(
            collapse_transcript("  Hello   world  \n  this is a   test \n\n"),
            "Hello world this is a test"
        );
        assert_eq!(collapse_transcript("\n\n  \n"), "");
        assert_eq!(collapse_transcript("single"), "single");
    }
}
