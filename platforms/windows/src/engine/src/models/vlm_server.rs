// Persistent llama.cpp server for VLM inference.
//
// Spawns `llama-server.exe` ONCE (loading the multi-GB model + mmproj a single
// time) and serves many images over its OpenAI-compatible
// `/v1/chat/completions` multimodal endpoint. This is the bulk path: the
// per-file `llama-mtmd-cli.exe` subprocess (`vlm::caption`) reloads the model
// on every call, so a whole-library Deep Analyze pass through the CLI is many
// hours; through the server it is ~1–3 s/file because the model stays resident.
//
// The CLI path remains as a fallback (single-file Deep Analyze, or when the
// server can't start). Binaries come from the same Vulkan runtime dir
// (`%LOCALAPPDATA%\FileID\Models\llama.cpp\`) that `VlmRunner` probes; the
// runtime bump to b9254 ships both `llama-mtmd-cli.exe` and `llama-server.exe`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use crate::resource_scheduler::{Demand, Lease, ResourceScheduler};
use super::native_admission::{reserve_host, HostAdmissionError, RequestWork};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use futures_util::StreamExt;
use tokio::process::Command;
use super::native_process::{OwnedProcess, ProcessTracker};

/// llama-server's own per-request measurements (`timings` in its reply).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Timings {
    pub prompt_tokens: u64,
    pub prompt_per_second: f64,
    pub predicted_tokens: u64,
    pub predicted_per_second: f64,
}

pub struct VlmServer {
    last_timings: parking_lot::Mutex<Option<Timings>>,
    process: OwnedProcess,
    base_url: String,
    api_key: String,
    client: reqwest::Client,
    scheduler: Arc<ResourceScheduler>,
    cancel: Arc<AtomicBool>,
    request_gate: tokio::sync::Mutex<()>,
}

/// Executable suffix for the bundled llama.cpp binaries — `.exe` on Windows,
/// bare elsewhere (parity with `whisper::BIN_EXT` / `vlm::BIN_EXT`).
#[cfg(windows)]
const BIN_EXT: &str = ".exe";
#[cfg(not(windows))]
const BIN_EXT: &str = "";
const MAX_VLM_ENCODED_BYTES: u64 = 64 * 1024 * 1024;
const MAX_VLM_RESPONSE_BYTES: usize = 1024 * 1024;

impl VlmServer {
    /// Candidate `llama-server` paths in preference order: the CUDA runtime
    /// first (faster on NVIDIA), then the universal Vulkan runtime. `start`
    /// tries each until one becomes healthy, so a present-but-broken CUDA
    /// runtime (e.g. missing cudart DLLs) transparently falls back to Vulkan.
    #[cfg(test)]
    pub(crate) async fn start_verified(model_kind: &str, cancel: &Arc<AtomicBool>) -> Result<Self> {
        let verified = super::model_provenance::verify_installed(model_kind, cancel.clone()).await?;
        verified.trace_verified_bytes();
        let (gguf, mmproj) = verified.paths();
        Self::start(gguf, mmproj, cancel).await
    }

    async fn server_binaries(cancel: &Arc<AtomicBool>) -> Result<Vec<PathBuf>> {
        super::native_runtime::candidates("llama-server", "FILEID_LLAMA_SERVER", cancel.clone()).await
    }

    /// Start the server with the given model + mmproj and wait for `/health`.
    /// The model load happens here (once); `complete()` calls are then cheap.
    /// Tries each candidate binary (CUDA → Vulkan) so a broken CUDA runtime
    /// never blocks the working Vulkan one.
    pub async fn start(gguf: &Path, mmproj: &Path, cancel: &Arc<AtomicBool>) -> Result<Self> {
        Self::start_with_arguments(gguf, mmproj, cancel, None).await
    }

    /// Start with explicit tuning flags instead of the stored profile. Used by
    /// the benchmark to try candidates; flags outside the allowlist are refused.
    pub async fn start_with_arguments(gguf: &Path, mmproj: &Path, cancel: &Arc<AtomicBool>, arguments: Option<&[String]>) -> Result<Self> {
        if arguments.is_some_and(|args| !super::launch_tuning::valid(args)) { bail!("Launch flags outside the tuning allowlist"); }
        if cancel.load(Ordering::Relaxed) { bail!("VLM server startup cancelled"); }
        let memory_mb = crate::util::model_memory::require_model_headroom(gguf,mmproj)
            .await.map_err(super::native_admission::HostAdmissionError)?;
        let scheduler = ResourceScheduler::shared();
        crate::models::runtime::ensure_gpu_inference_alive()?;
        let bins = Self::server_binaries(cancel).await?;
        if bins.is_empty() {
            bail!(
                "llama-server{BIN_EXT} not found — update the llama.cpp runtime from \
                 Settings -> Performance -> 'Install llama.cpp runtime'."
            );
        }
        let mut last_err: Option<anyhow::Error> = None;
        for bin in bins {
            crate::models::runtime::ensure_gpu_inference_alive()?;
            if cancel.load(Ordering::Relaxed) {
                bail!("VLM server startup cancelled");
            }
            let device = if crate::models::runtime::user_forced_cpu() { None }
                else { crate::models::vlm::discrete_gpu_device(&bin, cancel).await? };
            let resources = reserve_host(scheduler.clone(), Demand { memory_mb, cpu: 1, io: 1 }, cancel.clone()).await?;
            match Self::start_with_binary(&bin, gguf, mmproj, cancel, resources, device.as_deref(), arguments).await {
                Ok(server) => return Ok(server),
                Err(err) => {
                    if crate::coordinator::process_gpu_device_removed() || !ProcessTracker::shared().allows_fallback() {
                        return Err(err);
                    }
                    tracing::warn!(binary = %crate::platform::redact_path_for_log(&bin), ?err, "[VLM-SERVER] candidate failed; trying next backend");
                    last_err = Some(err);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("no VLM server binary could start")))
    }

    async fn start_with_binary(
        bin: &Path,
        gguf: &Path,
        mmproj: &Path,
        cancel: &Arc<AtomicBool>,
        resources: Arc<Lease>,
        device: Option<&str>,
        arguments: Option<&[String]>,
    ) -> Result<Self> {
        crate::models::runtime::ensure_gpu_inference_alive()?;
        let port = pick_free_port()?;
        let api_key = new_api_key();
        // GPU-TDR recovery parity: when the user pinned the EP to CPU, evacuate
        // the GPU for Deep Analyze too (not just the ORT scan path). (F-C1-006)
        let forced_cpu = crate::models::runtime::user_forced_cpu();
        // Flags kept by `fileid-bench --tune` for this model on this machine.
        // Forced CPU always wins over a tuned GPU offload.
        let tuned = if forced_cpu { None } else { arguments.map(<[String]>::to_vec).or_else(|| super::launch_tuning::lookup(gguf)) };
        let gpu_layers = tuned.as_ref().and_then(|args| args.chunks(2).find(|pair| pair[0] == "-ngl").map(|pair| pair[1].clone()));
        let mut cmd = Command::new(bin);
        cmd.arg("-m")
            .arg(gguf)
            .arg("--mmproj")
            .arg(mmproj)
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg(port.to_string())
            .arg("--api-key")
            .arg(&api_key)
            // Offload all layers to the GPU (0 when the user forced CPU). Falls
            // back to CPU layers if VRAM is short — llama.cpp handles the spill.
            .arg("-ngl")
            .arg(if forced_cpu { "0".to_owned() } else { gpu_layers.unwrap_or_else(|| "99".to_owned()) })
            .arg("-c")
            .arg("4096");
        for pair in tuned.iter().flat_map(|args| args.chunks(2)).filter(|pair| pair[0] != "-ngl") {
            cmd.arg(&pair[0]).arg(&pair[1]);
        }
        // Pin to the discrete GPU on hybrid iGPU+dGPU systems (no-op otherwise);
        // skipped under forced-CPU so we don't re-engage the evacuated GPU.
        if !forced_cpu {
            if let Some(device) = device { cmd.arg("--device").arg(device); }
        }
        if cancel.load(Ordering::Relaxed) {
            bail!("VLM server startup cancelled");
        }
        cmd.stdout(Stdio::null())
            .stderr(Stdio::null())
            .stdin(Stdio::null());
        let client = build_loopback_client()?;
        let child = OwnedProcess::spawn(&mut cmd, Some(resources.clone()), ProcessTracker::shared()).context("spawn owned llama-server")?;
        let base_url = format!("http://127.0.0.1:{port}");

        // Health-poll until ready, but bail FAST if the child exits early — a
        // CUDA build missing its runtime DLLs dies on launch, and we don't want
        // to wait the full timeout before falling back to Vulkan.
        let health_url = format!("{base_url}/health");
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            if let Err(err) = crate::models::runtime::ensure_gpu_inference_alive() {
                child.shutdown().await?;
                return Err(err);
            }
            if cancel.load(Ordering::Relaxed) {
                child.shutdown().await?;
                bail!("VLM server startup cancelled");
            }
            if let Some(status) = child.poll_exit()? {
                bail!("llama-server exited early ({status}) — likely missing GPU runtime DLLs");
            }
            // Short per-request timeout so a server that accepts the connection
            // but stalls the /health response can't defeat the 120s deadline by
            // hanging on the client's 300s default. (audit E13)
            if let Ok(resp) = client
                .get(&health_url)
                .bearer_auth(&api_key)
                .timeout(Duration::from_secs(2))
                .send()
                .await
            {
                if resp.status().is_success() {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    if let Some(status) = child.poll_exit().context("poll VLM server after health")? {
                        bail!("llama-server exited after health check ({status})");
                    }
                    break;
                }
            }
            if Instant::now() >= deadline {
                child.shutdown().await?;
                bail!("llama-server did not become healthy within 120s");
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        tracing::info!(
            binary = %crate::platform::redact_path_for_log(bin),
            model = %crate::platform::redact_path_for_log(gguf),
            "[VLM-SERVER] ready"
        );
        resources.retain_memory();
        Ok(Self {
            last_timings: parking_lot::Mutex::new(None),
            process: child,
            base_url,
            api_key,
            client,
            scheduler: ResourceScheduler::shared(),
            cancel: Arc::new(AtomicBool::new(false)),
            request_gate: tokio::sync::Mutex::new(()),
        })
    }

    /// Run one multimodal completion: image + text prompt → text. The image is
    /// read from disk and inlined as a base64 data URI (the format
    /// `/v1/chat/completions` accepts for `image_url`).
    #[cfg(test)]
    pub async fn complete(&self, image_path: &Path, prompt: &str, max_tokens: u32) -> Result<String> {
        self.complete_with_cancel(image_path, prompt, max_tokens, self.cancel.clone()).await
    }
    pub async fn complete_with_cancel(&self, image_path: &Path, prompt: &str, max_tokens: u32, cancel: Arc<AtomicBool>) -> Result<String> {
        self.complete_sequence_with_cancel(&[image_path], prompt, max_tokens, cancel).await
    }
    pub async fn complete_sequence_with_cancel(&self, image_paths: &[&Path], prompt: &str, max_tokens: u32, cancel: Arc<AtomicBool>) -> Result<String> {
        validate_sequence(image_paths, prompt, max_tokens)?;
        let _turn = tokio::select! { biased;
            _ = wait_request_cancelled(&cancel, &self.cancel) => return Err(HostAdmissionError(anyhow!("Native request cancelled before admission")).into()),
            turn = self.request_gate.lock() => turn,
        };
        if let Some(status) = self.process.poll_exit()? { bail!("VLM server already exited ({status})"); }
        let stop = self.cancel.clone(); let request = cancel.clone();
        let resources = super::native_admission::reserve_host_when(self.scheduler.clone(), Demand { memory_mb: 512, cpu: 1, io: 1 },
            move || stop.load(Ordering::Relaxed) || request.load(Ordering::Relaxed)).await?;
        if cancel.load(Ordering::Relaxed) || self.cancel.load(Ordering::Relaxed) { return Err(HostAdmissionError(anyhow!("Native request cancelled before dispatch")).into()); }
        let mut work = RequestWork::new(&self.process, resources)?;
        let (result, cancelled) = tokio::select! { biased;
            _ = wait_request_cancelled(&cancel, &self.cancel) => (Err(HostAdmissionError(anyhow!("Native request cancelled")).into()), true),
            result = self.complete_owned(image_paths, prompt, max_tokens, &mut work) => (result, false),
        };
        let native_incomplete = work.native_incomplete();
        drop(work);
        if cancelled && native_incomplete {
            if let Err(cleanup) = self.process.stop_and_wait().await { return Err(HostAdmissionError(cleanup.context("Cancelled request cleanup failed")).into()); }
        }
        result
    }

    /// Timings from the most recent completed request, when the server sent them.
    pub fn last_timings(&self) -> Option<Timings> {
        *self.last_timings.lock()
    }

    pub(crate) fn pid(&self) -> Option<u32> {
        self.process.pid()
    }

    pub(crate) fn reusable_idle(&self) -> bool {
        !self.cancel.load(Ordering::Relaxed) && self.process.reusable_idle()
    }
    pub(crate) async fn shutdown(self) -> Result<()> {
        self.cancel.store(true, Ordering::Relaxed);
        self.process.shutdown().await
    }

    async fn complete_owned(&self, image_paths: &[&Path], prompt: &str, max_tokens: u32, work: &mut RequestWork<'_>) -> Result<String> {
        let body = multimodal_body_admitted(image_paths, prompt, max_tokens, work.host_lease()).await?;
        let body_bytes = serde_json::to_vec(&body).context("encode VLM request body")?;
        let url = format!("{}/v1/chat/completions", self.base_url);
        work.begin_dispatch();
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .header("content-type", "application/json")
            .body(body_bytes)
            .send()
            .await
            .context("VLM chat/completions request")?;
        let status = resp.status();
        let bytes = read_response_bounded(resp).await?;
        work.complete();
        let text = String::from_utf8(bytes).context("VLM response wasn't UTF-8")?;
        if !status.is_success() { bail!("llama-server returned {status}: {text}"); }
        let json: serde_json::Value =
            serde_json::from_str(&text).context("parse VLM response JSON")?;
        *self.last_timings.lock() = parse_timings(&json["timings"]);
        let content = json["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| anyhow!("VLM response missing choices[0].message.content: {text}"))?;
        Ok(content.trim().to_string())
    }
}

async fn wait_request_cancelled(request: &AtomicBool, stop: &AtomicBool) {
    while !request.load(Ordering::Relaxed) && !stop.load(Ordering::Relaxed) { tokio::time::sleep(Duration::from_millis(25)).await; }
}

fn parse_timings(value: &serde_json::Value) -> Option<Timings> {
    let number = |key: &str| value[key].as_f64().filter(|v| v.is_finite() && *v >= 0.0);
    Some(Timings {
        prompt_tokens: value["prompt_n"].as_u64()?,
        prompt_per_second: number("prompt_per_second")?,
        predicted_tokens: value["predicted_n"].as_u64()?,
        predicted_per_second: number("predicted_per_second")?,
    })
}

fn build_loopback_client() -> Result<reqwest::Client> {
    build_loopback_client_with(reqwest::Client::builder())
}

fn build_loopback_client_with(builder: reqwest::ClientBuilder) -> Result<reqwest::Client> {
    builder
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(300))
        .build()
        .context("build VLM HTTP client")
}

async fn read_image_admitted(path: &Path, limit: u64, host: Arc<crate::resource_scheduler::Lease>) -> Result<Vec<u8>> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let _host = host;
        read_image_bounded_sync(&path, limit)
    }).await.context("join admitted image read")?
}

#[cfg(test)]
async fn read_image_bounded(path: &Path) -> Result<Vec<u8>> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || read_image_bounded_sync(&path, MAX_VLM_ENCODED_BYTES)).await.context("join fixture image read")?
}

fn read_image_bounded_sync(path: &Path, limit: u64) -> Result<Vec<u8>> {
    use std::io::Read;
    let safe_path = crate::platform::redact_path_for_log(path);
    let len = std::fs::metadata(path).with_context(|| format!("stat image {safe_path}"))?.len();
    if len > limit {
        bail!("image {} is {} bytes, exceeding the VLM encoded-input cap of {} bytes", safe_path, len, limit);
    }
    let file = std::fs::File::open(path).with_context(|| format!("open image {safe_path}"))?;
    let mut bytes = Vec::with_capacity(len as usize);
    file.take(limit + 1).read_to_end(&mut bytes).with_context(|| format!("read image {safe_path}"))?;
    if bytes.len() as u64 > limit { bail!("image {} grew beyond the VLM encoded-input cap while reading", safe_path); }
    Ok(bytes)
}

fn validate_sequence(image_paths: &[&Path], prompt: &str, max_tokens: u32) -> Result<()> {
    anyhow::ensure!((1..=8).contains(&image_paths.len()) && prompt.len() <= 32768 && (1..=4096).contains(&max_tokens), "Native request prompt/token bounds exceeded");
    Ok(())
}

async fn multimodal_body_admitted(image_paths: &[&Path], prompt: &str, max_tokens: u32, host: Arc<crate::resource_scheduler::Lease>) -> Result<serde_json::Value> {
    validate_sequence(image_paths, prompt, max_tokens)?;
    let mut total = 0u64;
    for path in image_paths {
        let metadata = tokio::fs::metadata(path).await.context("Read visual-frame size")?;
        let len = metadata.len();
        total = total.checked_add(len).context("Visual-frame size overflow")?;
        if (len == 0 && metadata.is_file()) || total > MAX_VLM_ENCODED_BYTES { bail!("Visual frames are empty or exceed the aggregate encoded-input limit"); }
    }
    let mut remaining = MAX_VLM_ENCODED_BYTES;
    let mut content = vec![serde_json::json!({"type": "text", "text": prompt})];
    for path in image_paths {
        let bytes = read_image_admitted(path, remaining, host.clone()).await?;
        if bytes.is_empty() { bail!("A visual-model frame is empty"); }
        remaining -= bytes.len() as u64;
        content.push(serde_json::json!({"type": "image_url", "image_url": {"url": format!("data:{};base64,{}", image_mime(&bytes), base64::engine::general_purpose::STANDARD.encode(&bytes))}}));
    }
    Ok(serde_json::json!({"messages": [{"role": "user", "content": content}], "max_tokens": max_tokens, "n_predict": max_tokens, "temperature": 0.0, "stream": false}))
}

#[cfg(test)]
async fn multimodal_body(image_paths: &[&Path], prompt: &str, max_tokens: u32) -> Result<serde_json::Value> {
    validate_sequence(image_paths, prompt, max_tokens)?;
    let mut total = 0u64;
    for path in image_paths {
        let metadata = tokio::fs::metadata(path).await.context("Read visual-frame size")?;
        let len = metadata.len();
        total = total.checked_add(len).context("Visual-frame size overflow")?;
        if (len == 0 && metadata.is_file()) || total > MAX_VLM_ENCODED_BYTES { bail!("Visual frames are empty or exceed the aggregate encoded-input limit"); }
    }
    let mut remaining = MAX_VLM_ENCODED_BYTES;
    let mut content = vec![serde_json::json!({"type": "text", "text": prompt})];
    for path in image_paths {
        let path = path.to_path_buf();
        let bytes = tokio::task::spawn_blocking(move || read_image_bounded_sync(&path, remaining)).await.context("join fixture image read")??;
        if bytes.is_empty() { bail!("A visual-model frame is empty"); }
        remaining -= bytes.len() as u64;
        content.push(serde_json::json!({"type": "image_url", "image_url": {"url": format!("data:{};base64,{}", image_mime(&bytes), base64::engine::general_purpose::STANDARD.encode(&bytes))}}));
    }
    Ok(serde_json::json!({"messages": [{"role": "user", "content": content}], "max_tokens": max_tokens, "n_predict": max_tokens, "temperature": 0.0, "stream": false}))
}

async fn read_response_bounded(resp: reqwest::Response) -> Result<Vec<u8>> {
    let mut stream = resp.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("read VLM response chunk")?;
        if bytes.len().saturating_add(chunk.len()) > MAX_VLM_RESPONSE_BYTES {
            bail!("VLM response exceeded {MAX_VLM_RESPONSE_BYTES} bytes");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn new_api_key() -> String {
    format!("fileid-{}", uuid::Uuid::new_v4().simple())
}

/// llama-server cannot inherit a pre-bound listener, so the parent must release
/// the selected loopback port before spawn. Startup still fails closed: health
/// requires an unguessable bearer token and readiness is accepted only after the
/// spawned child is confirmed alive, so a process that wins the bind race cannot
/// be mistaken for FileID's server.
fn pick_free_port() -> Result<u16> {
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").context("bind ephemeral port for VLM server")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

/// Sniff the image MIME type from the leading bytes so the data URI declares
/// the real format. `rasterize_for_vlm` passes image files through untouched
/// (PNG/WebP/etc.), so hard-coding image/jpeg was wrong; llama-server's loader
/// sniffs content, but declaring the truth is correct and not fragile.
fn image_mime(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        "image/png"
    } else if bytes.starts_with(&[0xFF, 0xD8]) {
        "image/jpeg"
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else if bytes.starts_with(b"GIF8") {
        "image/gif"
    } else if bytes.starts_with(&[0x42, 0x4D]) {
        "image/bmp"
    } else {
        "image/jpeg"
    }
}

#[cfg(test)]
#[path = "server_host_admission_tests.rs"]
mod admission_tests;

#[cfg(test)]
#[path = "server_preflight_typing_tests.rs"]
mod preflight_tests;

#[cfg(test)]
#[path = "request_terminal_lifetime_tests.rs"]
mod terminal_lifetime_tests;

#[cfg(test)]
#[path = "request_cancel_tests.rs"]
mod request_cancel_tests;

#[cfg(test)]
#[path = "resident_owner_tests.rs"]
mod resident_owner_tests;

#[cfg(test)]
#[path = "probe_fallback_tests.rs"]
mod probe_fallback_tests;
#[cfg(test)]
#[path = "benchmark_server_tests.rs"]
mod benchmark_server_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sequence_body_preserves_frame_order_and_generation_bound() {
        let directory = std::env::temp_dir().join(format!("fileid-sequence-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let first = directory.join("first.jpg");
        let second = directory.join("second.png");
        let jpeg = [0xFF, 0xD8, 0xFF];
        let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        std::fs::write(&first, jpeg).unwrap();
        std::fs::write(&second, png).unwrap();
        let body = multimodal_body(&[&second, &first], "Frame times: 1, 2", 512).await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
        let content = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 3);
        assert_eq!(content[0]["text"], "Frame times: 1, 2");
        assert_eq!(content[1]["image_url"]["url"], format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png)));
        assert_eq!(content[2]["image_url"]["url"], format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(jpeg)));
        assert_eq!(body["max_tokens"], 512);
        assert_eq!(body["n_predict"], 512);
        assert_eq!(body["temperature"], 0.0);
        assert_eq!(body["stream"], false);
    }

    #[tokio::test]
    async fn invalid_sequence_requests_fail_before_reading_files() {
        let missing = Path::new("missing-visual-frame.jpg");
        assert!(multimodal_body(&[], "frames", 512).await.is_err());
        assert!(multimodal_body(&[missing; 9], "frames", 512).await.is_err());
        assert!(multimodal_body(&[missing], &"x".repeat(32_769), 512).await.is_err());
        assert!(multimodal_body(&[missing], "frames", 0).await.is_err());
        assert!(multimodal_body(&[missing], "frames", 4097).await.is_err());
    }

    #[tokio::test]
    async fn aggregate_sequence_limit_rejects_before_allocating_sparse_frames() {
        let directory = std::env::temp_dir().join(format!("fileid-sequence-budget-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let first = directory.join("first.jpg");
        let second = directory.join("second.jpg");
        std::fs::File::create(&first).unwrap().set_len(MAX_VLM_ENCODED_BYTES).unwrap();
        std::fs::write(&second, [1]).unwrap();
        assert!(multimodal_body(&[&first, &second], "frames", 512).await.unwrap_err().to_string().contains("aggregate"));
        std::fs::remove_dir_all(directory).unwrap();
    }


    #[tokio::test]
    async fn bounded_image_reader_rejects_oversized_sparse_file() {
        let path = std::env::temp_dir().join(format!(
            "fileid-vlm-bound-{}-{}.jpg",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_VLM_ENCODED_BYTES + 1).unwrap();
        drop(file);
        let result = read_image_bounded(&path).await;
        let _ = std::fs::remove_file(path);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn bounded_image_reader_accepts_small_file() {
        let path = std::env::temp_dir().join(format!(
            "fileid-vlm-small-{}-{}.jpg",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, [0xFF, 0xD8, 0xFF]).unwrap();
        let bytes = read_image_bounded(&path).await.unwrap();
        let _ = std::fs::remove_file(path);
        assert_eq!(bytes, [0xFF, 0xD8, 0xFF]);
    }

    #[test]
    fn server_credentials_are_unique_and_full_strength() {
        let first = new_api_key();
        let second = new_api_key();
        assert_ne!(first, second);
        assert!(first.starts_with("fileid-"));
        let token = &first["fileid-".len()..];
        assert_eq!(token.len(), 32);
        assert!(token.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn loopback_client_never_follows_redirects() {
        use std::io::{Read, Write};

        let redirect = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let target = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        target.set_nonblocking(true).unwrap();
        let target_url = format!("http://{}/capture", target.local_addr().unwrap());
        let redirect_url = format!("http://{}/start", redirect.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = redirect.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            write!(
                stream,
                "HTTP/1.1 302 Found\r\nLocation: {target_url}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let response = runtime
            .block_on(async { build_loopback_client().unwrap().get(redirect_url).send().await })
            .unwrap();
        server.join().unwrap();
        assert!(response.status().is_redirection());
        std::thread::sleep(Duration::from_millis(25));
        assert!(matches!(target.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock));
    }

    #[test]
    fn loopback_client_bypasses_configured_proxy() {
        use std::io::{Read, Write};

        let target = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let proxy = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        proxy.set_nonblocking(true).unwrap();
        let target_url = format!("http://{}/health", target.local_addr().unwrap());
        let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = target.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK"
            )
            .unwrap();
        });
        let configured = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all(proxy_url).unwrap());
        let client = build_loopback_client_with(configured).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let response = runtime
            .block_on(async { client.get(target_url).send().await })
            .unwrap();
        server.join().unwrap();
        assert!(response.status().is_success());
        std::thread::sleep(Duration::from_millis(25));
        assert!(matches!(proxy.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock));
    }

    #[test]
    fn selected_server_port_is_loopback_only() {
        let port = pick_free_port().unwrap();
        let rebound = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).unwrap();
        assert_eq!(rebound.local_addr().unwrap().ip(), std::net::Ipv4Addr::LOCALHOST);
    }
}
