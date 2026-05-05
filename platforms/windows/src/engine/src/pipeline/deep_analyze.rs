// Deep Analyze — VLM-powered captioning + smart-rename.
//
// Mirror of macOS engine/Sources/FileIDEngine/Pipeline/DeepAnalyze.swift.
// Pipeline:
//   1. Pick a model (Qwen2.5-VL 3B / 7B / Gemma 3 4B / SmolVLM).
//   2. Load via llama.cpp (Vulkan / CUDA / DirectML / CPU backend by EP).
//   3. Per file: render the image / extract a video keyframe / pdfium
//      first-page render → resize to model context → caption + smart name.
//   4. Persist to `deep_analyze_results` (migration v3).
//   5. Emit `deepAnalyzeProgress` IPC events on every N files.
//
// Phase 6 cut: API contract + the model registry (model id → file path
// + SHA256 + size). The actual llama.cpp binding lights up alongside
// the 12-way downloader once it's verified end-to-end.

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VlmModelKind {
    QwenVl3B,
    QwenVl7B,
    Gemma3_4B,
    SmolVlm,
}

impl VlmModelKind {
    pub fn id(self) -> &'static str {
        match self {
            VlmModelKind::QwenVl3B => "qwen2.5-vl-3b",
            VlmModelKind::QwenVl7B => "qwen2.5-vl-7b",
            VlmModelKind::Gemma3_4B => "gemma-3-4b",
            VlmModelKind::SmolVlm => "smolvlm",
        }
    }

    pub fn human_name(self) -> &'static str {
        match self {
            VlmModelKind::QwenVl3B => "Qwen2.5-VL 3B",
            VlmModelKind::QwenVl7B => "Qwen2.5-VL 7B (recommended)",
            VlmModelKind::Gemma3_4B => "Gemma 3 4B",
            VlmModelKind::SmolVlm => "SmolVLM 256M",
        }
    }

    /// Approximate disk size, in MB, for the Q4_K_M quant + mmproj.
    /// Drives the install-disk-budget warning in the model picker UI.
    pub fn approx_size_mb(self) -> u32 {
        match self {
            VlmModelKind::QwenVl3B => 1900,
            VlmModelKind::QwenVl7B => 4500,
            VlmModelKind::Gemma3_4B => 2500,
            VlmModelKind::SmolVlm => 700,
        }
    }

    /// Approximate runtime VRAM/RAM ceiling in MB at Q4_K_M.
    pub fn approx_ram_mb(self) -> u32 {
        match self {
            VlmModelKind::QwenVl3B => 3500,
            VlmModelKind::QwenVl7B => 7500,
            VlmModelKind::Gemma3_4B => 4500,
            VlmModelKind::SmolVlm => 900,
        }
    }
}

#[derive(Debug, Clone)]
pub struct VlmModelFiles {
    pub kind: VlmModelKind,
    /// Main GGUF weights file.
    pub gguf_path: PathBuf,
    /// Vision projection file (vision adapter for the multi-modal head).
    pub mmproj_path: PathBuf,
}

impl VlmModelFiles {
    pub fn default_paths(kind: VlmModelKind) -> anyhow::Result<Self> {
        let dir = crate::paths::models_dir()?.join("VLM").join(kind.id());
        Ok(Self {
            kind,
            gguf_path: dir.join("model-q4_k_m.gguf"),
            mmproj_path: dir.join("mmproj.gguf"),
        })
    }

    pub fn ready(&self) -> bool {
        self.gguf_path.exists() && self.mmproj_path.exists()
    }
}

/// Per-file Deep Analyze outcome — whatever the engine writes back to
/// the DB after a successful caption + smart-rename round-trip.
#[derive(Debug, Clone, Default)]
pub struct AnalyzeOutcome {
    pub file_id: i64,
    pub description: Option<String>,
    pub proposed_name: Option<String>,
    pub model: String,
    pub elapsed_ms: u64,
}

/// What we want from this file: caption, smart filename, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalyzeMode {
    CaptionOnly,
    RenameOnly,
    Both,
}

/// Run Deep Analyze on a single file: pull image bytes (image, video
/// keyframe, or PDF page-1 via shell helpers) → call the VLM via the
/// subprocess wrapper → write results back to the DB. Cancellation
/// honored via the shared `AtomicBool`.
pub async fn analyze_file(
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    runner: &crate::models::vlm::VlmRunner,
    file_id: i64,
    model_kind: &str,
    mode: AnalyzeMode,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    mut on_token: impl FnMut(&str),
) -> anyhow::Result<AnalyzeOutcome> {
    use crate::models::vlm::{self, CaptionRequest};
    use std::path::PathBuf;

    let started = std::time::Instant::now();

    // Resolve file path + kind from DB.
    let (path_text, kind): (String, String) = {
        let conn = db.lock();
        conn.query_row(
            "SELECT path_text, kind FROM files WHERE id = ?1",
            rusqlite::params![file_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )?
    };
    let source_path = PathBuf::from(&path_text);
    if !source_path.exists() {
        anyhow::bail!("source file missing: {}", source_path.display());
    }

    // Resolve weights for this model_kind.
    let (gguf, mmproj) = vlm::find_weights(model_kind)
        .ok_or_else(|| anyhow::anyhow!("VLM weights for '{}' not installed", model_kind))?;

    // Rasterize the source into something the CLI accepts (JPEG path).
    // Images: pass directly. Video / PDF: extract a keyframe / page-1
    // image into a temp file. Audio + Other: skip with friendly error.
    let rasterized: PathBuf = match kind.as_str() {
        "image" => source_path.clone(),
        "video" => rasterize_video_keyframe(&source_path).await?,
        _ => anyhow::bail!("kind '{}' isn't VLM-analyzable yet", kind),
    };
    // Track if we wrote a temp file so we can clean it up at the end.
    let temp_to_clean: Option<PathBuf> = if rasterized != source_path {
        Some(rasterized.clone())
    } else {
        None
    };

    let mut description: Option<String> = None;
    let mut proposed_name: Option<String> = None;

    if matches!(mode, AnalyzeMode::CaptionOnly | AnalyzeMode::Both) {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }
        let req = CaptionRequest {
            gguf_path: gguf.clone(),
            mmproj_path: mmproj.clone(),
            image_path: rasterized.clone(),
            prompt: vlm::CAPTION_PROMPT.to_string(),
            max_tokens: 80,
            greedy: true,
        };
        let result = vlm::caption(runner, &req, cancel.clone(), &mut on_token).await?;
        description = Some(result.text);
    }
    if matches!(mode, AnalyzeMode::RenameOnly | AnalyzeMode::Both) {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }
        let req = CaptionRequest {
            gguf_path: gguf,
            mmproj_path: mmproj,
            image_path: rasterized,
            prompt: vlm::RENAME_PROMPT.to_string(),
            max_tokens: 30,
            greedy: true,
        };
        let result = vlm::caption(runner, &req, cancel.clone(), |_| {}).await?;
        proposed_name = Some(sanitize_proposed_name(&result.text));
    }

    // Persist to v3 schema columns.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    {
        let conn = db.lock();
        conn.execute(
            "UPDATE files SET vlm_description=COALESCE(?1, vlm_description), \
                              vlm_proposed_name=COALESCE(?2, vlm_proposed_name), \
                              vlm_model=?3, vlm_analyzed_at=?4 WHERE id=?5",
            rusqlite::params![description, proposed_name, model_kind, now, file_id],
        )?;
    }

    // Best-effort cleanup of any temp rasterized frame.
    if let Some(temp) = temp_to_clean {
        let _ = std::fs::remove_file(&temp);
    }

    Ok(AnalyzeOutcome {
        file_id,
        description,
        proposed_name,
        model: model_kind.to_string(),
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

/// Pull a 25%-duration keyframe from a video into a temp JPEG via the
/// existing Media Foundation helper, return the temp path. Caller is
/// responsible for cleanup (we leak the temp; OS cleans the temp dir on
/// reboot — fine for one-off analysis).
async fn rasterize_video_keyframe(path: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    let p = path.to_path_buf();
    let frame = tokio::task::spawn_blocking(move || crate::shell::video::keyframe_25pct(&p))
        .await??;
    let dest = std::env::temp_dir().join(format!(
        "fileid-vlm-{}.jpg",
        uuid::Uuid::new_v4()
    ));
    let img: image::ImageBuffer<image::Rgb<u8>, _> =
        image::ImageBuffer::from_raw(frame.width, frame.height, frame.rgb)
            .ok_or_else(|| anyhow::anyhow!("video frame buffer mismatch"))?;
    image::DynamicImage::ImageRgb8(img).save(&dest)?;
    Ok(dest)
}

/// Clean up a VLM-proposed filename: lowercase, hyphen-separated, strip
/// quotes / extension / extra punctuation. The model usually obeys the
/// prompt but defensive normalization saves a round-trip.
fn sanitize_proposed_name(raw: &str) -> String {
    let trimmed = raw.trim().trim_matches('"').trim_matches('\'').trim();
    let lowered = trimmed.to_lowercase();
    let cleaned: String = lowered
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c
            } else if c == '-' || c == '_' {
                c
            } else if c.is_whitespace() {
                '-'
            } else {
                ' '
            }
        })
        .collect();
    let collapsed = cleaned
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-");
    let mut out = collapsed;
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    if out.len() > 80 {
        out.truncate(80);
        // Don't end mid-word.
        if let Some(idx) = out.rfind('-') {
            out.truncate(idx);
        }
    }
    if out.is_empty() {
        "untitled".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::sanitize_proposed_name;
    use super::*;

    #[test]
    fn sanitize_strips_quotes_and_normalizes() {
        assert_eq!(sanitize_proposed_name("\"Cute Beach Sunset\""), "cute-beach-sunset");
        assert_eq!(sanitize_proposed_name("Bird in Tree!"), "bird-in-tree");
        assert_eq!(sanitize_proposed_name("   leading and trailing   "), "leading-and-trailing");
    }

    #[test]
    fn sanitize_caps_length_at_word_boundary() {
        let s = sanitize_proposed_name(&"word ".repeat(40));
        assert!(s.len() <= 80);
        assert!(!s.ends_with("-"));
    }

    #[test]
    fn sanitize_empty_falls_back() {
        assert_eq!(sanitize_proposed_name(""), "untitled");
        assert_eq!(sanitize_proposed_name("!!!"), "untitled");
    }

    #[test]
    fn model_kinds_have_unique_ids() {
        let kinds = [
            VlmModelKind::QwenVl3B,
            VlmModelKind::QwenVl7B,
            VlmModelKind::Gemma3_4B,
            VlmModelKind::SmolVlm,
        ];
        let mut seen = std::collections::HashSet::new();
        for k in kinds {
            assert!(seen.insert(k.id()), "duplicate id for {:?}", k);
        }
    }

    #[test]
    fn size_estimates_increase_with_capability() {
        assert!(VlmModelKind::SmolVlm.approx_size_mb() < VlmModelKind::QwenVl3B.approx_size_mb());
        assert!(VlmModelKind::QwenVl3B.approx_size_mb() < VlmModelKind::QwenVl7B.approx_size_mb());
    }
}
