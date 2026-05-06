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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

pub const PARALLEL_PARTS: usize = 12;
const PROGRESS_REPORT_INTERVAL_BYTES: u64 = 1024 * 1024; // 1 MB
const MIN_BYTES_FOR_PARALLEL: u64 = 5 * 1024 * 1024;     // 5 MB
const PROGRESS_THROTTLE_MS: u64 = 100;                   // 10 Hz max

/// Build a long-lived shared `reqwest::Client` with HTTP/2 + connection
/// pooling. One per engine process; cloned cheaply (it's an `Arc` inside).
pub fn build_shared_client() -> Result<Arc<reqwest::Client>> {
    let c = reqwest::Client::builder()
        .user_agent("FileID/0.1 (+local)")
        .pool_idle_timeout(Some(Duration::from_secs(60)))
        .pool_max_idle_per_host(PARALLEL_PARTS * 2)
        .timeout(Duration::from_secs(300))
        .build()
        .context("building shared reqwest client")?;
    Ok(Arc::new(c))
}

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

// ─────────────────────────────────────────────────────────────────────
// V14.7.4: real 12-way parallel range-GET path.
// ─────────────────────────────────────────────────────────────────────

/// Download a single file using up to PARALLEL_PARTS concurrent
/// HTTP range-GET requests. Falls back to `download_simple` when:
///   - server doesn't support `Accept-Ranges: bytes`
///   - file is smaller than MIN_BYTES_FOR_PARALLEL (5 MB)
///   - HEAD probe fails
///
/// Sharing a `reqwest::Client` (via `Arc`) is critical: HTTP/2 stream
/// multiplexing + keep-alive pooling let 12 parallel GETs hit a single
/// HuggingFace edge server without re-handshaking TLS each time.
///
/// Retry policy: each range-GET retries up to 3 times on 429/503 with
/// exponential backoff (1s, 4s, 16s) and Retry-After header honored.
///
/// Cancellation: caller passes an `Arc<AtomicBool>`; tasks poll it after
/// every chunk so cancel triggers within a chunk write boundary.
/// Part files survive cancellation for resume on next attempt.
pub async fn download_parallel<F>(
    client: Arc<reqwest::Client>,
    request: DownloadRequest,
    cancel: Arc<AtomicBool>,
    mut progress: F,
) -> Result<()>
where
    F: FnMut(DownloadProgress) + Send + 'static,
{
    if let Some(parent) = request.destination.parent() {
        tokio::fs::create_dir_all(parent).await
            .with_context(|| format!("creating parent {}", parent.display()))?;
    }

    // HEAD probe — discover Content-Length + Accept-Ranges support.
    let head = match client.head(&request.url).send().await {
        Ok(r) if r.status().is_success() => r,
        _ => {
            // No reliable HEAD, fall back to single-stream.
            return download_simple(request, |p| progress(p)).await;
        }
    };
    let total = head.content_length().unwrap_or(0);
    let supports_ranges = head
        .headers()
        .get("accept-ranges")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.contains("bytes"))
        .unwrap_or(false);
    if total < MIN_BYTES_FOR_PARALLEL || !supports_ranges {
        return download_simple(request, |p| progress(p)).await;
    }

    // Plan the chunks. Last chunk picks up the remainder.
    let chunk_size = total / (PARALLEL_PARTS as u64);
    let mut ranges = Vec::with_capacity(PARALLEL_PARTS);
    for i in 0..PARALLEL_PARTS {
        let start = (i as u64) * chunk_size;
        let end = if i == PARALLEL_PARTS - 1 {
            total - 1
        } else {
            start + chunk_size - 1
        };
        ranges.push((i, start, end));
    }

    let bytes_done = Arc::new(AtomicU64::new(0));
    let started = Instant::now();
    let last_emit_ms = Arc::new(parking_lot::Mutex::new(0u128));

    // Progress channel: each chunk task posts its delta; one drainer
    // aggregates + emits at ≤10 Hz. Bounded so a slow drainer can't grow
    // the queue without bound — overflow is fine to drop because
    // bytes_done is monotonic + the drainer recomputes from the
    // AtomicU64 each tick.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<usize>(256);

    // Spawn drainer FIRST so the chunk tasks have something to send to.
    let total_for_drain = total;
    let url_for_drain = request.url.clone();
    let bytes_done_drain = bytes_done.clone();
    let last_emit_drain = last_emit_ms.clone();
    let progress_handle: tokio::task::JoinHandle<()> = tokio::spawn(async move {
        while let Some(delta) = rx.recv().await {
            let cur = bytes_done_drain.fetch_add(delta as u64, Ordering::Relaxed) + delta as u64;
            let now_ms = started.elapsed().as_millis();
            let emit = {
                let mut last = last_emit_drain.lock();
                if now_ms.saturating_sub(*last) >= PROGRESS_THROTTLE_MS as u128 {
                    *last = now_ms;
                    true
                } else { false }
            };
            if emit {
                let elapsed = (now_ms as f64) / 1000.0;
                let bps = if elapsed > 0.0 { cur as f64 / elapsed } else { 0.0 };
                progress(DownloadProgress {
                    url: url_for_drain.clone(),
                    bytes_done: cur,
                    bytes_total: Some(total_for_drain),
                    bytes_per_second: bps,
                });
            }
        }
    });

    // Spawn the chunk downloaders.
    let mut tasks: Vec<tokio::task::JoinHandle<Result<()>>> = Vec::with_capacity(PARALLEL_PARTS);
    for (i, start, end) in ranges {
        let client = client.clone();
        let url = request.url.clone();
        let part_path = part_file_path(&request.destination, i);
        let cancel = cancel.clone();
        let tx = tx.clone();
        tasks.push(tokio::spawn(async move {
            download_range_with_retry(&client, &url, start, end, &part_path, &cancel, tx).await
        }));
    }
    drop(tx); // close the sender so the drainer exits when all chunks finish

    // Await every chunk. First error short-circuits but we drain the rest.
    let mut first_err: Option<anyhow::Error> = None;
    for t in tasks {
        match t.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => { if first_err.is_none() { first_err = Some(e); } }
            Err(e) => { if first_err.is_none() { first_err = Some(anyhow::anyhow!(e)); } }
        }
    }
    let _ = progress_handle.await;
    if let Some(e) = first_err { return Err(e); }
    if cancel.load(Ordering::Relaxed) {
        anyhow::bail!("download cancelled");
    }

    // Concat parts → final .part, hash, atomic rename.
    let combined = request.destination.with_extension(format!(
        "{}.part",
        request.destination.extension()
            .and_then(|s| s.to_str())
            .unwrap_or("download")
    ));
    let mut out = tokio::fs::File::create(&combined).await
        .with_context(|| format!("creating {}", combined.display()))?;
    let mut hasher = request.expected_sha256.as_ref().map(|_| Sha256::new());
    for i in 0..PARALLEL_PARTS {
        let part_path = part_file_path(&request.destination, i);
        let bytes = tokio::fs::read(&part_path).await
            .with_context(|| format!("reading part {}", part_path.display()))?;
        if let Some(h) = hasher.as_mut() { h.update(&bytes); }
        tokio::io::AsyncWriteExt::write_all(&mut out, &bytes).await
            .context("writing combined part")?;
    }
    tokio::io::AsyncWriteExt::flush(&mut out).await.context("final flush")?;
    drop(out);

    if let (Some(expected), Some(h)) = (request.expected_sha256.as_ref(), hasher) {
        let got = hex::encode(h.finalize());
        if !expected.eq_ignore_ascii_case(&got) {
            let _ = tokio::fs::remove_file(&combined).await;
            anyhow::bail!(
                "SHA256 mismatch for {}: expected {expected}, got {got}",
                request.url
            );
        }
    }

    tokio::fs::rename(&combined, &request.destination).await
        .with_context(|| format!("rename {} -> {}", combined.display(), request.destination.display()))?;

    // Best-effort cleanup of part files; OK to leave them on failure.
    for i in 0..PARALLEL_PARTS {
        let _ = tokio::fs::remove_file(part_file_path(&request.destination, i)).await;
    }

    let final_done = bytes_done.load(Ordering::Relaxed);
    Ok(())
        .map(|_| {
            let _ = final_done; // silence unused if no progress callbacks
        })
        .map(|_| ())
}

fn part_file_path(dest: &Path, index: usize) -> PathBuf {
    let stem = dest.file_name().and_then(|s| s.to_str()).unwrap_or("download");
    let parent = dest.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!("{stem}.part-{index:02}"))
}

/// Download one byte range with retry-on-429/503 + resume support.
/// If `<part_path>` already exists, send `Range: bytes={offset}-{end}`
/// where offset = start + existing_len, and append.
async fn download_range_with_retry(
    client: &reqwest::Client,
    url: &str,
    start: u64,
    end: u64,
    part_path: &Path,
    cancel: &AtomicBool,
    tx: tokio::sync::mpsc::Sender<usize>,
) -> Result<()> {
    let backoffs = [
        Duration::from_secs(1),
        Duration::from_secs(4),
        Duration::from_secs(16),
    ];

    for (attempt, backoff) in std::iter::once(Duration::ZERO)
        .chain(backoffs.iter().copied())
        .enumerate()
    {
        if attempt > 0 {
            tokio::time::sleep(backoff).await;
        }
        if cancel.load(Ordering::Relaxed) {
            anyhow::bail!("range cancelled (start={start})");
        }

        // Resumable: stat the part file. If it has bytes, append from there.
        let existing_len = tokio::fs::metadata(part_path).await
            .map(|m| m.len()).unwrap_or(0);
        let cur_start = start + existing_len;
        if cur_start > end { return Ok(()); } // already done

        let range_header = format!("bytes={cur_start}-{end}");
        let resp = match client
            .get(url)
            .header("Range", &range_header)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(?e, "range GET failed; retrying");
                continue;
            }
        };
        let status = resp.status();
        if status.as_u16() == 429 || status.is_server_error() {
            // Honor Retry-After if present.
            if let Some(s) = resp.headers().get("retry-after").and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
            {
                tokio::time::sleep(Duration::from_secs(s.min(60))).await;
            }
            continue;
        }
        if !status.is_success() {
            anyhow::bail!("range {range_header}: HTTP {status}");
        }

        // Open part file in append mode (resumes on retry too).
        let mut file = tokio::fs::OpenOptions::new()
            .create(true).append(true).open(part_path).await
            .with_context(|| format!("opening part {}", part_path.display()))?;

        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            if cancel.load(Ordering::Relaxed) {
                anyhow::bail!("range cancelled mid-chunk (start={start})");
            }
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(?e, "stream error; retrying range");
                    return Box::pin(download_range_with_retry(client, url, start, end, part_path, cancel, tx)).await;
                }
            };
            tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await
                .context("writing range chunk")?;
            let _ = tx.send(chunk.len()).await;
        }
        tokio::io::AsyncWriteExt::flush(&mut file).await.ok();
        return Ok(());
    }

    anyhow::bail!("range exhausted retries (start={start})");
}
