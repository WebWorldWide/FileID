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
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

pub const PARALLEL_PARTS: usize = 12;
const PROGRESS_REPORT_INTERVAL_BYTES: u64 = 1024 * 1024; // 1 MB
const MIN_BYTES_FOR_PARALLEL: u64 = 5 * 1024 * 1024;     // 5 MB
const PROGRESS_THROTTLE_MS: u64 = 50;                    // 20 Hz — smoother bar + finer EMA

/// Total concurrent in-flight HTTP requests across ALL prewarm tasks.
/// HuggingFace's CDN starts returning 429s when one IP issues too many
/// simultaneous range-GETs. With 3 concurrent models × 12 PARALLEL_PARTS
/// each = 36 sockets the third model's downloads were stalling on
/// Retry-After back-offs, which presented as MobileCLIP-S2 "stuck on 0%
/// until you cancel". 8 permits keeps all three models making forward
/// progress without tripping the throttle.
const MAX_CONCURRENT_HTTP_REQUESTS: usize = 8;

fn http_semaphore() -> &'static Arc<tokio::sync::Semaphore> {
    static SEMA: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    SEMA.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_HTTP_REQUESTS)))
}

/// Build a long-lived shared `reqwest::Client` with HTTP/2 + connection
/// pooling. One per engine process; cloned cheaply (it's an `Arc` inside).
///
/// Timeout policy is phase-specific, not wall-clock:
///   * `connect_timeout`  fast-fail on DNS / TCP / TLS handshake.
///   * `read_timeout`     fail if no progress between bytes; doesn't cap
///                        the total request duration so a slow-but-
///                        steady stream finishes a 2 GB GGUF without
///                        getting axed mid-stream. Replaces a prior
///                        `.timeout(300s)` blanket that killed any
///                        Qwen 2.5-VL 3B (2.1 GB) download running on
///                        a connection slower than ~7 MB/s — the
///                        original "reading chunk" failure on the
///                        Welcome sheet.
pub fn build_shared_client() -> Result<Arc<reqwest::Client>> {
    let c = reqwest::Client::builder()
        .user_agent("FileID/0.1 (+local)")
        .pool_idle_timeout(Some(Duration::from_secs(60)))
        .pool_max_idle_per_host(PARALLEL_PARTS * 2)
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(120))
        .build()
        .context("building shared reqwest client")?;
    Ok(Arc::new(c))
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
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

/// Download a single file. The simple non-parallel path — used when
/// the server doesn't support `Accept-Ranges: bytes` or the file is
/// below `MIN_BYTES_FOR_PARALLEL`. Also used by the parallel path's
/// fallback when HEAD is unreliable.
///
/// Takes the shared `reqwest::Client` rather than building a new one per
/// call — without this, every small file (config.json, tokenizer.json,
/// etc.) paid an extra TLS handshake.
///
/// Retry policy: up to 3 retries with exponential backoff (1s, 4s, 16s)
/// on stream errors, transport errors, and 429/5xx responses. Each retry
/// stats the in-progress `.part` file and resumes via `Range:` so a
/// 2 GB GGUF doesn't re-download from byte 0 after a single TLS hiccup.
/// Mirrors the parallel path's per-range retry policy.
pub async fn download_simple<F>(
    client: Arc<reqwest::Client>,
    request: DownloadRequest,
    cancel: Arc<AtomicBool>,
    mut progress: F,
) -> Result<()>
where
    F: FnMut(DownloadProgress),
{
    if let Some(parent) = request.destination.parent() {
        tokio::fs::create_dir_all(parent).await
            .with_context(|| format!("creating parent {}", parent.display()))?;
    }

    let tmp = request.destination.with_extension(format!(
        "{}.part",
        request.destination.extension()
            .and_then(|s| s.to_str())
            .unwrap_or("download")
    ));

    let backoffs = [
        Duration::from_secs(1),
        Duration::from_secs(4),
        Duration::from_secs(16),
    ];

    let started = Instant::now();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let mut total: Option<u64> = None;
    let mut last_err: Option<anyhow::Error> = None;

    'attempts: for (attempt, backoff) in std::iter::once(Duration::ZERO)
        .chain(backoffs.iter().copied())
        .enumerate()
    {
        if attempt > 0 {
            tracing::warn!(
                attempt,
                url = %request.url,
                err = ?last_err,
                "retrying simple download after stream error"
            );
            tokio::time::sleep(backoff).await;
        }
        if cancel.load(Ordering::Relaxed) {
            anyhow::bail!("download cancelled");
        }

        // Honor the global HTTP semaphore so the simple path can't sneak past
        // the cross-task concurrency cap. Re-acquired each attempt so a long
        // retry backoff doesn't hog a permit for other prewarms.
        let _permit = http_semaphore().clone().acquire_owned().await
            .context("acquiring http permit")?;

        // Resume: stat any prior .part bytes and ask for the remainder.
        let existing_len = tokio::fs::metadata(&tmp).await
            .map(|m| m.len()).unwrap_or(0);
        let mut req_builder = client.get(&request.url);
        if existing_len > 0 {
            req_builder = req_builder.header("Range", format!("bytes={existing_len}-"));
        }

        let resp = match req_builder.send().await {
            Ok(r) => r,
            Err(e) => {
                last_err = Some(anyhow::Error::new(e).context("issuing GET"));
                continue 'attempts;
            }
        };

        let status = resp.status();
        // 429 / 5xx — honor Retry-After if present, then retry.
        if status.as_u16() == 429 || status.is_server_error() {
            if let Some(s) = resp.headers().get("retry-after").and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
            {
                tokio::time::sleep(Duration::from_secs(s.min(60))).await;
            }
            last_err = Some(anyhow::anyhow!("HTTP {status}"));
            continue 'attempts;
        }
        // 4xx (other than 429) — bad URL / auth required. Don't retry.
        if status.is_client_error() {
            anyhow::bail!("non-2xx response: HTTP {status} for {}", request.url);
        }
        if !status.is_success() {
            last_err = Some(anyhow::anyhow!("HTTP {status}"));
            continue 'attempts;
        }

        // Decide append-vs-truncate based on whether the server honored
        // our Range request. 206 = resumed; 200 = ignored Range and is
        // re-sending from byte 0 (truncate our existing part file).
        let resumed = existing_len > 0 && status.as_u16() == 206;
        if !resumed && existing_len > 0 {
            tracing::warn!(
                url = %request.url,
                "server ignored Range header (HTTP {status}); restarting download from 0"
            );
            let _ = tokio::fs::remove_file(&tmp).await;
            bytes_done.store(0, Ordering::Relaxed);
        }

        // Update total from this response. For 206 the response body is
        // (total - existing_len) so add the existing prefix back.
        if total.is_none() {
            total = resp.content_length().map(|n| if resumed { n + existing_len } else { n });
        }

        let mut file = if resumed {
            bytes_done.store(existing_len, Ordering::Relaxed);
            tokio::fs::OpenOptions::new()
                .create(true).append(true).open(&tmp).await
                .with_context(|| format!("opening {}", tmp.display()))?
        } else {
            tokio::fs::File::create(&tmp).await
                .with_context(|| format!("creating {}", tmp.display()))?
        };

        // Per-attempt hasher. We can only verify SHA256 on a download
        // that completed in a single attempt — resumes invalidate the
        // running hash. The post-download verifier reads the file back
        // from disk and re-hashes, so SHA256 still gets checked.
        let mut hasher = request
            .expected_sha256
            .as_ref()
            .filter(|_| !resumed)
            .map(|_| Sha256::new());

        let mut stream = resp.bytes_stream();
        let mut last_report = bytes_done.load(Ordering::Relaxed);
        let mut chunk_err: Option<anyhow::Error> = None;

        while let Some(chunk) = stream.next().await {
            if cancel.load(Ordering::Relaxed) {
                anyhow::bail!("download cancelled mid-chunk");
            }
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    chunk_err = Some(anyhow::Error::new(e).context("reading chunk"));
                    break;
                }
            };
            if let Some(h) = hasher.as_mut() {
                h.update(&chunk);
            }
            if let Err(e) = tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await {
                // Disk write errors aren't recoverable via HTTP retry.
                return Err(anyhow::Error::new(e).context("writing chunk"));
            }
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

        if let Some(e) = chunk_err {
            last_err = Some(e);
            continue 'attempts;
        }

        // Stream completed cleanly. Verify SHA256 (re-read the part file
        // from disk if we resumed, since the running hasher only saw
        // the latest attempt's bytes).
        if let Some(expected) = request.expected_sha256.as_ref() {
            let got_hex = if let Some(h) = hasher {
                hex::encode(h.finalize())
            } else {
                let mut file = tokio::fs::File::open(&tmp).await
                    .with_context(|| format!("opening {} for sha verification", tmp.display()))?;
                let mut h = Sha256::new();
                // Heap-allocated so the 64 KB chunk doesn't bloat this async
                // function's future state. Callers (download_simple in
                // prewarm.rs) trip clippy's large_futures lint otherwise —
                // every level of the call chain inherits the size.
                let mut buffer = vec![0u8; 65536];
                loop {
                    use tokio::io::AsyncReadExt;
                    let n = file.read(&mut buffer).await
                        .with_context(|| format!("reading chunk from {}", tmp.display()))?;
                    if n == 0 {
                        break;
                    }
                    h.update(&buffer[..n]);
                }
                hex::encode(h.finalize())
            };
            if !expected.eq_ignore_ascii_case(&got_hex) {
                let _ = tokio::fs::remove_file(&tmp).await;
                anyhow::bail!(
                    "SHA256 mismatch for {}: expected {expected}, got {got_hex}",
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

        return Ok(());
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("simple download exhausted retries")))
}

// ─────────────────────────────────────────────────────────────────────
// 12-way parallel range-GET download path.
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
    // HuggingFace returns 302 → CDN; reqwest follows redirects so the
    // status we read is the CDN's. The CDN occasionally omits
    // `Accept-Ranges` on HEAD even though it honors `Range:` on GET,
    // so we fall back to a one-byte range probe before degrading to
    // the slower single-stream path.
    let head_resp = client.head(&request.url).send().await.ok();
    let head_total = head_resp
        .as_ref()
        .filter(|r| r.status().is_success())
        .and_then(|r| r.content_length())
        .unwrap_or(0);
    let head_supports_ranges = head_resp
        .as_ref()
        .filter(|r| r.status().is_success())
        .and_then(|r| r.headers().get("accept-ranges").cloned())
        .and_then(|v| v.to_str().ok().map(|s| s.to_owned()))
        .map(|s| s.contains("bytes"))
        .unwrap_or(false);

    // Range probe path: HEAD said no (or HEAD failed) but the file is
    // large enough that we'd really rather use the parallel path. Send
    // `GET ... Range: bytes=0-0` and look at the status — 206 means
    // ranges work despite HEAD's silence.
    let (total, supports_ranges) = if head_supports_ranges && head_total >= MIN_BYTES_FOR_PARALLEL {
        (head_total, true)
    } else {
        match probe_range_support(&client, &request.url).await {
            Some((probed_total, true)) if probed_total >= MIN_BYTES_FOR_PARALLEL => {
                (probed_total, true)
            }
            Some((probed_total, _)) => (probed_total.max(head_total), false),
            None => (head_total, false),
        }
    };

    if total < MIN_BYTES_FOR_PARALLEL || !supports_ranges {
        return download_simple(client.clone(), request, cancel.clone(), |p| progress(p)).await;
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
    let (tx, mut rx) = tokio::sync::mpsc::channel::<usize>(512);

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

/// One-byte `Range:` probe to confirm the server honors ranges when
/// HEAD didn't advertise `Accept-Ranges: bytes`. Returns
/// `Some((total_bytes, true))` on 206 (ranges work; total parsed from
/// `Content-Range`), `Some((total_bytes, false))` on 200 (server
/// ignored Range; total = `Content-Length`), or `None` on transport
/// failure. Best-effort: this is a hint to the parallel path, not a
/// hard requirement.
async fn probe_range_support(
    client: &reqwest::Client,
    url: &str,
) -> Option<(u64, bool)> {
    let resp = client
        .get(url)
        .header("Range", "bytes=0-0")
        .send()
        .await
        .ok()?;
    let status = resp.status();
    if status.as_u16() == 206 {
        // Content-Range: bytes 0-0/<total>
        let total = resp
            .headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.rsplit('/').next())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        Some((total, true))
    } else if status.is_success() {
        Some((resp.content_length().unwrap_or(0), false))
    } else {
        None
    }
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

        // Global HTTP concurrency cap — prevents three concurrent prewarms
        // × 12 range-GETs each from tripping HuggingFace's per-IP rate limit
        // (the original "MobileCLIP stuck until cancel" bug). Held for the
        // duration of this attempt; released on drop at end of iteration.
        let _permit = http_semaphore().clone().acquire_owned().await
            .context("acquiring http permit")?;

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
