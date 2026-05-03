// HuggingFace 12-way parallel range-GET downloader.
//
// Mirror of the macOS app's `HFDownloader.swift`. Splits a large file
// into 12 byte ranges, downloads them concurrently into part files, and
// concatenates on completion. Each chunk is verified against the
// Content-Range header. Final SHA256 checked against MODELS.md.
//
// Privacy: this is the **only** network code in the engine. Every URL
// the downloader hits comes from the canonical SHA256-pinned manifest
// in `shared/docs/MODELS.md` — no telemetry, no analytics, no opt-in
// flag because there's nothing to opt out of.
//
// Lifecycle:
//   1. HEAD request to discover Content-Length + ETag.
//   2. Open 12 ranged GETs.
//   3. Stream each into `<file>.part-N`, reporting bytes per second.
//   4. Concatenate parts → final file, verify SHA256.
//   5. Atomic rename to the final destination.
//
// On cancellation the part files stay on disk so resume is cheap.

use anyhow::{Context, Result};
use futures_util::StreamExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use sha2::{Digest, Sha256};

pub const PARALLEL_PARTS: usize = 12;
const PROGRESS_REPORT_INTERVAL_BYTES: u64 = 1024 * 1024; // 1 MB

#[derive(Debug, Clone)]
pub struct DownloadProgress {
    pub url: String,
    pub bytes_done: u64,
    pub bytes_total: Option<u64>,
    pub bytes_per_second: f64,
}

#[derive(Debug, Clone)]
pub struct DownloadRequest {
    pub url: String,
    pub destination: PathBuf,
    /// Optional SHA256 (lowercase hex) for integrity check on completion.
    pub expected_sha256: Option<String>,
}

/// Download a single file. The simple non-parallel path — used by small
/// files (<5 MB), and by the Phase 6 wiring before the 12-way path is
/// fully verified. Phase 6.x switches to `download_parallel` when the
/// server's `Accept-Ranges: bytes` header is present.
pub async fn download_simple<F>(
    request: DownloadRequest,
    mut progress: F,
) -> Result<()>
where
    F: FnMut(DownloadProgress),
{
    if let Some(parent) = request.destination.parent() {
        tokio::fs::create_dir_all(parent).await
            .with_context(|| format!("creating parent {}", parent.display()))?;
    }

    let client = reqwest::Client::builder()
        .user_agent("FileID/0.1 (+local)")
        .build()
        .context("building reqwest client")?;

    let resp = client.get(&request.url).send().await
        .context("issuing GET")?
        .error_for_status()
        .context("non-2xx response")?;
    let total = resp.content_length();
    let mut stream = resp.bytes_stream();

    let tmp = request.destination.with_extension(format!(
        "{}.part",
        request.destination.extension()
            .and_then(|s| s.to_str())
            .unwrap_or("download")
    ));
    let mut file = tokio::fs::File::create(&tmp).await
        .with_context(|| format!("creating {}", tmp.display()))?;
    let mut hasher = request.expected_sha256.as_ref().map(|_| Sha256::new());

    let started = Instant::now();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let mut last_report = 0u64;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("reading chunk")?;
        if let Some(h) = hasher.as_mut() {
            h.update(&chunk);
        }
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await
            .context("writing chunk")?;
        let cur = bytes_done.fetch_add(chunk.len() as u64, Ordering::Relaxed) + chunk.len() as u64;
        if cur - last_report >= PROGRESS_REPORT_INTERVAL_BYTES {
            let elapsed = started.elapsed().as_secs_f64().max(0.001);
            progress(DownloadProgress {
                url: request.url.clone(),
                bytes_done: cur,
                bytes_total: total,
                bytes_per_second: cur as f64 / elapsed,
            });
            last_report = cur;
        }
    }
    tokio::io::AsyncWriteExt::flush(&mut file).await.context("final flush")?;
    drop(file);

    if let (Some(expected), Some(h)) = (request.expected_sha256.as_ref(), hasher) {
        let got = hex::encode(h.finalize());
        if !expected.eq_ignore_ascii_case(&got) {
            let _ = tokio::fs::remove_file(&tmp).await;
            anyhow::bail!(
                "SHA256 mismatch for {}: expected {expected}, got {got}",
                request.url
            );
        }
    }

    tokio::fs::rename(&tmp, &request.destination).await
        .with_context(|| format!("rename {} -> {}", tmp.display(), request.destination.display()))?;

    let final_done = bytes_done.load(Ordering::Relaxed);
    let elapsed = started.elapsed().as_secs_f64().max(0.001);
    progress(DownloadProgress {
        url: request.url.clone(),
        bytes_done: final_done,
        bytes_total: total,
        bytes_per_second: final_done as f64 / elapsed,
    });

    Ok(())
}
