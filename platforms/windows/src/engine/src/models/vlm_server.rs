// Authenticated loopback inference transport. Admission and residency are owned
// by models::resident; native_process owns kill/reap and parent-death cleanup.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use futures_util::StreamExt;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use std::sync::Arc;
use crate::resource_scheduler::Lease;
use super::native_process::{OwnedProcess, ProcessTracker};

pub struct VlmServer {
    // Held so the child is killed on drop (kill_on_drop). Never read directly.
    process: super::native_process::OwnedProcess,
    #[cfg(test)]
    process_id: u32,
    base_url: String,
    api_key: String,
    client: reqwest::Client,
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
    pub(crate) fn server_binaries() -> Result<Vec<PathBuf>> {
        super::native_runtime::candidates("llama-server", "FILEID_LLAMA_SERVER")
    }

    /// Start the server with the given model + mmproj and wait for `/health`.
    /// The model load happens here (once); `complete()` calls are then cheap.
    /// Tries each candidate binary (CUDA → Vulkan) so a broken CUDA runtime
    /// never blocks the working Vulkan one.
    pub(crate) async fn start(gguf: &Path, mmproj: &Path, cancel: &AtomicBool, allow_gpu: bool, resources: Arc<Lease>, tracker: &ProcessTracker) -> Result<Self> {
        if cancel.load(Ordering::Relaxed) { bail!("VLM server startup cancelled"); }
        crate::util::model_memory::require_model_headroom(gguf,mmproj).await?;
        crate::models::runtime::ensure_gpu_inference_alive()?;
        let bins = Self::server_binaries()?;
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
            match Self::start_with_binary(&bin, gguf, mmproj, cancel, allow_gpu, resources.clone(), tracker).await {
                Ok(server) => return Ok(server),
                Err(err) => {
                    if tracker.has_unreaped() || crate::coordinator::process_gpu_device_removed() {
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
        cancel: &AtomicBool,
        allow_gpu: bool,
        resources: Arc<Lease>,
        tracker: &ProcessTracker,
    ) -> Result<Self> {
        crate::models::runtime::ensure_gpu_inference_alive()?;
        let port = pick_free_port()?;
        let api_key = new_api_key();
        // GPU-TDR recovery parity: when the user pinned the EP to CPU, evacuate
        // the GPU for Deep Analyze too (not just the ORT scan path). (F-C1-006)
        let forced_cpu = !allow_gpu || crate::models::runtime::user_forced_cpu()
            || crate::models::runtime::verified_gpu_capacity_mb().is_none();
        let mut cmd = Command::new(bin);
        cmd.arg("--offline").arg("-m")
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
            .arg(if forced_cpu { "0" } else { "99" })
            .arg("-c")
            .arg("4096");
        if forced_cpu { cmd.arg("--no-mmproj-offload"); }
        // Pin to the discrete GPU on hybrid iGPU+dGPU systems (no-op otherwise);
        // skipped under forced-CPU so we don't re-engage the evacuated GPU.
        if !forced_cpu {
            if let Some(dev) = crate::models::vlm::discrete_gpu_device(bin).await {
                cmd.arg("--device").arg(dev);
            }
        }
        if cancel.load(Ordering::Relaxed) {
            bail!("VLM server startup cancelled");
        }
        cmd.stdout(Stdio::null())
            .stderr(Stdio::null())
            .stdin(Stdio::null());
        // Kill the server if this handle is dropped (job done / cancelled /
        // engine exit) so we never orphan a multi-GB process.
        super::native_process::configure(&mut cmd);

        let client = build_loopback_client()?;
        let child = cmd.spawn().context("spawn bundled llama-server")?;
        #[cfg(test)]
        let process_id = child.id().context("native server process ID")?;
        let process = OwnedProcess::new(child, Some(resources), tracker);
        let base_url = format!("http://127.0.0.1:{port}");
        if let Err(error) = wait_for_readiness(&process, &client, &base_url, &api_key, cancel).await {
            process.shutdown().await.context("cleanup failed VLM startup")?;
            return Err(error);
        }
        tracing::info!(
            binary = %crate::platform::redact_path_for_log(bin),
            model = %crate::platform::redact_path_for_log(gguf),
            "[VLM-SERVER] ready"
        );
        Ok(Self {
            process,
            #[cfg(test)]
            process_id,
            base_url,
            api_key,
            client,
        })
    }

    /// Run one multimodal completion: image + text prompt → text. The image is
    /// read from disk and inlined as a base64 data URI (the format
    /// `/v1/chat/completions` accepts for `image_url`).
    #[allow(dead_code)]
    pub async fn complete(&self, image_path: &Path, prompt: &str, max_tokens: u32) -> Result<String> {
        self.image_request(image_path, prompt, max_tokens, false, |_| {}).await
    }
    pub async fn complete_stream(&self, image_path: &Path, prompt: &str, max_tokens: u32, on_token: impl FnMut(&str)) -> Result<String> {
        self.image_request(image_path, prompt, max_tokens, true, on_token).await
    }
    async fn image_request(&self, image_path: &Path, prompt: &str, max_tokens: u32, streaming: bool, on_token: impl FnMut(&str)) -> Result<String> {
        validate_generation("", prompt, max_tokens)?;
        let bytes = read_image_bounded(image_path).await?;
        let data_uri = format!(
            "data:{};base64,{}",
            image_mime(&bytes),
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        );
        let body = serde_json::json!({
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "text", "text": prompt },
                    { "type": "image_url", "image_url": { "url": data_uri } }
                ]
            }],
            // The OpenAI-compatible chat endpoint reads `max_tokens`; the native
            // completion endpoint reads `n_predict`. Send BOTH so the token cap
            // (80/40/30) is honored regardless of which the server build maps —
            // without this the server ran to its default cap (long, slow, and a
            // rename prompt could return a paragraph).
            "max_tokens": max_tokens,
            "n_predict": max_tokens,
            "temperature": 0.0,
            "stream": streaming
        });
        self.request(body, on_token).await
    }

    #[allow(dead_code)]
    pub async fn generate_text(
        &self, system: &str, prompt: &str, max_tokens: u32, on_token: impl FnMut(&str),
    ) -> Result<String> {
        validate_generation(system, prompt, max_tokens)?;
        self.request(serde_json::json!({
            "messages": [{"role": "system", "content": system}, {"role": "user", "content": prompt}],
            "max_tokens": max_tokens, "n_predict": max_tokens, "temperature": 0.0, "stream": true,
        }), on_token).await
    }

    async fn request(&self, body: serde_json::Value, mut on_token: impl FnMut(&str)) -> Result<String> {
        let streaming = body["stream"].as_bool() == Some(true);
        let body_bytes = serde_json::to_vec(&body).context("encode VLM request body")?;
        if body_bytes.len() > 96 * 1024 * 1024 { bail!("VLM request body exceeded cap"); }
        let resp = self.client.post(format!("{}/v1/chat/completions", self.base_url))
            .bearer_auth(&self.api_key).header("content-type", "application/json")
            .body(body_bytes).send().await.context("VLM chat/completions request")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = read_response_bounded(resp).await.unwrap_or_default();
            bail!("llama-server returned {status}: {text}");
        }
        if streaming {
            let mut stream = resp.bytes_stream();
            let mut decoder = SseDecoder::default();
            while let Some(chunk) = stream.next().await {
                if decoder.push(&chunk?, &mut on_token)? { return Ok(decoder.text); }
            }
            bail!("VLM stream ended without completion marker");
        }
        let text = read_response_bounded(resp).await?;
        let json: serde_json::Value = serde_json::from_str(&text).context("parse VLM response JSON")?;
        let content = json["choices"][0]["message"]["content"].as_str()
            .ok_or_else(|| anyhow!("VLM response missing message content"))?;
        Ok(content.trim().to_string())
    }

    #[cfg(test)]
    pub(crate) fn fixture(base_url: String, resources: Arc<Lease>, tracker: &ProcessTracker) -> Self {
        let mut command = super::native_process::test_child_command();
        let child = command.spawn().unwrap();
        Self { process_id: child.id().unwrap(), process: OwnedProcess::new(child, Some(resources), tracker), base_url, api_key: new_api_key(), client: build_loopback_client().unwrap() }
    }

    #[cfg(test)]
    pub(crate) fn process_id(&self) -> u32 { self.process_id }

    pub(crate) fn retain_work(&self, resources: Arc<Lease>) -> Result<()> { self.process.retain_work(resources) }
    pub(crate) fn release_work(&self) { self.process.release_work(); }
    pub async fn shutdown(self) -> Result<()> { self.process.shutdown().await }
}

async fn wait_for_readiness(process: &OwnedProcess, client: &reqwest::Client, base_url: &str, api_key: &str, cancel: &AtomicBool) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        crate::models::runtime::ensure_gpu_inference_alive()?;
        if cancel.load(Ordering::Relaxed) { bail!("VLM server startup cancelled"); }
        if let Some(status) = process.poll_exit()? { bail!("llama-server exited during startup ({status})"); }
        if let Ok(response) = client.get(format!("{base_url}/health")).bearer_auth(api_key).timeout(Duration::from_secs(2)).send().await {
            if response.status().is_success() {
                tokio::time::sleep(Duration::from_millis(100)).await;
                if let Some(status) = process.poll_exit()? { bail!("llama-server exited after health check ({status})"); }
                return Ok(());
            }
        }
        if Instant::now() >= deadline { bail!("llama-server did not become healthy within 120s"); }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

pub(crate) fn validate_generation(system: &str, prompt: &str, max_tokens: u32) -> Result<()> {
    if system.len().saturating_add(prompt.len()) > 32 * 1024 || prompt.trim().is_empty() || !(1..=512).contains(&max_tokens) {
        bail!("Invalid bounded generation request");
    }
    Ok(())
}

#[derive(Default)]
struct SseDecoder { pending: Vec<u8>, text: String, received: usize }
impl SseDecoder {
    fn push(&mut self, chunk: &[u8], on_token: &mut impl FnMut(&str)) -> Result<bool> {
        self.received = self.received.saturating_add(chunk.len());
        if self.received > MAX_VLM_RESPONSE_BYTES { bail!("VLM stream exceeded response cap"); }
        self.pending.extend_from_slice(chunk);
        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<_> = self.pending.drain(..=end).collect();
            let line = std::str::from_utf8(&line)?.trim();
            let Some(data) = line.strip_prefix("data:") else { continue };
            let data = data.trim();
            if data == "[DONE]" { return Ok(true); }
            let json: serde_json::Value = serde_json::from_str(data).context("parse VLM stream event")?;
            if json.get("error").is_some() { bail!("VLM stream returned an error"); }
            if let Some(token) = json["choices"][0]["delta"]["content"].as_str() {
                self.text.push_str(token); on_token(token);
            }
        }
        Ok(false)
    }

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



async fn read_image_bounded(path: &Path) -> Result<Vec<u8>> {
    let safe_path = crate::platform::redact_path_for_log(path);
    let len = tokio::fs::metadata(path)
        .await
        .with_context(|| format!("stat image {safe_path}"))?
        .len();
    if len > MAX_VLM_ENCODED_BYTES {
        bail!(
            "image {} is {} bytes, exceeding the VLM encoded-input cap of {} bytes",
            safe_path,
            len,
            MAX_VLM_ENCODED_BYTES
        );
    }
    let mut file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("open image {safe_path}"))?;
    let mut bytes = Vec::with_capacity(len as usize);
    tokio::io::AsyncReadExt::take(&mut file, MAX_VLM_ENCODED_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .with_context(|| format!("read image {safe_path}"))?;
    if bytes.len() as u64 > MAX_VLM_ENCODED_BYTES {
        bail!(
            "image {} grew beyond the VLM encoded-input cap while reading",
            safe_path
        );
    }
    Ok(bytes)
}

async fn read_response_bounded(resp: reqwest::Response) -> Result<String> {
    let mut stream = resp.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("read VLM response chunk")?;
        if bytes.len().saturating_add(chunk.len()) > MAX_VLM_RESPONSE_BYTES {
            bail!("VLM response exceeded {MAX_VLM_RESPONSE_BYTES} bytes");
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).context("VLM response wasn't UTF-8")
}

fn new_api_key() -> String {
    format!("fileid-{}", uuid::Uuid::new_v4().simple())
}

/// llama-server cannot inherit a pre-bound listener, so the parent must release
/// the selected loopback port before spawn. Upstream health is public; inference
/// requires the per-session bearer token. Readiness also checks that the spawned
/// child remains alive after health; a health reply alone is not proof that the
/// owned process is ready.
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
mod tests {
    use super::*;

    async fn delayed_readiness_cleanup(drop_startup: bool) {
        use crate::resource_scheduler::{Demand, Priority, ResourceScheduler};
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let (seen, health_seen) = tokio::sync::oneshot::channel();
        let responder = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 2048]; assert!(stream.read(&mut request).unwrap() > 0);
            write!(stream, "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            seen.send(()).unwrap();
        });
        let scheduler = ResourceScheduler::new(|| (16384, 12000), 1, 1);
        let tracker = Arc::new(ProcessTracker::default());
        let lease = Arc::new(scheduler.reserve(Demand { memory_mb: 1024, cpu: 1, io: 1 }, Priority::Interactive, || false).unwrap());
        let server = VlmServer::fixture(base_url, lease, &tracker);
        let pid = server.process_id();
        let cancel = Arc::new(AtomicBool::new(false));
        let startup = { let cancel = cancel.clone(); tokio::spawn(async move {
            let result = wait_for_readiness(&server.process, &server.client, &server.base_url, &server.api_key, &cancel).await;
            server.shutdown().await.unwrap();
            result
        }) };
        tokio::time::timeout(Duration::from_secs(3), health_seen).await.unwrap().unwrap();
        assert!(super::super::native_process::test_child_alive(pid));
        assert_eq!(scheduler.snapshot().memory_mb, 1024);
        assert!(scheduler.reserve(Demand { cpu: 1, io: 1, ..Demand::default() }, Priority::Background, || false).is_err());
        if drop_startup { startup.abort(); let _ = startup.await; }
        else { cancel.store(true, Ordering::Relaxed); assert!(startup.await.unwrap().unwrap_err().to_string().contains("startup cancelled")); }
        tracker.shutdown().await.unwrap();
        assert!(!super::super::native_process::test_child_alive(pid));
        assert_eq!(scheduler.snapshot().active, 0);
        responder.join().unwrap();
    }
    #[tokio::test]
    async fn cancelled_delayed_readiness_reaps_before_releasing_startup_admission() { delayed_readiness_cleanup(false).await; }
    #[tokio::test]
    async fn dropped_delayed_readiness_keeps_owned_cleanup_and_admission() { delayed_readiness_cleanup(true).await; }

    #[test]
    fn sse_fragmentation_utf8_crlf_and_done_are_bounded() {
        let mut decoder = SseDecoder::default();
        let mut tokens = String::new();
        let bytes = "data: {\"choices\":[{\"delta\":{\"content\":\"caf\u{e9}\"}}]}\r\n\r\ndata: [DONE]\n".as_bytes();
        let mut done = false;
        for byte in bytes { done |= decoder.push(&[*byte], &mut |token| tokens.push_str(token)).unwrap(); }
        assert!(done); assert_eq!(tokens, "café"); assert_eq!(decoder.text, tokens);
        assert!(SseDecoder::default().push(&vec![0; MAX_VLM_RESPONSE_BYTES + 1], &mut |_| {}).is_err());
        assert!(validate_generation("", "ok", 513).is_err());
        assert!(validate_generation("", &"x".repeat(32769), 1).is_err());
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
