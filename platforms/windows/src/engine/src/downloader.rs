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
use std::error::Error;
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

/// CA-allowlist TLS pinning (SECURITY.md hardening item; documented in
/// shared/security/tls-pins.json). These PEMs become the ONLY trust anchors
/// on the download client, so an active MITM holding any other OS-trusted CA
/// (interception proxy, compromised CA) fails the handshake instead of
/// silently re-signing the connection. Root-level pins, not leaf/intermediate:
/// leaves rotate ~90 days and the CDNs move between CA families; these roots
/// are stable for decades. Defense-in-depth alongside per-artifact SHA256.
const PINNED_ROOT_CERTS: [(&str, &[u8]); 11] = [
    ("amazon-root-ca-1", include_bytes!("../../../../../shared/security/pinned-roots/amazon-root-ca-1.pem")),
    ("amazon-root-ca-2", include_bytes!("../../../../../shared/security/pinned-roots/amazon-root-ca-2.pem")),
    ("amazon-root-ca-3", include_bytes!("../../../../../shared/security/pinned-roots/amazon-root-ca-3.pem")),
    ("amazon-root-ca-4", include_bytes!("../../../../../shared/security/pinned-roots/amazon-root-ca-4.pem")),
    ("digicert-global-g2", include_bytes!("../../../../../shared/security/pinned-roots/digicert-global-g2.pem")),
    ("digicert-global-g3", include_bytes!("../../../../../shared/security/pinned-roots/digicert-global-g3.pem")),
    ("isrg-root-x1", include_bytes!("../../../../../shared/security/pinned-roots/isrg-root-x1.pem")),
    ("isrg-root-x2", include_bytes!("../../../../../shared/security/pinned-roots/isrg-root-x2.pem")),
    ("starfield-services-g2", include_bytes!("../../../../../shared/security/pinned-roots/starfield-services-g2.pem")),
    ("usertrust-ecc", include_bytes!("../../../../../shared/security/pinned-roots/usertrust-ecc.pem")),
    ("usertrust-rsa", include_bytes!("../../../../../shared/security/pinned-roots/usertrust-rsa.pem")),
];

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
///                        Welcome sheet. Set to 60 s (not 120 s) so a
///                        genuinely stalled stream errors → resumes via
///                        download_range_with_retry within ~61 s, which
///                        re-emits progress and re-arms the app's 120 s
///                        no-progress install watchdog BEFORE it alarms
///                        the user mid-recovery. 60 s of total silence is
///                        a dead connection, never a healthy slow one
///                        (any byte resets the timer).
///
/// TLS trust is pinned to the roots in `PINNED_ROOT_CERTS` (built-in roots
/// disabled), per shared/security/tls-pins.json. `FILEID_DISABLE_TLS_PINNING=1`
/// reverts to the OS root store — validation only ever changes, never the
/// egress surface — and is logged loudly as a diagnostic escape hatch for
/// networks that intercept HTTPS.
pub fn build_shared_client() -> Result<Arc<reqwest::Client>> {
    // Restrict redirects to the host families we actually download from (HF +
    // its CDN, GitHub + its objects CDN, NVIDIA). reqwest's default follows up to
    // 10 redirects to ANY host — an on-path attacker could bounce a 302 chain to
    // an off-allowlist host, dodging the source-URL allowlist that only checks
    // the ORIGINAL URL. Suffix-match with a leading dot for subdomains so
    // "evilhuggingface.co" never matches ".huggingface.co".
    const REDIRECT_ALLOWED: &[&str] = &[
        "huggingface.co",
        "hf.co",
        "github.com",
        "githubusercontent.com",
        "download.nvidia.com",
        "developer.nvidia.com",
    ];
    let redirect_policy = reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= 10 {
            return attempt.stop();
        }
        // Never follow a redirect off https. The host allowlist alone would let
        // a 302 downgrade to plaintext http:// on an allowlisted host, which an
        // on-path attacker could MITM. Every allowlisted CDN serves https, so
        // this never blocks a legitimate redirect. (audit E11)
        if attempt.url().scheme() != "https" {
            return attempt.stop();
        }
        match attempt.url().host_str() {
            Some(h)
                if REDIRECT_ALLOWED
                    .iter()
                    .any(|d| h == *d || h.ends_with(&format!(".{d}"))) =>
            {
                attempt.follow()
            }
            _ => attempt.stop(),
        }
    });
    let mut builder = reqwest::Client::builder()
        .user_agent("FileID/0.1 (+local)")
        .pool_idle_timeout(Some(Duration::from_secs(60)))
        .pool_max_idle_per_host(PARALLEL_PARTS * 2)
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(60))
        .redirect(redirect_policy);
    if matches!(std::env::var("FILEID_DISABLE_TLS_PINNING").as_deref(), Ok("1")) {
        tracing::warn!(
            "FILEID_DISABLE_TLS_PINNING=1 — CA-allowlist TLS pinning is DISABLED; model \
             downloads will trust the OS root store, including any locally installed \
             interception/proxy CA. Diagnostic escape hatch only; unset the variable to \
             restore pinning."
        );
    } else {
        builder = builder.tls_built_in_root_certs(false);
        for (slug, pem) in PINNED_ROOT_CERTS {
            builder = builder.add_root_certificate(
                reqwest::Certificate::from_pem(pem)
                    .with_context(|| format!("parsing pinned root CA '{slug}'"))?,
            );
        }
    }
    let c = builder.build().context("building shared reqwest client")?;
    Ok(Arc::new(c))
}

fn message_indicates_pin_failure(msg: &str) -> bool {
    msg.contains("UnknownIssuer") || msg.contains("invalid peer certificate")
}

/// rustls reports a chain that doesn't terminate at a pinned root as
/// `invalid peer certificate: UnknownIssuer`, but only as Display text buried
/// in the reqwest → hyper → io source chain (no typed variant survives the
/// wrapping), so a string match over the chain is the only stable detection.
fn source_chain_indicates_pin_failure(err: &(dyn Error + 'static)) -> bool {
    let mut cur: Option<&(dyn Error + 'static)> = Some(err);
    while let Some(e) = cur {
        if message_indicates_pin_failure(&e.to_string()) {
            return true;
        }
        cur = e.source();
    }
    false
}

pub fn is_pin_failure(err: &reqwest::Error) -> bool {
    source_chain_indicates_pin_failure(err)
}

/// Pin detection for the `anyhow::Error`s this module returns: `chain()`
/// descends through every context layer and the wrapped source errors, so
/// this sees the rustls message wherever the retry loops buried it.
pub fn chain_has_pin_failure(err: &anyhow::Error) -> bool {
    err.chain().any(|e| match e.downcast_ref::<reqwest::Error>() {
        Some(re) => is_pin_failure(re),
        None => message_indicates_pin_failure(&e.to_string()),
    })
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
    /// Registry size estimate (`approx_bytes`) for a loose post-download size
    /// sanity check — catches a truncated stream / HTML error page standing in
    /// for a model even when no SHA256 is pinned. An estimate, so only an
    /// implausibly-small result is rejected (see `check_size_plausible`).
    pub expected_bytes: Option<u64>,
}

/// Loose post-download size sanity. `approx_bytes` in the registry is an
/// ESTIMATE, so we reject only implausibly-small results: a truncated stream,
/// an HTML error page, or an auth wall standing in for a multi-GB model are
/// orders of magnitude off, not a few percent. A 4× floor never false-rejects
/// a reasonable estimate but always catches a KB-for-GB substitution. (SHA256,
/// when pinned, is the exact check; this guards the common no-hash case.)
fn check_size_plausible(actual: u64, expected: Option<u64>, url: &str) -> Result<()> {
    if let Some(expected) = expected {
        if expected > 0 {
            let floor = (expected / 4).max(1);
            if actual < floor {
                anyhow::bail!(
                    "size sanity failed for {url}: got {actual} bytes, expected \
                     ~{expected} (floor {floor}) — likely a truncated download or \
                     an error page, not the model"
                );
            }
        }
    }
    Ok(())
}

const RETRY_BACKOFFS: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(4),
    Duration::from_secs(16),
];
const MAX_ATTEMPTS_WITHOUT_PROGRESS: u32 = 4;

/// Progress-aware retry budget shared by `download_simple` and
/// `download_range_with_retry`. A fixed 4-attempt budget combined with the
/// client's 60 s `read_timeout` to hard-fail a slow-but-progressing download
/// (large GGUF on spotty wifi) after 4 stalls, even though every attempt
/// resumed further along. Only attempts that left the part file un-grown burn
/// budget — growth observed at the top of the next attempt resets the
/// counter — so a long flaky download survives any number of stalls while it
/// keeps moving. Zero-progress behavior is unchanged from H13 (same ladder,
/// bail after `MAX_ATTEMPTS_WITHOUT_PROGRESS`): a dead connection still can't
/// spin forever. The ladder is indexed by total failures, repeating its last
/// step, so a chronically flaky link settles into 16 s pauses instead of
/// hammering 1 s retries after every reset. Growth is judged against a
/// high-water mark, not the previous stat, so a discard-and-refetch cycle
/// (416 / non-206 resume / oversized stale part) can't refund budget by
/// re-downloading the same bytes. (audit C3)
struct RetryBudget {
    total_failures: u32,
    attempts_without_progress: u32,
    high_water_len: u64,
}

impl RetryBudget {
    fn new() -> Self {
        Self {
            total_failures: 0,
            attempts_without_progress: 0,
            high_water_len: 0,
        }
    }

    fn observe_len(&mut self, len: u64) {
        if len > self.high_water_len {
            self.high_water_len = len;
            self.attempts_without_progress = 0;
        }
    }

    fn next_backoff(&mut self) -> Option<Duration> {
        self.total_failures += 1;
        self.attempts_without_progress += 1;
        if self.attempts_without_progress >= MAX_ATTEMPTS_WITHOUT_PROGRESS {
            return None;
        }
        let idx = ((self.total_failures - 1) as usize).min(RETRY_BACKOFFS.len() - 1);
        Some(RETRY_BACKOFFS[idx])
    }
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
/// Retry policy: progress-aware (`RetryBudget`) on stream errors, transport
/// errors, and 429/5xx responses. Each retry stats the in-progress `.part`
/// file and resumes via `Range:` so a 2 GB GGUF doesn't re-download from
/// byte 0 after a single TLS hiccup; growth between attempts refunds the
/// budget. Mirrors the parallel path's per-range retry policy.
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

    let started = Instant::now();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let mut total: Option<u64> = None;
    let mut last_err: Option<anyhow::Error> = None;
    let mut budget = RetryBudget::new();

    'attempts: loop {
        // Resume: stat any prior .part bytes and ask for the remainder.
        // Statted before the budget decision so a failed-but-progressing
        // attempt refunds the no-progress counter before it can bail.
        let existing_len = tokio::fs::metadata(&tmp).await
            .map(|m| m.len()).unwrap_or(0);
        budget.observe_len(existing_len);

        if last_err.is_some() {
            let Some(backoff) = budget.next_backoff() else { break 'attempts };
            tracing::warn!(
                attempt = budget.total_failures,
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
        // HTTP 416 (Range Not Satisfiable) on a resume means our existing
        // `.part` is at/past the server's current file length — a stale or
        // already-complete part. Discard it and restart from 0 on the next
        // attempt instead of bailing permanently (which left the download
        // stuck unrecoverable forever).
        if status.as_u16() == 416 && existing_len > 0 {
            tracing::warn!(url = %request.url, existing_len,
                "HTTP 416 on resume; discarding stale part and restarting from 0");
            let _ = tokio::fs::remove_file(&tmp).await;
            bytes_done.store(0, Ordering::Relaxed);
            last_err = Some(anyhow::anyhow!("HTTP 416 (stale part, restarting)"));
            continue 'attempts;
        }
        // 4xx (other than 429 / 416-on-resume) — bad URL / auth required. Don't retry.
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

        // Size sanity before the atomic rename — a too-small .part (truncation
        // / error page) must never become the destination.
        let actual_len = tokio::fs::metadata(&tmp).await.map(|m| m.len()).unwrap_or(0);
        if let Err(e) = check_size_plausible(actual_len, request.expected_bytes, &request.url) {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(e);
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
/// Retry policy: each range-GET retries on 429/503, transport, and stream
/// errors with the progress-aware `RetryBudget` (backoff 1s/4s/16s,
/// Retry-After honored); only zero-progress attempts burn budget.
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
        // Best-effort sweep of any .part-NN left by an earlier parallel attempt
        // (e.g. a prior run that partially downloaded, then this retry sees the
        // server no longer advertising ranges). download_simple uses its own
        // "{}.part" temp and can't resume from these, so they'd leak. (audit E16)
        for i in 0..PARALLEL_PARTS {
            let _ = tokio::fs::remove_file(part_file_path(&request.destination, i)).await;
        }
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
    // Stream each part through a fixed 64 KB buffer instead of reading the whole
    // ~170 MB part into RAM (PARALLEL_PARTS of a multi-GB GGUF). Mirrors the
    // download_simple SHA re-read loop above; integrity check is unchanged.
    let mut buffer = vec![0u8; 65536];
    for i in 0..PARALLEL_PARTS {
        let part_path = part_file_path(&request.destination, i);
        let mut part = tokio::fs::File::open(&part_path).await
            .with_context(|| format!("reading part {}", part_path.display()))?;
        loop {
            use tokio::io::AsyncReadExt;
            let n = part.read(&mut buffer).await
                .with_context(|| format!("reading chunk from {}", part_path.display()))?;
            if n == 0 {
                break;
            }
            if let Some(h) = hasher.as_mut() { h.update(&buffer[..n]); }
            tokio::io::AsyncWriteExt::write_all(&mut out, &buffer[..n]).await
                .context("writing combined part")?;
        }
    }
    tokio::io::AsyncWriteExt::flush(&mut out).await.context("final flush")?;
    drop(out);

    if let (Some(expected), Some(h)) = (request.expected_sha256.as_ref(), hasher) {
        let got = hex::encode(h.finalize());
        if !expected.eq_ignore_ascii_case(&got) {
            let _ = tokio::fs::remove_file(&combined).await;
            // Also remove the per-range parts. A byte-complete-but-wrong part
            // (corrupt/compromised mirror, or a same-size remote revision across
            // attempts) would otherwise be treated as "already complete" on the
            // next attempt (cur_start > end), re-concatenated, and re-fail the SHA
            // forever — a permanently-stuck install until the user manually clears
            // the cache. Removing them forces a clean re-fetch on Retry.
            for i in 0..PARALLEL_PARTS {
                let _ = tokio::fs::remove_file(part_file_path(&request.destination, i)).await;
            }
            anyhow::bail!(
                "SHA256 mismatch for {}: expected {expected}, got {got}",
                request.url
            );
        }
    }

    // Size sanity before the atomic rename (mirrors download_simple).
    let actual_len = tokio::fs::metadata(&combined).await.map(|m| m.len()).unwrap_or(0);
    if let Err(e) = check_size_plausible(actual_len, request.expected_bytes, &request.url) {
        let _ = tokio::fs::remove_file(&combined).await;
        for i in 0..PARALLEL_PARTS {
            let _ = tokio::fs::remove_file(part_file_path(&request.destination, i)).await;
        }
        return Err(e);
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
    let mut budget = RetryBudget::new();
    let mut retrying = false;
    // Kept so the exhausted-retries bail preserves the underlying cause —
    // without it a TLS pin failure (or any transport error) degraded to an
    // opaque "range exhausted retries" with no source chain to classify.
    let mut last_err: Option<anyhow::Error> = None;

    'retry: loop {
        // Resumable: stat the part file. If it has bytes, append from there.
        let range_len = end - start + 1;
        let mut existing_len = tokio::fs::metadata(part_path).await
            .map(|m| m.len()).unwrap_or(0);
        // A part larger than its planned range is stale — leftover from a prior
        // download of a different-sized remote file. Its bytes would corrupt the
        // concat, so discard and re-fetch the range rather than the old behavior
        // of treating an oversized part as "already done" (which kept bad bytes).
        if existing_len > range_len {
            tracing::warn!(
                part = %part_path.display(), existing_len, range_len,
                "discarding oversized stale part before resume"
            );
            let _ = tokio::fs::remove_file(part_path).await;
            existing_len = 0;
        }
        // Observed after the stale-part discard so leftover bytes from a
        // different remote file can't masquerade as progress.
        budget.observe_len(existing_len);
        let cur_start = start + existing_len;
        if cur_start > end { return Ok(()); } // exactly complete

        if retrying {
            let Some(backoff) = budget.next_backoff() else {
                let msg = format!("range exhausted retries (start={start})");
                return Err(match last_err {
                    Some(e) => e.context(msg),
                    None => anyhow::anyhow!(msg),
                });
            };
            tokio::time::sleep(backoff).await;
        }
        retrying = true;
        if cancel.load(Ordering::Relaxed) {
            anyhow::bail!("range cancelled (start={start})");
        }

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
                last_err = Some(anyhow::Error::new(e).context("issuing range GET"));
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
            last_err = Some(anyhow::anyhow!("HTTP {status}"));
            continue;
        }
        if !status.is_success() {
            anyhow::bail!("range {range_header}: HTTP {status}");
        }
        // If we're resuming a partially-downloaded range (bytes already on disk)
        // but the server answered 200 (full body) instead of 206 (partial),
        // appending would splice our partial prefix in front of the full file →
        // a corrupt, oversized part. Discard the partial and restart this range
        // cleanly. (download_simple handles the same 200-on-resume case.)
        if existing_len > 0 && status.as_u16() != 206 {
            tracing::warn!(
                part = %part_path.display(), %status,
                "range resume answered non-206; discarding partial and restarting"
            );
            let _ = tokio::fs::remove_file(part_path).await;
            last_err = Some(anyhow::anyhow!("range resume answered HTTP {status} (restarted)"));
            continue;
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
                    // Retry by continuing the OUTER loop, not recursing while
                    // holding `_permit`: recursing re-acquired a 2nd permit, and
                    // under throttling all 8 permit-holders could recurse and
                    // block acquiring a 9th — a permanent deadlock. `continue`
                    // drops `_permit` at end of iteration; the loop re-stats the
                    // .part file and resumes via Range, on the progress-aware
                    // backoff schedule (zero-progress attempts still bail after
                    // a finite budget instead of spinning forever).
                    tracing::warn!(?e, "stream error; retrying range");
                    last_err = Some(anyhow::Error::new(e).context("reading range chunk"));
                    continue 'retry;
                }
            };
            tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await
                .context("writing range chunk")?;
            let _ = tx.send(chunk.len()).await;
        }
        tokio::io::AsyncWriteExt::flush(&mut file).await.ok();
        return Ok(());
    }
}

#[cfg(test)]
mod tests {
    use super::{
        chain_has_pin_failure, check_size_plausible, message_indicates_pin_failure,
        source_chain_indicates_pin_failure, RetryBudget, PINNED_ROOT_CERTS,
    };
    use std::time::Duration;

    #[test]
    fn every_pinned_root_pem_parses_as_certificate() {
        assert_eq!(PINNED_ROOT_CERTS.len(), 11);
        for (slug, pem) in PINNED_ROOT_CERTS {
            assert!(
                reqwest::Certificate::from_pem(pem).is_ok(),
                "pinned root '{slug}' failed to parse"
            );
        }
    }

    #[test]
    fn pin_failure_matcher_recognizes_rustls_wording() {
        assert!(message_indicates_pin_failure(
            "invalid peer certificate: UnknownIssuer"
        ));
        assert!(message_indicates_pin_failure(
            "client error (Connect): invalid peer certificate: UnknownIssuer"
        ));
        assert!(!message_indicates_pin_failure("connection reset by peer"));
        assert!(!message_indicates_pin_failure(
            "dns error: failed to lookup address information"
        ));
        assert!(!message_indicates_pin_failure("HTTP 503 Service Unavailable"));
        assert!(!message_indicates_pin_failure(
            "certificate expired: verification time 1 (UNIX)"
        ));
    }

    // Outer error whose Display does NOT include its source (the hyper
    // wrapping shape) so the test exercises the source() walk, not just
    // the top-level message match.
    #[derive(Debug)]
    struct OpaqueWrap(std::io::Error);
    impl std::fmt::Display for OpaqueWrap {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "error trying to connect")
        }
    }
    impl std::error::Error for OpaqueWrap {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }

    #[test]
    fn pin_failure_source_chain_walk_finds_nested_rustls_message() {
        let wrapped = OpaqueWrap(std::io::Error::other(
            "invalid peer certificate: UnknownIssuer",
        ));
        assert!(source_chain_indicates_pin_failure(&wrapped));
        let plain = OpaqueWrap(std::io::Error::other("connection reset by peer"));
        assert!(!source_chain_indicates_pin_failure(&plain));
    }

    #[test]
    fn chain_pin_failure_detected_through_anyhow_context_layers() {
        let err = anyhow::Error::new(OpaqueWrap(std::io::Error::other(
            "invalid peer certificate: UnknownIssuer",
        )))
        .context("issuing GET")
        .context("downloading model.onnx");
        assert!(chain_has_pin_failure(&err));

        let plain = anyhow::anyhow!("HTTP 429 Too Many Requests").context("issuing GET");
        assert!(!chain_has_pin_failure(&plain));
    }

    #[test]
    fn retry_budget_zero_progress_matches_old_four_attempt_ladder() {
        let mut b = RetryBudget::new();
        b.observe_len(0);
        assert_eq!(b.next_backoff(), Some(Duration::from_secs(1)));
        b.observe_len(0);
        assert_eq!(b.next_backoff(), Some(Duration::from_secs(4)));
        b.observe_len(0);
        assert_eq!(b.next_backoff(), Some(Duration::from_secs(16)));
        b.observe_len(0);
        assert_eq!(b.next_backoff(), None);
    }

    #[test]
    fn retry_budget_growth_refunds_and_ladder_repeats_last_step() {
        let mut b = RetryBudget::new();
        let mut len = 0;
        for expected in [1, 4, 16, 16, 16, 16] {
            len += 1024;
            b.observe_len(len);
            assert_eq!(b.next_backoff(), Some(Duration::from_secs(expected)));
        }
    }

    #[test]
    fn retry_budget_re_exhausts_after_a_refund() {
        let mut b = RetryBudget::new();
        for _ in 0..3 {
            b.observe_len(0);
            assert!(b.next_backoff().is_some());
        }
        b.observe_len(1);
        assert_eq!(b.next_backoff(), Some(Duration::from_secs(16)));
        for _ in 0..2 {
            b.observe_len(1);
            assert!(b.next_backoff().is_some());
        }
        b.observe_len(1);
        assert_eq!(b.next_backoff(), None);
    }

    #[test]
    fn retry_budget_discard_and_refetch_below_high_water_is_not_progress() {
        let mut b = RetryBudget::new();
        b.observe_len(10_000);
        assert!(b.next_backoff().is_some());
        b.observe_len(0);
        assert!(b.next_backoff().is_some());
        b.observe_len(5_000);
        assert!(b.next_backoff().is_some());
        b.observe_len(9_999);
        assert_eq!(b.next_backoff(), None);
    }

    #[test]
    fn size_check_passes_when_no_estimate() {
        assert!(check_size_plausible(0, None, "u").is_ok());
        assert!(check_size_plausible(10, Some(0), "u").is_ok());
    }

    #[test]
    fn size_check_passes_within_loose_band() {
        // Exact, under, and over the estimate all pass — the estimate is loose.
        assert!(check_size_plausible(1_000_000, Some(1_000_000), "u").is_ok());
        assert!(check_size_plausible(800_000, Some(1_000_000), "u").is_ok());
        assert!(check_size_plausible(5_000_000, Some(1_000_000), "u").is_ok());
        // Just above the 4× floor passes.
        assert!(check_size_plausible(260_000, Some(1_000_000), "u").is_ok());
    }

    #[test]
    fn size_check_rejects_truncation_and_error_pages() {
        // A few-KB HTML error page standing in for a ~900 MB model.
        assert!(check_size_plausible(4_096, Some(925_600_000), "u").is_err());
        // Truncated to well under the 4× floor.
        assert!(check_size_plausible(100_000, Some(1_000_000), "u").is_err());
        // Zero-byte result against a 38 MB expectation.
        assert!(check_size_plausible(0, Some(38_696_353), "u").is_err());
    }
}
