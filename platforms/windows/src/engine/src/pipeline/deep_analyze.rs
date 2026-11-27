// Deep Analyze: VLM-powered captioning, tagging and smart-rename. Resolves the selected registry model, loads it via llama.cpp (Vulkan / CUDA / CPU), renders each file (image, video keyframe, pdfium first page) down to the model context, persists to `deep_analyze_results`, and emits `deepAnalyzeProgress` events.

use anyhow::Context;

use super::deep_analyze_naming as naming;
use super::naming_context::NamingContext;
use super::text_reading;

#[derive(Debug, Clone, Default)]
pub struct AnalyzeOutcome {
    pub file_id: i64,
    pub description: Option<String>,
    pub proposed_name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalyzeMode {
    /// Tags only: the fast background auto-tag path. One VLM call per file instead of three, so a whole-library pass is ~3× faster; caption and proposed-name columns are left untouched.
    TagsOnly,
    Both,
    /// Caption + tags but no smart-rename (the manual pass with "Propose renames" unticked); the proposed-name column is left untouched.
    CaptionAndTags,
}

impl AnalyzeMode {
    pub(crate) fn requests_caption(self) -> bool {
        matches!(self, Self::Both | Self::CaptionAndTags)
    }

    pub(crate) fn requests_tags(self) -> bool {
        matches!(self, Self::TagsOnly | Self::Both | Self::CaptionAndTags)
    }

    pub(crate) fn requests_rename(self) -> bool {
        self == Self::Both
    }

    /// The passes the model is asked for. A reviewed label names the file, so
    /// the rename request is dropped.
    pub(crate) fn asked_of_model(self, context: &NamingContext) -> Self {
        if self == Self::Both && context.label.is_some() { Self::CaptionAndTags } else { self }
    }

    fn establishes_completion(self) -> bool {
        matches!(self, Self::Both | Self::CaptionAndTags)
    }
}

/// Removes the temp rasterized frame on EVERY exit path (`?` error, cancel bail, success).
struct TempFileGuard(Option<std::path::PathBuf>);
impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = std::fs::remove_file(&p);
        }
    }
}

pub(crate) async fn analyze_file(
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    runner: &crate::models::vlm::VlmRunner,
    file_id: i64,
    model_kind: &str,
    mode: AnalyzeMode,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    context: &NamingContext,
    on_token: impl FnMut(&str),
) -> anyhow::Result<AnalyzeOutcome> {
    analyze_file_scoped(db, runner, file_id, model_kind, mode, cancel, context, on_token, None).await
}

pub(crate) async fn analyze_file_scoped(
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    runner: &crate::models::vlm::VlmRunner,
    file_id: i64,
    model_kind: &str,
    mode: AnalyzeMode,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    context: &NamingContext,
    mut on_token: impl FnMut(&str),
    scope: Option<&crate::commands::deep_scope::DeepScope>,
) -> anyhow::Result<AnalyzeOutcome> {
    use crate::models::vlm::{self, CaptionRequest};
    let face_names = &context.subjects[..];
    let asked = mode.asked_of_model(context);

    // Documents, audio and (without installed weights) 3D models are named
    // from their own text and metadata, so they work with no VLM at all.
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        anyhow::bail!("cancelled");
    }
    let weights_installed = naming::vlm_weights_installed(model_kind);
    if let Some(outcome) =
        analyze_metadata_named_file(&db, file_id, model_kind, mode, weights_installed, true, &cancel, context, scope).await?
    {
        return Ok(outcome);
    }

    let verified = runner.verified_weights(model_kind, cancel.clone()).await?;
    let (gguf, mmproj) = verified.paths();
    let (gguf, mmproj) = (gguf.to_owned(), mmproj.to_owned());

    let (rasterized, temp_to_clean) = match rasterize_for_vlm(&db, file_id, scope).await {
        Ok(r) => r,
        Err(e) => {
            if let Some(outcome) =
                analyze_metadata_named_file(&db, file_id, model_kind, mode, false, true, &cancel, context, scope).await?
            {
                return Ok(outcome);
            }
            return Err(e);
        }
    };
    let _temp_guard = TempFileGuard(temp_to_clean);

    let mut description: Option<String> = None;
    let mut proposed_name: Option<String> = None;

    if matches!(mode, AnalyzeMode::Both | AnalyzeMode::CaptionAndTags) {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }
        let req = CaptionRequest {
            gguf_path: gguf.clone(),
            mmproj_path: mmproj.clone(),
            image_path: rasterized.clone(),
            prompt: vlm::caption_prompt_with_faces(face_names),
            max_tokens: 80,
            greedy: true,
        };
        let result = vlm::caption(runner, &req, cancel.clone(), &mut on_token).await?;
        description = Some(result.text);
    }
    // VLM scene/content tags (source='vlm'), generated in "Both" and "TagsOnly" so a Deep Analyze run produces the chip tags that replace CLIP zero-shot if the user drops CLIP. Clones the weights and rasterized frame so the rename branch below can still take ownership.
    let mut tags: Vec<String> = Vec::new();
    if matches!(mode, AnalyzeMode::Both | AnalyzeMode::TagsOnly | AnalyzeMode::CaptionAndTags) {
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
    if asked == AnalyzeMode::Both {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }
        let req = CaptionRequest {
            gguf_path: gguf.clone(),
            mmproj_path: mmproj.clone(),
            image_path: rasterized.clone(),
            prompt: vlm::rename_prompt_with_faces(face_names),
            max_tokens: 30,
            greedy: true,
        };
        let result = vlm::caption(runner, &req, cancel.clone(), |_| {}).await?;
        proposed_name = Some(apply_person_prefix(&sanitize_proposed_name(&result.text), face_names));
    }

    {
        proposed_name = proposed_name.and_then(|name| crate::util::smart_name::readable_stem(&name, face_names));
        let conn = db.lock();
        proposed_name = persist_vlm_results(
            &conn,
            file_id,
            model_kind,
            mode,
            description.as_deref(),
            proposed_name.as_deref(),
            &tags,
            context,
        )?;
    }
    if mode.requests_caption() && transcription_wanted(&db, file_id) && !cancel.load(std::sync::atomic::Ordering::Relaxed) {
        let req = CaptionRequest {
            gguf_path: gguf,
            mmproj_path: mmproj,
            image_path: rasterized,
            prompt: text_reading::TRANSCRIPTION_PROMPT.to_string(),
            max_tokens: TRANSCRIPTION_MAX_TOKENS,
            greedy: true,
        };
        match vlm::caption(runner, &req, cancel.clone(), |_| {}).await {
            Ok(result) => store_transcription_output(&db, file_id, &result.text),
            Err(error) => tracing::warn!(?error, "[DEEP] transcription failed"),
        }
    }

    Ok(AnalyzeOutcome {
        file_id,
        description,
        proposed_name,
    })
}

/// Opening a cloud placeholder (OneDrive Files On-Demand, iCloud) downloads it, which is network egress the user did not ask for.
fn ensure_available_offline(path: &std::path::Path, online_only: bool) -> anyhow::Result<()> {
    if online_only {
        anyhow::bail!("File is online-only; make it available offline first: {}", path.display());
    }
    Ok(())
}

/// Resolves a file's on-disk path and rasterizes it for the VLM: images pass through, video → 25%-duration keyframe, PDF → page-1 render. Returns the image path plus an optional temp path the caller must clean up. Shared by the per-file CLI and the persistent-server path.
pub(crate) async fn rasterize_for_vlm(
    db: &std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    file_id: i64,
    scope: Option<&crate::commands::deep_scope::DeepScope>,
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
    let source_path = match scope {
        Some(scope) => scope.source_path(file_id, &source_path)?
            .ok_or(crate::commands::deep_scope::ScopeError::ExcludedTarget)?,
        None => source_path,
    };
    let metadata = std::fs::metadata(&source_path).map_err(|_| anyhow::anyhow!("source file missing: {}", source_path.display()))?;
    ensure_available_offline(&source_path, crate::pipeline::discovery::is_online_only(&metadata))?;
    match kind.as_str() {
        "image" => {
            // llama.cpp's image loader (stb_image) reads JPEG/PNG natively, so those pass through untouched. Everything else (webp, bmp, tiff, gif, …) is transcoded to a temp JPEG: the server declares the real MIME but stb_image has NO webp support, so a .webp would fail per-file with no tags. image-rs decodes webp/bmp/tiff/gif; HEIC isn't supported and falls through to a decode error.
            let ext = source_path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if matches!(ext.as_str(), "jpg" | "jpeg" | "png") {
                let checked = source_path.clone();
                let (width, height) = tokio::task::spawn_blocking(move || {
                    naming::validate_vlm_image_dimensions(&checked)
                })
                .await??;
                // Native-resolution 24-48 MP photos multiply the VLM's prefill and mtmd preprocessing cost several-fold with no caption/tag gain, so oversized inputs are downscaled via the transcode path.
                if width.max(height) <= naming::MAX_VLM_INPUT_EDGE {
                    Ok((source_path, None))
                } else {
                    let transcoded = transcode_image_to_jpeg(&source_path).await?;
                    Ok((transcoded.clone(), Some(transcoded)))
                }
            } else {
                let transcoded = transcode_image_to_jpeg(&source_path).await?;
                Ok((transcoded.clone(), Some(transcoded)))
            }
        }
        "model" => {
            let src = source_path.clone();
            let dest = std::env::temp_dir().join(format!("fileid-obj-{}.png", uuid::Uuid::new_v4()));
            let rendered = dest.clone();
            tokio::task::spawn_blocking(move || crate::pipeline::obj_render::render_obj_to_png(&src, &rendered))
                .await??;
            Ok((dest.clone(), Some(dest)))
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

/// Decodes an arbitrary image, downscales anything over [`naming::MAX_VLM_INPUT_EDGE`], and re-encodes a temp JPEG the stb_image-based loader can read. Caller cleans up the temp file. Runs on a blocking thread (CPU-bound).
async fn transcode_image_to_jpeg(
    path: &std::path::Path,
) -> anyhow::Result<std::path::PathBuf> {
    let p = path.to_path_buf();
    tokio::task::spawn_blocking(move || -> anyhow::Result<std::path::PathBuf> {
        let image_rs_decode = || -> anyhow::Result<image::DynamicImage> {
            naming::validate_vlm_image_dimensions(&p)?;
            image::open(&p).map_err(|e| anyhow::anyhow!("decode {}: {e}", p.display()))
        };
        let img = match image_rs_decode() {
            Ok(img) => img,
            Err(primary_error) => {
                #[cfg(windows)]
                let image = match crate::shell::heic::decode(&p) {
                    Ok((rgb, width, height)) => {
                        naming::validate_vlm_pixel_count(width, height)?;
                        let image = image::RgbImage::from_raw(width, height, rgb).ok_or_else(|| {
                            anyhow::anyhow!("Windows bitmap decoder returned an invalid RGB buffer")
                        })?;
                        image::DynamicImage::ImageRgb8(image)
                    }
                    Err(wic_error) => {
                        let thumbnail = crate::shell::thumbnail::render_thumbnail_only_at(
                            &p,
                            naming::MAX_VLM_INPUT_EDGE as i32,
                        )
                        .map_err(|shell_error| {
                            anyhow::anyhow!(
                                "{primary_error}; Windows bitmap decoder fallback failed: {wic_error}; Windows thumbnail provider fallback failed: {shell_error}"
                            )
                        })?;
                        naming::validate_vlm_pixel_count(thumbnail.width, thumbnail.height)?;
                        let image = image::RgbaImage::from_raw(
                            thumbnail.width,
                            thumbnail.height,
                            thumbnail.rgba,
                        )
                        .ok_or_else(|| {
                            anyhow::anyhow!("Windows thumbnail provider returned an invalid RGBA buffer")
                        })?;
                        image::DynamicImage::ImageRgba8(image)
                    }
                };
                #[cfg(not(windows))]
                let image = {
                    let ext = p
                        .extension()
                        .and_then(|value| value.to_str())
                        .unwrap_or_default();
                    if !ext.eq_ignore_ascii_case("heic") && !ext.eq_ignore_ascii_case("heif") {
                        return Err(primary_error);
                    }
                    crate::shell::heic::decode(&p).map_err(|heic_error| {
                        anyhow::anyhow!("{primary_error}; HEIC fallback failed: {heic_error}")
                    })?
                };
                #[cfg(not(windows))]
                let image = {
                    let (rgb, width, height) = image;
                    naming::validate_vlm_pixel_count(width, height)?;
                    let image = image::RgbImage::from_raw(width, height, rgb).ok_or_else(|| {
                        anyhow::anyhow!("HEIC decoder returned an invalid RGB buffer")
                    })?;
                    image::DynamicImage::ImageRgb8(image)
                };
                image
            }
        };
        let img = if img.width().max(img.height()) > naming::MAX_VLM_INPUT_EDGE {
            img.resize(
                naming::MAX_VLM_INPUT_EDGE,
                naming::MAX_VLM_INPUT_EDGE,
                image::imageops::FilterType::Triangle,
            )
        } else {
            img
        };
        let dest = std::env::temp_dir().join(format!("fileid-vlm-{}.jpg", uuid::Uuid::new_v4()));
        image::DynamicImage::ImageRgb8(img.to_rgb8())
            .save_with_format(&dest, image::ImageFormat::Jpeg)
            .map_err(|e| anyhow::anyhow!("encode jpeg {}: {e}", dest.display()))?;
        Ok(dest)
    })
    .await?
}

/// Persists VLM enrichment: caption + proposed name into the v3 `files` columns, tags into `tags` as `source='vlm'` (replacing prior vlm tags). Shared by the CLI and server paths.
fn persist_vlm_results(
    conn: &rusqlite::Connection,
    file_id: i64,
    model_kind: &str,
    mode: AnalyzeMode,
    description: Option<&str>,
    proposed_name: Option<&str>,
    tags: &[String],
    context: &NamingContext,
) -> anyhow::Result<Option<String>> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    // Single transaction so the caption/name UPDATE and the vlm-tag DELETE+INSERT commit atomically: a crash between them must not drop a file's VLM tags. `unchecked_` because callers hold `conn` behind a parking_lot::Mutex.
    let tx = conn.unchecked_transaction()?;
    context.validate(&tx)?;
    let reviewed_name = (mode.requests_rename() && context.label.is_some()).then(|| context.proposed_name());
    let proposed_name = match &reviewed_name {
        Some(reviewed) => reviewed.as_deref(),
        None => proposed_name,
    };
    // A requested output replaces the old one even when it came back empty; an output not asked for is kept.
    let description = description.filter(|text| !text.is_empty());
    let proposed_name = proposed_name.filter(|name| !name.is_empty());
    let (updates_description, updates_name) = (mode.requests_caption(), mode.requests_rename());
    if mode.establishes_completion() {
        tx.execute(
            "UPDATE files SET vlm_description=CASE WHEN ?6 THEN ?2 ELSE vlm_description END, \
                              vlm_proposed_name=CASE WHEN ?7 THEN ?3 ELSE vlm_proposed_name END, \
                              vlm_model=?1, vlm_full_model=?1, vlm_analyzed_at=?4, vlm_inherited_from=NULL WHERE id=?5",
            rusqlite::params![model_kind, description, proposed_name, now, file_id, updates_description, updates_name],
        )?;
    } else {
        let completed_with_model = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM files WHERE id=?1 AND vlm_full_model=?2)",
            rusqlite::params![file_id, model_kind],
            |row| row.get::<_, bool>(0),
        )?;
        if completed_with_model {
            tx.execute(
                "UPDATE files SET vlm_description=CASE WHEN ?4 THEN ?1 ELSE vlm_description END, \
                                  vlm_proposed_name=CASE WHEN ?5 THEN ?2 ELSE vlm_proposed_name END, vlm_inherited_from=NULL \
                 WHERE id=?3",
                rusqlite::params![description, proposed_name, file_id, updates_description, updates_name],
            )?;
        } else {
            tx.execute(
                "UPDATE files SET vlm_description=CASE WHEN ?4 THEN ?1 ELSE vlm_description END, \
                                  vlm_proposed_name=CASE WHEN ?5 THEN ?2 ELSE vlm_proposed_name END, \
                                  vlm_model=NULL, vlm_full_model=NULL, vlm_analyzed_at=NULL, vlm_inherited_from=NULL \
                 WHERE id=?3",
                rusqlite::params![description, proposed_name, file_id, updates_description, updates_name],
            )?;
        }
    }
    if mode.requests_tags() {
        tx.execute(
            "DELETE FROM tags WHERE file_id=?1 AND source='vlm'",
            rusqlite::params![file_id],
        )?;
        let mut stmt = tx.prepare(
            "INSERT OR IGNORE INTO tags (file_id, tag, source, score) VALUES (?1, ?2, 'vlm', NULL)",
        )?;
        for t in tags.iter().map(|tag| tag.trim()).filter(|tag| !tag.is_empty()) {
            stmt.execute(rusqlite::params![file_id, t])?;
        }
    }
    tx.commit()?;
    Ok(proposed_name.map(str::to_owned))
}

/// Copy a near-duplicate's persisted caption and VLM tags onto `file_id`, flagged
/// with `vlm_inherited_from`, instead of running the model again (burst frames and
/// RAW+JPEG pairs). `proposed_name` is the member's own sequence-suffixed name.
pub(crate) fn inherit_vlm_results(
    conn: &rusqlite::Connection,
    file_id: i64,
    from_id: i64,
    model_kind: &str,
    mode: AnalyzeMode,
    proposed_name: Option<&str>,
    context: &NamingContext,
) -> anyhow::Result<AnalyzeOutcome> {
    let (description, source_name): (Option<String>, Option<String>) = conn.query_row(
        "SELECT vlm_description, vlm_proposed_name FROM files WHERE id=?1",
        rusqlite::params![from_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let tags: Vec<String> = {
        let mut stmt = conn.prepare("SELECT tag FROM tags WHERE file_id=?1 AND source='vlm' ORDER BY rowid")?;
        let rows = stmt.query_map(rusqlite::params![from_id], |r| r.get::<_, String>(0))?;
        rows.collect::<Result<_, _>>()?
    };
    let description = if mode.requests_caption() { description } else { None };
    let proposed_name = if mode.requests_rename() { proposed_name.map(str::to_owned).or(source_name) } else { None };
    let proposed_name = persist_vlm_results(conn, file_id, model_kind, mode, description.as_deref(), proposed_name.as_deref(), &tags, context)?;
    conn.execute("UPDATE files SET vlm_inherited_from=?1 WHERE id=?2", rusqlite::params![from_id, file_id])?;
    Ok(AnalyzeOutcome { file_id, description, proposed_name })
}

/// One-shot probe that the persistent llama-server accepts our multimodal `image_url` data-URI payload shape, which was never hardware-verified. If a build rejects it every file in the batch would fail identically and silently, so one throwaway JPEG up front lets the batch fall back to the per-file CLI path instead of producing zero tags.
pub(crate) async fn vlm_server_payload_ok(
    server: &crate::models::vlm_server::VlmServer,
    cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> anyhow::Result<()> {
    let test_img = std::env::temp_dir().join(format!(
        "fileid-vlm-selftest-{}.jpg",
        uuid::Uuid::new_v4()
    ));
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        32,
        32,
        image::Rgb([128u8, 128, 128]),
    ))
    .save_with_format(&test_img, image::ImageFormat::Jpeg)
    .map_err(|e| anyhow::anyhow!("write VLM self-test image: {e}"))?;
    let result = server.complete_with_cancel(&test_img, "Reply with: ok", 1, cancel.clone()).await;
    let _ = std::fs::remove_file(&test_img);
    result.map(|_| ())
}

const TRANSCRIPTION_MAX_TOKENS: u32 = 320;

/// Text-bearing image (or image-only PDF) whose OCR is missing or mangled — see
/// `text_reading::needs_transcription`.
fn transcription_wanted(db: &std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>, file_id: i64) -> bool {
    text_reading::transcription_candidate(&db.lock(), file_id).unwrap_or(false)
}

/// The caption already persisted, so a failed transcription only logs; the next
/// Deep Analyze run retries it.
fn store_transcription_output(db: &std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>, file_id: i64, raw: &str) {
    if let Some((text, handwritten)) = text_reading::parse_transcription(raw)
        && let Err(error) = text_reading::store_transcription(&db.lock(), file_id, &text, handwritten) {
            tracing::warn!(?error, "[DEEP] transcription persist failed");
        }
}

async fn complete_cancellable(
    server: &crate::models::vlm_server::VlmServer,
    image: &std::path::Path, prompt: &str, max_tokens: u32,
    cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> anyhow::Result<String> {
    server.complete_with_cancel(image, prompt, max_tokens, cancel.clone()).await
}

trait VisualAsker {
    fn ask(&self, prompt: &str, max_tokens: u32) -> impl std::future::Future<Output = anyhow::Result<String>> + Send;
}

struct ServerFrame<'a> {
    server: &'a crate::models::vlm_server::VlmServer,
    image: &'a std::path::Path,
    cancel: &'a std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl VisualAsker for ServerFrame<'_> {
    fn ask(&self, prompt: &str, max_tokens: u32) -> impl std::future::Future<Output = anyhow::Result<String>> + Send {
        complete_cancellable(self.server, self.image, prompt, max_tokens, self.cancel)
    }
}

/// The model requests for an image, video frame or PDF page through a
/// persistent server: caption, tags and name in one request, with separate
/// prompts only for the sections the answer missed. `ask` sends one prompt;
/// a reviewed label drops the name request upstream (`AnalyzeMode::asked_of_model`).
async fn visual_answers<A: VisualAsker>(
    asked: AnalyzeMode,
    face_names: &[String],
    ask: &A,
) -> anyhow::Result<(Option<String>, Vec<String>, Option<String>)> {
    use crate::models::vlm;
    let mut description: Option<String> = None;
    let mut proposed_name: Option<String> = None;
    let mut tags: Vec<String> = Vec::new();
    if asked.requests_caption() {
        let include_filename = asked.requests_rename();
        let prompt = naming::combined_visual_prompt(face_names, include_filename);
        let raw = ask.ask(&prompt, if include_filename { 140 } else { 110 }).await?;
        (description, tags, proposed_name) = naming::parse_combined_visual_completion(&raw, include_filename);
        if description.is_none() {
            let cap_prompt = vlm::caption_prompt_with_faces(face_names);
            description = Some(ask.ask(&cap_prompt, 80).await?);
        }
        if tags.is_empty() {
            tags = naming::grounded_fallback_tags(description.as_deref().unwrap_or(&raw));
        }
        if include_filename && proposed_name.is_none() {
            proposed_name = description.as_deref().and_then(crate::util::keywords::grounded_filename);
        }
        if include_filename && proposed_name.is_none() {
            let ren_prompt = vlm::rename_prompt_with_faces(face_names);
            proposed_name = naming::generated_filename_candidate(&ask.ask(&ren_prompt, 30).await?);
            if proposed_name.is_none() {
                proposed_name = naming::generated_filename_candidate(&ask.ask(vlm::RENAME_RETRY_PROMPT, 30).await?);
            }
        }
    } else if asked.requests_tags() {
        let raw = ask.ask(vlm::TAG_PROMPT, 40).await?;
        tags = parse_vlm_tags(&raw);
        if tags.is_empty() {
            tags = naming::grounded_fallback_tags(&raw);
        }
    }
    Ok((description, tags, proposed_name))
}

/// Analyze one file through the PERSISTENT llama-server (model already loaded, no per-call reload). Documents are named from their text, audio from tags or a speech transcript, 3D models from a render (or embedded names when it fails); everything else takes the visual path. `Both` and `CaptionAndTags` ask for caption, tags and filename in ONE request, with separate prompts only as fallbacks. Results go to `on_token` in one shot (non-streaming).
pub(crate) async fn analyze_file_via_server(
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    server: &crate::models::vlm_server::VlmServer,
    file_id: i64,
    model_kind: &str,
    mode: AnalyzeMode,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    context: &NamingContext,
    mut on_token: impl FnMut(&str),
    scope: Option<&crate::commands::deep_scope::DeepScope>,
) -> anyhow::Result<AnalyzeOutcome> {
    let face_names = &context.subjects[..];
    let asked = mode.asked_of_model(context);
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        anyhow::bail!("cancelled");
    }
    if let Some(outcome) =
        analyze_document_via_server(&db, server, file_id, model_kind, mode, &cancel, &mut on_token, context, scope).await?
    {
        return Ok(outcome);
    }
    // The caller holds a resident lease; transcribing would acquire another
    // and wait on that same lock until the user cancels.
    if let Some(outcome) = analyze_metadata_named_file(&db, file_id, model_kind, mode, true, false, &cancel, context, scope).await? {
        return Ok(outcome);
    }
    let (rasterized, temp_to_clean) = match rasterize_for_vlm(&db, file_id, scope).await {
        Ok(r) => r,
        Err(e) => {
            if let Some(outcome) =
                analyze_metadata_named_file(&db, file_id, model_kind, mode, false, false, &cancel, context, scope).await?
            {
                return Ok(outcome);
            }
            return Err(e);
        }
    };
    let _temp_guard = TempFileGuard(temp_to_clean);

    let mut description: Option<String> = None;
    let mut proposed_name: Option<String> = None;
    let mut tags: Vec<String> = Vec::new();

    if asked.requests_caption() || asked.requests_tags() {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }
        (description, tags, proposed_name) = visual_answers(asked, face_names, &ServerFrame { server, image: &rasterized, cancel: &cancel }).await?;
        if asked.requests_caption()
            && let Some(value) = description.as_deref()
        {
            on_token(value);
        }
        if !tags.is_empty() {
            on_token(&tags.join(", "));
        }
    }

    if let Some(name) = proposed_name.take() {
        proposed_name = crate::util::smart_name::readable_stem(&apply_person_prefix(&name, face_names), face_names);
    }
    {
        let conn = db.lock();
        proposed_name = naming::vetted_generated_proposed_name(&conn, file_id, proposed_name)?;
        proposed_name = persist_vlm_results(&conn, file_id, model_kind, mode, description.as_deref(), proposed_name.as_deref(), &tags, context)?;
    }
    if mode.requests_caption() && transcription_wanted(&db, file_id) && !cancel.load(std::sync::atomic::Ordering::Relaxed) {
        match complete_cancellable(server, &rasterized, text_reading::TRANSCRIPTION_PROMPT, TRANSCRIPTION_MAX_TOKENS, &cancel)
            .await
        {
            Ok(raw) => store_transcription_output(&db, file_id, &raw),
            Err(error) => tracing::warn!(?error, "[DEEP] transcription failed"),
        }
    }

    Ok(AnalyzeOutcome {
        file_id,
        description,
        proposed_name,
    })
}

/// The Mac app's engine: one file through the MLX analysis
/// (`DeepAnalyze.analyze` in FileIDAppleKit), which decodes the file, prompts,
/// parses and grounds the reply. Audio keeps the metadata path; a 3D model
/// the model can't read falls back to it.
#[cfg(all(target_os = "macos", feature = "apple-adapters"))]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn analyze_file_via_mlx(
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    lease: &mut crate::apple::vlm::MlxLease,
    file_id: i64,
    model_kind: &str,
    mode: AnalyzeMode,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    context: &NamingContext,
    on_token: impl FnMut(&str),
    scope: Option<&crate::commands::deep_scope::DeepScope>,
) -> anyhow::Result<AnalyzeOutcome> {
    use std::sync::atomic::Ordering;
    let face_names = &context.subjects[..];
    let asked = mode.asked_of_model(context);
    if cancel.load(Ordering::Relaxed) {
        anyhow::bail!("cancelled");
    }
    let (path, kind) = analysis_source(&db, file_id, scope)?;
    if kind == "audio"
        && let Some(outcome) = analyze_metadata_named_file(&db, file_id, model_kind, mode, false, true, &cancel, context, scope).await?
    {
        return Ok(outcome);
    }
    let document_text = if kind == "doc" || kind == "pdf" {
        let persisted: Option<String> =
            db.lock().query_row("SELECT text FROM doc_text WHERE file_id=?1", [file_id], |row| row.get(0)).ok();
        match persisted {
            Some(text) => Some(text),
            None => {
                let source = path.clone();
                tokio::task::spawn_blocking(move || crate::pipeline::doc_extract::extract(&source, None))
                    .await
                    .context("document extraction task failed")?
                    .ok()
                    .flatten()
            }
        }
    } else {
        None
    };
    let request = crate::apple::vlm::AnalyzeRequest {
        path: path.clone(),
        media_kind: kind.clone(),
        document_text,
        face_names: context.subjects.clone(),
        propose_filename: asked.requests_rename(),
        tags_only: mode == AnalyzeMode::TagsOnly,
        scene_caption: false,
    };
    let reply = lease.analyze(&request, &cancel, on_token).await?;
    if cancel.load(Ordering::Relaxed) {
        anyhow::bail!("cancelled");
    }
    let failed = if mode == AnalyzeMode::TagsOnly { reply.description_failed() } else { reply.failed() };
    if failed {
        if kind == "model"
            && let Some(outcome) = analyze_metadata_named_file(&db, file_id, model_kind, mode, false, true, &cancel, context, scope).await?
        {
            return Ok(outcome);
        }
        anyhow::bail!(reply.description);
    }
    let description = mode.requests_caption().then_some(reply.description);
    let tags = if mode.requests_tags() { reply.tags } else { Vec::new() };
    let proposed_name = {
        let conn = db.lock();
        let proposed_name = if mode.requests_rename() {
            naming::vetted_generated_proposed_name(&conn, file_id, reply.proposed_name)?
                .and_then(|name| crate::util::smart_name::readable_stem(&name, face_names))
        } else {
            None
        };
        persist_vlm_results(&conn, file_id, model_kind, mode, description.as_deref(), proposed_name.as_deref(), &tags, context)?
    };
    if mode.requests_caption() && (kind == "image" || kind == "pdf") && transcription_wanted(&db, file_id) && !cancel.load(Ordering::Relaxed) {
        match lease.transcribe(&path, kind == "pdf", &cancel).await {
            Ok(raw) => store_transcription_output(&db, file_id, &raw),
            Err(error) => tracing::warn!(?error, "[DEEP] transcription failed"),
        }
    }
    Ok(AnalyzeOutcome { file_id, description, proposed_name })
}

async fn complete_text_cancellable(
    server: &crate::models::vlm_server::VlmServer,
    prompt: &str,
    max_tokens: u32,
    cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> anyhow::Result<String> {
    server.complete_text_with_cancel(prompt, max_tokens, cancel.clone()).await
}

fn analysis_source(
    db: &std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    file_id: i64,
    scope: Option<&crate::commands::deep_scope::DeepScope>,
) -> anyhow::Result<(std::path::PathBuf, String)> {
    let (path_text, kind): (String, String) = db.lock().query_row(
        "SELECT path_text, kind FROM files WHERE id = ?1",
        [file_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let path = std::path::PathBuf::from(path_text);
    let path = match scope {
        Some(scope) => scope
            .source_path(file_id, &path)?
            .ok_or(crate::commands::deep_scope::ScopeError::ExcludedTarget)?,
        None => path,
    };
    if let Ok(metadata) = std::fs::metadata(&path) {
        ensure_available_offline(&path, crate::pipeline::discovery::is_online_only(&metadata))?;
    }
    Ok((path, kind))
}

#[allow(clippy::too_many_arguments)]
async fn analyze_document_via_server(
    db: &std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    server: &crate::models::vlm_server::VlmServer,
    file_id: i64,
    model_kind: &str,
    mode: AnalyzeMode,
    cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    on_token: &mut impl FnMut(&str),
    context: &NamingContext,
    scope: Option<&crate::commands::deep_scope::DeepScope>,
) -> anyhow::Result<Option<AnalyzeOutcome>> {
    let (path, kind) = analysis_source(db, file_id, scope)?;
    if kind != "doc" {
        return Ok(None);
    }
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        anyhow::bail!("cancelled");
    }
    let persisted: Option<String> = db
        .lock()
        .query_row("SELECT text FROM doc_text WHERE file_id=?1", [file_id], |row| row.get(0))
        .ok();
    let text = match persisted {
        Some(text) => text,
        None => {
            let source = path.clone();
            tokio::task::spawn_blocking(move || crate::pipeline::doc_extract::extract(&source, None))
                .await
                .context("document extraction task failed")??
                .unwrap_or_default()
        }
    };
    let Some(text) = naming::bounded_document_text(&text) else {
        return analyze_metadata_named_file(db, file_id, model_kind, mode, false, false, cancel, context, scope).await;
    };
    let (_, fallback_name, deterministic_tags) = naming::document_metadata_from_text(&text);
    let extension = path.extension().and_then(|value| value.to_str()).unwrap_or_default();
    let mut description = None;
    let mut proposed_name = None;
    let asked = mode.asked_of_model(context);
    if asked.requests_caption() || asked.requests_rename() {
        let prompt = naming::document_analysis_prompt(extension, &text, asked)?;
        let max_tokens = match (asked.requests_caption(), asked.requests_rename()) {
            (true, true) => 120,
            (true, false) => 90,
            (false, true) => 40,
            (false, false) => 1,
        };
        let raw = complete_text_cancellable(server, &prompt, max_tokens, cancel).await?;
        let (parsed_description, parsed_name) = naming::parse_document_completion(&raw);
        if mode.requests_caption() {
            let grounded = naming::remove_unsupported_document_visual_claims(
                parsed_description.as_deref().unwrap_or_default(),
                &text,
            );
            on_token(&grounded);
            description = Some(grounded);
        }
        if asked.requests_rename() {
            proposed_name = naming::grounded_document_filename(parsed_name, &text).or(fallback_name);
        }
    }
    let tags = if mode.requests_tags() { deterministic_tags } else { Vec::new() };
    if !tags.is_empty() {
        on_token(&tags.join(", "));
    }
    {
        let conn = db.lock();
        proposed_name = naming::vetted_generated_proposed_name(&conn, file_id, proposed_name)?;
        proposed_name = persist_vlm_results(&conn, file_id, model_kind, mode, description.as_deref(), proposed_name.as_deref(), &tags, context)?;
    }
    Ok(Some(AnalyzeOutcome { file_id, description, proposed_name }))
}

/// Name audio from its tags (or a speech transcript when it has no title),
/// documents from their text, and 3D models from their embedded object and
/// material names. 3D models prefer the visual path when `model_to_vlm`.
/// Returns None for every other kind.
async fn analyze_metadata_named_file(
    db: &std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    file_id: i64,
    model_kind: &str,
    mode: AnalyzeMode,
    model_to_vlm: bool,
    may_transcribe: bool,
    cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    context: &NamingContext,
    scope: Option<&crate::commands::deep_scope::DeepScope>,
) -> anyhow::Result<Option<AnalyzeOutcome>> {
    let (path, kind) = analysis_source(db, file_id, scope)?;
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_lowercase).unwrap_or_default();
    let is_model = naming::is_3d_model_ext(&ext);
    if kind != "doc" && kind != "audio" && !is_model {
        return Ok(None);
    }
    if is_model && model_to_vlm {
        return Ok(None);
    }
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        anyhow::bail!("cancelled");
    }
    let (source, task_kind) = (path.clone(), kind.clone());
    let (mut description, mut proposed_name, tags) =
        tokio::task::spawn_blocking(move || metadata_naming_blocking(&source, &task_kind))
            .await
            .context("metadata naming task failed")??;
    if may_transcribe && kind == "audio" && proposed_name.is_none() && mode.asked_of_model(context).requests_rename()
        && let Some((name, transcript)) = transcribe_audio_name(&path, cancel).await {
            proposed_name = Some(name);
            description = description.or(Some(transcript));
        }
    let description = if mode.requests_caption() { description } else { None };
    let proposed_name = if mode.requests_rename() { proposed_name } else { None };
    let tags = if mode.requests_tags() { tags } else { Vec::new() };
    let proposed_name = {
        let conn = db.lock();
        let proposed_name = naming::vetted_proposed_name(&conn, file_id, proposed_name)?;
        persist_vlm_results(&conn, file_id, model_kind, mode, description.as_deref(), proposed_name.as_deref(), &tags, context)?
    };
    Ok(Some(AnalyzeOutcome { file_id, description, proposed_name }))
}

fn metadata_naming_blocking(
    path: &std::path::Path,
    kind: &str,
) -> anyhow::Result<(Option<String>, Option<String>, Vec<String>)> {
    if kind == "doc" {
        let text = crate::pipeline::doc_extract::extract(path, None).ok().flatten().unwrap_or_default();
        Ok(naming::document_metadata_from_text(&text))
    } else if kind == "audio" {
        let m = crate::pipeline::audio_meta::extract_structured(path);
        let name = naming::build_audio_name(m.title.as_deref(), m.artist.as_deref());
        let description = naming::audio_description(&m);
        let mut tags: Vec<String> = Vec::new();
        for value in [&m.artist, &m.album].into_iter().flatten() {
            naming::push_unique(&mut tags, value);
        }
        Ok((description, name, tags))
    } else {
        let (objects, materials) = naming::parse_obj_names(path);
        let mut tags: Vec<String> = Vec::new();
        for value in objects.iter().chain(materials.iter()) {
            naming::push_unique(&mut tags, value);
        }
        tags.truncate(6);
        Ok((naming::obj_description(&objects, &materials), naming::build_obj_name(&objects, &materials), tags))
    }
}

/// Transcribe the first 30 seconds of an untitled recording with the installed
/// speech model and derive a name from what was said. Best-effort: any failure
/// keeps the metadata result.
async fn transcribe_audio_name(
    path: &std::path::Path,
    cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Option<(String, String)> {
    let speech = crate::commands::timeline::installed_speech_model()?;
    let (source, flag) = (path.to_path_buf(), cancel.clone());
    let audio = tokio::task::spawn_blocking(move || crate::commands::video_export::audio_wav(&source, 0.0, 30.0, &flag))
        .await
        .ok()?
        .ok()?;
    let wav = std::env::temp_dir().join(format!("fileid-name-{}.wav", uuid::Uuid::new_v4()));
    std::fs::write(&wav, audio).ok()?;
    let reply = match crate::models::resident_service::ResidentService::shared().acquire(speech, cancel.clone()).await {
        Ok(lease) => lease
            .complete_with_cancel(&wav, crate::pipeline::timeline_analysis::SPEECH_PROMPT, 200, cancel.clone())
            .await
            .ok(),
        Err(error) => {
            tracing::debug!(?error, "speech model unavailable for audio naming");
            None
        }
    };
    let _ = std::fs::remove_file(&wav);
    naming::name_from_transcript(&crate::pipeline::timeline_analysis::transcript_text(&reply?))
}

/// Pulls a 25%-duration keyframe from a video into a temp JPEG via the Media Foundation helper. The caller wraps the path in a `TempFileGuard`.
async fn rasterize_video_keyframe(path: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    let p = path.to_path_buf();
    // If Media Foundation can read the container but seeking fails (some VFR/fragmented MP4s have unreliable duration), the helper falls back to offset 0. We retry once on top-level errors to rescue transient I/O on USB drives and network shares.
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

#[cfg(feature = "pdf-analyze")]
async fn rasterize_pdf_page(path: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    use pdfium_render::prelude::*;
    let p = path.to_path_buf();
    tokio::task::spawn_blocking(move || -> anyhow::Result<std::path::PathBuf> {
        // Pdfium::default() panics on a missing pdfium.dll. Bind explicitly so
        // the caller gets a per-file Err instead of an engine crash.
        let bindings = Pdfium::bind_to_system_library()
            .map_err(|e| anyhow::anyhow!("pdfium bind: {e}"))?;
        let pdfium = Pdfium::new(bindings);
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

/// Deterministically prefixes the named people onto the VLM proposed filename so they ALWAYS land (the model treats the prompt hint as optional). Each person's first-name token (lowercase ASCII-alphanumeric after folding Latin-1 and Latin Extended-A accents, ≥2 chars, deduped against the name and each other, capped at 3 sorted alphabetically) is prefixed, then the whole name is re-sanitized.
pub(crate) fn apply_person_prefix(name: &str, person_names: &[String]) -> String {
    if name.is_empty() {
        return name.to_string();
    }
    let existing: std::collections::HashSet<String> = fold_prefix_text(&name.to_lowercase(), true)
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let mut tokens: Vec<String> = Vec::new();
    for display in person_names {
        let first_word = display.split_whitespace().next().unwrap_or("");
        let token = fold_prefix_text(&first_word.to_lowercase(), false);
        if token.chars().count() < 2 || existing.contains(&token) || tokens.contains(&token) {
            continue;
        }
        tokens.push(token);
    }
    if tokens.is_empty() {
        return name.to_string();
    }
    tokens.sort();
    tokens.truncate(3);
    sanitize_proposed_name(&format!("{} {}", tokens.join(" "), name))
}

fn fold_prefix_text(lowered: &str, other_is_separator: bool) -> String {
    let mut out = String::with_capacity(lowered.len());
    for c in lowered.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if let Some(folded) = fold_latin_letter(c) {
            out.push_str(folded);
        } else if ('\u{300}'..='\u{36f}').contains(&c) {
        } else if other_is_separator {
            out.push(' ');
        }
    }
    out
}

fn fold_latin_letter(c: char) -> Option<&'static str> {
    Some(match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => "c",
        'ð' | 'ď' | 'đ' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => "e",
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => "g",
        'ĥ' | 'ħ' => "h",
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => "i",
        'ĵ' => "j",
        'ķ' => "k",
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => "l",
        'ñ' | 'ń' | 'ņ' | 'ň' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => "o",
        'ŕ' | 'ŗ' | 'ř' => "r",
        'ś' | 'ŝ' | 'ş' | 'š' => "s",
        'ţ' | 'ť' | 'ŧ' => "t",
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => "u",
        'ŵ' => "w",
        'ý' | 'ÿ' | 'ŷ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        'æ' => "ae",
        'œ' => "oe",
        'ß' => "ss",
        'þ' => "th",
        _ => return None,
    })
}

pub(crate) fn sanitize_proposed_name(raw: &str) -> String {
    let trimmed = raw.trim().trim_matches('"').trim_matches('\'').trim();
    let lowered = trimmed.to_lowercase();
    let cleaned: String = lowered
        .chars()
        .map(|c| match c {
            c if c.is_ascii_alphanumeric() => c,
            '-' | '_' => c,
            c if c.is_whitespace() => '-',
            _ => ' ',
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

/// Generic, low-information tokens VLMs emit despite the prompt. A tag is dropped if any word is one of these: they describe the medium, not the content, and read as noise as a Library chip.
pub(crate) const VLM_TAG_STOPWORDS: &[&str] = &[
    "photo", "photos", "image", "images", "picture", "pictures", "object",
    "objects", "thing", "things", "scene", "background", "foreground",
    "location", "text", "item", "items", "stuff", "view", "misc", "unknown",
    "none",
];

/// Parses a VLM tag completion ("dog, Beach.") into clean, deduplicated, lowercase tags. Tolerates numbering, bullets, trailing punctuation, quotes and the model returning a sentence (pieces over 2 words are dropped). `VLM_TAG_STOPWORDS` are filtered and the result is capped at `MAX_VLM_TAGS` so the Library shows 1-2 descriptive tags.
pub(crate) fn parse_vlm_tags(raw: &str) -> Vec<String> {
    const MAX_VLM_TAGS: usize = 2;
    let mut out: Vec<String> = Vec::new();
    for piece in raw.split([',', '\n', ';']) {
        let lowered = piece.trim().to_lowercase();
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
        if stripped.split_whitespace().count() > 2 {
            continue;
        }
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
    fn a_person_name_with_accents_is_prefixed_folded_not_dropped() {
        let people = |names: &[&str]| names.iter().map(|n| (*n).to_string()).collect::<Vec<_>>();
        assert_eq!(apply_person_prefix("beach-sunset", &people(&["José García"])), "jose-beach-sunset");
        assert_eq!(apply_person_prefix("beach-sunset", &people(&["Zoe\u{308} Lund"])), "zoe-beach-sunset");
        assert_eq!(apply_person_prefix("beach-sunset", &people(&["Łukasz", "Søren", "Ærin"])), "aerin-lukasz-soren-beach-sunset");
        assert_eq!(apply_person_prefix("jose-at-beach", &people(&["José"])), "jose-at-beach");
        assert_eq!(apply_person_prefix("beach", &people(&["Müller", "Muller"])), "muller-beach");
        assert_eq!(apply_person_prefix("beach", &people(&["東京"])), "beach");
    }

    #[test]
    fn an_online_only_file_is_refused_before_it_is_opened() {
        let path = std::path::Path::new("cloud.jpg");
        let error = ensure_available_offline(path, true).unwrap_err().to_string();
        assert!(error.contains("online-only"), "{error}");
        assert!(ensure_available_offline(path, false).is_ok());
    }

    struct Source {
        conn: rusqlite::Connection,
        dir: std::path::PathBuf,
        path: std::path::PathBuf,
        revision: String,
    }

    impl Drop for Source {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn source(tag: &str) -> Source {
        let dir = std::env::temp_dir().join(format!("fileid-deep-naming-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("IMG_001.jpg");
        std::fs::write(&path, [1u8]).unwrap();
        let metadata = std::fs::metadata(&path).unwrap();
        let modified = crate::util::catalog_unix_seconds(metadata.modified().unwrap()).unwrap();
        let revision = super::super::naming_context::source_revision(1, Some(modified));
        let conn = crate::db::test_support::memory_db();
        conn.execute(
            "INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,kind,extension,scanned_at) VALUES(1,?1,1,1,?2,'image','jpg',1)",
            rusqlite::params![path.to_string_lossy(), modified],
        )
        .unwrap();
        Source { conn, dir, path, revision }
    }

    fn reviewed_chapter(source: &Source, title: &str) {
        source
            .conn
            .execute(
                "INSERT INTO catalog_chapters(id,file_id,start_seconds,end_seconds,title,source_revision,model_version,confidence,user_edited,stale) VALUES('c',1,0,1,?1,?2,'user',1,1,0)",
                rusqlite::params![title, source.revision],
            )
            .unwrap();
    }

    fn stored_name(source: &Source) -> Option<String> {
        source.conn.query_row("SELECT vlm_proposed_name FROM files WHERE id=1", [], |r| r.get(0)).unwrap()
    }

    fn context(source: &Source) -> NamingContext {
        super::super::naming_context::load(&source.conn, 1, None, "p").unwrap()
    }

    #[test]
    fn a_reviewed_label_names_the_file_instead_of_the_model_proposal() {
        let source = source("label");
        reviewed_chapter(&source, "Maya hits a home run");
        let context = context(&source);
        let written =
            persist_vlm_results(&source.conn, 1, "m", AnalyzeMode::Both, Some("caption"), Some("Sunset Over Bay"), &[], &context).unwrap();
        assert_eq!(written, context.proposed_name());
        assert_eq!(stored_name(&source), context.proposed_name());
        assert!(stored_name(&source).unwrap().to_lowercase().contains("home"));
    }

    #[test]
    fn without_a_reviewed_label_the_model_proposal_is_kept() {
        let source = source("no-label");
        let context = context(&source);
        persist_vlm_results(&source.conn, 1, "m", AnalyzeMode::Both, Some("caption"), Some("Sunset Over Bay"), &[], &context).unwrap();
        assert_eq!(stored_name(&source).as_deref(), Some("Sunset Over Bay"));
    }

    #[test]
    fn a_reviewed_label_does_not_rename_when_renames_were_not_requested() {
        let source = source("no-rename");
        reviewed_chapter(&source, "Maya hits a home run");
        let context = context(&source);
        persist_vlm_results(&source.conn, 1, "m", AnalyzeMode::CaptionAndTags, Some("caption"), None, &[], &context).unwrap();
        assert_eq!(stored_name(&source), None);
    }

    #[test]
    fn evidence_that_changed_before_commit_writes_nothing() {
        for fault in ["label", "source"] {
            let source = source(&format!("late-{fault}"));
            reviewed_chapter(&source, "Maya hits a home run");
            let context = context(&source);
            if fault == "label" {
                source.conn.execute("UPDATE catalog_chapters SET title='Birthday gift'", []).unwrap();
            } else {
                std::fs::write(&source.path, [1u8, 2]).unwrap();
            }
            let error = persist_vlm_results(
                &source.conn, 1, "m", AnalyzeMode::Both, Some("stale result"), context.proposed_name().as_deref(), &["tag".into()], &context,
            )
            .unwrap_err();
            assert!(error.is::<super::super::naming_context::EvidenceChanged>(), "{fault}");
            assert_eq!(stored_name(&source), None, "{fault}");
            let description: Option<String> =
                source.conn.query_row("SELECT vlm_description FROM files WHERE id=1", [], |r| r.get(0)).unwrap();
            assert_eq!(description, None, "{fault}");
            let tags: i64 = source.conn.query_row("SELECT COUNT(*) FROM tags", [], |r| r.get(0)).unwrap();
            assert_eq!(tags, 0, "{fault}");
        }
    }

    type Stored = (Option<String>, Option<String>, Option<String>, Option<String>, Vec<(String, String)>);

    fn stored(source: &Source) -> Stored {
        let (description, name, model, full) = source
            .conn
            .query_row("SELECT vlm_description,vlm_proposed_name,vlm_model,vlm_full_model FROM files WHERE id=1", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .unwrap();
        let mut statement = source.conn.prepare("SELECT source,tag FROM tags WHERE file_id=1 ORDER BY source,tag").unwrap();
        let tags = statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        (description, name, model, full, tags)
    }

    fn some(value: &str) -> Option<String> {
        Some(value.to_owned())
    }

    fn tags(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(source, tag)| ((*source).to_owned(), (*tag).to_owned())).collect()
    }

    #[test]
    fn a_full_pass_replaces_old_model_output_even_when_the_new_output_is_empty_and_keeps_user_tags() {
        let source = source("full-clears");
        source.conn.execute("INSERT INTO tags(file_id,tag,source,score) VALUES(1,'grandma','user',NULL),(1,'beach','ram',0.9)", []).unwrap();
        let context = context(&source);
        persist_vlm_results(&source.conn, 1, "model-a", AnalyzeMode::Both, Some("old desc"), Some("old-name"), &["old-tag".into()], &context).unwrap();
        assert_eq!(
            stored(&source),
            (some("old desc"), some("old-name"), some("model-a"), some("model-a"), tags(&[("ram", "beach"), ("user", "grandma"), ("vlm", "old-tag")]))
        );
        persist_vlm_results(&source.conn, 1, "model-b", AnalyzeMode::Both, Some(""), None, &[" ".into()], &context).unwrap();
        assert_eq!(
            stored(&source),
            (None, None, some("model-b"), some("model-b"), tags(&[("ram", "beach"), ("user", "grandma")])),
            "nothing from model A is left under model B's name; tags the user or the scan gave stay"
        );
    }

    #[test]
    fn a_pass_changes_only_the_outputs_it_was_asked_for() {
        let source = source("partial");
        let context = context(&source);
        persist_vlm_results(&source.conn, 1, "model-a", AnalyzeMode::Both, Some("old desc"), Some("keep-name"), &["old-tag".into()], &context).unwrap();
        persist_vlm_results(&source.conn, 1, "model-a", AnalyzeMode::CaptionAndTags, Some(""), None, &[], &context).unwrap();
        assert_eq!(stored(&source), (None, some("keep-name"), some("model-a"), some("model-a"), vec![]), "renames were not asked for");
        persist_vlm_results(&source.conn, 1, "model-a", AnalyzeMode::Both, Some("desc"), Some("name"), &["tag".into()], &context).unwrap();
        persist_vlm_results(&source.conn, 1, "model-a", AnalyzeMode::TagsOnly, Some("ignored"), Some("ignored"), &[], &context).unwrap();
        assert_eq!(
            stored(&source),
            (some("desc"), some("name"), some("model-a"), some("model-a"), vec![]),
            "a tags pass of the same model keeps the caption, name and completion and replaces only the tags"
        );
        persist_vlm_results(&source.conn, 1, "model-b", AnalyzeMode::TagsOnly, None, None, &["new".into()], &context).unwrap();
        assert_eq!(
            stored(&source),
            (some("desc"), some("name"), None, None, tags(&[("vlm", "new")])),
            "a tags pass of another model does not claim the full pass"
        );
    }

    #[test]
    fn a_reviewed_label_survives_a_full_pass_whose_model_proposed_nothing() {
        let source = source("label-kept");
        reviewed_chapter(&source, "Maya hits a home run");
        let context = context(&source);
        persist_vlm_results(&source.conn, 1, "m", AnalyzeMode::Both, Some(""), None, &[], &context).unwrap();
        assert_eq!(stored_name(&source), context.proposed_name());
        assert!(stored_name(&source).is_some());
    }

    struct Recorder<'a> {
        sent: &'a parking_lot::Mutex<Vec<(String, u32)>>,
        reply: &'a str,
    }

    impl VisualAsker for Recorder<'_> {
        fn ask(&self, prompt: &str, max_tokens: u32) -> impl std::future::Future<Output = anyhow::Result<String>> + Send {
            self.sent.lock().push((prompt.to_owned(), max_tokens));
            std::future::ready(Ok(self.reply.to_owned()))
        }
    }

    async fn requests_for(mode: AnalyzeMode, context: &NamingContext, reply: &str) -> (Vec<(String, u32)>, Option<String>) {
        let sent = parking_lot::Mutex::new(Vec::new());
        let (_, _, name) =
            visual_answers(mode.asked_of_model(context), &context.subjects, &Recorder { sent: &sent, reply }).await.unwrap();
        (sent.into_inner(), name)
    }

    const MODEL_REPLY: &str = "DESCRIPTION: A boy swings a bat.\nTAGS: boy, bat\nFILENAME: boy-swinging-bat";

    #[tokio::test]
    async fn a_reviewed_label_skips_the_model_name_request() {
        let source = source("skip");
        reviewed_chapter(&source, "Maya hits a home run");
        let with_label = context(&source);
        let (sent, name) = requests_for(AnalyzeMode::Both, &with_label, MODEL_REPLY).await;
        assert_eq!(sent.len(), 1);
        assert!(!sent[0].0.contains("FILENAME"), "the one request must not ask for a name");
        assert_eq!(sent[0].1, 110);
        assert_eq!(name, None);
        assert_eq!(AnalyzeMode::Both.asked_of_model(&with_label), AnalyzeMode::CaptionAndTags);
        assert_eq!(AnalyzeMode::TagsOnly.asked_of_model(&with_label), AnalyzeMode::TagsOnly);
    }

    #[tokio::test]
    async fn a_reviewed_label_sends_no_fallback_name_prompt_after_a_reply_without_a_name() {
        let source = source("skip-fallback");
        reviewed_chapter(&source, "Maya hits a home run");
        let (sent, name) = requests_for(AnalyzeMode::Both, &context(&source), "DESCRIPTION: A boy.\nTAGS: boy").await;
        assert_eq!(sent.len(), 1);
        assert_eq!(name, None);
    }

    #[tokio::test]
    async fn without_a_reviewed_label_the_model_is_asked_for_a_name() {
        let source = source("ask");
        let none = context(&source);
        let (sent, name) = requests_for(AnalyzeMode::Both, &none, MODEL_REPLY).await;
        assert_eq!(sent.len(), 1);
        assert!(sent[0].0.contains("FILENAME") && sent[0].1 == 140);
        assert_eq!(name.as_deref(), Some("boy-swinging-bat"));
        assert_eq!(AnalyzeMode::Both.asked_of_model(&none), AnalyzeMode::Both);
    }

    #[test]
    fn a_reviewed_label_leaves_the_name_out_of_a_document_request() {
        let source = source("document");
        reviewed_chapter(&source, "Maya hits a home run");
        let asked = AnalyzeMode::Both.asked_of_model(&context(&source));
        assert!(!naming::document_analysis_prompt("txt", "hello", asked).unwrap().contains("FILENAME: A 3-5"));
        assert!(naming::document_analysis_prompt("txt", "hello", AnalyzeMode::Both).unwrap().contains("FILENAME: A 3-5"));
    }

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
        // Capped at 2; still lowercases and strips the trailing period.
        assert_eq!(parse_vlm_tags("Dog, beach."), vec!["dog", "beach"]);
    }

    #[test]
    fn parse_vlm_tags_strips_numbering_and_dedupes() {
        assert_eq!(parse_vlm_tags("1. dog\n2. dog\n3. ocean"), vec!["dog", "ocean"]);
    }

    #[test]
    fn parse_vlm_tags_drops_sentence_fragments_keeps_short() {
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
        assert_eq!(
            parse_vlm_tags("photo, golden retriever, object"),
            vec!["golden retriever"]
        );
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
