// Deep Analyze — VLM-powered captioning + smart-rename.
//
// Pipeline:
//   1. Resolve selected registry model (Qwen3-VL, Qwen2.5-VL, Gemma 3, Mistral-Small).
//   2. Load via llama.cpp (Vulkan / CUDA / DirectML / CPU backend by EP).
//   3. Per file: render the image / extract a video keyframe / pdfium
//      first-page render → resize to model context → caption + smart name.
//   4. Persist to `deep_analyze_results` (migration v3).
//   5. Emit `deepAnalyzeProgress` IPC events on every N files.

use anyhow::Context;

use super::deep_analyze_naming as naming;

/// Per-file Deep Analyze outcome — whatever the engine writes back to
/// the DB after a successful caption + smart-rename round-trip.
#[derive(Debug, Clone, Default)]
pub struct AnalyzeOutcome {
    pub file_id: i64,
    pub description: Option<String>,
    pub proposed_name: Option<String>,
}

/// Which VLM passes run for a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalyzeMode {
    /// Tags only — the fast path for background auto-tagging. One VLM call per
    /// file (tags) instead of three (caption + tags + rename), so a whole-library
    /// pass is ~3× faster. Caption + proposed-name columns are left untouched.
    TagsOnly,
    Both,
    /// Caption + tags, but NO smart-rename — the full manual pass with the
    /// "Propose renames" checkbox unticked. Same VLM calls as Both minus the
    /// rename call; the proposed-name column is left untouched.
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

    fn establishes_completion(self) -> bool {
        matches!(self, Self::Both | Self::CaptionAndTags)
    }
}

/// Run Deep Analyze on a single file: pull image bytes (image, video
/// keyframe, or PDF page-1 via shell helpers) → call the VLM via the
/// subprocess wrapper → write results back to the DB. Cancellation
/// honored via the shared `AtomicBool`.
/// Removes the temp rasterized frame on EVERY exit path (`?` error, cancel
/// bail, success), so VLM error paths no longer leak temp files (#24). Mirrors
/// the discovery.rs `TempDir` Drop pattern.
struct TempFileGuard(Option<std::path::PathBuf>);
impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = std::fs::remove_file(&p);
        }
    }
}

pub async fn analyze_file(
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    runner: &crate::models::vlm::VlmRunner,
    file_id: i64,
    model_kind: &str,
    mode: AnalyzeMode,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    face_names: &[String],
    on_token: impl FnMut(&str),
) -> anyhow::Result<AnalyzeOutcome> {
    analyze_file_scoped(db, runner, file_id, model_kind, mode, cancel, face_names, on_token, None).await
}

pub(crate) async fn analyze_file_scoped(
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    runner: &crate::models::vlm::VlmRunner,
    file_id: i64,
    model_kind: &str,
    mode: AnalyzeMode,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    face_names: &[String],
    mut on_token: impl FnMut(&str),
    scope: Option<&crate::commands::deep_scope::DeepScope>,
) -> anyhow::Result<AnalyzeOutcome> {
    use crate::models::vlm::{self, CaptionRequest};

    // Documents, audio and (without installed weights) 3D models are named
    // from their own text and metadata, so they work with no VLM at all.
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        anyhow::bail!("cancelled");
    }
    let weights_installed = naming::vlm_weights_installed(model_kind);
    if let Some(outcome) =
        analyze_metadata_named_file(&db, file_id, model_kind, mode, weights_installed, &cancel, scope).await?
    {
        return Ok(outcome);
    }

    let verified = runner.verified_weights(model_kind, cancel.clone()).await?;
    let (gguf, mmproj) = verified.paths();
    let (gguf, mmproj) = (gguf.to_owned(), mmproj.to_owned());

    // Resolve + rasterize the source (image; video keyframe; PDF page 1; 3D render).
    let (rasterized, temp_to_clean) = match rasterize_for_vlm(&db, file_id, scope).await {
        Ok(r) => r,
        Err(e) => {
            if let Some(outcome) =
                analyze_metadata_named_file(&db, file_id, model_kind, mode, false, &cancel, scope).await?
            {
                return Ok(outcome);
            }
            return Err(e);
        }
    };
    // Guard cleans the temp frame on any exit, including the `?`/bail paths
    // below that previously leaked it (#24).
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
    // VLM scene/content tags (source='vlm'). Generated in "Both" (full
    // enrichment) and "TagsOnly" (the fast background auto-tag pass), so a Deep
    // Analyze run over the library produces the chip tags that REPLACE CLIP
    // zero-shot if the user drops CLIP. Clones the weights + rasterized frame so
    // the rename branch below can still take ownership.
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
    if mode == AnalyzeMode::Both {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }
        let req = CaptionRequest {
            gguf_path: gguf,
            mmproj_path: mmproj,
            image_path: rasterized,
            prompt: vlm::rename_prompt_with_faces(face_names),
            max_tokens: 30,
            greedy: true,
        };
        let result = vlm::caption(runner, &req, cancel.clone(), |_| {}).await?;
        proposed_name = Some(apply_person_prefix(&sanitize_proposed_name(&result.text), face_names));
    }

    // Persist caption + proposed name (v3 `files` columns) + VLM tags.
    {
        proposed_name = proposed_name.and_then(|name| crate::util::smart_name::readable_stem(&name, face_names));
        let conn = db.lock();
        persist_vlm_results(
            &conn,
            file_id,
            model_kind,
            mode,
            description.as_deref(),
            proposed_name.as_deref(),
            &tags,
        )?;
    }

    // (temp frame removed by `_temp_guard` on drop — #24)

    Ok(AnalyzeOutcome {
        file_id,
        description,
        proposed_name,
    })
}

/// Resolve a file's on-disk path and rasterize it to an image the VLM can read:
/// images pass through; video → 25%-duration keyframe; PDF → page-1 render.
/// Returns the image path + an optional temp path the caller must clean up.
/// Shared by the per-file CLI (`analyze_file`) and the persistent-server path.
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
    if !source_path.exists() {
        anyhow::bail!("source file missing: {}", source_path.display());
    }
    match kind.as_str() {
        "image" => {
            // Llama.cpp's image loader (stb_image) reads JPEG/PNG natively
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
                let checked = source_path.clone();
                let (width, height) = tokio::task::spawn_blocking(move || {
                    naming::validate_vlm_image_dimensions(&checked)
                })
                .await??;
                // Native-resolution 24-48 MP photos multiply the VLM's prefill
                // and mtmd preprocessing cost several-fold with no caption/tag
                // gain — downscale oversized inputs via the transcode path.
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
            // 3D model → a shaded 3/4-view PNG the VLM can recognize.
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

/// Decode an arbitrary image (webp/bmp/tiff/gif/…), downscale anything
/// over [`naming::MAX_VLM_INPUT_EDGE`], and re-encode as a temp JPEG the VLM's
/// stb_image-based loader can read. Caller cleans up the temp file. Runs on a
/// blocking thread (image-rs decode/encode is CPU-bound).
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
    mode: AnalyzeMode,
    description: Option<&str>,
    proposed_name: Option<&str>,
    tags: &[String],
) -> anyhow::Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    // Single transaction so the caption/name UPDATE and the vlm-tag
    // DELETE+INSERT-replace commit atomically — a crash between the DELETE and
    // the INSERT loop must not drop a file's VLM tags (#23). `unchecked_`
    // because the callers hold `conn` behind a parking_lot::Mutex and pass &ref.
    let tx = conn.unchecked_transaction()?;
    if mode.establishes_completion() {
        tx.execute(
            "UPDATE files SET vlm_description=COALESCE(?2, vlm_description), \
                              vlm_proposed_name=COALESCE(?3, vlm_proposed_name), \
                              vlm_model=?1, vlm_full_model=?1, vlm_analyzed_at=?4, vlm_inherited_from=NULL WHERE id=?5",
            rusqlite::params![model_kind, description, proposed_name, now, file_id],
        )?;
    } else {
        let completed_with_model = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM files WHERE id=?1 AND vlm_full_model=?2)",
            rusqlite::params![file_id, model_kind],
            |row| row.get::<_, bool>(0),
        )?;
        if completed_with_model {
            tx.execute(
                "UPDATE files SET vlm_description=COALESCE(?1, vlm_description), \
                                  vlm_proposed_name=COALESCE(?2, vlm_proposed_name), vlm_inherited_from=NULL \
                 WHERE id=?3",
                rusqlite::params![description, proposed_name, file_id],
            )?;
        } else {
            tx.execute(
                "UPDATE files SET vlm_description=COALESCE(?1, vlm_description), \
                                  vlm_proposed_name=COALESCE(?2, vlm_proposed_name), \
                                  vlm_model=NULL, vlm_full_model=NULL, vlm_analyzed_at=NULL, vlm_inherited_from=NULL \
                 WHERE id=?3",
                rusqlite::params![description, proposed_name, file_id],
            )?;
        }
    }
    if !tags.is_empty() {
        tx.execute(
            "DELETE FROM tags WHERE file_id=?1 AND source='vlm'",
            rusqlite::params![file_id],
        )?;
        let mut stmt = tx.prepare(
            "INSERT OR IGNORE INTO tags (file_id, tag, source, score) VALUES (?1, ?2, 'vlm', NULL)",
        )?;
        for t in tags {
            stmt.execute(rusqlite::params![file_id, t])?;
        }
    }
    tx.commit()?;
    Ok(())
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
    persist_vlm_results(conn, file_id, model_kind, mode, description.as_deref(), proposed_name.as_deref(), &tags)?;
    conn.execute("UPDATE files SET vlm_inherited_from=?1 WHERE id=?2", rusqlite::params![from_id, file_id])?;
    Ok(AnalyzeOutcome { file_id, description, proposed_name })
}

/// one-shot probe that the persistent llama-server actually accepts our
/// multimodal `image_url` data-URI payload shape. This payload format was never
/// hardware-verified; if the server build rejects it (e.g.
/// 400 on the request), EVERY file in the batch would fail identically and
/// silently. Sending one tiny throwaway JPEG up front lets the batch detect the
/// incompatibility and fall back to the per-file CLI path (a different,
/// known-good code path) instead of producing zero tags.
pub(crate) async fn vlm_server_payload_ok(
    server: &crate::models::vlm_server::VlmServer,
    cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>,
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
    let result = server.complete_with_cancel(&test_img, "Reply with: ok", 1, cancel.clone()).await;
    let _ = std::fs::remove_file(&test_img);
    result.map(|_| ())
}

async fn complete_cancellable(
    server: &crate::models::vlm_server::VlmServer,
    image: &std::path::Path, prompt: &str, max_tokens: u32,
    cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> anyhow::Result<String> {
    server.complete_with_cancel(image, prompt, max_tokens, cancel.clone()).await
}

/// Analyze one file through the PERSISTENT llama-server (model already loaded),
/// with NO per-call model reload. Documents are named from their text, audio
/// from tags or a speech transcript, 3D models from a render (or their embedded
/// names when it fails); everything else takes the visual path. `Both` and
/// `CaptionAndTags` ask for caption, tags and filename in ONE request; the
/// separate prompts are fallbacks for answers that miss a section. Results go
/// to `on_token` in one shot (these server calls are non-streaming).
pub(crate) async fn analyze_file_via_server(
    db: std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    server: &crate::models::vlm_server::VlmServer,
    file_id: i64,
    model_kind: &str,
    mode: AnalyzeMode,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    face_names: &[String],
    mut on_token: impl FnMut(&str),
    scope: Option<&crate::commands::deep_scope::DeepScope>,
) -> anyhow::Result<AnalyzeOutcome> {
    use crate::models::vlm;
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        anyhow::bail!("cancelled");
    }
    if let Some(outcome) =
        analyze_document_via_server(&db, server, file_id, model_kind, mode, &cancel, &mut on_token, scope).await?
    {
        return Ok(outcome);
    }
    if let Some(outcome) = analyze_metadata_named_file(&db, file_id, model_kind, mode, true, &cancel, scope).await? {
        return Ok(outcome);
    }
    let (rasterized, temp_to_clean) = match rasterize_for_vlm(&db, file_id, scope).await {
        Ok(r) => r,
        Err(e) => {
            // A 3D model we couldn't render falls back to its embedded names.
            if let Some(outcome) =
                analyze_metadata_named_file(&db, file_id, model_kind, mode, false, &cancel, scope).await?
            {
                return Ok(outcome);
            }
            return Err(e);
        }
    };
    // Guard cleans the temp frame on any exit, including the cancel-bail/`?`
    // paths below that previously leaked it (#24).
    let _temp_guard = TempFileGuard(temp_to_clean);

    let mut description: Option<String> = None;
    let mut proposed_name: Option<String> = None;
    let mut tags: Vec<String> = Vec::new();

    if mode.requests_caption() {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }
        let include_filename = mode.requests_rename();
        let prompt = naming::combined_visual_prompt(face_names, include_filename);
        let raw = complete_cancellable(server, &rasterized, &prompt, if include_filename { 140 } else { 110 }, &cancel)
            .await?;
        (description, tags, proposed_name) = naming::parse_combined_visual_completion(&raw, include_filename);
        if description.is_none() {
            let cap_prompt = vlm::caption_prompt_with_faces(face_names);
            description = Some(complete_cancellable(server, &rasterized, &cap_prompt, 80, &cancel).await?);
        }
        if tags.is_empty() {
            tags = naming::grounded_fallback_tags(description.as_deref().unwrap_or(&raw));
        }
        if include_filename && proposed_name.is_none() {
            proposed_name = description.as_deref().and_then(crate::util::keywords::grounded_filename);
        }
        if include_filename && proposed_name.is_none() {
            let ren_prompt = vlm::rename_prompt_with_faces(face_names);
            proposed_name = naming::generated_filename_candidate(
                &complete_cancellable(server, &rasterized, &ren_prompt, 30, &cancel).await?,
            );
            if proposed_name.is_none() {
                proposed_name = naming::generated_filename_candidate(
                    &complete_cancellable(server, &rasterized, vlm::RENAME_RETRY_PROMPT, 30, &cancel).await?,
                );
            }
        }
        if let Some(value) = description.as_deref() {
            on_token(value);
        }
        if !tags.is_empty() {
            on_token(&tags.join(", "));
        }
    } else if mode.requests_tags() {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }
        let raw = complete_cancellable(server, &rasterized, vlm::TAG_PROMPT, 40, &cancel).await?;
        tags = parse_vlm_tags(&raw);
        if tags.is_empty() {
            tags = naming::grounded_fallback_tags(&raw);
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
        persist_vlm_results(&conn, file_id, model_kind, mode, description.as_deref(), proposed_name.as_deref(), &tags)?;
    }
    // (temp frame removed by `_temp_guard` on drop — #24)

    Ok(AnalyzeOutcome {
        file_id,
        description,
        proposed_name,
    })
}

async fn complete_text_cancellable(
    server: &crate::models::vlm_server::VlmServer,
    prompt: &str,
    max_tokens: u32,
    cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> anyhow::Result<String> {
    server.complete_text_with_cancel(prompt, max_tokens, cancel.clone()).await
}

/// File path and kind for `file_id`, resolved through the Deep Analyze scope
/// when one applies.
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
    Ok((path, kind))
}

/// Name a document from its text with a text-only request. Returns None for
/// every other kind so the caller continues with metadata or visual analysis.
#[allow(clippy::too_many_arguments)]
async fn analyze_document_via_server(
    db: &std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>,
    server: &crate::models::vlm_server::VlmServer,
    file_id: i64,
    model_kind: &str,
    mode: AnalyzeMode,
    cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    on_token: &mut impl FnMut(&str),
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
        return analyze_metadata_named_file(db, file_id, model_kind, mode, false, cancel, scope).await;
    };
    let (_, fallback_name, deterministic_tags) = naming::document_metadata_from_text(&text);
    let extension = path.extension().and_then(|value| value.to_str()).unwrap_or_default();
    let mut description = None;
    let mut proposed_name = None;
    if mode.requests_caption() || mode.requests_rename() {
        let prompt = naming::document_analysis_prompt(extension, &text, mode)?;
        let max_tokens = match (mode.requests_caption(), mode.requests_rename()) {
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
        if mode.requests_rename() {
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
        persist_vlm_results(&conn, file_id, model_kind, mode, description.as_deref(), proposed_name.as_deref(), &tags)?;
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
    cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>,
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
    if kind == "audio" && proposed_name.is_none() && mode.requests_rename() {
        if let Some((name, transcript)) = transcribe_audio_name(&path, cancel).await {
            proposed_name = Some(name);
            description = description.or(Some(transcript));
        }
    }
    let description = if mode.requests_caption() { description } else { None };
    let proposed_name = if mode.requests_rename() { proposed_name } else { None };
    let tags = if mode.requests_tags() { tags } else { Vec::new() };
    let proposed_name = {
        let conn = db.lock();
        let proposed_name = naming::vetted_proposed_name(&conn, file_id, proposed_name)?;
        persist_vlm_results(&conn, file_id, model_kind, mode, description.as_deref(), proposed_name.as_deref(), &tags)?;
        proposed_name
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

/// Pull a 25%-duration keyframe from a video into a temp JPEG via the
/// existing Media Foundation helper, return the temp path. The caller wraps
/// the returned path in a `TempFileGuard` so it is removed on every exit path.
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

/// Clean up a VLM-proposed filename: lowercase, hyphen-separated, strip
/// quotes / extension / extra punctuation. The model usually obeys the
/// prompt but defensive normalization saves a round-trip.
/// Deterministically prefix the named people onto the VLM proposed filename so
/// they ALWAYS land (the model treats the prompt hint as optional, so names often
/// never reach the FILENAME). Each person's first-name token — lowercase
/// ASCII-alphanumeric, ≥2 chars, deduped against words already in the name and
/// against each other, capped at 3 sorted alphabetically — is prefixed, then the
/// whole thing is re-sanitized. Byte-faithful with the Swift engine's
/// `applyPersonPrefix`. (item 3)
pub(crate) fn apply_person_prefix(name: &str, person_names: &[String]) -> String {
    if name.is_empty() {
        return name.to_string();
    }
    let existing: std::collections::HashSet<String> = name
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_string())
        .collect();
    let mut tokens: Vec<String> = Vec::new();
    for display in person_names {
        let first_word = display.split_whitespace().next().unwrap_or("");
        let token: String = first_word
            .to_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();
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

/// Generic, low-information tokens VLMs sometimes emit despite the prompt.
/// A tag is dropped if any of its words is one of these — they describe the
/// medium, not the content, and read as noise as a Library chip ("has location"
/// used to be the worst offender; that one is no longer emitted at all).
pub(crate) const VLM_TAG_STOPWORDS: &[&str] = &[
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
