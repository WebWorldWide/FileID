//! AI-model download driver for the global `D` shortcut.
//!
//! Installs the models the TUI uses in-process with the engine's SHA-256
//! verified Hugging Face downloader (`downloader::install_model_blocking`) on a
//! worker thread, streaming an install gauge + status line. The default models
//! come from `huggingface.co` — the only network egress the project ever makes
//! (see the no-telemetry principle).
//!
//! Non-blocking, mirroring [`crate::scan`]: the UI keeps drawing, `q` keeps
//! quitting (and cancels the download). On success it reloads the library and
//! sends [`LoadMsg::Done`]; any failure becomes a single [`LoadMsg::Error`].

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::SyncSender as Sender;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use fileid_engine::downloader::{install_model_blocking, InstallFileProgress};
use fileid_engine::models::registry::{lookup_full, LookupResult, Model};
use fileid_engine::util::display::{rate, size_pair};

use crate::data::{self, LoadMsg};

/// The non-VLM models the TUI uses (~1.6 GB): faces (`arcface` = YuNet +
/// SFace), image search/tags (`mobileclip_s2`, `ram_plus`) and text search
/// (`clip_text`, `bge_text`). The Deep-Analyze VLMs (~24 GB) are left out — the
/// TUI has no tab for them.
const DOWNLOAD_MODELS: [&str; 5] = [
    "arcface",
    "mobileclip_s2",
    "ram_plus",
    "clip_text",
    "bge_text",
];

/// Held by the UI thread so quitting can stop an in-flight download.
#[derive(Clone)]
pub struct DownloadHandle {
    cancelled: Arc<AtomicBool>,
}

impl DownloadHandle {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

/// Start the download on a worker thread. Non-blocking. On success reloads
/// `db` and sends `Done` (which clears the gauge); any failure becomes a single
/// `LoadMsg::Error`.
pub fn spawn_download(
    db: PathBuf,
    query: String,
    tx: Sender<LoadMsg>,
    generation: u64,
) -> DownloadHandle {
    let handle = DownloadHandle {
        cancelled: Arc::new(AtomicBool::new(false)),
    };
    let cancelled = Arc::clone(&handle.cancelled);
    std::thread::spawn(move || match run_download(&tx, &cancelled) {
        Ok(()) => {
            let _ = tx.send(LoadMsg::Status(
                "AI models installed — refreshing…".to_string(),
            ));
            // Reloading never adds files (it fetched weights, not library rows),
            // but it refreshes state and clears the `downloading`/`loading`
            // flags. A not-yet-created scratch DB loads as an empty snapshot.
            match data::load(&db, &query, &tx, generation) {
                Ok(snap) => {
                    let _ = tx.send(LoadMsg::Status(
                        "AI models ready. Press s to scan a folder with full AI.".to_string(),
                    ));
                    let _ = data::send_versioned(&tx, generation, LoadMsg::Done(Box::new(snap)));
                    data::run_deferred_dupes(&db, &tx, generation);
                }
                Err(e) => {
                    let _ = data::send_versioned(
                        &tx,
                        generation,
                        LoadMsg::Error(format!("models installed, but reload failed: {e}")),
                    );
                }
            }
        }
        Err(e) => {
            let _ = data::send_versioned(&tx, generation, LoadMsg::Error(format!("{e:#}")));
        }
    });
    handle
}

fn run_download(tx: &Sender<LoadMsg>, cancelled: &Arc<AtomicBool>) -> Result<()> {
    let ensure_live = || {
        anyhow::ensure!(
            !cancelled.load(Ordering::Acquire),
            "model download was cancelled"
        );
        Ok(())
    };
    ensure_live()?;
    let models = DOWNLOAD_MODELS
        .iter()
        .map(|kind| match lookup_full(kind) {
            LookupResult::Found(model) => Ok(model),
            LookupResult::Unknown => anyhow::bail!("model {kind} is not in the engine registry"),
        })
        .collect::<Result<Vec<Model>>>()?;
    let sizes: Vec<u64> = models
        .iter()
        .map(|model| model.files.iter().map(|file| file.approx_bytes).sum())
        .collect();
    let total: u64 = sizes.iter().sum();
    let _ = tx.send(LoadMsg::Status(
        "Downloading AI models from huggingface.co…".to_string(),
    ));

    let shown = Arc::new(AtomicU64::new(0));
    let mut completed = 0u64;
    for (index, (model, size)) in models.iter().zip(&sizes).enumerate() {
        ensure_live()?;
        let progress = progress_sink(
            tx.clone(),
            Arc::clone(&shown),
            model.id,
            index,
            completed,
            *size,
            total,
        );
        install_model_blocking(model, Arc::clone(cancelled), progress).with_context(|| {
            format!(
                "installing {} — check your network and free disk space, then retry",
                model.id
            )
        })?;
        ensure_live()?;
        completed += size;
        let _ = tx.send(LoadMsg::Status(format!("✓ {} installed", model.id)));
    }
    let _ = tx.send(LoadMsg::DownloadProgress {
        percent: 100,
        label: "done".to_string(),
    });
    Ok(())
}

/// Per-model progress callback: byte-weighted overall percent (kept monotonic
/// across concurrently reported bundle files) plus a short label. Updates are
/// dropped rather than blocking the downloader when the UI channel is full.
fn progress_sink(
    tx: Sender<LoadMsg>,
    shown: Arc<AtomicU64>,
    name: &'static str,
    index: usize,
    completed: u64,
    size: u64,
    total: u64,
) -> Arc<dyn Fn(InstallFileProgress) + Send + Sync> {
    Arc::new(move |p: InstallFileProgress| {
        let raw = overall_percent(completed, size, total, &p);
        let percent = shown.fetch_max(raw, Ordering::Relaxed).max(raw) as u16;
        let label = format!(
            "{name} · {} · {} · model {}/{}",
            size_pair(p.bytes_done, p.bytes_total),
            rate(p.bytes_per_second),
            index + 1,
            DOWNLOAD_MODELS.len(),
        );
        let _ = tx.try_send(LoadMsg::DownloadProgress { percent, label });
    })
}

fn overall_percent(completed: u64, size: u64, total: u64, p: &InstallFileProgress) -> u64 {
    let within = match p.bytes_total {
        Some(t) if t > 0 => (p.bytes_done as f64 / t as f64).clamp(0.0, 1.0),
        _ => 0.0,
    };
    let file_fraction = if p.file_count == 0 {
        0.0
    } else {
        ((p.file_index as f64 + within) / p.file_count as f64).clamp(0.0, 1.0)
    };
    let done = completed as f64 + size as f64 * file_fraction;
    ((done / total.max(1) as f64).clamp(0.0, 1.0) * 100.0).floor() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(
        file_index: usize,
        file_count: usize,
        done: u64,
        total: Option<u64>,
    ) -> InstallFileProgress {
        InstallFileProgress {
            file_index,
            file_count,
            file_name: "model.onnx".to_string(),
            bytes_done: done,
            bytes_total: total,
            bytes_per_second: 0.0,
        }
    }

    #[test]
    fn downloads_only_the_tui_models_never_the_vlms() {
        for vlm in ["mistral_small_3_2", "qwen2_5_vl_7b", "gemma_3_4b"] {
            assert!(!DOWNLOAD_MODELS.contains(&vlm), "must not download {vlm}");
        }
        for required in fileid_engine::models::required::REQUIRED_SCAN_MODELS {
            assert!(
                DOWNLOAD_MODELS.contains(&required),
                "{required} is needed to scan"
            );
        }
    }

    #[test]
    fn cancelled_download_stops_before_any_network() {
        let (tx, rx) = std::sync::mpsc::sync_channel(8);
        let error = run_download(&tx, &Arc::new(AtomicBool::new(true))).unwrap_err();
        assert!(error.to_string().contains("cancelled"));
        assert!(rx.try_recv().is_err(), "nothing was started");
    }

    #[test]
    fn overall_percent_is_byte_weighted_and_bounded() {
        assert_eq!(
            overall_percent(0, 100, 400, &progress(0, 1, 50, Some(100))),
            12
        );
        assert_eq!(
            overall_percent(100, 300, 400, &progress(1, 2, 150, Some(150))),
            100
        );
        assert_eq!(overall_percent(0, 0, 0, &progress(0, 0, 0, None)), 0);
    }

    #[test]
    fn progress_updates_never_go_backwards_or_block() {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let shown = Arc::new(AtomicU64::new(0));
        let sink = progress_sink(tx, shown, "arcface", 1, 0, 100, 100);
        sink(progress(0, 1, 80, Some(100)));
        sink(progress(0, 1, 10, Some(100)));
        let Ok(LoadMsg::DownloadProgress { percent, label }) = rx.try_recv() else {
            panic!("first update must arrive");
        };
        assert_eq!(percent, 80);
        assert!(
            label.starts_with("arcface · ") && label.ends_with("model 2/5"),
            "{label}"
        );
        sink(progress(0, 1, 20, Some(100)));
        let Ok(LoadMsg::DownloadProgress { percent, .. }) = rx.try_recv() else {
            panic!("later update must arrive");
        };
        assert_eq!(percent, 80);
    }
}
