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
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use futures_util::StreamExt;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};

pub struct VlmServer {
    // Held so the child is killed on drop (kill_on_drop). Never read directly.
    _child: Child,
    base_url: String,
    api_key: String,
    client: reqwest::Client,
    /// Inference slots the server was started with (`-np`); the batch driver
    /// bounds its in-flight request count to this.
    pub slots: usize,
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
    fn server_binaries() -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(root) = crate::paths::models_dir() {
            for dir in [root.join("llama.cpp-cuda"), root.join("llama.cpp")] {
                for cand in [
                    dir.join(format!("llama-server{BIN_EXT}")),
                    dir.join("bin").join(format!("llama-server{BIN_EXT}")),
                ] {
                    if cand.exists() {
                        out.push(cand);
                    }
                }
            }
        }
        out
    }

    /// Start the server with the given model + mmproj and wait for `/health`.
    /// The model load happens here (once); `complete()` calls are then cheap.
    /// Tries each candidate binary (CUDA → Vulkan) so a broken CUDA runtime
    /// never blocks the working Vulkan one.
    pub async fn start(gguf: &Path, mmproj: &Path, cancel: &AtomicBool) -> Result<Self> {
        crate::models::runtime::ensure_gpu_inference_alive()?;
        let bins = Self::server_binaries();
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
            match Self::start_with_binary(&bin, gguf, mmproj, cancel).await {
                Ok(server) => return Ok(server),
                Err(err) => {
                    if crate::coordinator::process_gpu_device_removed() {
                        return Err(err);
                    }
                    tracing::warn!(binary = %crate::platform::redact_path_for_log(&bin), ?err, "[VLM-SERVER] candidate failed; trying next backend");
                    last_err = Some(err);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("no VLM server binary could start")))
    }

    /// Server inference slots. Two concurrent requests keep the GPU busy while
    /// the other request is in CPU-side mtmd image preprocessing; more mostly
    /// grows KV/context VRAM for no wall-clock win at our short 30-80 token
    /// generations. 1 on small-VRAM cards, under forced-CPU, and when the
    /// weights themselves don't comfortably fit VRAM (a spilled model gains
    /// nothing from a second slot — it just doubles KV while layers run on
    /// CPU). Overridable via `FILEID_VLM_PARALLEL` (clamped 1..=4) for
    /// on-hardware tuning.
    fn parallel_slots(forced_cpu: bool, gguf: &Path) -> usize {
        // Q4 GGUF file size ≈ resident weight footprint; reserve ~4 GB for
        // the mmproj, KV across slots, and CUDA arenas.
        const SLOT_HEADROOM_MB: u64 = 4_000;
        let weights_mb = std::fs::metadata(gguf)
            .map(|m| m.len() / (1024 * 1024))
            .unwrap_or(u64::MAX);
        let default = if forced_cpu {
            1
        } else {
            match crate::platform::dedicated_vram_mb() {
                Some(vram_mb) if vram_mb >= 12_000 && weights_mb.saturating_add(SLOT_HEADROOM_MB) <= vram_mb => 2,
                _ => 1,
            }
        };
        std::env::var("FILEID_VLM_PARALLEL")
            .ok()
            .and_then(|s| s.trim().parse::<usize>().ok())
            .map(|v| v.clamp(1, 4))
            .unwrap_or(default)
    }

    async fn start_with_binary(
        bin: &Path,
        gguf: &Path,
        mmproj: &Path,
        cancel: &AtomicBool,
    ) -> Result<Self> {
        crate::models::runtime::ensure_gpu_inference_alive()?;
        let port = pick_free_port()?;
        let api_key = new_api_key();
        // GPU-TDR recovery parity: when the user pinned the EP to CPU, evacuate
        // the GPU for Deep Analyze too (not just the ORT scan path). (F-C1-006)
        let forced_cpu = crate::models::runtime::user_forced_cpu();
        let slots = Self::parallel_slots(forced_cpu, gguf);
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
            .arg(if forced_cpu { "0" } else { "99" })
            // -c is the TOTAL context, divided across slots by llama-server —
            // keep 4096 per slot. Continuous batching is default-on in this
            // build; flash attention defaults to 'auto'.
            .arg("-np")
            .arg(slots.to_string())
            .arg("-c")
            .arg((4096 * slots).to_string());
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
        cmd.kill_on_drop(true);

        let mut child = cmd.spawn().context("spawn bundled llama-server")?;
        let base_url = format!("http://127.0.0.1:{port}");
        let client = build_loopback_client()?;

        // Health-poll until ready, but bail FAST if the child exits early — a
        // CUDA build missing its runtime DLLs dies on launch, and we don't want
        // to wait the full timeout before falling back to Vulkan.
        let health_url = format!("{base_url}/health");
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            if let Err(err) = crate::models::runtime::ensure_gpu_inference_alive() {
                stop_child(&mut child).await;
                return Err(err);
            }
            if cancel.load(Ordering::Relaxed) {
                stop_child(&mut child).await;
                bail!("VLM server startup cancelled");
            }
            if let Ok(Some(status)) = child.try_wait() {
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
                    if let Some(status) = child.try_wait().context("poll VLM server after health")? {
                        bail!("llama-server exited after health check ({status})");
                    }
                    break;
                }
            }
            if Instant::now() >= deadline {
                stop_child(&mut child).await;
                bail!("llama-server did not become healthy within 120s");
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        tracing::info!(
            binary = %crate::platform::redact_path_for_log(bin),
            model = %crate::platform::redact_path_for_log(gguf),
            "[VLM-SERVER] ready"
        );
        Ok(Self {
            _child: child,
            base_url,
            api_key,
            client,
            slots,
        })
    }

    /// Run one multimodal completion: image + text prompt → text. The image is
    /// read from disk and inlined as a base64 data URI (the format
    /// `/v1/chat/completions` accepts for `image_url`).
    pub async fn complete(&self, image_path: &Path, prompt: &str, max_tokens: u32) -> Result<String> {
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
            "stream": false
        });
        // reqwest is built without the `json` feature here, so serialize the
        // body + parse the reply by hand via serde_json.
        let body_bytes = serde_json::to_vec(&body).context("encode VLM request body")?;
        let url = format!("{}/v1/chat/completions", self.base_url);
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .header("content-type", "application/json")
            .body(body_bytes)
            .send()
            .await
            .context("VLM chat/completions request")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = read_response_bounded(resp).await.unwrap_or_default();
            bail!("llama-server returned {status}: {text}");
        }
        let text = read_response_bounded(resp).await?;
        let json: serde_json::Value =
            serde_json::from_str(&text).context("parse VLM response JSON")?;
        let content = json["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| anyhow!("VLM response missing choices[0].message.content: {text}"))?;
        Ok(content.trim().to_string())
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

async fn stop_child(child: &mut Child) {
    if child.try_wait().ok().flatten().is_some() {
        return;
    }
    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
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
mod tests {
    use super::*;

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
