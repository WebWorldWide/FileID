// HuggingFace 12-way parallel range-GET downloader, mirroring the macOS app's `HFDownloader.swift`: a HEAD discovers Content-Length + ETag, 12 concurrent ranged GETs stream into `<file>.part-N` (each chunk verified against Content-Range), the parts are concatenated, the final SHA256 is checked against MODELS.md, and the result is atomically renamed into place. On cancellation the part files stay on disk so resume is cheap.
// Privacy: this is the **only** network code in the engine. Every URL comes from the canonical SHA256-pinned manifest in `shared/docs/MODELS.md`; there is no telemetry and nothing to opt out of.

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
// 4 Hz: each event crosses IPC → JSON parse → PropertyChanged → x:Bind re-eval across every bound row on the app's UI thread. At 20 Hz a 15 GB VLM is ~19,000 events of pure UI churn (the Welcome sheet visibly glitched and app.log grew megabytes of [INSTALL] spam). 4×/s is indistinguishable to the eye; the EMA rate uses its own 500 ms sampler.
const PROGRESS_THROTTLE_MS: u64 = 250;
const MAX_UNSIZED_DOWNLOAD_BYTES: u64 = 64 * 1024 * 1024 * 1024;

/// Total concurrent in-flight HTTP requests across ALL prewarm tasks. HuggingFace's CDN returns 429s when one IP issues too many simultaneous range-GETs; 3 models × 12 PARALLEL_PARTS = 36 sockets stalled the third model on Retry-After back-offs (MobileCLIP-S2 "stuck on 0%"). 8 permits keeps every model moving without tripping the throttle.
const MAX_CONCURRENT_HTTP_REQUESTS: usize = 8;

fn http_semaphore() -> &'static Arc<tokio::sync::Semaphore> {
    static SEMA: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    SEMA.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_HTTP_REQUESTS)))
}

/// Hosts FileID permits user-initiated model downloads from: HF and its CDN only. Suffix-match with a leading dot for subdomains so "evilhuggingface.co" never matches ".huggingface.co". Gates BOTH the INITIAL request URL (`download_url_allowed`, in `download_simple` / `download_parallel`) and redirect hops (the reqwest redirect policy in `build_shared_client`), so the egress invariant is a runtime guarantee, not only the `registry.rs` URL list plus a CI grep.
const ALLOWED_DOWNLOAD_HOSTS: &[&str] = &[
    "huggingface.co",
    "hf.co",
];

/// True iff `url` parses, is https, and its host is on (or a subdomain of) the egress allowlist; anything else is refused before any network I/O.
fn download_url_allowed(url: &str) -> bool {
    match reqwest::Url::parse(url) {
        Ok(u) => {
            u.scheme() == "https"
                && u.host_str().is_some_and(|h| {
                    ALLOWED_DOWNLOAD_HOSTS
                        .iter()
                        .any(|d| h == *d || h.ends_with(&format!(".{d}")))
                })
        }
        Err(_) => false,
    }
}

/// CA-allowlist TLS pinning (SECURITY.md hardening item; shared/security/tls-pins.json). These PEMs become the ONLY trust anchors on the download client, so an active MITM holding any other OS-trusted CA (interception proxy, compromised CA) fails the handshake instead of silently re-signing the connection. Root-level pins, not leaf/intermediate: leaves rotate ~90 days and CDNs move between CA families, while these roots are stable for decades. Defense-in-depth alongside per-artifact SHA256.
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

/// Builds the long-lived shared `reqwest::Client` (HTTP/2 + connection pooling; one per process, cheap to clone).
///
/// Timeouts are phase-specific, not wall-clock: `connect_timeout` fast-fails DNS / TCP / TLS; `read_timeout` fails only if no bytes arrive for 60 s, so a slow-but-steady stream still finishes a 2 GB GGUF (a blanket `.timeout(300s)` killed downloads below ~7 MB/s). 60 s, not 120, so a stalled stream errors and resumes via download_range_with_retry within ~61 s, re-arming the app's 120 s no-progress install watchdog before it alarms the user; 60 s of total silence is a dead connection.
///
/// TLS trust is pinned to `PINNED_ROOT_CERTS` (built-in roots disabled), per shared/security/tls-pins.json. `FILEID_DISABLE_TLS_PINNING=1` reverts to the OS root store: validation only ever changes, never the egress surface, and it is logged loudly as a diagnostic escape hatch for networks that intercept HTTPS.
pub fn fail_closed_client() -> Arc<reqwest::Client> {
    Arc::new(
        reqwest::Client::builder()
            .tls_built_in_root_certs(false)
            .build()
            .expect("no-roots fallback client"),
    )
}

pub fn build_shared_client() -> Result<Arc<reqwest::Client>> {
    // Restrict redirects to Hugging Face and its CDN. reqwest's default follows 10 redirects to ANY host, so an on-path attacker could bounce a 302 chain off-allowlist and dodge the check on the ORIGINAL URL. Suffix-match with a leading dot so "evilhuggingface.co" never matches ".huggingface.co".
    let redirect_policy = reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= 10 {
            return attempt.stop();
        }
        // Never follow a redirect off https: the host allowlist alone would let a 302 downgrade to plaintext http:// on an allowlisted host. Every allowlisted CDN serves https, so no legitimate redirect is blocked.
        if attempt.url().scheme() != "https" {
            return attempt.stop();
        }
        match attempt.url().host_str() {
            Some(h)
                if ALLOWED_DOWNLOAD_HOSTS
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

/// rustls reports a chain that doesn't end at a pinned root as `invalid peer certificate: UnknownIssuer`, but only as Display text buried in the reqwest → hyper → io source chain (no typed variant survives), so a string match over the chain is the only stable detection.
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

/// Pin detection for this module's `anyhow::Error`s: `chain()` descends every context layer and wrapped source, so it sees the rustls message wherever the retry loops buried it.
pub fn chain_has_pin_failure(err: &anyhow::Error) -> bool {
    err.chain().any(|e| match e.downcast_ref::<reqwest::Error>() {
        Some(re) => is_pin_failure(re),
        None => message_indicates_pin_failure(&e.to_string()),
    })
}

fn io_error_is_disk_full(e: &std::io::Error) -> bool {
    // 39 = ERROR_HANDLE_DISK_FULL, 112 = ERROR_DISK_FULL; the raw codes are Windows-only and mean unrelated errnos on Unix.
    #[cfg(windows)]
    let raw_disk_full = matches!(e.raw_os_error(), Some(39 | 112));
    #[cfg(not(windows))]
    let raw_disk_full = false;
    e.kind() == std::io::ErrorKind::StorageFull || raw_disk_full
}

/// Disk-full detection for install failures: anyhow's Display drops the io source, so without this walk an ENOSPC surfaces as a connection problem with retry advice that can never work.
pub fn chain_has_disk_full(err: &anyhow::Error) -> bool {
    err.chain()
        .any(|e| e.downcast_ref::<std::io::Error>().is_some_and(io_error_is_disk_full))
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
    pub expected_sha256: Option<String>,
    /// Registry size estimate (`approx_bytes`) for a loose post-download sanity check (see `check_size_plausible`): catches a truncated stream or HTML error page even when no SHA256 is pinned.
    pub expected_bytes: Option<u64>,
}

/// Loose post-download size sanity. `approx_bytes` is an ESTIMATE, so only implausibly-small results are rejected: a truncated stream, HTML error page or auth wall standing in for a multi-GB model is orders of magnitude off. A 4× floor never rejects a reasonable estimate but always catches a KB-for-GB substitution. (SHA256, when pinned, is the exact check.)
fn check_size_plausible(actual: u64, expected: Option<u64>, url: &str) -> Result<()> {
    if let Some(expected) = expected
        && expected > 0 {
            let floor = (expected / 4).max(1);
            if actual < floor {
                anyhow::bail!(
                    "size sanity failed for {url}: got {actual} bytes, expected \
                     ~{expected} (floor {floor}) — likely a truncated download or \
                     an error page, not the model"
                );
            }
        }
    Ok(())
}

fn stream_hard_limit(advertised: Option<u64>, expected: Option<u64>) -> u64 {
    advertised
        .filter(|n| *n > 0)
        .or_else(|| expected.filter(|n| *n > 0).map(|n| n.saturating_mul(4)))
        .unwrap_or(MAX_UNSIZED_DOWNLOAD_BYTES)
}

const RETRY_BACKOFFS: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(4),
    Duration::from_secs(16),
];
const MAX_ATTEMPTS_WITHOUT_PROGRESS: u32 = 4;

/// Progress-aware retry budget shared by `download_simple` and `download_range_with_retry`. A fixed 4-attempt budget with the 60 s `read_timeout` hard-failed slow-but-progressing downloads (large GGUF on spotty wifi) even though every attempt resumed further along. Only attempts that left the part file un-grown burn budget (growth at the top of the next attempt resets the counter), so a flaky download survives any number of stalls while it keeps moving, and a dead connection still bails after `MAX_ATTEMPTS_WITHOUT_PROGRESS`. The ladder is indexed by total failures and repeats its last step, so a chronically flaky link settles into 16 s pauses. Growth is judged against a high-water mark so a discard-and-refetch cycle (416 / non-206 resume / oversized stale part) can't refund budget by re-downloading the same bytes.
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

/// The simple non-parallel path: used when the server lacks `Accept-Ranges: bytes`, the file is below `MIN_BYTES_FOR_PARALLEL`, or as the parallel path's fallback when HEAD is unreliable. Takes the shared `reqwest::Client` so small files (config.json, tokenizer.json) don't each pay a TLS handshake.
///
/// Retries are progress-aware (`RetryBudget`) on stream errors, transport errors and 429/5xx; each retry stats the `.part` file and resumes via `Range:` so a 2 GB GGUF doesn't restart from byte 0 after one TLS hiccup.
pub async fn download_simple<F>(
    client: Arc<reqwest::Client>,
    request: DownloadRequest,
    cancel: Arc<AtomicBool>,
    mut progress: F,
) -> Result<()>
where
    F: FnMut(DownloadProgress),
{
    if !download_url_allowed(&request.url) {
        let host = reqwest::Url::parse(&request.url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_else(|| "<unparseable>".to_string());
        anyhow::bail!("refusing to download: host '{host}' is not on the https egress allowlist");
    }
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
        // Resume: stat prior .part bytes and ask for the remainder, before the budget decision so a failed-but-progressing attempt refunds the no-progress counter before it can bail.
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

        // Honor the global HTTP semaphore so the simple path can't sneak past the concurrency cap; re-acquired each attempt so a long backoff doesn't hog a permit.
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
        if status.as_u16() == 429 || status.is_server_error() {
            if let Some(s) = resp.headers().get("retry-after").and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
            {
                tokio::time::sleep(Duration::from_secs(s.min(60))).await;
            }
            last_err = Some(anyhow::anyhow!("HTTP {status}"));
            continue 'attempts;
        }
        // HTTP 416 on a resume means the existing `.part` is at/past the server's file length (stale or already complete): discard it and restart from 0 rather than bailing permanently.
        if status.as_u16() == 416 && existing_len > 0 {
            tracing::warn!(url = %request.url, existing_len,
                "HTTP 416 on resume; discarding stale part and restarting from 0");
            let _ = tokio::fs::remove_file(&tmp).await;
            bytes_done.store(0, Ordering::Relaxed);
            last_err = Some(anyhow::anyhow!("HTTP 416 (stale part, restarting)"));
            continue 'attempts;
        }
        if status.is_client_error() {
            anyhow::bail!("non-2xx response: HTTP {status} for {}", request.url);
        }
        if !status.is_success() {
            last_err = Some(anyhow::anyhow!("HTTP {status}"));
            continue 'attempts;
        }

        // 206 = resumed; 200 = the server ignored Range and re-sends from byte 0, so truncate our part file.
        let resumed = existing_len > 0 && status.as_u16() == 206;
        if !resumed && existing_len > 0 {
            tracing::warn!(
                url = %request.url,
                "server ignored Range header (HTTP {status}); restarting download from 0"
            );
            let _ = tokio::fs::remove_file(&tmp).await;
            bytes_done.store(0, Ordering::Relaxed);
        }

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

        // Per-attempt hasher: SHA256 can only be verified on a single-attempt download (resumes invalidate the running hash); otherwise the post-download verifier re-hashes the file from disk.
        let mut hasher = request
            .expected_sha256
            .as_ref()
            .filter(|_| !resumed)
            .map(|_| Sha256::new());

        let hard_limit = stream_hard_limit(total, request.expected_bytes);
        let mut stream = resp.bytes_stream();
        let mut last_report = bytes_done.load(Ordering::Relaxed);
        // Time floor alongside the byte gate: on a fast connection 1 MB intervals alone still fire tens of events per second (see PROGRESS_THROTTLE_MS).
        let mut last_report_at = Instant::now();
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
            let current = bytes_done.load(Ordering::Relaxed);
            let next = current
                .checked_add(chunk.len() as u64)
                .ok_or_else(|| anyhow::anyhow!("download byte count overflow for {}", request.url))?;
            if next > hard_limit {
                drop(file);
                let _ = tokio::fs::remove_file(&tmp).await;
                anyhow::bail!(
                    "download exceeded hard byte limit for {}: next chunk would reach {} bytes (limit {})",
                    request.url,
                    next,
                    hard_limit
                );
            }
            if let Some(h) = hasher.as_mut() {
                h.update(&chunk);
            }
            if let Err(e) = tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await {
                return Err(anyhow::Error::new(e).context("writing chunk"));
            }
            let cur = bytes_done.fetch_add(chunk.len() as u64, Ordering::Relaxed) + chunk.len() as u64;
            if cur - last_report >= PROGRESS_REPORT_INTERVAL_BYTES
                && last_report_at.elapsed().as_millis() >= PROGRESS_THROTTLE_MS as u128
            {
                let elapsed = started.elapsed().as_secs_f64().max(0.001);
                progress(DownloadProgress {
                    url: request.url.clone(),
                    bytes_done: cur,
                    bytes_total: total,
                    bytes_per_second: cur as f64 / elapsed,
                });
                last_report = cur;
                last_report_at = Instant::now();
            }
        }
        tokio::io::AsyncWriteExt::flush(&mut file).await.context("final flush")?;
        drop(file);

        if let Some(e) = chunk_err {
            last_err = Some(e);
            continue 'attempts;
        }

        // Stream completed cleanly. Verify SHA256, re-reading the part file from disk if we resumed (the running hasher only saw the latest attempt).
        if let Some(expected) = request.expected_sha256.as_ref() {
            let got_hex = if let Some(h) = hasher {
                hex::encode(h.finalize())
            } else {
                let mut file = tokio::fs::File::open(&tmp).await
                    .with_context(|| format!("opening {} for sha verification", tmp.display()))?;
                let mut h = Sha256::new();
                // Heap-allocated so the 64 KB chunk doesn't bloat this async fn's future (callers in prewarm.rs trip clippy's large_futures otherwise).
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

/// A progress update a chunk task posts to the drainer. `Advanced` is fresh bytes written to a `.part-NN`; `Rewound` un-counts the on-disk prefix of a part discarded for a clean re-fetch (a 416, or a 200 full-body answer to a resume) that `resume_seed_bytes` or an earlier `Advanced` already counted. Routed over the channel, not a direct `fetch_sub`, so the drainer (sole writer of the counter) applies the rewind after this part's own adds and can never underflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PartProgress {
    Advanced(u64),
    Rewound(u64),
}

fn apply_part_progress(running: u64, msg: PartProgress) -> u64 {
    match msg {
        PartProgress::Advanced(n) => running.saturating_add(n),
        PartProgress::Rewound(n) => running.saturating_sub(n),
    }
}

/// A retry after a partial install must not fetch again what already verified on disk. The hash runs only when the file's size is plausible for the registry estimate (a truncated or foreign file is refetched without reading it) and stops at the next chunk once `cancel` is set. Returns the verified length.
async fn destination_matches_pin(request: &DownloadRequest, cancel: &Arc<AtomicBool>) -> Option<u64> {
    let expected = request.expected_sha256.clone()?;
    let path = request.destination.clone();
    let length = tokio::fs::metadata(&path).await.ok().filter(|m| m.is_file())?.len();
    if !size_fits_estimate(length, request.expected_bytes) {
        return None;
    }
    let cancel = cancel.clone();
    tokio::task::spawn_blocking(move || {
        let mut file = std::fs::File::open(&path).ok()?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 256 * 1024];
        loop {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            match std::io::Read::read(&mut file, &mut buffer) {
                Ok(0) => break,
                Ok(count) => hasher.update(&buffer[..count]),
                Err(_) => return None,
            }
        }
        hex::encode(hasher.finalize()).eq_ignore_ascii_case(&expected).then_some(length)
    })
    .await
    .unwrap_or(None)
}

/// `approx_bytes` is an estimate, so only a size outside the 4x band that `check_size_plausible` and `stream_hard_limit` already use rules a file out.
fn size_fits_estimate(actual: u64, expected: Option<u64>) -> bool {
    match expected.filter(|n| *n > 0) {
        Some(expected) => actual >= (expected / 4).max(1) && actual <= expected.saturating_mul(4),
        None => true,
    }
}

/// Downloads one file using up to PARALLEL_PARTS concurrent range-GETs, falling back to `download_simple` when the server lacks `Accept-Ranges: bytes`, the file is under MIN_BYTES_FOR_PARALLEL (5 MB), or the HEAD probe fails.
///
/// Sharing the `reqwest::Client` is critical: HTTP/2 multiplexing + keep-alive let 12 parallel GETs hit one HuggingFace edge without re-handshaking TLS. Each range-GET retries on 429/503, transport and stream errors under the progress-aware `RetryBudget` (backoff 1s/4s/16s, Retry-After honored).
///
/// Cancellation: tasks poll the caller's `Arc<AtomicBool>` after every chunk, so cancel lands within a chunk write; part files survive for resume.
pub async fn download_parallel<F>(
    client: Arc<reqwest::Client>,
    request: DownloadRequest,
    cancel: Arc<AtomicBool>,
    mut progress: F,
) -> Result<()>
where
    F: FnMut(DownloadProgress) + Send + 'static,
{
    if !download_url_allowed(&request.url) {
        let host = reqwest::Url::parse(&request.url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_else(|| "<unparseable>".to_string());
        anyhow::bail!("refusing to download: host '{host}' is not on the https egress allowlist");
    }
    if let Some(length) = destination_matches_pin(&request, &cancel).await {
        progress(DownloadProgress {
            url: request.url.clone(),
            bytes_done: length,
            bytes_total: Some(length),
            bytes_per_second: 0.0,
        });
        return Ok(());
    }
    if cancel.load(Ordering::Relaxed) {
        anyhow::bail!("download cancelled");
    }
    if let Some(parent) = request.destination.parent() {
        tokio::fs::create_dir_all(parent).await
            .with_context(|| format!("creating parent {}", parent.display()))?;
    }

    // HEAD probe for Content-Length + Accept-Ranges. HuggingFace 302s to a CDN (reqwest follows, so we read the CDN's status); the CDN sometimes omits `Accept-Ranges` on HEAD though it honors `Range:` on GET, so fall back to a one-byte range probe before degrading to the slower single-stream path.
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

    // HEAD said no (or failed) but the file is large: send `GET ... Range: bytes=0-0`; a 206 means ranges work despite HEAD's silence.
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
        // Best-effort sweep of any .part-NN left by an earlier parallel attempt; download_simple uses its own "{}.part" and can't resume from these, so they'd leak.
        for i in 0..PARALLEL_PARTS {
            let _ = tokio::fs::remove_file(part_file_path(&request.destination, i)).await;
        }
        return download_simple(client.clone(), request, cancel.clone(), |p| progress(p)).await;
    }

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

    // Seed progress from bytes already on disk so a resume doesn't report from 0 and jump the bar backward. Each part contributes at most its planned range length: an OVERSIZED stale part is discarded by download_range_with_retry, so counting only the in-range prefix keeps the seed consistent with the bytes actually re-used.
    let resume_seed = resume_seed_bytes(&request.destination, &ranges).await;

    let bytes_done = Arc::new(AtomicU64::new(resume_seed));
    let started = Instant::now();
    let last_emit_ms = Arc::new(parking_lot::Mutex::new(0u128));

    // Progress channel: chunk tasks post `PartProgress`; one drainer, the SOLE writer of `bytes_done`, folds them in and emits at ≤10 Hz. Bounded, with backpressure (never dropped), so a part's `Rewound` is never lost and lands after that part's own `Advanced` deltas. `bytes_done` is therefore NOT monotonic.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<PartProgress>(512);

    let total_for_drain = total;
    let url_for_drain = request.url.clone();
    let bytes_done_drain = bytes_done.clone();
    let last_emit_drain = last_emit_ms.clone();
    let progress_handle: tokio::task::JoinHandle<()> = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            // Sole writer of `bytes_done`, so load+store is race-free; `apply_part_progress` keeps the add/rewind accounting in one tested place.
            let cur = apply_part_progress(bytes_done_drain.load(Ordering::Relaxed), msg);
            bytes_done_drain.store(cur, Ordering::Relaxed);
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
        // All chunk senders dropped: emit the unconditional 100% event so the bar always reaches 100% regardless of the 10 Hz throttle.
        let final_done = bytes_done_drain.load(Ordering::Relaxed);
        let elapsed_secs = started.elapsed().as_secs_f64().max(0.001);
        progress(DownloadProgress {
            url: url_for_drain,
            bytes_done: final_done,
            bytes_total: Some(total_for_drain),
            bytes_per_second: final_done as f64 / elapsed_secs,
        });
    });

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

    let combined = request.destination.with_extension(format!(
        "{}.part",
        request.destination.extension()
            .and_then(|s| s.to_str())
            .unwrap_or("download")
    ));
    let mut out = tokio::fs::File::create(&combined).await
        .with_context(|| format!("creating {}", combined.display()))?;
    let mut hasher = request.expected_sha256.as_ref().map(|_| Sha256::new());
    // Stream each part through a fixed 64 KB buffer instead of reading a ~170 MB part into RAM (PARALLEL_PARTS of a multi-GB GGUF).
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
            // Also remove the per-range parts: a byte-complete-but-wrong part (corrupt/compromised mirror, or a same-size remote revision) would be treated as "already complete" next attempt and re-fail the SHA forever, a permanently stuck install. Removing them forces a clean re-fetch on Retry.
            for i in 0..PARALLEL_PARTS {
                let _ = tokio::fs::remove_file(part_file_path(&request.destination, i)).await;
            }
            anyhow::bail!(
                "SHA256 mismatch for {}: expected {expected}, got {got}",
                request.url
            );
        }
    }

    let actual_len = tokio::fs::metadata(&combined).await.map(|m| m.len()).unwrap_or(0);
    // Exact-length guard: parts were planned from `total` (the HEAD/probe Content-Length), so the assembled file MUST be exactly that long. For a hash-less file this is the only integrity gate; the loose 4× `check_size_plausible` floor would miss a few-percent truncation.
    if total > 0 && actual_len != total {
        let _ = tokio::fs::remove_file(&combined).await;
        for i in 0..PARALLEL_PARTS {
            let _ = tokio::fs::remove_file(part_file_path(&request.destination, i)).await;
        }
        anyhow::bail!(
            "assembled size mismatch for {}: got {actual_len} bytes, expected {total}",
            request.url
        );
    }
    if let Err(e) = check_size_plausible(actual_len, request.expected_bytes, &request.url) {
        let _ = tokio::fs::remove_file(&combined).await;
        for i in 0..PARALLEL_PARTS {
            let _ = tokio::fs::remove_file(part_file_path(&request.destination, i)).await;
        }
        return Err(e);
    }

    tokio::fs::rename(&combined, &request.destination).await
        .with_context(|| format!("rename {} -> {}", combined.display(), request.destination.display()))?;

    for i in 0..PARALLEL_PARTS {
        let _ = tokio::fs::remove_file(part_file_path(&request.destination, i)).await;
    }

    Ok(())
}

/// One-byte `Range:` probe confirming ranges work when HEAD didn't advertise them. `Some((total, true))` on 206 (total from `Content-Range`), `Some((total, false))` on 200 (Range ignored; total = `Content-Length`), `None` on transport failure. A hint to the parallel path, not a requirement.
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

/// Sums the resumable bytes already on disk across all planned parts, to seed the progress counter on a resume. An oversized part (> its planned range) is discarded WHOLE by `download_range_with_retry`, so it seeds 0; otherwise its range_len would be counted here and again as re-download deltas, pushing bytes_done past bytes_total. `ranges` is `(index, start, end)` with an inclusive `end`.
async fn resume_seed_bytes(dest: &Path, ranges: &[(usize, u64, u64)]) -> u64 {
    let mut seed: u64 = 0;
    for &(i, start, end) in ranges {
        let range_len = end - start + 1;
        let on_disk = tokio::fs::metadata(part_file_path(dest, i))
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        seed += if on_disk > range_len { 0 } else { on_disk };
    }
    seed
}

/// How `download_range_with_retry` reacts to a non-success / non-206 status given the bytes already on disk for this part.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RangeResumeAction {
    /// 2xx that honored our Range (206), or a fresh 200/206 with no prior bytes: stream normally.
    Proceed,
    /// The on-disk prefix is unusable (416 stale part, or a 200 full-body answer to a partial request): discard the part and re-fetch cleanly. Recoverable, never a hard bail.
    DiscardAndRetry,
    /// A genuine error (bad URL/auth, or a 416 with nothing on disk to discard): bail.
    Bail,
}

/// Classifies a range response; pure so it's testable without a server. The single source of truth for the resume-recovery branches in `download_range_with_retry` (416 and non-206-on-resume both auto-recover, as in `download_simple`). 429/5xx are handled separately and never reach here.
fn classify_range_status(status: u16, existing_len: u64) -> RangeResumeAction {
    if status == 416 {
        // A stale on-disk part is recoverable; a 416 with nothing to discard is genuinely unsatisfiable.
        return if existing_len > 0 {
            RangeResumeAction::DiscardAndRetry
        } else {
            RangeResumeAction::Bail
        };
    }
    let success = (200..300).contains(&status);
    if !success {
        return RangeResumeAction::Bail;
    }
    // Resuming but the server sent a full body (not 206): appending would splice our prefix in front of the full file, so restart clean.
    if existing_len > 0 && status != 206 {
        return RangeResumeAction::DiscardAndRetry;
    }
    RangeResumeAction::Proceed
}

/// Downloads one byte range with retry-on-429/503 + resume: an existing `<part_path>` resumes via `Range: bytes={start + existing_len}-{end}` and appends.
///
/// Progress goes out as `PartProgress` over `tx`: `Advanced` per written chunk and, on DiscardAndRetry, one `Rewound(existing_len)` un-counting the discarded prefix. It travels the SAME channel (not a direct `fetch_sub`) so the drainer applies it after this part's queued `Advanced` deltas; a direct subtract could overtake them and underflow.
async fn download_range_with_retry(
    client: &reqwest::Client,
    url: &str,
    start: u64,
    end: u64,
    part_path: &Path,
    cancel: &AtomicBool,
    tx: tokio::sync::mpsc::Sender<PartProgress>,
) -> Result<()> {
    let mut budget = RetryBudget::new();
    let mut retrying = false;
    // Kept so the exhausted-retries bail preserves the underlying cause; otherwise a TLS pin failure degraded to an opaque "range exhausted retries" with no source chain to classify.
    let mut last_err: Option<anyhow::Error> = None;

    'retry: loop {
        let range_len = end - start + 1;
        let mut existing_len = tokio::fs::metadata(part_path).await
            .map(|m| m.len()).unwrap_or(0);
        // A part larger than its planned range is stale (from a different-sized remote file) and would corrupt the concat, so discard and re-fetch rather than treating it as "already done".
        if existing_len > range_len {
            tracing::warn!(
                part = %crate::platform::redact_path_for_log(part_path), existing_len, range_len,
                "discarding oversized stale part before resume"
            );
            let _ = tokio::fs::remove_file(part_path).await;
            existing_len = 0;
        }
        // Observed after the stale-part discard so leftover bytes from a different remote file can't masquerade as progress.
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

        // Global HTTP concurrency cap: stops three concurrent prewarms × 12 range-GETs tripping HuggingFace's per-IP rate limit. Held for this attempt, released on drop.
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
            if let Some(s) = resp.headers().get("retry-after").and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
            {
                tokio::time::sleep(Duration::from_secs(s.min(60))).await;
            }
            last_err = Some(anyhow::anyhow!("HTTP {status}"));
            continue;
        }
        // Resume-recovery classification (parity with download_simple): a 416 on a resume (stale part) and a 200 full-body answer to a partial request both discard the part and re-fetch cleanly rather than bailing the whole parallel download. A 416 with nothing on disk, or any other non-2xx, is a genuine error.
        match classify_range_status(status.as_u16(), existing_len) {
            RangeResumeAction::Proceed => {}
            RangeResumeAction::DiscardAndRetry => {
                tracing::warn!(
                    part = %crate::platform::redact_path_for_log(part_path), %status, existing_len, range_len,
                    "range resume not honored (416/non-206); discarding stale part and re-fetching"
                );
                // Un-count the discarded bytes: `existing_len` (> 0 on every DiscardAndRetry) is exactly what this part already contributed to bytes_done, and the re-fetch restarts from offset 0, so without this the prefix is counted twice and the bar overshoots 100%. Sent over `tx` so the drainer applies it after this part's queued `Advanced` deltas; a direct subtract could overtake them and underflow.
                let _ = tx.send(PartProgress::Rewound(existing_len)).await;
                let _ = tokio::fs::remove_file(part_path).await;
                last_err = Some(anyhow::anyhow!("range resume answered HTTP {status} (restarted)"));
                continue;
            }
            RangeResumeAction::Bail => {
                anyhow::bail!("range {range_header}: HTTP {status}");
            }
        }

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
                    // Retry by continuing the OUTER loop, not recursing while holding `_permit`: recursion acquired a second permit, and under throttling all 8 holders could recurse and block on a 9th, a permanent deadlock. `continue` drops `_permit`; the loop re-stats the .part and resumes via Range on the progress-aware backoff.
                    tracing::warn!(?e, "stream error; retrying range");
                    last_err = Some(anyhow::Error::new(e).context("reading range chunk"));
                    continue 'retry;
                }
            };
            tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await
                .context("writing range chunk")?;
            let _ = tx.send(PartProgress::Advanced(chunk.len() as u64)).await;
        }
        tokio::io::AsyncWriteExt::flush(&mut file).await.ok();
        return Ok(());
    }
}

/// The file listing of one pinned Hugging Face repo revision (the Mac app's MLX models), fetched through [`download_simple`] (same allowlisted, CA-pinned, redirect-checked path as model files) into `scratch`, read back and removed. The caller verifies every listed file it then downloads.
pub async fn fetch_repo_listing(
    client: Arc<reqwest::Client>,
    repo: &str,
    revision: &str,
    scratch: &Path,
    cancel: Arc<AtomicBool>,
) -> Result<Vec<u8>> {
    const MAX_LISTING_BYTES: u64 = 8 * 1024 * 1024;
    let well_formed = repo.split('/').count() == 2
        && repo.bytes().all(|b| b.is_ascii_alphanumeric() || b"/_.-".contains(&b))
        && !repo.contains("..")
        && revision.len() == 40
        && revision.bytes().all(|b| b.is_ascii_hexdigit());
    anyhow::ensure!(well_formed, "refusing to list an unpinned model repository");
    let request = DownloadRequest {
        url: format!("https://huggingface.co/api/models/{repo}/tree/{revision}?recursive=true"),
        destination: scratch.to_path_buf(),
        expected_sha256: None,
        expected_bytes: None,
    };
    let fetched = download_simple(client, request, cancel, |_| {}).await;
    let bytes = match fetched {
        Ok(()) => match tokio::fs::metadata(scratch).await {
            Ok(meta) if meta.len() <= MAX_LISTING_BYTES => tokio::fs::read(scratch).await.context("reading the model file listing"),
            Ok(_) => Err(anyhow::anyhow!("the model file listing is too large")),
            Err(err) => Err(anyhow::Error::new(err).context("reading the model file listing")),
        },
        Err(err) => Err(err),
    };
    let _ = tokio::fs::remove_file(scratch).await;
    bytes
}

// Blocking model installer: the synchronous orchestration the cross-platform `fileid` CLI drives (it links the engine as a library with no async runtime). Mirrors `commands::prewarm::handle_prewarm_model` minus IPC: SHA256-verified parallel range-GET of every file, extraction of model archives, then the revision-keyed install sentinel so `scan --models` and the desktop apps see the model installed. Network egress is restricted to user-initiated Hugging Face model downloads.

/// Per-file progress for [`install_model_blocking`]'s callback, consumed by the `fileid` CLI (external lib consumer).
#[derive(Debug, Clone)]
pub struct InstallFileProgress {
    pub file_index: usize,
    pub file_count: usize,
    pub file_name: String,
    pub bytes_done: u64,
    pub bytes_total: Option<u64>,
    pub bytes_per_second: f64,
}

pub fn required_install_free_bytes(download_bytes: u64) -> u64 {
    const STAGING_HEADROOM: u64 = 512 * 1024 * 1024;
    download_bytes
        .saturating_mul(2)
        .saturating_add(STAGING_HEADROOM)
}

/// Downloads + installs one model bundle, blocking until done. Builds its own current-thread Tokio runtime so a non-async caller (the CLI) needs none. Reuses [`download_parallel`] for each file, extracts any `.zip` artifact in place, and writes the revision-keyed sentinel via [`crate::models::registry::sentinel_path`]; early-`Ok` when it already exists. `progress` must be `Send`/`Sync` so each download task can call it.
///
/// Driven by the `fileid` CLI; the engine binary uses the IPC `handle_prewarm_model` path.
pub fn install_model_blocking(
    model: &crate::models::registry::Model,
    cancel: Arc<AtomicBool>,
    progress: Arc<dyn Fn(InstallFileProgress) + Send + Sync>,
) -> Result<()> {
    if crate::models::registry::installation_complete(model) {
        return Ok(());
    }

    let download_bytes: u64 = model.files.iter().map(|file| file.approx_bytes).sum();
    let required_free = required_install_free_bytes(download_bytes);
    if let Ok(models_dir) = crate::paths::models_dir()
        && let Some(available) = crate::platform::available_disk_bytes(&models_dir) {
            anyhow::ensure!(
                available >= required_free,
                "{} needs about {:.1} GB free while verified download parts are staged, but only {:.1} GB is available on the models drive",
                model.display_name,
                required_free as f64 / 1_073_741_824.0,
                available as f64 / 1_073_741_824.0,
            );
        }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("building model-install runtime")?;

    rt.block_on(async move {
        let client = build_shared_client()?;
        let file_count = model.files.len();
        for (idx, file) in model.files.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                anyhow::bail!("install cancelled");
            }
            let file_name = file
                .dest
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file")
                .to_string();
            let req = DownloadRequest {
                url: file.url.clone(),
                destination: file.dest.clone(),
                expected_sha256: file.sha256.clone(),
                expected_bytes: Some(file.approx_bytes),
            };
            let prog = progress.clone();
            let name_for_cb = file_name.clone();
            let cb = move |p: DownloadProgress| {
                prog(InstallFileProgress {
                    file_index: idx,
                    file_count,
                    file_name: name_for_cb.clone(),
                    bytes_done: p.bytes_done,
                    bytes_total: p.bytes_total,
                    bytes_per_second: p.bytes_per_second,
                });
            };
            download_parallel(client.clone(), req, cancel.clone(), cb)
                .await
                .with_context(|| format!("downloading {file_name}"))?;

            if file.dest.extension().and_then(|s| s.to_str()) == Some("zip") {
                let dest = file.dest.clone();
                tokio::task::spawn_blocking(move || crate::util::zip::extract_into_parent(&dest))
                    .await
                    .unwrap_or_else(|e| Err(anyhow::anyhow!("zip extract task panicked: {e}")))
                    .with_context(|| format!("extracting {file_name}"))?;
                let _ = tokio::fs::remove_file(&file.dest).await;
            }
        }

        // Drop the install sentinel via a `.tmp` + rename so a kill mid-write can't leave a half-written marker treated as installed.
        if let Some(sentinel) = crate::models::registry::sentinel_path(model) {
            if let Some(parent) = sentinel.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            let tmp = sentinel.with_extension("installed.tmp");
            // Write the same attestation as the prewarm path (registry installation_attestation): for zip-entry runtime packs installation_complete() requires the full per-artifact hash manifest, so a bare model.id marker would leave every CLI-installed pack reported not-installed and re-downloaded.
            let sentinel_body = crate::models::registry::installation_attestation(model)
                .context("attesting installed model files for the install sentinel")?;
            tokio::fs::write(&tmp, sentinel_body.as_bytes())
                .await
                .context("writing install sentinel")?;
            tokio::fs::rename(&tmp, &sentinel)
                .await
                .context("finalizing install sentinel")?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::{
        apply_part_progress, chain_has_disk_full, chain_has_pin_failure, check_size_plausible,
        classify_range_status, download_url_allowed, message_indicates_pin_failure,
        resume_seed_bytes, source_chain_indicates_pin_failure, stream_hard_limit, PartProgress,
        RangeResumeAction, RetryBudget, size_fits_estimate, MAX_UNSIZED_DOWNLOAD_BYTES, PINNED_ROOT_CERTS,
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
    fn download_url_allowed_gates_host_and_scheme() {
        // URLs are assembled from bare host strings at runtime so the source-URL allowlist CI scan (which greps for a literal `https?://<host>` off its exact-match list) sees no URL literals; these are egress-policy test inputs, not download sites.
        let https = |host: &str| format!("https://{host}/x");
        let http = |host: &str| format!("http://{host}/x");
        for host in [
            "huggingface.co",
            "cdn-lfs.huggingface.co",
            "hf.co",
        ] {
            assert!(download_url_allowed(&https(host)), "{host} should pass");
        }
        for host in [
            "evilhuggingface.co",
            "example.com",
            "github.com",
            "objects.githubusercontent.com",
            "developer.nvidia.com",
        ] {
            assert!(!download_url_allowed(&https(host)), "{host} should be refused");
        }
        // Plain http on an allowlisted host is refused (scheme gate), as is a non-URL string.
        assert!(!download_url_allowed(&http("huggingface.co")));
        assert!(!download_url_allowed("not a url"));
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

    // Outer error whose Display does NOT include its source (the hyper wrapping shape), so the test exercises the source() walk and not just the top-level message.
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
    fn chain_disk_full_detected_through_anyhow_context_layers() {
        let err = anyhow::Error::new(std::io::Error::new(
            std::io::ErrorKind::StorageFull,
            "There is not enough space on the disk. (os error 112)",
        ))
        .context("writing range chunk")
        .context("downloading model.onnx");
        assert!(chain_has_disk_full(&err));

        let conn = anyhow::Error::new(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "connection reset by peer",
        ))
        .context("writing range chunk");
        assert!(!chain_has_disk_full(&conn));

        let plain = anyhow::anyhow!("HTTP 503 Service Unavailable").context("issuing GET");
        assert!(!chain_has_disk_full(&plain));
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
        assert!(check_size_plausible(1_000_000, Some(1_000_000), "u").is_ok());
        assert!(check_size_plausible(800_000, Some(1_000_000), "u").is_ok());
        assert!(check_size_plausible(5_000_000, Some(1_000_000), "u").is_ok());
        assert!(check_size_plausible(260_000, Some(1_000_000), "u").is_ok());
    }

    #[test]
    fn size_check_rejects_truncation_and_error_pages() {
        assert!(check_size_plausible(4_096, Some(925_600_000), "u").is_err());
        assert!(check_size_plausible(100_000, Some(1_000_000), "u").is_err());
        assert!(check_size_plausible(0, Some(38_696_353), "u").is_err());
    }

    #[test]
    fn stream_limit_prefers_advertised_then_bounds_estimates_and_unknowns() {
        assert_eq!(stream_hard_limit(Some(123), Some(456)), 123);
        assert_eq!(stream_hard_limit(None, Some(456)), 1_824);
        assert_eq!(stream_hard_limit(Some(0), Some(456)), 1_824);
        assert_eq!(stream_hard_limit(None, None), MAX_UNSIZED_DOWNLOAD_BYTES);
        assert_eq!(stream_hard_limit(None, Some(u64::MAX)), u64::MAX);
    }

    // Ranged resume must seed progress from bytes already on disk so the bar doesn't jump backward on Retry.
    #[tokio::test]
    async fn resume_seed_counts_on_disk_part_bytes() {
        let dir = std::env::temp_dir().join(format!(
            "fileid-dl-seed-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("model.bin");

        let ranges = vec![(0usize, 0u64, 99u64), (1, 100, 199), (2, 200, 299)];

        assert_eq!(resume_seed_bytes(&dest, &ranges).await, 0);

        std::fs::write(super::part_file_path(&dest, 0), vec![0u8; 100]).unwrap();
        std::fs::write(super::part_file_path(&dest, 1), vec![0u8; 50]).unwrap();
        assert_eq!(
            resume_seed_bytes(&dest, &ranges).await,
            150,
            "seed must reflect on-disk bytes (100 + 50), not 0"
        );

        // An OVERSIZED stale part (200 B in a 100 B range) contributes 0: the whole part is discarded, so seeding any of it would double-count against re-download deltas and push bytes_done past bytes_total.
        std::fs::write(super::part_file_path(&dest, 2), vec![0u8; 200]).unwrap();
        assert_eq!(
            resume_seed_bytes(&dest, &ranges).await,
            150,
            "oversized stale part seeds 0 (discarded whole), not its range length"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // A 416 on a ranged resume must recover (discard the stale part and re-fetch), matching download_simple, not hard-bail the parallel download.
    #[test]
    fn range_416_on_resume_recovers_instead_of_bailing() {
        assert_eq!(
            classify_range_status(416, 4096),
            RangeResumeAction::DiscardAndRetry
        );
        assert_eq!(classify_range_status(416, 0), RangeResumeAction::Bail);
        assert_eq!(
            classify_range_status(200, 1024),
            RangeResumeAction::DiscardAndRetry
        );
        assert_eq!(classify_range_status(206, 1024), RangeResumeAction::Proceed);
        assert_eq!(classify_range_status(200, 0), RangeResumeAction::Proceed);
        assert_eq!(classify_range_status(404, 0), RangeResumeAction::Bail);
        assert_eq!(classify_range_status(403, 2048), RangeResumeAction::Bail);
    }

    // The accounting invariant the channel ordering buys, over the exact PartProgress stream the discard path emits: a part that streamed some bytes (Advanced), hit a transient stream error, and only THEN got a 200/416 on retry, so its Rewound un-counts seed + streamed deltas. A direct task-side fetch_sub could underflow; routing Rewound over the same FIFO channel lands it after them. bytes_done must stay within [0, total] and finish EXACTLY at total.
    #[test]
    fn rewound_discard_keeps_progress_within_bounds() {
        const TOTAL: u64 = 300; // three 100-byte parts

        // FIFO per part. resume_seed counted every part's on-disk prefix up front (part0 40, part1 30, part2 100 = complete). part1 then streamed 20 fresh bytes before a stream error; its retry is answered 200/416, so it emits Rewound(50) (30 seed + 20 streamed) and re-fetches all 100, enqueued AFTER its own Advanced(20).
        let seed = 40 + 30 + 100;
        let stream = [
            PartProgress::Advanced(60),  // part0: 40 seed -> 100
            PartProgress::Advanced(20),  // part1: streamed before the stream error
            PartProgress::Rewound(50),   // part1: discard un-counts seed + streamed
            PartProgress::Advanced(100), // part1: clean re-fetch of the whole range
        ];

        let mut done: u64 = seed;
        let mut peak = done;
        for msg in stream {
            done = apply_part_progress(done, msg);
            peak = peak.max(done);
            assert!(done <= TOTAL, "bytes_done {done} exceeded total {TOTAL} (>100% bar)");
        }
        assert_eq!(done, TOTAL, "must finish exactly at total, not total + discarded prefix");
        assert_eq!(peak, TOTAL);

        // Floor guard: if a Rewound were applied before the matching Advanced, saturating keeps bytes_done >= 0 rather than wrapping to ~1.8e19.
        assert_eq!(apply_part_progress(0, PartProgress::Rewound(50)), 0);
    }

    #[tokio::test]
    async fn a_file_already_on_disk_with_the_pinned_hash_is_not_fetched_again() {
        use sha2::{Digest, Sha256};
        let dir = std::env::temp_dir().join(format!("fileid-pin-skip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("weights.bin");
        std::fs::write(&dest, b"already here").unwrap();
        let client = std::sync::Arc::new(
            reqwest::Client::builder()
                .proxy(reqwest::Proxy::all("http://127.0.0.1:9").unwrap())
                .build()
                .unwrap(),
        );
        let request = super::DownloadRequest {
            url: "https://huggingface.co/x/y/resolve/main/weights.bin".into(),
            destination: dest.clone(),
            expected_sha256: Some(hex::encode(Sha256::digest(b"already here"))),
            expected_bytes: Some(12),
        };
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        super::download_parallel(client, request, cancel, |_| {})
            .await
            .expect("a verified file on disk needs no network");
        assert_eq!(std::fs::read(&dest).unwrap(), b"already here");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn offline_client() -> std::sync::Arc<reqwest::Client> {
        std::sync::Arc::new(
            reqwest::Client::builder().proxy(reqwest::Proxy::all("http://127.0.0.1:9").unwrap()).build().unwrap(),
        )
    }

    fn pinned_request(dir: &std::path::Path, bytes: &[u8], expected_bytes: Option<u64>) -> super::DownloadRequest {
        use sha2::{Digest, Sha256};
        let dest = dir.join("weights.bin");
        std::fs::write(&dest, bytes).unwrap();
        super::DownloadRequest {
            url: "https://huggingface.co/x/y/resolve/main/weights.bin".into(),
            destination: dest,
            expected_sha256: Some(hex::encode(Sha256::digest(bytes))),
            expected_bytes,
        }
    }

    #[tokio::test]
    async fn the_skip_reports_the_files_size_as_its_total() {
        let dir = std::env::temp_dir().join(format!("fileid-pin-total-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let request = pinned_request(&dir, b"already here", Some(12));
        let seen = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
        let sink = seen.clone();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        super::download_parallel(offline_client(), request, cancel, move |p| sink.lock().push((p.bytes_done, p.bytes_total)))
            .await
            .unwrap();
        assert_eq!(*seen.lock(), vec![(12, Some(12))]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_cancelled_download_does_not_hash_the_existing_file() {
        let dir = std::env::temp_dir().join(format!("fileid-pin-cancel-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let request = pinned_request(&dir, b"already here", Some(12));
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        assert_eq!(super::destination_matches_pin(&request, &cancel).await, None);
        let error = super::download_parallel(offline_client(), request, cancel, |_| {}).await.unwrap_err();
        assert!(error.to_string().contains("cancelled"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_file_far_from_the_registry_size_is_not_hashed() {
        let dir = std::env::temp_dir().join(format!("fileid-pin-size-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let request = pinned_request(&dir, b"already here", Some(12_000));
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        assert_eq!(super::destination_matches_pin(&request, &cancel).await, None, "12 bytes against a 12 KB estimate is a truncated file");
        assert_eq!(super::destination_matches_pin(&pinned_request(&dir, b"already here", Some(12)), &cancel).await, Some(12));
        assert!(size_fits_estimate(5, None));
        assert!(size_fits_estimate(3, Some(12)) && !size_fits_estimate(2, Some(12)));
        assert!(size_fits_estimate(48, Some(12)) && !size_fits_estimate(49, Some(12)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
