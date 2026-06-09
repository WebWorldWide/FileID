// Subprocess wrapper around llama.cpp's `llama-mtmd-cli.exe` for VLM
// inference (Deep Analyze).
//
// We deliberately go through a subprocess instead of linking the
// `llama-cpp-2` Rust bindings by default. The native binding adds ~150
// MB of build artifacts and requires cmake at build time. The subprocess
// path keeps CI builds fast and the runtime DLL surface small. Toggle
// `--features vlm-native` at build time if zero-subprocess inference
// matters for your deployment.
//
// Hardening: `find()` only probes `%LOCALAPPDATA%\FileID\Models\
// llama.cpp\` (no PATH lookup — supply-chain hardening). Binary is
// sanity-checked (PE header + size bounds) before spawning.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

/// Default caption prompt.
pub const CAPTION_PROMPT: &str = "Describe this image in one specific, factual sentence: name the main subjects (people by count, notable objects, the place, the activity) and transcribe any visible text verbatim. Be concrete and definite — no hedging like \"appears to be\" or \"likely\", no generic filler, no preamble.";

/// Default rename prompt — produces a short, kebab-cased filename
/// suitable for `sanitize_proposed_name`.
pub const RENAME_PROMPT: &str = "Suggest a 3 to 5 word lowercase filename that names the SPECIFIC subject of this image (never generic words like photo, image, or picture), hyphen-separated, no quotes, no extension.";

/// Tagging prompt — produces 1–2 specific, concrete content tags. Parsed by
/// `deep_analyze::parse_vlm_tags` (which caps at 2 and drops generic tokens)
/// into individual `source='vlm'` tags shown as Library chips. A real
/// vision-language model names what's actually in the image; the prompt is
/// deliberately strict (concrete nouns, no meta words) because smaller VLMs
/// otherwise drift toward vague labels like "photo" / "object".
pub const TAG_PROMPT: &str = "Give 1 or 2 specific lowercase tags naming the main subject of this image (for example: golden retriever, mountain lake, birthday cake, sushi platter). Use concrete nouns. Do not use generic words like photo, image, picture, object, thing, scene, background, location, or text. Comma-separated, no sentences, no numbering.";

#[derive(Debug)]
pub struct VlmRunner {
    pub binary: PathBuf,
}

#[derive(Debug, Clone)]
pub struct CaptionRequest {
    pub gguf_path: PathBuf,
    pub mmproj_path: PathBuf,
    pub image_path: PathBuf,
    pub prompt: String,
    pub max_tokens: u32,
    pub greedy: bool,
}

#[derive(Debug, Clone)]
pub struct CaptionResult {
    pub text: String,
}

impl VlmRunner {
    /// Locate `llama-mtmd-cli.exe` under `%LOCALAPPDATA%\FileID\Models\
    /// llama.cpp\` and verify it's a sane PE binary in the expected
    /// size range. Returns Err with a friendly message if missing —
    /// callers surface that to the user as "VLM runtime not installed".
    pub fn find() -> Result<Self> {
        let root = crate::paths::models_dir().context("resolving Models dir")?;
        let vulkan_dir = root.join("llama.cpp");
        let cuda_dir = root.join("llama.cpp-cuda");
        // Prefer the CUDA runtime (faster on NVIDIA) when it has the multimodal
        // binary AND it actually launches; otherwise the universal Vulkan
        // runtime. `sanity_check_binary` runs `--version`, so a CUDA build
        // missing its runtime DLLs fails the probe and we fall through to Vulkan
        // instead of erroring.
        for dir in [&cuda_dir, &vulkan_dir] {
            for cand in [
                dir.join("llama-mtmd-cli.exe"),
                dir.join("bin").join("llama-mtmd-cli.exe"),
            ] {
                if cand.exists() && sanity_check_binary(&cand).is_ok() {
                    return Ok(VlmRunner { binary: cand });
                }
            }
        }
        // Distinguish "not installed at all" from "installed but too old".
        // Pre-mtmd-unification llama.cpp builds (≈b4400 and earlier) ship
        // llama-server.exe / llama-llava-cli.exe / llama-qwen2vl-cli.exe but
        // NOT the unified llama-mtmd-cli.exe this code drives — and they also
        // predate Qwen2.5-VL. Emit an accurate, actionable message rather than
        // the misleading "runtime not found" when a stale runtime is present.
        if vulkan_dir.join("llama-server.exe").exists()
            || vulkan_dir.join("llama-cli.exe").exists()
            || cuda_dir.join("llama-server.exe").exists()
        {
            bail!(
                "The installed llama.cpp runtime is too old for image analysis \
                 (missing llama-mtmd-cli.exe, and pre-Qwen2.5-VL). Update it from \
                 Settings -> Performance -> 'Install llama.cpp runtime'."
            )
        }
        bail!(
            "llama.cpp runtime not found under {}. Install it from Settings -> Performance -> 'Install llama.cpp runtime'.",
            vulkan_dir.display()
        )
    }
}

/// Resolve the (gguf, mmproj) pair for a given model_kind. Returns None
/// if either file is missing — the caller surfaces "model not installed"
/// without crashing.
pub fn find_weights(model_kind: &str) -> Option<(PathBuf, PathBuf)> {
    let root = crate::paths::models_dir().ok()?;
    let dir = root.join("vlm").join(model_kind);
    let gguf = dir.join("model.gguf");
    let mmproj = dir.join("mmproj.gguf");
    if gguf.exists() && mmproj.exists() {
        Some((gguf, mmproj))
    } else {
        None
    }
}

fn sanity_check_binary(p: &PathBuf) -> Result<()> {
    let meta = std::fs::metadata(p).with_context(|| format!("stat {}", p.display()))?;
    let len = meta.len();
    // Floor is 20 KB, not 3 MB: modern llama.cpp ships a thin launcher
    // (llama-mtmd-cli.exe ~89 KB, the heavy code lives in mtmd.dll / ggml DLLs),
    // so a 3 MB floor rejected a perfectly valid binary and surfaced a bogus
    // "runtime too old, re-install" toast. 20 KB still catches a truncated or
    // empty download; the --version probe below catches missing DLLs.
    if !(20_000..=200_000_000).contains(&len) {
        bail!(
            "{}: unexpected size {} bytes (expected 20 KB..200 MB)",
            p.display(),
            len
        );
    }
    let mut buf = [0u8; 2];
    use std::io::Read;
    let mut f = std::fs::File::open(p).with_context(|| format!("open {}", p.display()))?;
    f.read_exact(&mut buf).context("reading PE header")?;
    if buf != [b'M', b'Z'] {
        bail!("{}: not a PE binary (missing MZ header)", p.display());
    }
    // PE-header + size pass even if dependent DLLs are missing.
    // Spawning --version trips on dyld errors so we can emit an
    // actionable "reinstall the runtime" message instead of letting
    // caption() fail later with STATUS_DLL_NOT_FOUND.
    let out = std::process::Command::new(p)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    match out {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => bail!(
            "{}: --version exited with status {:?}. The binary is present but \
             likely missing dependent DLLs (a Performance Pack install probably \
             didn't finish). Re-install from Settings -> Performance.",
            p.display(),
            o.status.code()
        ),
        Err(err) => bail!(
            "{}: could not spawn for --version probe ({err}). Likely \
             missing dependent DLLs — re-install the runtime from \
             Settings -> Performance.",
            p.display()
        ),
    }
}

/// Caption an image. Streams stdout line-by-line, calling `on_token`
/// per chunk and accumulating the final text. Honors cancellation via
/// the shared `AtomicBool` — flips kill the child process within
/// 50-100 ms.
pub async fn caption(
    runner: &VlmRunner,
    req: &CaptionRequest,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    mut on_token: impl FnMut(&str),
) -> Result<CaptionResult> {
    #[cfg(feature = "vlm-native")]
    {
        return native::caption(runner, req, cancel, on_token).await;
    }

    #[cfg(not(feature = "vlm-native"))]
    {
        let mut cmd = Command::new(&runner.binary);
        cmd.arg("-m").arg(&req.gguf_path);
        cmd.arg("--mmproj").arg(&req.mmproj_path);
        cmd.arg("--image").arg(&req.image_path);
        cmd.arg("-p").arg(&req.prompt);
        cmd.arg("--n-predict").arg(req.max_tokens.to_string());
        if req.greedy {
            cmd.arg("--temp").arg("0");
        }
        // Don't print the prompt back at us; we only want the completion.
        cmd.arg("--no-display-prompt");
        // P2: offload all layers to the GPU, mirroring the persistent server
        // (vlm_server.rs). Modern llama.cpp defaults to 0 GPU layers, so without
        // this the per-file CLI path ran the entire VLM decode + vision
        // projector on the CPU — many-fold slower. Quality-neutral (same
        // weights/prompt/sampling); a no-op on a CPU-only llama.cpp build, and
        // on a small-VRAM card llama.cpp spills the overflow layers back to CPU.
        cmd.arg("-ngl").arg("99");
        // Pin to the discrete GPU on hybrid iGPU+dGPU systems (no-op otherwise).
        if let Some(dev) = discrete_gpu_device(&runner.binary).await {
            cmd.arg("--device").arg(dev);
        }
        cmd.stdout(std::process::Stdio::piped());
        // Discard stderr rather than pipe-and-not-read it. llama-mtmd-cli writes
        // a large volume of diagnostics to stderr (model/mmproj load, ggml +
        // GPU-backend init, per-token timing). If we `piped()` it but only drain
        // stdout, the child blocks on a full stderr pipe (~64 KB) while we block
        // on stdout it can no longer produce — a classic undrained-pipe deadlock
        // that hangs the per-file Deep Analyze caption with no outer timeout.
        // The completion text comes on stdout; stderr is diagnostics only.
        // Mirrors vlm_server.rs / probe_discrete_gpu_device, which also null it.
        cmd.stderr(std::process::Stdio::null());
        cmd.stdin(std::process::Stdio::null());
        // Kill the child if the parent task is dropped mid-caption so we
        // don't orphan llama-mtmd-cli for the OS session.
        cmd.kill_on_drop(true);

        let mut child = cmd.spawn().with_context(|| format!("spawn {}", runner.binary.display()))?;
        // Drain stderr concurrently. llama.cpp is extremely verbose on stderr
        // (backend init, full model/mmproj metadata, sampling params, timings —
        // tens of KB, emitted during load BEFORE any stdout token). With the
        // pipe captured but never read, the child blocks on its next stderr
        // write once the OS pipe buffer fills while we block reading stdout —
        // a deadlock that hangs Deep Analyze. Read it to EOF on its own task.
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    tracing::debug!(target: "vlm", "{l}");
                }
            });
        }
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("missing stdout pipe"))?;
        let mut reader = BufReader::new(stdout).lines();

        let mut text = String::new();
        loop {
            tokio::select! {
                line = reader.next_line() => {
                    match line {
                        Ok(Some(l)) => {
                            on_token(&l);
                            if !text.is_empty() {
                                text.push('\n');
                            }
                            text.push_str(&l);
                        }
                        Ok(None) => break,
                        Err(err) => {
                            tracing::warn!(?err, "VLM stdout read error");
                            break;
                        }
                    }
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                        let _ = child.kill().await;
                        bail!("cancelled");
                    }
                }
            }
        }

        let status = child.wait().await.context("waiting on VLM child")?;
        if !status.success() {
            bail!("VLM exited with status {:?}", status);
        }
        Ok(CaptionResult { text: text.trim().to_string() })
    }
}

#[cfg(feature = "vlm-native")]
mod native {
    // Native (in-process) llama.cpp path. Off by default; the user opts
    // in via `cargo build --features vlm-native --release`. Adds ~150 MB
    // build artifacts + requires cmake.
    //
    // For now this is a placeholder that delegates to the subprocess
    // path so the feature builds cleanly. A real implementation lands
    // in a follow-up; the contract is the same.
    use super::*;
    pub async fn caption(
        runner: &VlmRunner,
        req: &CaptionRequest,
        cancel: Arc<std::sync::atomic::AtomicBool>,
        on_token: impl FnMut(&str),
    ) -> Result<CaptionResult> {
        // Falls through to the subprocess code path so the feature is
        // additive rather than an alternate code path. Replace with a
        // real llama-cpp-2 invocation once the native build wires up.
        let _ = (runner, req, cancel, on_token);
        bail!("vlm-native is a build-time placeholder; rebuild without the feature")
    }
}

// ── Discrete-GPU selection for the llama.cpp runner ────────────────────
//
// On hybrid iGPU+dGPU systems the Vulkan backend can default to the integrated
// GPU. We probe the runner once with `--list-devices`, and when a clearly
// dominant (≥2 GiB more VRAM) discrete device exists we pass `--device VulkanN`
// so Deep Analyze runs on the dGPU. Mirrors the DirectML `device_id` pinning
// the ORT scan path does (see runtime.rs::execution_providers_for_chain).
//
// Safety: every failure path returns None → the caller passes no flag and
// llama.cpp keeps its default. CUDA builds list "CUDAn" (not "Vulkann") device
// lines, so this is a no-op there — and on a single-GPU box there's nothing to
// pin. Because the probe uses the SAME binary that runs inference, a parseable
// device list also proves the build supports `--device`.

/// Best-effort discrete-GPU device name (e.g. `"Vulkan0"`) for `--device`,
/// cached per binary. Returns None when it can't confidently pick one.
pub(crate) async fn discrete_gpu_device(binary: &std::path::Path) -> Option<String> {
    use std::collections::HashMap;
    use std::sync::OnceLock;
    use tokio::sync::Mutex;
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Option<String>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = cache.lock().await.get(binary) {
        return hit.clone();
    }
    let resolved = probe_discrete_gpu_device(binary).await;
    cache
        .lock()
        .await
        .insert(binary.to_path_buf(), resolved.clone());
    if let Some(dev) = &resolved {
        tracing::info!(%dev, binary = %binary.display(), "[VLM] pinning llama.cpp to discrete GPU");
    }
    resolved
}

async fn probe_discrete_gpu_device(binary: &std::path::Path) -> Option<String> {
    let mut cmd = Command::new(binary);
    cmd.arg("--list-devices");
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.stdin(std::process::Stdio::null());
    cmd.kill_on_drop(true);
    let child = cmd.spawn().ok()?;
    let out = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait_with_output())
        .await
        .ok()? // probe timed out
        .ok()?; // spawn / wait failed
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    parse_best_vulkan_device(&text)
}

/// Parse `--list-devices` output for `"VulkanN: <name> (<vram> MiB, …)"` lines
/// and return the device with the most VRAM — but only when it beats the
/// runner-up by ≥2 GiB (a clear discrete-vs-integrated split). Returns None for
/// <2 Vulkan devices or an ambiguous spread so we never pin the wrong adapter.
fn parse_best_vulkan_device(text: &str) -> Option<String> {
    let mut devices: Vec<(String, u64)> = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        let Some(colon) = l.find(':') else { continue };
        let name = &l[..colon];
        let is_vulkan = name.len() > 6
            && name.starts_with("Vulkan")
            && name[6..].chars().all(|c| c.is_ascii_digit());
        if !is_vulkan {
            continue;
        }
        // First "<digits> MiB" token-pair on the line is the device VRAM.
        let toks: Vec<&str> = l.split_whitespace().collect();
        let vram = toks.windows(2).find_map(|w| {
            if w[1].starts_with("MiB") {
                w[0].trim_matches(|c: char| !c.is_ascii_digit())
                    .parse::<u64>()
                    .ok()
            } else {
                None
            }
        });
        if let Some(v) = vram {
            devices.push((name.to_string(), v));
        }
    }
    if devices.len() < 2 {
        return None;
    }
    devices.sort_by_key(|d| std::cmp::Reverse(d.1));
    let (best_name, best_vram) = &devices[0];
    let (_, second_vram) = &devices[1];
    if best_vram.saturating_sub(*second_vram) >= 2048 {
        Some(best_name.clone())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::parse_best_vulkan_device;

    #[test]
    fn picks_discrete_over_integrated() {
        let out = "Available devices:\n  \
            Vulkan0: NVIDIA GeForce RTX 4070 (8188 MiB, 7900 MiB free)\n  \
            Vulkan1: Intel(R) Iris(R) Xe Graphics (128 MiB, 64 MiB free)\n";
        assert_eq!(parse_best_vulkan_device(out).as_deref(), Some("Vulkan0"));
    }

    #[test]
    fn none_when_single_device() {
        let out = "Available devices:\n  Vulkan0: NVIDIA GeForce RTX 4070 (8188 MiB, 7900 MiB free)\n";
        assert_eq!(parse_best_vulkan_device(out), None);
    }

    #[test]
    fn none_when_vram_close() {
        // Two similar dGPUs — no clear discrete/integrated split; don't guess.
        let out = "  Vulkan0: GPU A (8188 MiB, x)\n  Vulkan1: GPU B (8000 MiB, y)\n";
        assert_eq!(parse_best_vulkan_device(out), None);
    }

    #[test]
    fn none_for_cuda_device_lines() {
        // CUDA build lists "CUDAn" not "Vulkann" → nothing to pin (default
        // device 0 is the dGPU on hybrid systems).
        let out = "Available devices:\n  CUDA0: NVIDIA GeForce RTX 4070 (8188 MiB, free)\n";
        assert_eq!(parse_best_vulkan_device(out), None);
    }
}
