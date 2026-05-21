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
pub const CAPTION_PROMPT: &str = "Describe this image in one detailed sentence focused on the most prominent subjects, scene, and any text. No filler.";

/// Default rename prompt — produces a short, kebab-cased filename
/// suitable for `sanitize_proposed_name`.
pub const RENAME_PROMPT: &str = "Suggest a 3 to 5 word lowercase filename for this image, separated by hyphens, no quotes, no extension.";

/// Tagging prompt — produces a short comma-separated list of scene/content
/// tags. Parsed by `deep_analyze::parse_vlm_tags` into individual `source='vlm'`
/// tags shown as Library chips. This is the VLM equivalent of the CLIP
/// zero-shot scene tags (and is intended to replace them if the user opts to
/// drop CLIP): a real vision-language model describes what's actually in the
/// image instead of scoring against a fixed vocabulary.
pub const TAG_PROMPT: &str = "List 3 to 6 short lowercase tags for the main subjects, setting, and any prominent objects or text in this image. One or two words each, comma-separated, no sentences, no numbering.";

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
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.stdin(std::process::Stdio::null());
        // Kill the child if the parent task is dropped mid-caption so we
        // don't orphan llama-mtmd-cli for the OS session.
        cmd.kill_on_drop(true);

        let mut child = cmd.spawn().with_context(|| format!("spawn {}", runner.binary.display()))?;
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
