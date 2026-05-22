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
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use tokio::process::{Child, Command};

pub struct VlmServer {
    // Held so the child is killed on drop (kill_on_drop). Never read directly.
    _child: Child,
    base_url: String,
    client: reqwest::Client,
}

impl VlmServer {
    /// Candidate `llama-server.exe` paths in preference order: the CUDA runtime
    /// first (faster on NVIDIA), then the universal Vulkan runtime. `start`
    /// tries each until one becomes healthy, so a present-but-broken CUDA
    /// runtime (e.g. missing cudart DLLs) transparently falls back to Vulkan.
    fn server_binaries() -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(root) = crate::paths::models_dir() {
            for dir in [root.join("llama.cpp-cuda"), root.join("llama.cpp")] {
                for cand in [
                    dir.join("llama-server.exe"),
                    dir.join("bin").join("llama-server.exe"),
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
    pub async fn start(gguf: &Path, mmproj: &Path) -> Result<Self> {
        let bins = Self::server_binaries();
        if bins.is_empty() {
            bail!(
                "llama-server.exe not found — update the llama.cpp runtime from \
                 Settings -> Performance -> 'Install llama.cpp runtime'."
            );
        }
        let mut last_err: Option<anyhow::Error> = None;
        for bin in bins {
            match Self::start_with_binary(&bin, gguf, mmproj).await {
                Ok(server) => return Ok(server),
                Err(err) => {
                    tracing::warn!(binary = %bin.display(), ?err, "[VLM-SERVER] candidate failed; trying next backend");
                    last_err = Some(err);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("no VLM server binary could start")))
    }

    async fn start_with_binary(bin: &Path, gguf: &Path, mmproj: &Path) -> Result<Self> {
        let port = pick_free_port()?;
        let mut cmd = Command::new(bin);
        cmd.arg("-m")
            .arg(gguf)
            .arg("--mmproj")
            .arg(mmproj)
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg(port.to_string())
            // Offload all layers to the GPU. Falls back to CPU layers if VRAM is
            // short — llama.cpp handles the spill.
            .arg("-ngl")
            .arg("99")
            .arg("-c")
            .arg("4096");
        // Pin to the discrete GPU on hybrid iGPU+dGPU systems (no-op otherwise).
        if let Some(dev) = crate::models::vlm::discrete_gpu_device(bin).await {
            cmd.arg("--device").arg(dev);
        }
        cmd.stdout(Stdio::null())
            .stderr(Stdio::null())
            .stdin(Stdio::null());
        // Kill the server if this handle is dropped (job done / cancelled /
        // engine exit) so we never orphan a multi-GB process.
        cmd.kill_on_drop(true);

        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawn {}", bin.display()))?;
        let base_url = format!("http://127.0.0.1:{port}");
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .context("build VLM HTTP client")?;

        // Health-poll until ready, but bail FAST if the child exits early — a
        // CUDA build missing its runtime DLLs dies on launch, and we don't want
        // to wait the full timeout before falling back to Vulkan.
        let health_url = format!("{base_url}/health");
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            if let Ok(Some(status)) = child.try_wait() {
                bail!("llama-server exited early ({status}) — likely missing GPU runtime DLLs");
            }
            if let Ok(resp) = client.get(&health_url).send().await {
                if resp.status().is_success() {
                    break;
                }
            }
            if Instant::now() >= deadline {
                let _ = child.start_kill();
                bail!("llama-server did not become healthy within 120s");
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        tracing::info!(binary = %bin.display(), model = %gguf.display(), "[VLM-SERVER] ready");
        Ok(Self {
            _child: child,
            base_url,
            client,
        })
    }

    /// Run one multimodal completion: image + text prompt → text. The image is
    /// read from disk and inlined as a base64 data URI (the format
    /// `/v1/chat/completions` accepts for `image_url`).
    pub async fn complete(&self, image_path: &Path, prompt: &str, max_tokens: u32) -> Result<String> {
        let bytes = tokio::fs::read(image_path)
            .await
            .with_context(|| format!("read image {}", image_path.display()))?;
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
            .header("content-type", "application/json")
            .body(body_bytes)
            .send()
            .await
            .context("VLM chat/completions request")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("llama-server returned {status}: {text}");
        }
        let text = resp.text().await.context("read VLM response body")?;
        let json: serde_json::Value =
            serde_json::from_str(&text).context("parse VLM response JSON")?;
        let content = json["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| anyhow!("VLM response missing choices[0].message.content: {text}"))?;
        Ok(content.trim().to_string())
    }
}

/// Bind an ephemeral port, read it, release it, and hand it to the server.
/// There's a small TOCTOU window between releasing and the server binding, but
/// it's the standard approach and collisions on 127.0.0.1 are vanishingly rare.
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
