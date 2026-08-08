//! Subprocess wrapper around whisper.cpp's `whisper-cli` for on-device speech
//! transcription (Deep Analyze audio naming). Same shape as `vlm.rs` (llama.cpp): the
//! binary is a downloaded runtime pack under `%LOCALAPPDATA%\FileID\Models\whisper.cpp\`
//! and the ggml model under `…\Models\whisper\`. Both MIT (OpenAI Whisper + whisper.cpp
//! port) — commercial-clean. The engine feeds it the 16 kHz mono WAV that
//! `pipeline::audio_decode` produces; the transcript becomes the file's descriptive name.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{bail, Context, Result};

/// Hard ceiling on one transcription. The decoded WAV is capped at `audio_decode`'s
/// `MAX_SECONDS`; whisper.cpp base runs ~1–2× real-time on CPU, and naming only needs the
/// leading words, so a few minutes is ample — past it the file is pathological and we kill
/// it rather than wedge a worker.
const TRANSCRIBE_TIMEOUT: Duration = Duration::from_secs(180);

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
    /// transcript; Err on spawn/non-zero exit/timeout. Blocking (caller runs it on a
    /// blocking thread, like the VLM subprocess).
    ///
    /// Hard-bounded by `TRANSCRIBE_TIMEOUT`: whisper-cli is killed if it hasn't finished,
    /// so a pathological/garbage audio file (or a hung binary) can't pin a blocking-pool
    /// thread forever — the caller then falls back to metadata naming. Mirrors the
    /// kill-on-stall discipline of the VLM subprocess (`vlm::caption`). stdout is drained
    /// on a reader thread so a chatty child never fills the pipe and blocks.
    pub fn transcribe(&self, model: &Path, wav: &Path, cancel: &AtomicBool) -> Result<String> {
        use std::io::Read;
        let mut command = Command::new(&self.binary);
        command
            .arg("-m")
            .arg(model)
            .arg("-f")
            .arg(wav)
            .arg("-nt") // no timestamps — just the text
            .arg("-np") // no progress prints
            .arg("-l")
            .arg("auto") // auto-detect language
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        crate::platform::configure_child_lifetime(&mut command);
        let mut child = command
            .spawn()
            .with_context(|| format!("spawn {}", self.binary.display()))?;
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            crate::platform::assign_child_to_engine_job(child.as_raw_handle());
        }
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("whisper child stdout pipe was unavailable"))?;
        let reader = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = stdout.read_to_string(&mut s);
            s
        });
        let text = wait_for_transcript(child, reader, cancel, TRANSCRIBE_TIMEOUT)?;
        Ok(collapse_transcript(&text))
    }
}

fn wait_for_transcript(
    mut child: Child,
    reader: std::thread::JoinHandle<String>,
    cancel: &AtomicBool,
    timeout: Duration,
) -> Result<String> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if cancel.load(Ordering::Relaxed) {
            crate::platform::terminate_child_tree(&mut child);
            let _ = reader.join();
            bail!("cancelled");
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let text = reader.join().unwrap_or_default();
                if !status.success() {
                    bail!("whisper-cli exited with status {}", status);
                }
                return Ok(text);
            }
            Ok(None) => {}
            Err(error) => {
                crate::platform::terminate_child_tree(&mut child);
                let _ = reader.join();
                return Err(error).context("whisper-cli try_wait");
            }
        }
        if std::time::Instant::now() >= deadline {
            crate::platform::terminate_child_tree(&mut child);
            let _ = reader.join();
            bail!("whisper-cli timed out after {:?}", timeout);
        }
        std::thread::sleep(Duration::from_millis(100));
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
    fn cancellation_kills_and_reaps_child() {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("models::whisper::tests::whisper_test_child")
            .arg("--nocapture")
            .env("FILEID_WHISPER_TEST_CHILD", "1")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let reader = std::thread::spawn(move || {
            use std::io::Read as _;
            let mut text = String::new();
            let _ = stdout.read_to_string(&mut text);
            text
        });
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let cancel_for_thread = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            cancel_for_thread.store(true, Ordering::Relaxed);
        });
        let started = std::time::Instant::now();
        let error = wait_for_transcript(child, reader, &cancel, Duration::from_secs(30))
            .unwrap_err();
        assert!(error.to_string().contains("cancelled"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn whisper_test_child() {
        if std::env::var("FILEID_WHISPER_TEST_CHILD").as_deref() != Ok("1") {
            return;
        }
        loop {
            std::thread::sleep(Duration::from_secs(60));
        }
    }

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
