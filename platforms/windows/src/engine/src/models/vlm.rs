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
// sanity-checked (host-compatible PE/ELF header + size bounds) before spawning.

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
    fn binaries() -> Result<Vec<PathBuf>> { super::native_runtime::candidates("llama-mtmd-cli", "FILEID_LLAMA_CLI") }
    pub fn find() -> Result<Self> {
        for binary in Self::binaries()? {
            if super::native_runtime::probe_version(&binary).is_ok() {
                return Ok(Self { binary });
            }
        }
        bail!("Compatible installed llama-mtmd-cli runtime not found; configure FILEID_LLAMA_CLI or install the runtime in Settings")
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
    mut on_token: impl FnMut(&str),
) -> Result<CaptionResult> {
    #[cfg(feature = "vlm-native")]
    {
        return native::caption(runner, req, cancel, on_token).await;
    }

    #[cfg(not(feature = "vlm-native"))]
    {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) { bail!("VLM startup cancelled"); }
        super::runtime::ensure_gpu_inference_alive()?;
        let bytes = tokio::fs::metadata(&req.gguf_path).await?.len().saturating_add(tokio::fs::metadata(&req.mmproj_path).await?.len());
        let mb = bytes.div_ceil(1_048_576);
        let memory_mb = mb.saturating_add(mb.div_ceil(4)).saturating_add(1408);
        let admission_cancel = cancel.clone();
        let forced_cpu = super::runtime::user_forced_cpu() || super::runtime::verified_gpu_capacity_mb()
            .is_none_or(|capacity| memory_mb.saturating_add(crate::resource_scheduler::ResourceScheduler::shared().snapshot().gpu_mb) > capacity);
        let lease = tokio::task::spawn_blocking(move || crate::resource_scheduler::ResourceScheduler::shared().reserve_with_accelerators(
            crate::resource_scheduler::Demand { memory_mb, cpu: 1, io: 1 },
            crate::resource_scheduler::AcceleratorDemand { gpu_mb: if forced_cpu { 0 } else { memory_mb }, npu_mb: 0 },
            crate::resource_scheduler::Priority::Interactive, || admission_cancel.load(std::sync::atomic::Ordering::Relaxed))).await??;
        crate::util::model_memory::require_model_headroom(&req.gguf_path,&req.mmproj_path).await?;
        let mut cmd = Command::new(&runner.binary);
        cmd.arg("--offline").arg("-m").arg(&req.gguf_path);
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
        // 0 offloaded layers when the user pinned the EP to CPU (GPU-TDR
        // recovery), evacuating the GPU for the per-file CLI path too. (F-C1-006)
        cmd.arg("-ngl").arg(if forced_cpu { "0" } else { "99" });
        if forced_cpu { cmd.arg("--no-mmproj-offload"); }
        // Pin to the discrete GPU on hybrid iGPU+dGPU systems (no-op otherwise);
        // skipped under forced-CPU so we don't re-engage the evacuated GPU.
        if !forced_cpu {
            if let Some(dev) = discrete_gpu_device(&runner.binary).await {
                cmd.arg("--device").arg(dev);
            }
        }
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.stdin(std::process::Stdio::null());
        // Kill the child if the parent task is dropped mid-caption so we
        // don't orphan llama-mtmd-cli for the OS session.
        super::native_process::configure(&mut cmd);

        let mut child = cmd.spawn().with_context(|| format!("spawn {}", crate::platform::redact_path_for_log(&runner.binary)))?;
        let stderr = child.stderr.take();
        let stdout = child.stdout.take();
        let tracker = super::native_process::ProcessTracker::default();
        let process = super::native_process::OwnedProcess::new(child, Some(Arc::new(lease)), &tracker);
        process.poll_exit()?;
        // Drain stderr concurrently. llama.cpp is extremely verbose on stderr
        // (backend init, full model/mmproj metadata, sampling params, timings —
        // tens of KB, emitted during load BEFORE any stdout token). With the
        // pipe captured but never read, the child blocks on its next stderr
        // write once the OS pipe buffer fills while we block reading stdout —
        // a deadlock that hangs Deep Analyze. Read it to EOF on its own task.
        if let Some(stderr) = stderr {
            // llama.cpp echoes the --image arg (a user path) into its
            // diagnostics; the model/binary paths are app-structural and
            // pass redaction unchanged.
            let image_raw = req.image_path.to_string_lossy().into_owned();
            let image_redacted = crate::platform::redact_path_for_log(&req.image_path);
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    tracing::debug!(target: "vlm", "{}", l.replace(&image_raw, &image_redacted));
                }
            });
        }
        let stdout = stdout.ok_or_else(|| anyhow!("missing stdout pipe"))?;
        let stdout = tokio::io::AsyncReadExt::take(stdout, 1024 * 1024 + 1);
        let mut reader = BufReader::new(stdout).lines();

        let mut text = String::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
        loop {
            tokio::select! {
                line = reader.next_line() => {
                    match line {
                        Ok(Some(l)) => {
                            if text.len().saturating_add(l.len()) > 1024 * 1024 {
                                process.shutdown().await?;
                                bail!("VLM CLI output exceeded cap");
                            }
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
                    if cancel.load(std::sync::atomic::Ordering::Relaxed) || std::time::Instant::now() >= deadline || crate::coordinator::process_gpu_device_removed() {
                        process.shutdown().await?;
                        bail!("cancelled");
                    }
                }
            }
        }

        let status = match process.wait_exit().await {
            Ok(status) => status,
            Err(error) => { process.shutdown().await?; return Err(error); }
        };
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
        tracing::info!(%dev, binary = %crate::platform::redact_path_for_log(binary), "[VLM] pinning llama.cpp to discrete GPU");
    }
    resolved
}

async fn probe_discrete_gpu_device(binary: &std::path::Path) -> Option<String> {
    let mut cmd = Command::new(binary);
    cmd.arg("--list-devices");
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.stdin(std::process::Stdio::null());
    super::native_process::configure(&mut cmd);
    let child = cmd.spawn().ok()?;
    #[cfg(windows)]
    let _ownership = super::native_process::ParentOwnership::new(&child).ok()?;
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

#[cfg(test)]
mod configured_runtime_tests {
    use super::*;
    #[test]
    #[ignore = "internal configured discovery fixture"]
    fn configured_cli_fixture() {
        let expected = std::path::PathBuf::from(std::env::var_os("FILEID_LLAMA_CLI").unwrap());
        if std::env::var("FILEID_TEST_CLI_CASE").unwrap() == "valid" {
            assert_eq!(VlmRunner::binaries().unwrap(), vec![expected]);
        } else { assert!(VlmRunner::binaries().is_err()); }
    }
    #[test]
    fn canonical_configured_cli_validates_absolute_host_paths() {
        let binary = std::env::current_exe().unwrap();
        let foreign = std::env::temp_dir().join(format!("fileid-foreign-cli-{}", uuid::Uuid::new_v4()));
        std::fs::write(&foreign, vec![0; 20000]).unwrap();
        for (case, path) in [("valid", binary.clone()), ("relative", "relative-cli".into()), ("foreign", foreign.clone())] {
            let status = std::process::Command::new(&binary)
                .args(["--exact", "models::vlm::configured_runtime_tests::configured_cli_fixture", "--ignored", "--nocapture"])
                .env("FILEID_LLAMA_CLI", path).env("FILEID_LLAMA_MTMD", "unapproved-name-must-not-be-read")
                .env("FILEID_TEST_CLI_CASE", case).status().unwrap();
            assert!(status.success(), "{case}");
        }
        std::fs::remove_file(foreign).unwrap();
    }
    #[test]
    #[ignore = "requires explicitly configured approved installed native CLI"]
    fn configured_installed_cli_is_discovered_and_version_probed() {
        let expected = std::path::PathBuf::from(std::env::var_os("FILEID_LLAMA_CLI").unwrap());
        assert_eq!(VlmRunner::find().unwrap().binary, expected);
    }
}
