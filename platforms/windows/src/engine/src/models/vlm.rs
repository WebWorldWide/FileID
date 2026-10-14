// Subprocess wrapper around llama.cpp's `llama-mtmd-cli.exe` for VLM
// inference (Deep Analyze).
//
// A subprocess keeps the engine free of a linked llama.cpp build (cmake,
// ~150 MB of artifacts) and lets it ship the upstream release binaries.
//
// Hardening: explicit canonical runtime or app-owned bundles, never PATH.
// Binary is host/header/size-checked before an admitted owned version probe.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, bail, Result};
use tokio::process::Command;

#[path = "vlm_cli_output.rs"]
mod cli_output;

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

/// Caption prompt with the named people in the image injected so the VLM refers
/// to them by name. Plain prompt when no one is named. Mirrors the macOS
/// `nameContext` injection. (item 3)
pub fn caption_prompt_with_faces(face_names: &[String]) -> String {
    if face_names.is_empty() {
        CAPTION_PROMPT.to_string()
    } else {
        format!(
            "{} The known people in this image are: {}. Refer to them by these names.",
            CAPTION_PROMPT,
            face_names.join(", ")
        )
    }
}

/// Rename prompt with the named people injected. A deterministic prefix is also
/// applied after generation (`apply_person_prefix`), so the names land even when
/// the model ignores this hint. (item 3)
pub fn rename_prompt_with_faces(face_names: &[String]) -> String {
    if face_names.is_empty() {
        RENAME_PROMPT.to_string()
    } else {
        format!(
            "{} If any of these people are present, include their names: {}.",
            RENAME_PROMPT,
            face_names.join(", ")
        )
    }
}

#[derive(Debug)]
pub struct VlmRunner {
    pub binary: PathBuf,
    weights: super::model_job::ModelJob,
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
    pub fn new(binary: PathBuf) -> Self { Self { binary, weights: super::model_job::ModelJob::default() } }
    pub(crate) async fn verified_weights(&self, kind: &str, cancel: Arc<std::sync::atomic::AtomicBool>) -> Result<Arc<super::model_provenance::VerifiedModel>> {
        self.weights.resolve(kind, cancel).await
    }
    pub async fn find(cancel: &Arc<std::sync::atomic::AtomicBool>) -> Result<Self> {
        let binaries = super::native_runtime::candidates("llama-mtmd-cli", "FILEID_LLAMA_CLI", cancel.clone()).await?;
        let mut last_error = None;
        for binary in binaries {
            let mut command = Command::new(&binary); command.arg("--version");
            let output = super::native_probe::run_probe(command, crate::resource_scheduler::ResourceScheduler::shared(),
                cancel.clone(), super::native_process::ProcessTracker::shared(), std::time::Duration::from_secs(5)).await?;
            if output.status.success() { return Ok(Self::new(binary)); }
            last_error = Some(anyhow!("Installed llama.cpp CLI --version failed: {}", output.status));
        }
        Err(last_error.unwrap_or_else(|| anyhow!("llama-mtmd-cli runtime not installed; configure FILEID_LLAMA_CLI or install the runtime from Settings")))
    }
}

/// Resolve the (gguf, mmproj) pair for a given model_kind. Returns None
/// if either file is missing — the caller surfaces "model not installed"
/// without crashing.
pub fn find_weights(model_kind: &str) -> Option<(PathBuf, PathBuf)> {
    let root = crate::paths::models_dir().ok()?;
    find_weights_at(&root, model_kind)
}

fn find_weights_at(root: &std::path::Path, model_kind: &str) -> Option<(PathBuf, PathBuf)> {
    let dir = root.join("vlm").join(super::registry::vlm_dir_name(model_kind)?);
    let gguf = dir.join("model.gguf");
    let mmproj = dir.join("mmproj.gguf");
    if gguf.is_file() && mmproj.is_file() {
        Some((gguf, mmproj))
    } else {
        None
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
    on_token: impl FnMut(&str),
) -> Result<CaptionResult> {
    if cancel.load(std::sync::atomic::Ordering::Relaxed) { bail!("VLM startup cancelled"); }
    if req.prompt.len() > 32 * 1024 || !(1..=4096).contains(&req.max_tokens) { bail!("VLM generation exceeds prompt/token bounds"); }
    let model_mb = crate::util::model_memory::require_model_headroom(&req.gguf_path,&req.mmproj_path)
        .await.map_err(super::native_admission::HostAdmissionError)?;
    let verified = runner.weights.require_pair(&req.gguf_path, &req.mmproj_path, cancel.clone()).await?;
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
    // Offload all layers to the GPU, mirroring the persistent server
    // (vlm_server.rs). Modern llama.cpp defaults to 0 GPU layers, so without
    // this the per-file CLI path ran the entire VLM decode + vision
    // projector on the CPU — many-fold slower. Quality-neutral (same
    // weights/prompt/sampling); a no-op on a CPU-only llama.cpp build, and
    // on a small-VRAM card llama.cpp spills the overflow layers back to CPU.
    // 0 offloaded layers when the user pinned the EP to CPU (GPU-TDR
    // recovery), evacuating the GPU for the per-file CLI path too.
    let forced_cpu = crate::models::runtime::user_forced_cpu();
    cmd.arg("-ngl").arg(if forced_cpu { "0" } else { "99" });
    // Pin to the discrete GPU on hybrid iGPU+dGPU systems (no-op otherwise);
    // skipped under forced-CPU so we don't re-engage the evacuated GPU.
    if !forced_cpu {
        if let Some(dev) = discrete_gpu_device(&runner.binary, &cancel).await? {
            cmd.arg("--device").arg(dev);
        }
    }
    verified.recheck(cancel.clone()).await?;
    let memory_mb = model_mb.checked_add(512)
        .ok_or_else(|| super::native_admission::HostAdmissionError(anyhow!("VLM CLI host estimate overflow")))?;
    let resources = super::native_admission::reserve_host(crate::resource_scheduler::ResourceScheduler::shared(),
        crate::resource_scheduler::Demand { memory_mb, cpu: 1, io: 1 }, cancel.clone()).await?;
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.stdin(std::process::Stdio::null());
    let text = cli_output::run_caption_command(cmd, resources, cancel, &req.image_path,
        on_token, super::native_process::ProcessTracker::shared()).await?;
    Ok(CaptionResult { text })
}

// ── Discrete-GPU selection for the llama.cpp runner ────────────────────
//
// On hybrid iGPU+dGPU systems the Vulkan backend can default to the integrated
// GPU. We probe the runner once with `--list-devices`, and when a clearly
// dominant (≥2 GiB more VRAM) discrete device exists we pass `--device VulkanN`
// so Deep Analyze runs on the dGPU. Mirrors the DirectML `device_id` pinning
// the ORT scan path does (see runtime.rs::execution_providers_for_chain).
//
// Unsupported clean probes select no flag; admission, cap, timeout and cleanup
// failures terminate startup. CUDA builds list "CUDAn" (not "Vulkann") device
// lines, so this is a no-op there — and on a single-GPU box there's nothing to
// pin. Because the probe uses the SAME binary that runs inference, a parseable
// device list also proves the build supports `--device`.

/// Best-effort discrete-GPU device name (e.g. `"Vulkan0"`) for `--device`,
/// cached per binary. Returns None when it can't confidently pick one.
pub(crate) async fn discrete_gpu_device(binary: &std::path::Path, cancel: &Arc<std::sync::atomic::AtomicBool>) -> Result<Option<String>> {
    use std::collections::HashMap;
    use std::sync::OnceLock;
    use tokio::sync::Mutex;
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Option<String>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if cancel.load(std::sync::atomic::Ordering::Relaxed) { return Err(super::native_admission::HostAdmissionError(anyhow!("Native device probe cancelled")).into()); }
    if let Some(hit) = cache.lock().await.get(binary) { return Ok(hit.clone()); }
    let resolved = probe_discrete_gpu_device(binary, cancel).await?;
    cache.lock().await.insert(binary.to_path_buf(), resolved.clone());
    if let Some(device) = &resolved {
        tracing::info!(%device, binary = %crate::platform::redact_path_for_log(binary), "[VLM] pinning llama.cpp to discrete GPU");
    }
    Ok(resolved)
}

async fn probe_discrete_gpu_device(binary: &std::path::Path, cancel: &Arc<std::sync::atomic::AtomicBool>) -> Result<Option<String>> {
    let mut command = Command::new(binary); command.arg("--list-devices");
    let output = super::native_probe::run_probe(command, crate::resource_scheduler::ResourceScheduler::shared(),
        cancel.clone(), super::native_process::ProcessTracker::shared(), std::time::Duration::from_secs(5)).await?;
    if !output.status.success() { return Ok(None); }
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(parse_best_vulkan_device(&text))
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
    use super::{find_weights_at, parse_best_vulkan_device};
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn installed_weights_resolve_from_wire_model_kind() {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "fileid-vlm-weights-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        for (kind, dir) in [
            ("qwen2_5_vl_7b", "qwen2.5-vl-7b"),
            ("gemma_3_4b", "gemma-3-4b"),
            ("mistral_small_3_2", "mistral-small-3.2"),
            ("qwen3_vl_4b", "qwen3-vl-4b"),
            ("qwen3_vl_8b", "qwen3-vl-8b"),
        ] {
            let installed = root.join("vlm").join(dir);
            std::fs::create_dir_all(&installed).unwrap();
            let gguf = installed.join("model.gguf");
            let mmproj = installed.join("mmproj.gguf");
            std::fs::write(&gguf, b"gguf").unwrap();
            assert_eq!(find_weights_at(&root, kind), None, "incomplete {kind}");
            std::fs::write(&mmproj, b"mmproj").unwrap();
            assert_eq!(find_weights_at(&root, kind), Some((gguf, mmproj)));
        }
        assert_eq!(find_weights_at(&root, "unknown"), None);
        std::fs::remove_dir_all(root).unwrap();
    }

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
