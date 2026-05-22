// Deep Analyze — VLM-powered captioning + smart-rename.
//
// Pipeline:
//   1. Pick a model (Qwen2.5-VL 3B / 7B / Gemma 3 4B / SmolVLM).
//   2. Load via llama.cpp (Vulkan / CUDA / DirectML / CPU backend by EP).
//   3. Per file: render the image / extract a video keyframe / pdfium
//      first-page render → resize to model context → caption + smart name.
//   4. Persist to `deep_analyze_results` (migration v3).
//   5. Emit `deepAnalyzeProgress` IPC events on every N files.

/// Enumerates the VLM model kinds the Deep Analyze pipeline can run.
/// Kept around (even though the registry is the source of truth for
/// download metadata) so unit tests can sanity-check id uniqueness +
/// size-tier ordering without exercising the full registry surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum VlmModelKind {
    QwenVl3B,
    QwenVl7B,
    Gemma3_4B,
    SmolVlm,
}

#[allow(dead_code)]
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
            VlmModelKind::SmolVlm => "SmolVLM 500M",
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

/// Per-file Deep Analyze outcome — whatever the engine writes back to
/// the DB after a successful caption + smart-rename round-trip.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct AnalyzeOutcome {
    pub file_id: i64,
    pub description: Option<String>,
    pub proposed_name: Option<String>,
    pub model: String,
    pub elapsed_ms: u64,
}

/// What we want from this file: caption, smart filename, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum AnalyzeMode {
    CaptionOnly,
    RenameOnly,
    /// Tags only — the fast path for background auto-tagging. One VLM call per
    /// file (tags) instead of three (caption + tags + rename), so a whole-library
    /// pass is ~3× faster. Caption + proposed-name columns are left untouched.
    TagsOnly,
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

    let started = std::time::Instant::now();

    // Resolve weights for this model_kind.
    let (gguf, mmproj) = vlm::find_weights(model_kind)
        .ok_or_else(|| anyhow::anyhow!("VLM weights for '{}' not installed", model_kind))?;

    // Resolve + rasterize the source (image as-is; video keyframe; PDF page-1).
    let (rasterized, temp_to_clean) = rasterize_for_vlm(&db, file_id).await?;

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
    // VLM scene/content tags (source='vlm'). Generated in "Both" (full
    // enrichment) and "TagsOnly" (the fast background auto-tag pass), so a Deep
    // Analyze run over the library produces the chip tags that REPLACE CLIP
    // zero-shot if the user drops CLIP. Clones the weights + rasterized frame so
    // the rename branch below can still take ownership.
    let mut tags: Vec<String> = Vec::new();
    if matches!(mode, AnalyzeMode::Both | AnalyzeMode::TagsOnly) {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }
        let req = CaptionRequest {
            gguf_path: gguf.clone(),
            mmproj_path: mmproj.clone(),
            image_path: rasterized.clone(),
            prompt: vlm::TAG_PROMPT.to_string(),
            max_tokens: 40,
            greedy: true,
        };
        let result = vlm::caption(runner, &req, cancel.clone(), |_| {}).await?;
        tags = parse_vlm_tags(&result.text);
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

    // Persist caption + proposed name (v3 `files` columns) + VLM tags.
    {
        let conn = db.lock();
        persist_vlm_results(
            &conn,
            file_id,
            model_kind,
            description.as_deref(),
            proposed_name.as_deref(),
            &tags,
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

/// Resolve a file's on-disk path and rasterize it to an image the VLM can read:
/// images pass through; video → 25%-duration keyframe; PDF → page-1 render.
/// Returns the image path + an optional temp path the caller must clean up.
/// Shared by the per-file CLI (`analyze_file`) and the persistent-server path.
pub(crate) async fn rasterize_for_vlm(
    db: &std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    file_id: i64,
) -> anyhow::Result<(std::path::PathBuf, Option<std::path::PathBuf>)> {
    let (path_text, kind): (String, String) = {
        let conn = db.lock();
        conn.query_row(
            "SELECT path_text, kind FROM files WHERE id = ?1",
            rusqlite::params![file_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )?
    };
    let source_path = std::path::PathBuf::from(&path_text);
    if !source_path.exists() {
        anyhow::bail!("source file missing: {}", source_path.display());
    }
    match kind.as_str() {
        "image" => {
            // C3: llama.cpp's image loader (stb_image) reads JPEG/PNG natively
            // — pass those through untouched (the overwhelming common case).
            // Everything else (webp, bmp, tiff, gif, …) gets transcoded to a
            // temp JPEG so the VLM's mmproj doesn't silently reject it: the
            // server declares the real MIME, but the loader is stb_image, which
            // has NO webp support, so a .webp reaches it and fails per-file with
            // no tags. image-rs decodes webp/bmp/tiff/gif (Cargo features);
            // HEIC isn't supported and falls through to a decode error — no
            // worse than today, where it would fail at the VLM instead.
            let ext = source_path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if matches!(ext.as_str(), "jpg" | "jpeg" | "png") {
                Ok((source_path, None))
            } else {
                let transcoded = transcode_image_to_jpeg(&source_path).await?;
                Ok((transcoded.clone(), Some(transcoded)))
            }
        }
        "video" => {
            let r = rasterize_video_keyframe(&source_path).await?;
            Ok((r.clone(), Some(r)))
        }
        "pdf" => {
            let r = rasterize_pdf_page(&source_path).await?;
            Ok((r.clone(), Some(r)))
        }
        _ => anyhow::bail!("kind '{}' isn't VLM-analyzable yet", kind),
    }
}

/// C3: decode an arbitrary image (webp/bmp/tiff/gif/…) and re-encode it as a
/// temp JPEG the VLM's stb_image-based loader can read. Caller cleans up the
/// temp file. Runs on a blocking thread (image-rs decode/encode is CPU-bound).
async fn transcode_image_to_jpeg(
    path: &std::path::Path,
) -> anyhow::Result<std::path::PathBuf> {
    let p = path.to_path_buf();
    tokio::task::spawn_blocking(move || -> anyhow::Result<std::path::PathBuf> {
        let img = image::open(&p)
            .map_err(|e| anyhow::anyhow!("decode {}: {e}", p.display()))?;
        let dest = std::env::temp_dir().join(format!("fileid-vlm-{}.jpg", uuid::Uuid::new_v4()));
        // Flatten to RGB8 — JPEG has no alpha channel.
        image::DynamicImage::ImageRgb8(img.to_rgb8())
            .save_with_format(&dest, image::ImageFormat::Jpeg)
            .map_err(|e| anyhow::anyhow!("encode jpeg {}: {e}", dest.display()))?;
        Ok(dest)
    })
    .await?
}

/// Persist VLM enrichment for one file: caption + proposed name into the v3
/// `files` columns, and tags into `tags` as `source='vlm'` (replacing any prior
/// vlm tags for the file). Shared by the CLI + server paths.
fn persist_vlm_results(
    conn: &rusqlite::Connection,
    file_id: i64,
    model_kind: &str,
    description: Option<&str>,
    proposed_name: Option<&str>,
    tags: &[String],
) -> anyhow::Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    conn.execute(
        "UPDATE files SET vlm_description=COALESCE(?1, vlm_description), \
                          vlm_proposed_name=COALESCE(?2, vlm_proposed_name), \
                          vlm_model=?3, vlm_analyzed_at=?4 WHERE id=?5",
        rusqlite::params![description, proposed_name, model_kind, now, file_id],
    )?;
    if !tags.is_empty() {
        conn.execute(
            "DELETE FROM tags WHERE file_id=?1 AND source='vlm'",
            rusqlite::params![file_id],
        )?;
        let mut stmt = conn.prepare(
            "INSERT OR IGNORE INTO tags (file_id, tag, source, score) VALUES (?1, ?2, 'vlm', NULL)",
        )?;
        for t in tags {
            stmt.execute(rusqlite::params![file_id, t])?;
        }
    }
    Ok(())
}

/// A2: one-shot probe that the persistent llama-server actually accepts our
/// multimodal `image_url` data-URI payload shape. This payload format was never
/// hardware-verified (see NEXT.md V16.8); if the server build rejects it (e.g.
/// 400 on the request), EVERY file in the batch would fail identically and
/// silently. Sending one tiny throwaway JPEG up front lets the batch detect the
/// incompatibility and fall back to the per-file CLI path (a different,
/// known-good code path) instead of producing zero tags.
pub(crate) async fn vlm_server_payload_ok(
    server: &crate::models::vlm_server::VlmServer,
) -> anyhow::Result<()> {
    let test_img = std::env::temp_dir().join(format!(
        "fileid-vlm-selftest-{}.jpg",
        uuid::Uuid::new_v4()
    ));
    // 32×32 mid-gray JPEG — smallest input that still exercises the mmproj +
    // chat-completions payload path.
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        32,
        32,
        image::Rgb([128u8, 128, 128]),
    ))
    .save_with_format(&test_img, image::ImageFormat::Jpeg)
    .map_err(|e| anyhow::anyhow!("write VLM self-test image: {e}"))?;
    let result = server.complete(&test_img, "Reply with: ok", 1).await;
    let _ = std::fs::remove_file(&test_img);
    result.map(|_| ())
}

/// Analyze one file through the PERSISTENT llama-server (model already loaded),
/// with NO per-call model reload. `mode` selects which VLM calls run: `Both`
/// does caption + tags + smart-rename (3 HTTP calls); `TagsOnly` does just the
/// tag call (1 call → ~3× faster — the background auto-tag path); CaptionOnly /
/// RenameOnly do their single call. The caption (or, in TagsOnly, the joined
/// tags) is handed to `on_token` in one shot (these server calls are
/// non-streaming). Mirrors `analyze_file`'s outputs so the batch loop is
/// backend-agnostic.
pub(crate) async fn analyze_file_via_server(
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    server: &crate::models::vlm_server::VlmServer,
    file_id: i64,
    model_kind: &str,
    mode: AnalyzeMode,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    mut on_token: impl FnMut(&str),
) -> anyhow::Result<AnalyzeOutcome> {
    use crate::models::vlm;
    let started = std::time::Instant::now();
    let (rasterized, temp_to_clean) = rasterize_for_vlm(&db, file_id).await?;

    let mut description: Option<String> = None;
    let mut proposed_name: Option<String> = None;
    let mut tags: Vec<String> = Vec::new();

    if matches!(mode, AnalyzeMode::CaptionOnly | AnalyzeMode::Both) {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }
        let d = server.complete(&rasterized, vlm::CAPTION_PROMPT, 80).await?;
        on_token(&d);
        description = Some(d);
    }

    if matches!(mode, AnalyzeMode::TagsOnly | AnalyzeMode::Both) {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }
        tags = parse_vlm_tags(&server.complete(&rasterized, vlm::TAG_PROMPT, 40).await?);
        // Surface tags in the live stream so a tags-only pass shows feedback.
        if !tags.is_empty() {
            on_token(&tags.join(", "));
        }
    }

    if matches!(mode, AnalyzeMode::RenameOnly | AnalyzeMode::Both) {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }
        proposed_name = Some(sanitize_proposed_name(
            &server.complete(&rasterized, vlm::RENAME_PROMPT, 30).await?,
        ));
    }

    {
        let conn = db.lock();
        persist_vlm_results(
            &conn,
            file_id,
            model_kind,
            description.as_deref(),
            proposed_name.as_deref(),
            &tags,
        )?;
    }
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
    // First attempt the 25 %-of-duration keyframe. If Media Foundation
    // can read the container but seeking fails (some VFR/fragmented MP4s
    // have unreliable duration metadata), the helper internally falls back
    // to offset 0. We retry once on top-level errors to rescue most
    // one-shot transient I/O issues on USB drives / network shares.
    let frame = match tokio::task::spawn_blocking({
        let p = p.clone();
        move || crate::shell::video::keyframe_25pct(&p)
    })
    .await?
    {
        Ok(f) => f,
        Err(first) => {
            tracing::warn!(?first, file = %crate::platform::redact_path_for_log(path), "keyframe_25pct failed; retrying once");
            tokio::task::spawn_blocking(move || crate::shell::video::keyframe_25pct(&p))
                .await??
        }
    };
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

/// Render the first page of a PDF to a temp JPEG via the bundled
/// pdfium-render binary. Gated behind the `pdf-analyze` feature.
#[cfg(feature = "pdf-analyze")]
async fn rasterize_pdf_page(path: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    use pdfium_render::prelude::*;
    let p = path.to_path_buf();
    tokio::task::spawn_blocking(move || -> anyhow::Result<std::path::PathBuf> {
        let pdfium = Pdfium::default();
        let doc = pdfium
            .load_pdf_from_file(&p, None)
            .map_err(|e| anyhow::anyhow!("pdfium load: {e}"))?;
        let page = doc
            .pages()
            .get(0)
            .map_err(|_| anyhow::anyhow!("PDF has no pages"))?;
        let render_config = PdfRenderConfig::new()
            .set_target_width(1024)
            .set_maximum_height(1024);
        let bitmap = page
            .render_with_config(&render_config)
            .map_err(|e| anyhow::anyhow!("pdfium render: {e}"))?;
        let img = bitmap.as_image();
        let dest = std::env::temp_dir().join(format!(
            "fileid-pdf-{}.jpg",
            uuid::Uuid::new_v4()
        ));
        img.save(&dest)?;
        Ok(dest)
    })
    .await?
}

#[cfg(not(feature = "pdf-analyze"))]
#[allow(clippy::unused_async)]
async fn rasterize_pdf_page(_path: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    anyhow::bail!(
        "PDF analysis requires the pdf-analyze feature flag. \
         Rebuild with: cargo build --features pdf-analyze"
    )
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

/// Generic, low-information tokens SmolVLM sometimes emits despite the prompt.
/// A tag is dropped if any of its words is one of these — they describe the
/// medium, not the content, and read as noise as a Library chip ("has location"
/// used to be the worst offender; that one is no longer emitted at all).
const VLM_TAG_STOPWORDS: &[&str] = &[
    "photo", "photos", "image", "images", "picture", "pictures", "object",
    "objects", "thing", "things", "scene", "background", "foreground",
    "location", "text", "item", "items", "stuff", "view", "misc", "unknown",
    "none",
];

/// Parse a VLM tag completion ("dog, Beach.") into clean, deduplicated,
/// lowercase tags. Defensive against numbering ("1. dog"), bullets, trailing
/// punctuation, surrounding quotes, and the model occasionally returning a
/// sentence (pieces with >2 words are dropped). Generic tokens
/// (`VLM_TAG_STOPWORDS`) are filtered out, and the result is capped at
/// `MAX_VLM_TAGS` so the Library shows 1-2 descriptive tags.
pub(crate) fn parse_vlm_tags(raw: &str) -> Vec<String> {
    const MAX_VLM_TAGS: usize = 2;
    let mut out: Vec<String> = Vec::new();
    for piece in raw.split([',', '\n', ';']) {
        let lowered = piece.trim().to_lowercase();
        // Strip leading list markers ("1.", "-", "*", "•") then surrounding
        // quotes / stray punctuation.
        let stripped = lowered
            .trim_start_matches(|c: char| {
                c.is_ascii_digit()
                    || c == '.'
                    || c == ')'
                    || c == '-'
                    || c == '*'
                    || c == '•'
                    || c.is_whitespace()
            })
            .trim_matches(|c: char| c == '"' || c == '\'' || c == '.' || c.is_whitespace());
        if stripped.is_empty() || stripped.len() > 40 {
            continue;
        }
        // Tags are 1-2 words (the prompt asks for it); drop anything longer so
        // chips stay short and scannable.
        if stripped.split_whitespace().count() > 2 {
            continue;
        }
        // Drop generic, low-information tags ("photo", "object", "background",
        // "location", …) — they describe the medium, not the content.
        if stripped
            .split_whitespace()
            .any(|w| VLM_TAG_STOPWORDS.contains(&w))
        {
            continue;
        }
        let t = stripped.to_string();
        if !out.iter().any(|e| e == &t) {
            out.push(t);
        }
        if out.len() >= MAX_VLM_TAGS {
            break;
        }
    }
    out
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
    fn parse_vlm_tags_splits_lowercases_and_strips_punct() {
        // Caps at 2 now; still lowercases ("Beach"→"beach") and strips the
        // trailing period.
        assert_eq!(parse_vlm_tags("Dog, beach."), vec!["dog", "beach"]);
    }

    #[test]
    fn parse_vlm_tags_strips_numbering_and_dedupes() {
        assert_eq!(parse_vlm_tags("1. dog\n2. dog\n3. ocean"), vec!["dog", "ocean"]);
    }

    #[test]
    fn parse_vlm_tags_drops_sentence_fragments_keeps_short() {
        // First piece is a >3-word fragment → dropped; "beach" kept.
        assert_eq!(
            parse_vlm_tags("a dog running on the beach at sunset, beach"),
            vec!["beach"]
        );
    }

    #[test]
    fn parse_vlm_tags_empty_is_empty() {
        assert!(parse_vlm_tags("").is_empty());
        assert!(parse_vlm_tags("   ").is_empty());
    }

    #[test]
    fn parse_vlm_tags_caps_count() {
        let many = (0..20).map(|i| format!("tag{i}")).collect::<Vec<_>>().join(", ");
        assert_eq!(parse_vlm_tags(&many).len(), 2);
    }

    #[test]
    fn parse_vlm_tags_drops_generic_tokens() {
        // "photo" and "object" are generic medium-words → dropped; the concrete
        // "golden retriever" survives.
        assert_eq!(
            parse_vlm_tags("photo, golden retriever, object"),
            vec!["golden retriever"]
        );
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

    #[cfg(feature = "pdf-analyze")]
    #[test]
    fn rasterize_pdf_page_rejects_missing_file() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(rasterize_pdf_page(std::path::Path::new(
            "C:\\does-not-exist-fileid-test.pdf",
        )));
        assert!(result.is_err(), "expected Err for missing PDF, got {:?}", result);
    }

    #[cfg(not(feature = "pdf-analyze"))]
    #[test]
    fn rasterize_pdf_page_without_feature_errors() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(rasterize_pdf_page(std::path::Path::new("any.pdf")));
        assert!(result.is_err(), "expected feature-gate Err");
        let err = format!("{:#}", result.unwrap_err());
        assert!(err.contains("pdf-analyze"), "err should mention feature flag: {err}");
    }
}
