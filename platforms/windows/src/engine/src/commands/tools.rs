use crate::ipc::sink::Sink;
use crate::ipc::{
    CatalogChapter, EventPayload, IpcEvent, ToolCapability, ToolOutput, ToolRecipe, ToolRequest, ToolResponse, Wrap,
};
use super::media_recipes;
use crate::util::{path_safety::stable_path_hash, read_only};
use anyhow::{bail, Context, Result};
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, OnceLock,
    },
};

static ACTIVE_EXPORTS: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();

fn active_exports() -> &'static Mutex<HashMap<String, Arc<AtomicBool>>> {
    ACTIVE_EXPORTS.get_or_init(|| Mutex::new(HashMap::new()))
}

struct ActiveExport(String);

impl ActiveExport {
    fn register(id: &str, cancelled: Arc<AtomicBool>) -> Result<Self> {
        let mut active = active_exports().lock();
        if active.contains_key(id) {
            bail!("This export is already running")
        }
        active.insert(id.to_owned(), cancelled);
        Ok(Self(id.to_owned()))
    }
}

impl Drop for ActiveExport {
    fn drop(&mut self) {
        active_exports().lock().remove(&self.0);
    }
}

fn cancel_active(request: &ToolRequest) -> Result<ToolResponse> {
    if request.request_id.is_empty() || request.request_id.chars().count() > 200 {
        bail!("Invalid tools request.")
    }
    let id = request.operation_id.as_deref().context("Choose a running export first.")?;
    let active = active_exports().lock();
    let cancelled = active.get(id).context("No running export matches this request.")?;
    cancelled.store(true, Ordering::Release);
    let mut response = response(
        &request.request_id,
        "ok",
        "Stopping the current video worker. Completed exports remain recoverable.",
    );
    response.operation_id = Some(id.to_owned());
    Ok(response)
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Item {
    output: ToolOutput,
    source_hash: String,
    chapters: Vec<CatalogChapter>,
}
#[derive(Serialize, Deserialize)]
struct Plan {
    version: u32,
    r#type: String,
    recipe: ToolRecipe,
    items: Vec<Item>,
    #[serde(rename = "stagePaths", default, skip_serializing_if = "Option::is_none")]
    stage_paths: Option<Vec<String>>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Receipt {
    output: ToolOutput,
    hash: String,
    #[serde(rename = "derivedID", default, skip_serializing_if = "Option::is_none")]
    derived_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recovery_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recovery_published: Option<bool>,
}

fn export_failure_message(error: &anyhow::Error, folder: Option<&str>, exported: usize, total: usize) -> String {
    use crate::util::user_message::{io_cause, IoCause};
    let progress = if exported > 0 {
        format!(
            " {exported} of {} {} exported.",
            crate::util::display::count_noun(total as i64, "file", "files"),
            if exported == 1 { "was" } else { "were" }
        )
    } else {
        String::new()
    };
    let folder = folder.unwrap_or("that folder");
    match io_cause(error) {
        Some(IoCause::DiskFull) => format!("There isn’t enough free space in {folder}. Free up space or choose another folder.{progress}"),
        Some(IoCause::ReadOnly) => format!("{folder} is on a read-only disk, so FileID can’t save there. Choose another folder.{progress}"),
        Some(IoCause::NameTooLong) => format!("A name in this export is too long (the limit is 255 characters). Rename the files and try again.{progress}"),
        None if error.chain().any(|layer| layer.is::<std::io::Error>()) => {
            "Export didn’t finish. Some files may already be saved; press Show last export to check.".to_string()
        }
        None => error.to_string(),
    }
}

pub async fn handle(sink: Sink, database: Option<Arc<Mutex<Connection>>>, request: ToolRequest) {
    let id = request.request_id.clone();
    if request.action == "cancel" {
        let response = match cancel_active(&request) {
            Ok(response) => response,
            Err(error) => response(&id, "error", &error.to_string()),
        };
        sink.send(IpcEvent::now(EventPayload::ToolResponse(Wrap::new(response))))
            .await;
        return;
    }
    let result = tokio::task::spawn_blocking(move || {
        let db = database.context("FileID couldn't open its library. Quit and reopen FileID.")?;
        run_action(&mut Db::Shared(&db), &request, |source, target, published| {
            crate::util::file_move::move_no_clobber(source, target, published)
        })
    })
    .await;
    let response = match result {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => response(&id, "error", &error.to_string()),
        Err(error) => response(&id, "error", &error.to_string()),
    };
    sink.send(IpcEvent::now(EventPayload::ToolResponse(Wrap::new(response))))
        .await;
}
fn response(id: &str, status: &str, message: &str) -> ToolResponse {
    ToolResponse {
        request_id: id.into(),
        status: status.into(),
        message: message.into(),
        operation_id: None,
        outputs: vec![],
        capabilities: vec![],
    }
}
#[cfg(all(target_os = "macos", feature = "apple-adapters"))]
const UNSUPPORTED_RECIPE: &str = "Choose files, an output folder, and a supported recipe.";
#[cfg(not(all(target_os = "macos", feature = "apple-adapters")))]
const UNSUPPORTED_RECIPE: &str = "Unsupported export recipe";

fn preview_message(kind: &str, count: usize) -> String {
    let caveat = match kind {
        "photo" => Some("Photo outputs strip metadata and normalize to 8-bit SDR."),
        "video" => Some("Video outputs are 8-bit SDR and leave out camera and location details."),
        _ => None,
    };
    if cfg!(all(target_os = "macos", feature = "apple-adapters")) {
        let base = format!("Creates {}. Originals are preserved.", crate::util::display::count_noun(count as i64, "new file", "new files"));
        caveat.map_or(base.clone(), |caveat| format!("{base} {caveat}"))
    } else {
        match caveat {
            Some(_) => "Creates new files. Originals preserved; photo and video exports are 8-bit SDR and strip container metadata.".into(),
            None => "Creates new files. Originals are preserved.".into(),
        }
    }
}

fn capabilities() -> Vec<ToolCapability> {
    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    let video = apple_video::capabilities();
    #[cfg(not(all(target_os = "macos", feature = "apple-adapters")))]
    let video = portable_video_capabilities();
    portable_capabilities().into_iter().take(2).chain(video).chain(media_capabilities()).collect()
}
#[cfg(not(all(target_os = "macos", feature = "apple-adapters")))]
fn portable_video_capabilities() -> Vec<ToolCapability> {
    portable_capabilities().into_iter().skip(2).collect()
}
fn portable_capabilities() -> Vec<ToolCapability> {
    vec![ToolCapability{id:"photo".into(),available:true,input_formats:vec!["png".into(),"jpeg".into()],output_formats:vec!["png".into(),"jpeg".into(),"tiff".into()],detail:"Single-image conversion and bounded resize. Enlarge smaller photos with conventional interpolation when selected; this does not recover missing detail. EXIF orientation is applied. Camera/location metadata is stripped; inputs with embedded ICC profiles are rejected until color-managed conversion is available. Output is 8-bit SDR. JPEG transparency is flattened onto white. HEIC/TIFF inputs are not yet supported on this adapter.".into()},
    ToolCapability{id:"chapters".into(),available:true,input_formats:vec!["catalog chapters".into()],output_formats:vec!["json".into(),"vtt".into()],detail:"Export current chapter markers. WebVTT is a chapter cue list, not speech subtitles.".into()},
    ToolCapability{id:"video".into(),available:super::video_export::available(),input_formats:vec!["mp4".into(),"mov".into(),"m4v".into()],output_formats:vec!["mp4".into()],detail:"Vetted local FFmpeg/OpenH264 runtime exports 1280 or 1920 pixel H.264/AAC MP4. Source, 9:16, 16:9, 1:1 and 4:5 frames fit the entire image with padding. SDR only; one video and at most one audio stream. Originals are preserved.".into()},
    ToolCapability{id:"videoEnhancement".into(),available:false,input_formats:vec![],output_formats:vec![],detail:"Stabilization, AI upscaling, and tracked reframing are not installed yet.".into()}]
}

fn video_preview(recipe: &ToolRecipe, source: &Path) -> Result<String> {
    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    return Ok(crate::apple::media::convert_video(recipe, source, None, &AtomicBool::new(false))?.summary);
    #[cfg(not(all(target_os = "macos", feature = "apple-adapters")))]
    {
        let _ = recipe;
        super::video_export::probe(source)?;
        Ok(String::new())
    }
}
fn video_check(recipe: &ToolRecipe, source: &Path) -> Result<()> {
    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    {
        let _ = recipe;
        let extension = source.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
        if !matches!(extension.as_deref(), Some("mp4" | "mov" | "m4v")) {
            bail!("Video inputs must be MP4, MOV or M4V.")
        }
        Ok(())
    }
    #[cfg(not(all(target_os = "macos", feature = "apple-adapters")))]
    video_preview(recipe, source).map(drop)
}
fn video_export(recipe: &ToolRecipe, source: &Path, (stage, file): (&Path, &File), cancelled: &AtomicBool) -> Result<String> {
    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    {
        let _ = file;
        if fs::metadata(stage)?.len() != 0 {
            bail!("The staged output is occupied.")
        }
        fs::remove_file(stage)?;
        let converted = crate::apple::media::convert_video(recipe, source, Some(stage), cancelled)?;
        File::open(stage)?.sync_all()?;
        Ok(converted.summary)
    }
    #[cfg(not(all(target_os = "macos", feature = "apple-adapters")))]
    {
        let _ = stage;
        let before = super::video_export::probe(source)?;
        super::video_export::export(source, file, recipe, &before, cancelled)?;
        Ok(String::new())
    }
}
fn video_verify(recipe: &ToolRecipe, source: &Path, stage: &Path) -> Result<()> {
    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    {
        let _ = (recipe, source, stage);
        Ok(())
    }
    #[cfg(not(all(target_os = "macos", feature = "apple-adapters")))]
    {
        let before = super::video_export::probe(source)?;
        let after = super::video_export::probe_stage(stage)?;
        super::video_export::verify(&before, &after, recipe)
    }
}

/// Swift's video export rules and capability text, as the Mac engine runs the same AVFoundation export.
#[cfg(all(target_os = "macos", feature = "apple-adapters"))]
mod apple_video {
    use crate::ipc::{ToolCapability, ToolRecipe};

    pub(super) fn capabilities() -> Vec<ToolCapability> {
        vec![
            ToolCapability { id: "video".into(), available: true, input_formats: vec!["mp4".into(), "mov".into(), "m4v".into()], output_formats: vec!["mp4".into()], detail: "Native H.264/AAC SDR export at 1280, 1920 or 3840 pixels. Optional conventional enlargement renders smaller sources at the selected resolution without recovering detail. Optional 9:16, 16:9, 1:1 or 4:5 frames fit the entire picture, or follow detected people with smooth cropping and full-picture fallback. Follow-people mode samples twice per second and supports clips up to ten minutes. Optional stabilization corrects estimated translation and rotation at up to 60 fps, with up to 12% edge-covering zoom and reported uncorrected samples. One video and at most one audio track; HDR, alpha channels, subtitles and auxiliary tracks are rejected. Camera/location container metadata is stripped. Original files are preserved.".into() },
            ToolCapability { id: "videoEnhancement".into(), available: false, input_formats: vec![], output_formats: vec![], detail: "Neural super-resolution is unavailable. Conventional enlargement is available through video export.".into() },
        ]
    }

    pub(super) fn supports(recipe: &ToolRecipe) -> bool {
        let aspect = recipe.video_aspect_ratio.as_deref().unwrap_or("source");
        (1..=8192).contains(&recipe.max_dimension)
            && recipe.trim_start_seconds.is_none()
            && recipe.trim_end_seconds.is_none()
            && recipe.trim_mode.is_none()
            && recipe.rotation_degrees.is_none()
            && (recipe.allow_upscale != Some(true) || ["photo", "video"].contains(&recipe.kind.as_str()))
            && (recipe.video_aspect_ratio.is_none() || recipe.kind == "video")
            && (recipe.video_framing.is_none() || recipe.kind == "video")
            && (recipe.video_stabilization.is_none() || recipe.kind == "video")
            && recipe.video_framing.as_deref().is_none_or(|framing| ["fit", "auto"].contains(&framing))
            && (recipe.video_framing.as_deref() != Some("auto") || aspect != "source")
            && ["source", "9:16", "16:9", "1:1", "4:5"].contains(&aspect)
            && ((recipe.kind == "photo" && ["png", "jpeg", "tiff"].contains(&recipe.format.as_str()))
                || (recipe.kind == "video" && recipe.format == "mp4" && [1280, 1920, 3840].contains(&recipe.max_dimension)))
    }
}
fn media_capabilities() -> Vec<ToolCapability> {
    let runtime = media_recipes::runtime();
    let inputs = || vec!["mp4".to_owned(), "mov".to_owned(), "m4v".to_owned()];
    [("trim", vec!["mp4", "mov"]), ("remux", vec!["mp4", "mov"]), ("audio", vec!["m4a"]), ("proxy", vec!["mp4"]), ("rotate", vec!["mp4", "mov"])]
        .into_iter()
        .map(|(id, outputs)| ToolCapability {
            id: id.into(),
            available: runtime.is_some(),
            input_formats: inputs(),
            output_formats: outputs.into_iter().map(str::to_owned).collect(),
            detail: runtime.as_ref().map_or(media_recipes::UNAVAILABLE, |runtime| runtime.detail(id)).into(),
        })
        .collect()
}
fn supports(recipe: &ToolRecipe) -> bool {
    if media_recipes::is_media(&recipe.kind) {
        return media_recipes::validate(recipe).is_ok() && media_recipes::runtime().is_some();
    }
    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    if recipe.kind == "video" || recipe.allow_upscale == Some(true) || recipe.video_framing.is_some() || recipe.video_stabilization.is_some() {
        return apple_video::supports(recipe);
    }
    (1..=8192).contains(&recipe.max_dimension)
        && recipe.trim_start_seconds.is_none()
        && recipe.trim_end_seconds.is_none()
        && recipe.trim_mode.is_none()
        && recipe.rotation_degrees.is_none()
        && (recipe.allow_upscale != Some(true) || recipe.kind == "photo")
        && (recipe.video_aspect_ratio.is_none() || recipe.kind == "video")
        && recipe.video_framing.is_none()
        && recipe.video_stabilization.is_none()
        && ((recipe.kind == "photo" && ["png", "jpeg", "tiff"].contains(&recipe.format.as_str()))
            || (recipe.kind == "chapters" && ["json", "vtt"].contains(&recipe.format.as_str()))
            || (recipe.kind == "video"
                && recipe.format == "mp4"
                && [1280, 1920].contains(&recipe.max_dimension)
                && ["source", "9:16", "16:9", "1:1", "4:5"]
                    .contains(&recipe.video_aspect_ratio.as_deref().unwrap_or("source"))
                && super::video_export::available()))
}
fn hash(path: &Path) -> Result<String> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!("Choose a regular file, not a link or folder.")
    }
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    // Heap, not stack: a 1 MiB array on a 2 MiB blocking-pool stack can overflow, and a stack overflow aborts the whole
    // engine.
    let mut bytes = vec![0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        hash.update(&bytes[..n]);
    }
    Ok(hex::encode(hash.finalize()))
}
fn persist(conn: &Connection, id: &str, receipts: &[Receipt], state: &str) -> Result<()> {
    conn.execute(
        "UPDATE catalog_operations SET inverse_json=?1,state=?2 WHERE id=?3",
        params![serde_json::to_string(receipts)?, state, id],
    )?;
    Ok(())
}
fn load(conn: &Connection, id: &str) -> Result<(Plan, Vec<Receipt>, String)> {
    let (plan, inverse, state): (String, String, String) = conn.query_row(
        "SELECT plan_json,inverse_json,state FROM catalog_operations WHERE id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let plan: Plan = serde_json::from_str(&plan)?;
    if plan.r#type != "export" || plan.version != 1 {
        bail!("The export plan is unavailable.")
    }
    Ok((plan, serde_json::from_str(&inverse)?, state))
}

/// `Shared` locks per short catalog step only, so hashing, probing, encoding and ffmpeg run with the writer free for
/// scans and other commands.
enum Db<'a> {
    Held(&'a mut Connection),
    Shared(&'a Arc<Mutex<Connection>>),
}

impl Db<'_> {
    fn with<T>(&mut self, work: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        match self {
            Db::Held(conn) => work(conn),
            Db::Shared(shared) => work(&mut shared.lock()),
        }
    }
}

pub fn execute(conn: &mut Connection, request: &ToolRequest) -> Result<ToolResponse> {
    execute_with_move(conn, request, |source, target, published| {
        crate::util::file_move::move_no_clobber(source, target, published)
    })
}
fn execute_with_move(
    conn: &mut Connection,
    request: &ToolRequest,
    move_file: impl FnMut(&Path, &Path, &mut dyn FnMut() -> std::io::Result<()>) -> std::io::Result<()>,
) -> Result<ToolResponse> {
    run_action(&mut Db::Held(conn), request, move_file)
}

/// The writer was free while the output was produced, so a rename, rescan or Undo may have intervened.
fn require_unchanged(conn: &Connection, id: &str, item: &Item) -> Result<()> {
    let state: Option<String> = conn
        .query_row("SELECT state FROM catalog_operations WHERE id=?1", [id], |r| r.get(0))
        .optional()?;
    if state.as_deref() != Some("running") {
        bail!("The export operation changed while it ran; its output was not catalogued")
    }
    let path: Option<String> = conn
        .query_row("SELECT path_text FROM files WHERE id=?1", [item.output.file_id], |r| r.get(0))
        .optional()?;
    if path.as_deref() != Some(item.output.source_path.as_str()) {
        bail!("A source moved in the catalog during export; its output was not catalogued")
    }
    Ok(())
}

fn run_action(
    db: &mut Db,
    request: &ToolRequest,
    mut move_file: impl FnMut(&Path, &Path, &mut dyn FnMut() -> std::io::Result<()>) -> std::io::Result<()>,
) -> Result<ToolResponse> {
    if request.request_id.is_empty() || request.request_id.chars().count() > 200 {
        bail!("Invalid tools request.")
    }
    let mut result = response(&request.request_id, "ok", "");
    match request.action.as_str() {
        "capabilities" => result.capabilities = capabilities(),
        "history" => {
            let latest = db.with(|conn| {
                let id: Option<String> = conn.query_row("SELECT id FROM catalog_operations WHERE json_extract(plan_json,'$.type')='export' AND state IN ('completed','failed','undone') ORDER BY rowid DESC LIMIT 1",[],|r|r.get(0)).optional()?;
                id.map(|id| load(conn, &id).map(|loaded| (id, loaded))).transpose()
            })?;
            if let Some((id, (_, receipts, _))) = latest {
                result.operation_id = Some(id);
                result.outputs = receipts.into_iter().map(|r| r.output).collect();
                result.message = "Showing your last export.".into();
            } else {
                result.message = "No completed export history.".into();
            }
        }
        "preview" => {
            let ids = request.file_ids.as_ref().context("Choose files first.")?;
            let recipe = request.recipe.as_ref().context("Choose a recipe first.")?;
            let media = if media_recipes::is_media(&recipe.kind) {
                media_recipes::validate(recipe)?;
                Some(media_recipes::runtime().context(media_recipes::UNAVAILABLE)?)
            } else {
                None
            };
            if ids.is_empty()
                || ids.len() > 100
                || ids.iter().collect::<HashSet<_>>().len() != ids.len()
                || !supports(recipe)
            {
                bail!(UNSUPPORTED_RECIPE)
            }
            let destination = foundation_standardized(Path::new(request.destination.as_deref().context("Choose an output folder first.")?));
            let destination = destination.as_path();
            read_only::require_source_mutation(destination)?;
            if !destination.is_absolute() || !destination.is_dir() {
                bail!("The output folder must already exist.")
            }
            let mut items = vec![];
            let mut reserved = HashSet::new();
            for &file_id in ids {
                let (source, chapters) = db.with(|conn| {
                    let source: String =
                        conn.query_row("SELECT path_text FROM files WHERE id=?1", [file_id], |r| r.get(0))?;
                    let chapters = super::catalog::chapters(conn, file_id)?
                        .into_iter()
                        .filter(|c| !c.stale)
                        .collect::<Vec<_>>();
                    Ok((source, chapters))
                })?;
                let source_path = Path::new(&source);
                if recipe.kind == "chapters" && chapters.is_empty() {
                    bail!("A selected file has no current chapter markers.")
                }
                let source_hash = hash(source_path)?;
                if recipe.kind == "photo" {
                    validate_image(source_path)?;
                }
                let mut note = match &media {
                    Some(runtime) => media_recipes::preview(runtime.as_ref(), recipe, source_path)?.to_owned(),
                    None => String::new(),
                };
                if recipe.kind == "video" {
                    note = video_preview(recipe, source_path)?;
                }
                let stem = source_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .context("Invalid source name")?
                    .chars()
                    .take(50)
                    .collect::<String>();
                let label = if media.is_some() {
                    media_recipes::label(recipe).trim_start_matches(" - ").to_owned()
                } else if recipe.kind == "chapters" {
                    "Chapters".to_owned()
                } else {
                    "Export".to_owned()
                };
                let frame = if recipe.kind == "video" {
                    let framing = if recipe.video_framing.as_deref() == Some("auto") { "People" } else { "Fit" };
                    let stabilized = if recipe.video_stabilization == Some(true) { " - Stabilized" } else { "" };
                    recipe
                        .video_aspect_ratio
                        .as_deref()
                        .filter(|value| *value != "source")
                        .map(|value| format!(" - {} {framing}", value.replace(':', "x")))
                        .unwrap_or_default()
                        + stabilized
                } else {
                    String::new()
                };
                let ext = if recipe.format == "jpeg" { "jpg" } else { &recipe.format };
                let mut index = 1;
                let output = loop {
                    let suffix = if index == 1 {
                        String::new()
                    } else {
                        format!(" ({index})")
                    };
                    let output = destination.join(format!("{stem} - {label}{frame}{suffix}.{ext}"));
                    read_only::require_source_mutation(&output)?;
                    index += 1;
                    let key = output
                        .file_name()
                        .context("Invalid output name")?
                        .to_string_lossy()
                        .to_lowercase();
                    if !output.exists() && reserved.insert(key) {
                        break output;
                    }
                };
                items.push(Item {
                    output: ToolOutput {
                        file_id,
                        source_path: source,
                        output_path: output.to_string_lossy().into_owned(),
                        state: "pending".into(),
                        message: note,
                    },
                    source_hash,
                    chapters,
                });
            }
            let plan = Plan {
                version: 1,
                r#type: "export".into(),
                recipe: recipe.clone(),
                items,
                stage_paths: None,
            };
            let id = uuid::Uuid::new_v4().to_string();
            let plan_json = serde_json::to_string(&plan)?;
            db.with(|conn| Ok(conn.execute("INSERT INTO catalog_operations(id,plan_json,inverse_json,state,created_at) VALUES(?1,?2,'[]','preview',?3)",params![id,plan_json,chrono::Utc::now().timestamp_millis() as f64 / 1000.0])?))?;
            result.outputs = plan.items.into_iter().map(|i| i.output).collect();
            result.operation_id = Some(id);
            result.message = preview_message(&recipe.kind, result.outputs.len());
        }
        "execute" => {
            let id = request.operation_id.as_deref().context("Preview an export first.")?;
            let (mut plan, previous, state) = db.with(|conn| load(conn, id))?;
            let media = if media_recipes::is_media(&plan.recipe.kind) {
                media_recipes::validate(&plan.recipe)?;
                Some(media_recipes::runtime().context(media_recipes::UNAVAILABLE)?)
            } else {
                None
            };
            if !supports(&plan.recipe) {
                bail!("This export recipe is unavailable on this platform.")
            }
            if state != "preview" || !previous.is_empty() {
                bail!("This plan was already executed. Preview a fresh plan.")
            }
            let cancelled = (plan.recipe.kind == "video" || media.is_some()).then(|| Arc::new(AtomicBool::new(false)));
            let _active = cancelled
                .as_ref()
                .map(|flag| ActiveExport::register(id, flag.clone()))
                .transpose()?;
            for item in &plan.items {
                if cancelled.as_ref().is_some_and(|flag| flag.load(Ordering::Acquire)) {
                    bail!("Export cancelled before publication")
                }
                if plan.recipe.kind == "chapters" {
                    let current = db
                        .with(|conn| super::catalog::chapters(conn, item.output.file_id))?
                        .into_iter()
                        .filter(|c| !c.stale)
                        .collect::<Vec<_>>();
                    if serde_json::to_value(&current)? != serde_json::to_value(&item.chapters)? {
                        bail!("Chapter markers changed after preview. Preview again.")
                    }
                }
                let output = Path::new(&item.output.output_path);
                read_only::require_source_mutation(output)?;
                if hash(Path::new(&item.output.source_path))? != item.source_hash {
                    bail!("A source changed after preview. Preview again.")
                }
                if plan.recipe.kind == "video" {
                    video_check(&plan.recipe, Path::new(&item.output.source_path))?;
                }
                if let Some(runtime) = &media {
                    media_recipes::preview(runtime.as_ref(), &plan.recipe, Path::new(&item.output.source_path))?;
                }
                if output.exists() {
                    bail!("An output name is occupied. Preview again")
                }
            }
            if cancelled.as_ref().is_some_and(|flag| flag.load(Ordering::Acquire)) {
                bail!("Export cancelled before publication")
            }
            // Atomic claim: the plan was validated with the writer free, so a second execute of the same plan may have
            // got here first.
            let claimed = db.with(|conn| {
                Ok(conn.execute(
                    "UPDATE catalog_operations SET state='running' WHERE id=?1 AND state='preview' AND inverse_json='[]'",
                    [id],
                )?)
            })?;
            if claimed != 1 {
                bail!("This plan was already executed. Preview a fresh plan.")
            }
            let mut receipts = vec![];
            let run = (|| -> Result<()> {
                for item in plan.items.clone() {
                    if cancelled.as_ref().is_some_and(|flag| flag.load(Ordering::Acquire)) {
                        bail!("Export cancelled before publication")
                    }
                    let output = Path::new(&item.output.output_path);
                    let source = Path::new(&item.output.source_path);
                    let stage = output
                        .parent()
                        .context("Invalid output folder")?
                        .join(format!(".FileIDExport-{}.part", uuid::Uuid::new_v4()));
                    read_only::require_writable(&stage)?;
                    plan.stage_paths
                        .get_or_insert_with(Vec::new)
                        .push(stage.to_string_lossy().into_owned());
                    let plan_json = serde_json::to_string(&plan)?;
                    db.with(|conn| {
                        Ok(conn.execute("UPDATE catalog_operations SET plan_json=?1 WHERE id=?2", params![plan_json, id])?)
                    })?;
                    let mut file = OpenOptions::new().write(true).create_new(true).open(&stage)?;
                    let mut video_summary = String::new();
                    let prepared = (|| -> Result<()> {
                        let mut media_source = None;
                        if let Some(runtime) = &media {
                            media_source = Some(media_recipes::export(
                                runtime.as_ref(),
                                &plan.recipe,
                                source,
                                (&stage, &file),
                                cancelled.as_deref().context("Missing media cancellation state")?,
                            )?);
                        } else if plan.recipe.kind == "photo" {
                            export_photo(source, &mut file, &plan.recipe)?;
                        } else if plan.recipe.kind == "video" {
                            video_summary = video_export(
                                &plan.recipe,
                                source,
                                (&stage, &file),
                                cancelled.as_deref().context("Missing video cancellation state")?,
                            )?;
                        } else {
                            file.write_all(&export_chapters(&item.chapters, &plan.recipe.format)?)?;
                        }
                        file.sync_all()?;
                        drop(file);
                        if plan.recipe.kind == "photo" {
                            let decoder = ImageReader::open(&stage)?.with_guessed_format()?.into_decoder()?;
                            let decoded = DynamicImage::from_decoder(decoder)?;
                            if decoded.width() == 0
                                || decoded.height() == 0
                                || decoded.width().max(decoded.height()) > plan.recipe.max_dimension
                            {
                                bail!("Output validation failed.")
                            }
                        }
                        if plan.recipe.kind == "video" {
                            video_verify(&plan.recipe, source, &stage)?;
                        }
                        if let (Some(runtime), Some(before)) = (&media, &media_source) {
                            media_recipes::verify_stage(runtime.as_ref(), &plan.recipe, before, &stage)?;
                        }
                        if hash(source)? != item.source_hash {
                            bail!("A source changed during export; its output was not published")
                        }
                        if cancelled.as_ref().is_some_and(|flag| flag.load(Ordering::Acquire)) {
                            bail!("Export cancelled before publication")
                        }
                        let digest = hash(&stage)?;
                        let mut receipt = Receipt {
                            output: item.output.clone(),
                            hash: digest,
                            derived_id: None,
                            recovery_path: None,
                            recovery_published: None,
                        };
                        receipt.output.message =
                            "Prepared output; review this path after interrupted publication.".into();
                        receipts.push(receipt);
                        db.with(|conn| persist(conn, id, &receipts, "running"))?;
                        read_only::require_writable(output)?;
                        if cancelled.as_ref().is_some_and(|flag| flag.load(Ordering::Acquire)) {
                            bail!("Export cancelled before publication")
                        }
                        db.with(|conn| require_unchanged(conn, id, &item))?;
                        fs::hard_link(&stage, output)?;
                        let last = receipts.last_mut().context("Missing export receipt")?;
                        last.output.state = "completed".into();
                        last.output.message = match (&media, video_summary.is_empty()) {
                            (Some(runtime), _) => format!("Original preserved. {}", runtime.summary(&plan.recipe)),
                            (None, false) => format!("Original preserved. {video_summary}"),
                            (None, true) => "Original preserved.".into(),
                        };
                        let metadata = output.metadata()?;
                        let modified = metadata
                            .modified()?
                            .duration_since(std::time::UNIX_EPOCH)?
                            .as_secs_f64();
                        let kind = match plan.recipe.kind.as_str() {
                            "photo" => "image",
                            "video" => "video",
                            other if media_recipes::is_media(other) => media_recipes::output_kind(&plan.recipe),
                            _ => "doc",
                        };
                        let role = if media.is_some() { media_recipes::role(&plan.recipe) } else { "export" };
                        let recipe_json = serde_json::to_string(&plan.recipe)?;
                        db.with(|conn| {
                            let tx = conn.transaction()?;
                            require_unchanged(&tx, id, &item)?;
                            tx.execute("INSERT INTO files(path_text,path_hash,size_bytes,scanned_at,modified_at,kind,extension) VALUES(?1,?2,?3,0,?4,?5,?6)",params![item.output.output_path,stable_path_hash(&item.output.output_path),metadata.len(),modified,kind,output.extension().and_then(|s|s.to_str()).unwrap_or_default()])?;
                            let derived = tx.last_insert_rowid();
                            tx.execute("INSERT INTO catalog_assets(original_id,derived_id,role,recipe_json) VALUES(?1,?2,?3,?4)",params![item.output.file_id,derived,role,recipe_json])?;
                            if let Some(last) = receipts.last_mut() {
                                last.derived_id = Some(derived);
                            }
                            persist(&tx, id, &receipts, "running")?;
                            tx.commit()?;
                            Ok(())
                        })
                    })();
                    let _ = fs::remove_file(&stage);
                    prepared?;
                }
                Ok(())
            })();
            match run {
                Ok(()) => {
                    db.with(|conn| persist(conn, id, &receipts, "completed"))?;
                    result.message =
                        "Export complete. Undo moves the exported files into FileID’s recovery folder.".into();
                }
                Err(error) => {
                    db.with(|conn| persist(conn, id, &receipts, "failed"))?;
                    tracing::warn!(error = ?error, "export failed");
                    let exported = receipts.iter().filter(|receipt| receipt.output.state == "completed").count();
                    let folder = plan.items.first().and_then(|item| Path::new(&item.output.output_path).parent()).map(|parent| parent.display().to_string());
                    result.status = "error".into();
                    result.message = export_failure_message(&error, folder.as_deref(), exported, plan.items.len());
                }
            }
            result.operation_id = Some(id.into());
            result.outputs = receipts.into_iter().map(|r| r.output).collect();
        }
        "cancel" => return cancel_active(request),
        "undo" => {
            let id = request.operation_id.as_deref().context("Choose an export first.")?;
            let (_, mut receipts, state) = db.with(|conn| load(conn, id))?;
            for receipt in &receipts {
                if receipt.output.state != "completed" {
                    continue;
                }
                let source = Path::new(&receipt.output.output_path);
                read_only::require_source_mutation(source)?;
                if let Some(path) = receipt.recovery_path.as_deref() {
                    let recovery = Path::new(path);
                    if receipt.recovery_published == Some(true) && recovery.is_file() {
                        if hash(recovery)? != receipt.hash {
                            bail!("Recovery output was edited; Undo leaves it untouched")
                        }
                        if !source.exists() {
                            continue;
                        }
                    }
                }
                if !source.exists() {
                    bail!("The export is missing and no verified owned recovery is available. Undo leaves the operation unchanged")
                }
                if hash(source)? != receipt.hash {
                    bail!("An export was edited. Undo leaves all remaining exports untouched.")
                }
            }
            if !["completed", "failed"].contains(&state.as_str()) {
                bail!("This operation cannot be undone.")
            }
            let db_path: String = db.with(|conn| {
                Ok(conn.query_row("SELECT file FROM pragma_database_list WHERE name='main'", [], |r| r.get(0))?)
            })?;
            let directory = Path::new(&db_path)
                .parent()
                .context("Catalog has no recovery folder")?
                .join("ExportRecovery")
                .join(id);
            read_only::require_writable(&directory)?;
            fs::create_dir_all(&directory)?;
            for index in 0..receipts.len() {
                if receipts[index].output.state != "completed" {
                    continue;
                }
                let source = std::path::PathBuf::from(&receipts[index].output.output_path);
                read_only::require_source_mutation(&source)?;
                if let Some(path) = receipts[index].recovery_path.clone() {
                    let recovery = Path::new(&path);
                    read_only::require_writable(recovery)?;
                    if recovery.is_file() && receipts[index].recovery_published == Some(true) {
                        if hash(recovery)? != receipts[index].hash {
                            bail!("Recovery output was edited; Undo leaves it untouched")
                        }
                        if source.exists() {
                            if hash(&source)? != receipts[index].hash {
                                bail!("An export was edited. Undo leaves it untouched.")
                            }
                            fs::remove_file(&source)?;
                        }
                        if let Some(derived) = receipts[index].derived_id {
                            db.with(|conn| Ok(conn.execute("DELETE FROM files WHERE id=?1", [derived])?))?;
                        }
                        receipts[index].output.state = "undone".into();
                        receipts[index].output.message = format!("Kept for undo at {path}");
                        db.with(|conn| persist(conn, id, &receipts, &state))?;
                        continue;
                    }
                }
                if hash(&source)? != receipts[index].hash {
                    bail!("An export was edited. Undo leaves it untouched.")
                }
                let target = directory.join(format!(
                    "{index}-{}",
                    source.file_name().context("Invalid export path")?.to_string_lossy()
                ));
                read_only::require_writable(&target)?;
                receipts[index].recovery_path = Some(target.to_string_lossy().into_owned());
                receipts[index].recovery_published = Some(false);
                db.with(|conn| persist(conn, id, &receipts, &state))?;
                let expected = receipts[index].hash.clone();
                let mut published = || -> std::io::Result<()> {
                    receipts[index].recovery_published = Some(true);
                    receipts[index].output.message = "Undo is finishing. The exported files are being removed.".into();
                    db.with(|conn| persist(conn, id, &receipts, &state)).map_err(std::io::Error::other)?;
                    if hash(&source).map_err(std::io::Error::other)? != expected
                        || hash(&target).map_err(std::io::Error::other)? != expected
                    {
                        return Err(std::io::Error::other(
                            "Export changed during Undo; both paths were preserved",
                        ));
                    }
                    Ok(())
                };
                move_file(&source, &target, &mut published)?;
                if let Some(derived) = receipts[index].derived_id {
                    db.with(|conn| Ok(conn.execute("DELETE FROM files WHERE id=?1", [derived])?))?;
                }
                receipts[index].output.state = "undone".into();
                receipts[index].output.message = format!("Kept for undo at {}", target.display());
                db.with(|conn| persist(conn, id, &receipts, &state))?;
            }
            db.with(|conn| persist(conn, id, &receipts, "undone"))?;
            result.operation_id = Some(id.into());
            result.outputs = receipts.into_iter().map(|r| r.output).collect();
            result.message = "Exports moved to FileID’s recovery folder. Originals are unchanged.".into();
        }
        _ => bail!("Unsupported tools action."),
    }
    Ok(result)
}

/// Camera originals reach ~61 MP; 100 MP is the ceiling because decode holds the RGB8 frame (300 MB) next to its RGBA
/// copy inside the decoder's 512 MiB allocation limit.
const PHOTO_MAX_PIXELS: u64 = 100_000_000;

fn validate_image(path: &Path) -> Result<()> {
    let format = ImageReader::open(path)?
        .with_guessed_format()?
        .format()
        .context("Unknown image format")?;
    if ![ImageFormat::Png, ImageFormat::Jpeg].contains(&format) {
        bail!("Only JPEG and PNG photos can be exported here. For a RAW photo, choose the JPEG.")
    }
    if format == ImageFormat::Png {
        let mut file = File::open(path)?;
        let size = file.metadata()?.len();
        let mut offset = 8u64;
        while offset + 12 <= size {
            file.seek(SeekFrom::Start(offset))?;
            let mut header = [0u8; 8];
            file.read_exact(&mut header)?;
            let length = u64::from(u32::from_be_bytes(header[..4].try_into()?));
            if &header[4..] == b"acTL" {
                bail!("Animated PNG cannot be flattened by this tool")
            }
            let next = offset
                .checked_add(length)
                .and_then(|n| n.checked_add(12))
                .context("Malformed PNG")?;
            if next > size {
                bail!("Malformed PNG")
            }
            offset = next;
        }
    }
    let mut decoder = ImageReader::open(path)?.with_guessed_format()?.into_decoder()?;
    let (width, height) = decoder.dimensions();
    if u64::from(width) * u64::from(height) > PHOTO_MAX_PIXELS {
        bail!("This adapter currently limits input images to 100 megapixels")
    }
    if decoder.icc_profile()?.is_some() {
        bail!("This adapter requires untagged sRGB input; use color-managed conversion for embedded ICC profiles")
    }
    Ok(())
}
fn export_photo(source: &Path, file: &mut File, recipe: &ToolRecipe) -> Result<()> {
    if recipe.kind != "photo" || !supports(recipe) {
        bail!("Unsupported photo recipe.")
    }
    validate_image(source)?;
    let mut reader = ImageReader::open(source)?.with_guessed_format()?;
    let mut limits = Limits::default();
    limits.max_alloc = Some(512 * 1024 * 1024);
    limits.max_image_width = Some(65536);
    limits.max_image_height = Some(65536);
    reader.limits(limits);
    let mut decoder = reader.into_decoder()?;
    let orientation = decoder.orientation()?;
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    if recipe.allow_upscale == Some(true) || image.width().max(image.height()) > recipe.max_dimension {
        image = image.resize(
            recipe.max_dimension,
            recipe.max_dimension,
            image::imageops::FilterType::Lanczos3,
        );
    }
    let rgba = image.to_rgba8();
    image = if recipe.format == "jpeg" {
        let mut rgb = image::RgbImage::new(rgba.width(), rgba.height());
        for (x, y, pixel) in rgba.enumerate_pixels() {
            let a = u16::from(pixel[3]);
            let blend = |c: u8| -> u8 { ((u16::from(c) * a + 255 * (255 - a) + 127) / 255) as u8 };
            rgb.put_pixel(x, y, image::Rgb([blend(pixel[0]), blend(pixel[1]), blend(pixel[2])]));
        }
        DynamicImage::ImageRgb8(rgb)
    } else {
        DynamicImage::ImageRgba8(rgba)
    };
    let format = match recipe.format.as_str() {
        "png" => ImageFormat::Png,
        "jpeg" => ImageFormat::Jpeg,
        "tiff" => ImageFormat::Tiff,
        _ => bail!("Unsupported output"),
    };
    image.write_to(file, format)?;
    Ok(())
}
fn export_chapters(chapters: &[CatalogChapter], format: &str) -> Result<Vec<u8>> {
    if format == "json" {
        return Ok(serde_json::to_vec_pretty(chapters)?);
    }
    let stamp = |seconds: f64| {
        let ms = (seconds.min(359_999_999.0) * 1000.0).round() as u64;
        format!(
            "{:02}:{:02}:{:02}.{:03}",
            ms / 3_600_000,
            ms / 60_000 % 60,
            ms / 1000 % 60,
            ms % 1000
        )
    };
    let mut text = String::from("WEBVTT\n\n");
    for (i, c) in chapters.iter().enumerate() {
        let title = c
            .title
            .replace("-->", "→")
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace(['\n', '\r'], " ");
        use std::fmt::Write as _;
        write!(
            &mut text,
            "{}\n{} --> {}\n{}\n\n",
            i + 1,
            stamp(c.start_seconds),
            stamp(c.end_seconds.max(c.start_seconds + 0.001)),
            title
        )?;
    }
    Ok(text.into_bytes())
}

/// As `URL.standardizedFileURL.path`: `.` and `..` resolved and, on macOS, a leading `/private` dropped when the rest
/// exists.
fn foundation_standardized(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    if cfg!(target_os = "macos")
        && let Ok(rest) = out.strip_prefix("/private")
        && !rest.as_os_str().is_empty()
    {
        let stripped = Path::new("/").join(rest);
        if stripped.exists() {
            return stripped;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn export_failures_name_the_cause_in_plain_words() {
        let full = anyhow::Error::from(std::io::Error::from(std::io::ErrorKind::StorageFull)).context("copying");
        assert_eq!(
            super::export_failure_message(&full, Some("/Volumes/Photos"), 1, 6),
            "There isn’t enough free space in /Volumes/Photos. Free up space or choose another folder. 1 of 6 files was exported."
        );
        assert_eq!(
            super::export_failure_message(&full, Some("/Volumes/Photos"), 0, 1),
            "There isn’t enough free space in /Volumes/Photos. Free up space or choose another folder."
        );
        let read_only = anyhow::Error::from(std::io::Error::from(std::io::ErrorKind::ReadOnlyFilesystem));
        assert_eq!(
            super::export_failure_message(&read_only, Some("/Volumes/Disk"), 2, 5),
            "/Volumes/Disk is on a read-only disk, so FileID can’t save there. Choose another folder. 2 of 5 files were exported."
        );
        let other = anyhow::Error::from(std::io::Error::from(std::io::ErrorKind::TimedOut));
        let message = super::export_failure_message(&other, None, 0, 1);
        assert!(message.starts_with("Export didn’t finish.") && !message.contains("os error"), "{message}");
        assert_eq!(super::export_failure_message(&anyhow::anyhow!("A source changed after preview. Preview again."), None, 0, 1), "A source changed after preview. Preview again.");
    }

    #[test]
    fn preview_message_names_only_the_caveat_of_the_recipe_kind() {
        let photo = super::preview_message("photo", 2);
        let video = super::preview_message("video", 1);
        let trim = super::preview_message("trim", 1);
        assert!(!video.contains("Photo outputs"), "{video}");
        assert!(video.contains("ideo"), "{video}");
        assert!(!trim.contains("SDR") && !trim.contains("hoto"), "{trim}");
        assert!(trim.contains("Originals"), "{trim}");
        assert!(photo.contains("hoto"), "{photo}");
    }

    #[test]
    fn output_folders_are_named_as_foundation_standardizes_them() {
        use std::path::Path;
        assert_eq!(super::foundation_standardized(Path::new("/a/./b/../c")), Path::new("/a/c"));
        assert_eq!(super::foundation_standardized(Path::new("/private/no-such-fileid-folder")), Path::new("/private/no-such-fileid-folder"));
        #[cfg(target_os = "macos")]
        assert_eq!(super::foundation_standardized(Path::new("/private/tmp")), Path::new("/tmp"));
    }

    use super::media_recipes::tests::{landscape as clip_probe, recipe as media_recipe};
    use super::*;
    struct Fixture {
        root: std::path::PathBuf,
        source: std::path::PathBuf,
        conn: Connection,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("FileIDToolsTest-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&root).unwrap();
            let source = root.join("Portrait.png");
            image::RgbaImage::from_pixel(32, 16, image::Rgba([255, 0, 0, 0]))
                .save(&source)
                .unwrap();
            let conn = crate::db::open_writer(&root.join("catalog.sqlite")).unwrap();
            crate::db::test_support::insert_file(&conn, 1, source.to_str().unwrap(), "image");
            Self { root, source, conn }
        }

        fn request(&self, format: &str) -> ToolRequest {
            ToolRequest {
                request_id: "p".into(),
                action: "preview".into(),
                file_ids: Some(vec![1]),
                destination: Some(self.root.to_string_lossy().into_owned()),
                recipe: Some(ToolRecipe {
                    kind: "photo".into(),
                    format: format.into(),
                    max_dimension: 16,
                    allow_upscale: None,
                    video_aspect_ratio: None,
                    video_framing: None,
                    video_stabilization: None,
                    trim_start_seconds: None,
                    trim_end_seconds: None,
                    trim_mode: None,
                    rotation_degrees: None,
                }),
                operation_id: None,
                destination_bookmark: None,
            }
        }

        fn reopen(&mut self) {
            let conn = std::mem::replace(&mut self.conn, Connection::open_in_memory().unwrap());
            conn.close().unwrap();
            self.conn = crate::db::open_writer(&self.root.join("catalog.sqlite")).unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    fn action(id: Option<String>, action: &str) -> ToolRequest {
        ToolRequest {
            request_id: action.into(),
            action: action.into(),
            operation_id: id,
            file_ids: None,
            destination: None,
            recipe: None,
            destination_bookmark: None,
        }
    }
    #[test]
    fn cancel_targets_only_the_registered_video_operation() {
        let id = uuid::Uuid::new_v4().to_string();
        let cancelled = Arc::new(AtomicBool::new(false));
        let active = ActiveExport::register(&id, cancelled.clone()).unwrap();
        assert!(ActiveExport::register(&id, Arc::new(AtomicBool::new(false))).is_err());
        assert!(cancel_active(&action(Some(uuid::Uuid::new_v4().to_string()), "cancel")).is_err());
        assert!(!cancelled.load(Ordering::Acquire));
        let result = cancel_active(&action(Some(id.clone()), "cancel")).unwrap();
        assert_eq!(result.operation_id.as_deref(), Some(id.as_str()));
        assert!(cancelled.load(Ordering::Acquire));
        drop(active);
        assert!(cancel_active(&action(Some(id), "cancel")).is_err());
    }
    #[test]
    fn exports_and_undo_leave_the_writer_free_while_they_work() {
        let mut fixture = Fixture::new();
        let request = fixture.request("png");
        let conn = std::mem::replace(&mut fixture.conn, Connection::open_in_memory().unwrap());
        let shared = Arc::new(Mutex::new(conn));
        let move_file = |source: &Path, target: &Path, published: &mut dyn FnMut() -> std::io::Result<()>| {
            crate::util::file_move::move_no_clobber(source, target, published)
        };
        let preview = run_action(&mut Db::Shared(&shared), &request, move_file).unwrap();
        let id = preview.operation_id.clone();
        let exported = run_action(&mut Db::Shared(&shared), &action(id.clone(), "execute"), move_file).unwrap();
        assert_eq!(exported.status, "ok", "{}", exported.message);
        let probe = shared.clone();
        let undone = run_action(&mut Db::Shared(&shared), &action(id, "undo"), |source, target, published| {
            assert!(probe.try_lock().is_some(), "the writer must be free during file work");
            crate::util::file_move::move_no_clobber(source, target, published)
        })
        .unwrap();
        assert_eq!(undone.outputs[0].state, "undone");
    }

    fn write_gray_jpeg(path: &Path, width: u32, height: u32) {
        let mut encoded = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut encoded)
            .encode(&vec![128u8; width as usize * height as usize], width, height, image::ExtendedColorType::L8)
            .unwrap();
        fs::write(path, encoded).unwrap();
    }

    #[test]
    fn a_7008_by_4672_camera_photo_is_exported() {
        let mut fixture = Fixture::new();
        write_gray_jpeg(&fixture.source, 7008, 4672);
        let request = fixture.request("jpeg");
        let preview = execute(&mut fixture.conn, &request).unwrap();
        assert_eq!(preview.status, "ok", "{}", preview.message);
        let done = execute(&mut fixture.conn, &action(preview.operation_id.clone(), "execute")).unwrap();
        assert_eq!(done.status, "ok", "{}", done.message);
        let output = image::ImageReader::open(&done.outputs[0].output_path).unwrap().with_guessed_format().unwrap();
        assert_eq!(output.into_dimensions().unwrap(), (16, 11));
    }

    #[test]
    fn a_photo_over_100_megapixels_is_refused() {
        let mut fixture = Fixture::new();
        write_gray_jpeg(&fixture.source, 12_000, 8_400);
        let request = fixture.request("jpeg");
        let message = execute(&mut fixture.conn, &request).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(message.contains("100 megapixels"), "{message}");
    }

    #[test]
    fn a_photo_in_another_format_is_refused_in_plain_words() {
        let mut fixture = Fixture::new();
        let mut gif = Vec::new();
        image::codecs::gif::GifEncoder::new(&mut gif)
            .encode(&[200u8; 4 * 4 * 4], 4, 4, image::ExtendedColorType::Rgba8)
            .unwrap();
        fs::write(&fixture.source, gif).unwrap();
        let request = fixture.request("jpeg");
        let message = execute(&mut fixture.conn, &request).err().map(|e| e.to_string()).unwrap_or_default();
        assert_eq!(message, "Only JPEG and PNG photos can be exported here. For a RAW photo, choose the JPEG.");
    }

    #[test]
    fn preview_refuses_a_photo_that_execute_would_refuse() {
        use image::ImageEncoder;
        let mut fixture = Fixture::new();
        let mut png = Vec::new();
        let mut encoder = image::codecs::png::PngEncoder::new(&mut png);
        encoder.set_icc_profile(vec![0u8; 128]).unwrap();
        encoder.write_image(&[200u8; 32 * 16 * 3], 32, 16, image::ExtendedColorType::Rgb8).unwrap();
        fs::write(&fixture.source, png).unwrap();
        let request = fixture.request("jpeg");
        let refused = execute(&mut fixture.conn, &request);
        let message = refused.as_ref().map(|r| r.message.clone()).unwrap_or_else(|e| e.to_string());
        assert!(refused.is_err(), "preview accepted a source execute refuses: {message}");
        assert!(message.contains("ICC"), "{message}");
        let exports = fs::read_dir(&fixture.root).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().contains("Export")).count();
        assert_eq!(exports, 0, "nothing was written");
    }

    #[test]
    fn a_plan_runs_once_and_never_publishes_for_a_moved_source() {
        let mut fixture = Fixture::new();
        let request = fixture.request("png");
        let preview = execute(&mut fixture.conn, &request).unwrap();
        let output = std::path::PathBuf::from(&preview.outputs[0].output_path);
        fixture
            .conn
            .execute("UPDATE files SET path_text='/elsewhere/Portrait.png' WHERE id=1", [])
            .unwrap();
        let moved = execute(&mut fixture.conn, &action(preview.operation_id.clone(), "execute")).unwrap();
        assert_eq!(moved.status, "error");
        assert!(moved.message.contains("moved in the catalog"), "{}", moved.message);
        assert!(!output.exists(), "nothing is published for a source that moved");

        let source = fixture.source.to_str().unwrap().to_owned();
        fixture.conn.execute("UPDATE files SET path_text=?1 WHERE id=1", [source]).unwrap();
        let request = fixture.request("jpeg");
        let preview = execute(&mut fixture.conn, &request).unwrap();
        let first = execute(&mut fixture.conn, &action(preview.operation_id.clone(), "execute")).unwrap();
        assert_eq!(first.status, "ok", "{}", first.message);
        assert!(execute(&mut fixture.conn, &action(preview.operation_id, "execute")).is_err(), "a plan can't be executed twice");
    }

    #[test]
    fn enlargement_is_opt_in_preserves_sources_and_undo() {
        let mut fixture = Fixture::new();
        let original = hash(&fixture.source).unwrap();
        for flag in [None, Some(false), Some(true)] {
            for format in ["png", "jpeg", "tiff"] {
                let mut request = fixture.request(format);
                let recipe = request.recipe.as_mut().unwrap();
                recipe.max_dimension = 64;
                recipe.allow_upscale = flag;
                let preview = execute(&mut fixture.conn, &request).unwrap();
                let exported = execute(&mut fixture.conn, &action(preview.operation_id.clone(), "execute")).unwrap();
                assert_eq!(exported.status, "ok", "{}", exported.message);
                let output = std::path::PathBuf::from(&exported.outputs[0].output_path);
                let image = image::open(&output).unwrap();
                assert_eq!(
                    (image.width(), image.height()),
                    if flag == Some(true) { (64, 32) } else { (32, 16) }
                );
                assert_eq!(hash(&fixture.source).unwrap(), original);
                let undone = execute(&mut fixture.conn, &action(preview.operation_id.clone(), "undo")).unwrap();
                assert_eq!(undone.outputs[0].state, "undone");
                assert!(!output.exists());
                let history = execute(&mut fixture.conn, &action(None, "history")).unwrap();
                assert_eq!(history.operation_id, preview.operation_id);
                assert_eq!(history.outputs[0].state, "undone");
                assert!(history.outputs[0].message.contains("Kept for undo at"));
            }
        }
    }
    #[test]
    fn enlargement_flags_are_compatible_and_reject_wrong_types_and_tools() {
        let recipe: ToolRecipe = serde_json::from_str(r#"{"kind":"photo","format":"png","maxDimension":64}"#).unwrap();
        assert_eq!(recipe.allow_upscale, None);
        assert_eq!(recipe.video_aspect_ratio, None);
        assert!(serde_json::to_value(&recipe).unwrap().get("allowUpscale").is_none());
        assert!(serde_json::to_value(&recipe).unwrap().get("videoAspectRatio").is_none());
        assert!(serde_json::from_str::<ToolRecipe>(
            r#"{"kind":"photo","format":"png","maxDimension":64,"allowUpscale":1}"#
        )
        .is_err());
        let mac = cfg!(all(target_os = "macos", feature = "apple-adapters"));
        for (kind, format, dimension) in [("chapters", "json", 64), ("video", "mp4", 1920), ("photo", "png", 8193)] {
            assert_eq!(mac && kind == "video", supports(&ToolRecipe {
                kind: kind.into(),
                format: format.into(),
                max_dimension: dimension,
                allow_upscale: Some(true),
                video_aspect_ratio: None,
                video_framing: None,
                video_stabilization: None,
                trim_start_seconds: None,
                trim_end_seconds: None,
                trim_mode: None,
                rotation_degrees: None,
            }));
        }
        let framed: ToolRecipe =
            serde_json::from_str(r#"{"kind":"video","format":"mp4","maxDimension":1920,"videoAspectRatio":"9:16"}"#)
                .unwrap();
        assert_eq!(framed.video_aspect_ratio.as_deref(), Some("9:16"));
        assert_eq!(supports(&framed), super::super::timeline_media::available());
        assert!(serde_json::from_str::<ToolRecipe>(
            r#"{"kind":"video","format":"mp4","maxDimension":1920,"videoAspectRatio":1}"#
        )
        .is_err());
    }
    #[test]
    fn a_photo_export_refuses_video_only_options_and_every_video_choice_it_cannot_honour() {
        let recipe = |kind: &str, format: &str, dimension: u32, edit: &dyn Fn(&mut ToolRecipe)| {
            let mut recipe = ToolRecipe {
                kind: kind.into(),
                format: format.into(),
                max_dimension: dimension,
                allow_upscale: None,
                video_aspect_ratio: None,
                video_framing: None,
                video_stabilization: None,
                trim_start_seconds: None,
                trim_end_seconds: None,
                trim_mode: None,
                rotation_degrees: None,
            };
            edit(&mut recipe);
            recipe
        };
        assert!(supports(&recipe("photo", "png", 64, &|_| {})), "positive control");
        assert!(!supports(&recipe("photo", "png", 8193, &|_| {})), "8193 px is over the limit");
        assert!(!supports(&recipe("photo", "png", 64, &|r| r.video_aspect_ratio = Some("9:16".into()))));
        assert!(!supports(&recipe("photo", "png", 64, &|r| r.video_framing = Some("fit".into()))));
        assert!(!supports(&recipe("photo", "png", 64, &|r| r.video_stabilization = Some(true))));
        assert!(!supports(&recipe("chapters", "json", 64, &|r| r.allow_upscale = Some(true))));
        assert!(!supports(&recipe("video", "mp4", 1920, &|r| r.video_aspect_ratio = Some("10:3".into()))));
        assert!(!supports(&recipe("video", "mp4", 1920, &|r| r.video_framing = Some("zoom".into()))));
    }

    #[test]
    fn mac_video_plan_cannot_execute_as_chapter_text_on_portable_adapter() {
        let mut fixture = Fixture::new();
        let request = fixture.request("png");
        let preview = execute(&mut fixture.conn, &request).unwrap();
        let id = preview.operation_id.clone().unwrap();
        fixture.conn.execute("UPDATE catalog_operations SET plan_json=json_set(plan_json,'$.recipe.kind','video','$.recipe.format','mp4','$.recipe.maxDimension',1280) WHERE id=?1",[&id]).unwrap();
        let error = execute(&mut fixture.conn, &action(Some(id.clone()), "execute")).unwrap_err();
        assert!(
            error.to_string().contains("unavailable on this platform")
                || error.to_string().contains("Video inputs must be")
        );
        assert_eq!(
            fixture
                .conn
                .query_row("SELECT state FROM catalog_operations WHERE id=?1", [&id], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "preview"
        );
        assert!(!Path::new(&preview.outputs[0].output_path).exists());
        assert_eq!(
            fixture
                .conn
                .query_row("SELECT COUNT(*) FROM catalog_assets", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    #[test]
    fn conversion_validates_outputs_links_derivatives_and_undo_is_recoverable() {
        let mut fixture = Fixture::new();
        let request = fixture.request("jpeg");
        let original = hash(&fixture.source).unwrap();
        let preview = execute(&mut fixture.conn, &request).unwrap();
        let exported = execute(&mut fixture.conn, &action(preview.operation_id.clone(), "execute")).unwrap();
        assert_eq!(exported.status, "ok");
        let output = std::path::PathBuf::from(&exported.outputs[0].output_path);
        let image = image::open(&output).unwrap().to_rgb8();
        assert_eq!(image.dimensions(), (16, 8));
        assert_eq!(*image.get_pixel(0, 0), image::Rgb([255, 255, 255]));
        assert_eq!(hash(&fixture.source).unwrap(), original);
        assert_eq!(
            fixture
                .conn
                .query_row("SELECT COUNT(*) FROM catalog_assets", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        let history = execute(&mut fixture.conn, &action(None, "history")).unwrap();
        assert_eq!(history.operation_id, preview.operation_id);
        let undone = execute(&mut fixture.conn, &action(preview.operation_id, "undo")).unwrap();
        assert_eq!(undone.outputs[0].state, "undone");
        assert!(!output.exists());
        assert_eq!(hash(&fixture.source).unwrap(), original);
        assert_eq!(
            fixture
                .conn
                .query_row("SELECT COUNT(*) FROM catalog_assets", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    #[test]
    fn video_preview_execute_and_undo_keep_original_and_recovery() {
        if std::env::var_os("FILEID_FFMPEG_DIR").is_none() {
            return;
        }
        let mut fixture = Fixture::new();
        let source = fixture.root.join("Portrait.mp4");
        super::super::video_export::generate_test_video(&source);
        fixture
            .conn
            .execute(
                "UPDATE files SET path_text=?1,kind='video',extension='mp4' WHERE id=1",
                [source.to_str().unwrap()],
            )
            .unwrap();
        let original = hash(&source).unwrap();
        let mut request = fixture.request("mp4");
        request.recipe = Some(ToolRecipe {
            kind: "video".into(),
            format: "mp4".into(),
            max_dimension: 1280,
            allow_upscale: None,
            video_aspect_ratio: Some("16:9".into()),
            video_framing: None,
            video_stabilization: None,
            trim_start_seconds: None,
            trim_end_seconds: None,
            trim_mode: None,
            rotation_degrees: None,
        });
        let preview = execute(&mut fixture.conn, &request).unwrap();
        assert!(preview.outputs[0].output_path.contains("16x9 Fit"));
        let exported = execute(&mut fixture.conn, &action(preview.operation_id.clone(), "execute")).unwrap();
        assert_eq!(exported.status, "ok", "{}", exported.message);
        let output = std::path::PathBuf::from(&exported.outputs[0].output_path);
        let inspected = super::super::video_export::probe(&output).unwrap();
        assert_eq!((inspected.width, inspected.height), (1280, 720));
        assert_eq!(inspected.audio_count, 1);
        assert_eq!(hash(&source).unwrap(), original);
        assert_eq!(
            fixture
                .conn
                .query_row("SELECT COUNT(*) FROM catalog_assets", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        let undone = execute(&mut fixture.conn, &action(preview.operation_id, "undo")).unwrap();
        assert_eq!(undone.outputs[0].state, "undone");
        assert!(!output.exists());
        assert_eq!(hash(&source).unwrap(), original);
        assert_eq!(
            fixture
                .conn
                .query_row("SELECT COUNT(*) FROM catalog_assets", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    #[test]
    fn cancelled_video_never_publishes_or_changes_original() {
        if std::env::var_os("FILEID_FFMPEG_DIR").is_none() {
            return;
        }
        let mut fixture = Fixture::new();
        let source = fixture.root.join("Portrait.mp4");
        super::super::video_export::generate_test_video(&source);
        fixture
            .conn
            .execute(
                "UPDATE files SET path_text=?1,kind='video',extension='mp4' WHERE id=1",
                [source.to_str().unwrap()],
            )
            .unwrap();
        let original = hash(&source).unwrap();
        let mut request = fixture.request("mp4");
        request.recipe = Some(ToolRecipe {
            kind: "video".into(),
            format: "mp4".into(),
            max_dimension: 1280,
            allow_upscale: None,
            video_aspect_ratio: Some("16:9".into()),
            video_framing: None,
            video_stabilization: None,
            trim_start_seconds: None,
            trim_end_seconds: None,
            trim_mode: None,
            rotation_degrees: None,
        });
        let preview = execute(&mut fixture.conn, &request).unwrap();
        let id = preview.operation_id.clone().unwrap();
        let canceller = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                if cancel_active(&action(Some(id.clone()), "cancel")).is_ok() {
                    break;
                }
                assert!(std::time::Instant::now() < deadline, "export was never registered");
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        });
        let result = execute(&mut fixture.conn, &action(preview.operation_id, "execute"));
        canceller.join().unwrap();
        let message = match result {
            Ok(response) => response.message,
            Err(error) => error.to_string(),
        };
        assert!(message.contains("cancelled"), "{message}");
        assert!(!Path::new(&preview.outputs[0].output_path).exists());
        assert_eq!(hash(&source).unwrap(), original);
        assert_eq!(
            fixture
                .conn
                .query_row("SELECT COUNT(*) FROM catalog_assets", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            fs::read_dir(&fixture.root)
                .unwrap()
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.file_name().to_string_lossy().starts_with(".FileIDExport-"))
                .count(),
            0
        );
    }
    #[test]
    fn cancelled_staged_video_removes_partial_output() {
        if std::env::var_os("FILEID_FFMPEG_DIR").is_none() {
            return;
        }
        let mut fixture = Fixture::new();
        let source = fixture.root.join("Portrait.mp4");
        super::super::video_export::generate_test_video(&source);
        fixture
            .conn
            .execute(
                "UPDATE files SET path_text=?1,kind='video',extension='mp4' WHERE id=1",
                [source.to_str().unwrap()],
            )
            .unwrap();
        let original = hash(&source).unwrap();
        let mut request = fixture.request("mp4");
        request.recipe = Some(ToolRecipe {
            kind: "video".into(),
            format: "mp4".into(),
            max_dimension: 1920,
            allow_upscale: None,
            video_aspect_ratio: Some("16:9".into()),
            video_framing: None,
            video_stabilization: None,
            trim_start_seconds: None,
            trim_end_seconds: None,
            trim_mode: None,
            rotation_degrees: None,
        });
        let preview = execute(&mut fixture.conn, &request).unwrap();
        let id = preview.operation_id.clone().unwrap();
        let root = fixture.root.clone();
        let canceller = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let staged = fs::read_dir(&root)
                    .unwrap()
                    .filter_map(|entry| entry.ok())
                    .any(|entry| entry.file_name().to_string_lossy().starts_with(".FileIDExport-"));
                if staged {
                    cancel_active(&action(Some(id.clone()), "cancel")).unwrap();
                    break;
                }
                assert!(std::time::Instant::now() < deadline, "export never staged an output");
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        });
        let result = execute(&mut fixture.conn, &action(preview.operation_id, "execute"));
        canceller.join().unwrap();
        let response = result.unwrap();
        assert_eq!(response.status, "error");
        assert!(response.message.contains("cancelled"));
        assert!(!Path::new(&preview.outputs[0].output_path).exists());
        assert_eq!(hash(&source).unwrap(), original);
        assert_eq!(
            fs::read_dir(&fixture.root)
                .unwrap()
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.file_name().to_string_lossy().starts_with(".FileIDExport-"))
                .count(),
            0
        );
    }
    #[test]
    fn execution_rejects_changed_sources_and_occupied_outputs() {
        let mut fixture = Fixture::new();
        let request = fixture.request("png");
        let preview = execute(&mut fixture.conn, &request).unwrap();
        let output = Path::new(&preview.outputs[0].output_path);
        fs::write(output, b"Existing").unwrap();
        assert!(execute(&mut fixture.conn, &action(preview.operation_id.clone(), "execute")).is_err());
        assert_eq!(fs::read(output).unwrap(), b"Existing");
        fs::remove_file(output).unwrap();
        fs::write(&fixture.source, b"Changed").unwrap();
        assert!(execute(&mut fixture.conn, &action(preview.operation_id, "execute")).is_err());
        assert!(!output.exists());
    }
    #[test]
    fn undo_keeps_edited_exports_and_rejects_protected_destinations() {
        let mut fixture = Fixture::new();
        let request = fixture.request("png");
        let preview = execute(&mut fixture.conn, &request).unwrap();
        let exported = execute(&mut fixture.conn, &action(preview.operation_id.clone(), "execute")).unwrap();
        let output = Path::new(&exported.outputs[0].output_path);
        fs::write(output, b"Edited").unwrap();
        assert!(execute(&mut fixture.conn, &action(preview.operation_id, "undo")).is_err());
        assert_eq!(fs::read(output).unwrap(), b"Edited");
        let mut protected = request;
        protected.destination = Some("/Volumes/Adlon".into());
        assert!(execute(&mut fixture.conn, &protected).is_err());
    }
    #[test]
    fn restart_cleanup_preserves_unrelated_files_and_sources() {
        let fixture = Fixture::new();
        let stage = fixture
            .root
            .join(format!(".FileIDExport-{}.part", uuid::Uuid::new_v4()));
        let unrelated = fixture.root.join(".FileIDExport-user.part");
        fs::write(&stage, b"Partial output").unwrap();
        fs::write(&unrelated, b"Unrelated").unwrap();
        let original = hash(&fixture.source).unwrap();
        let item = Item {
            output: ToolOutput {
                file_id: 1,
                source_path: fixture.source.to_string_lossy().into_owned(),
                output_path: fixture.root.join("Export.png").to_string_lossy().into_owned(),
                state: "pending".into(),
                message: String::new(),
            },
            source_hash: original.clone(),
            chapters: vec![],
        };
        let plan = Plan {
            version: 1,
            r#type: "export".into(),
            recipe: ToolRecipe {
                kind: "photo".into(),
                format: "png".into(),
                max_dimension: 16,
                allow_upscale: None,
                video_aspect_ratio: None,
                video_framing: None,
                video_stabilization: None,
                trim_start_seconds: None,
                trim_end_seconds: None,
                trim_mode: None,
                rotation_degrees: None,
            },
            items: vec![item],
            stage_paths: Some(vec![
                stage.to_string_lossy().into_owned(),
                unrelated.to_string_lossy().into_owned(),
            ]),
        };
        fixture.conn.execute("INSERT INTO catalog_operations(id,plan_json,inverse_json,state,created_at) VALUES('interrupted',?1,'[]','running',0)",[serde_json::to_string(&plan).unwrap()]).unwrap();
        recover(&fixture.conn).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
        while stage.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(!stage.exists());
        assert!(unrelated.exists());
        assert_eq!(hash(&fixture.source).unwrap(), original);
    }

    // A leftover stage path that was replaced by a link to the user's file must not be followed and deleted.
    #[cfg(unix)]
    #[test]
    fn restart_cleanup_never_deletes_what_a_stage_path_links_to() {
        let fixture = Fixture::new();
        let aliased = fixture.root.join(format!(".FileIDExport-{}.part", uuid::Uuid::new_v4()));
        std::os::unix::fs::symlink(&fixture.source, &aliased).unwrap();
        let real = fixture.root.join(format!(".FileIDExport-{}.part", uuid::Uuid::new_v4()));
        fs::write(&real, b"Partial output").unwrap();
        let original = hash(&fixture.source).unwrap();
        let plan = Plan {
            version: 1,
            r#type: "export".into(),
            recipe: ToolRecipe {
                kind: "photo".into(),
                format: "png".into(),
                max_dimension: 16,
                allow_upscale: None,
                video_aspect_ratio: None,
                video_framing: None,
                video_stabilization: None,
                trim_start_seconds: None,
                trim_end_seconds: None,
                trim_mode: None,
                rotation_degrees: None,
            },
            items: vec![Item {
                output: ToolOutput {
                    file_id: 1,
                    source_path: fixture.source.to_string_lossy().into_owned(),
                    output_path: fixture.root.join("Export.png").to_string_lossy().into_owned(),
                    state: "pending".into(),
                    message: String::new(),
                },
                source_hash: original.clone(),
                chapters: vec![],
            }],
            // The link comes first, so it has been looked at by the time the real stage file is gone.
            stage_paths: Some(vec![aliased.to_string_lossy().into_owned(), real.to_string_lossy().into_owned()]),
        };
        fixture.conn.execute("INSERT INTO catalog_operations(id,plan_json,inverse_json,state,created_at) VALUES('interrupted',?1,'[]','running',0)",[serde_json::to_string(&plan).unwrap()]).unwrap();
        recover(&fixture.conn).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
        while real.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(!real.exists(), "the real stage file is cleaned up");
        assert!(fs::symlink_metadata(&aliased).is_ok_and(|m| m.file_type().is_symlink()), "the link is left alone");
        assert_eq!(hash(&fixture.source).unwrap(), original, "the user's file is untouched");
        let state: String = fixture.conn.query_row("SELECT state FROM catalog_operations WHERE id='interrupted'", [], |r| r.get(0)).unwrap();
        assert_eq!(state, "failed", "an interrupted export is marked failed, not left running");
    }

    struct CrossDeviceUndo {
        copy_error: bool,
        delete_error: bool,
        cleanup_error: bool,
    }
    impl crate::util::file_move::MoveFileIo for CrossDeviceUndo {
        fn link_source(&self, _: &Path, _: &Path) -> std::io::Result<()> {
            Err(std::io::ErrorKind::CrossesDevices.into())
        }
        fn copy(&self, source: &mut File, target: &mut File) -> std::io::Result<u64> {
            if self.copy_error {
                Err(std::io::ErrorKind::StorageFull.into())
            } else {
                std::io::copy(source, target)
            }
        }
        fn remove_source(&self, path: &Path) -> std::io::Result<()> {
            if self.delete_error {
                Err(std::io::ErrorKind::PermissionDenied.into())
            } else {
                fs::remove_file(path)
            }
        }
        fn remove_stage(&self, path: &Path) -> std::io::Result<()> {
            if self.cleanup_error {
                Err(std::io::ErrorKind::PermissionDenied.into())
            } else {
                fs::remove_file(path)
            }
        }
    }
    fn exported_fixture() -> (Fixture, String, std::path::PathBuf) {
        let mut fixture = Fixture::new();
        let request = fixture.request("png");
        let preview = execute(&mut fixture.conn, &request).unwrap();
        let id = preview.operation_id.unwrap();
        let exported = execute(&mut fixture.conn, &action(Some(id.clone()), "execute")).unwrap();
        (fixture, id, std::path::PathBuf::from(&exported.outputs[0].output_path))
    }
    #[test]
    fn undo_publication_race_cannot_clobber_even_identical_unowned_bytes() {
        for identical in [false, true] {
            let (mut fixture, id, output) = exported_fixture();
            let bytes = fs::read(&output).unwrap();
            let original = hash(&fixture.source).unwrap();
            let result = execute_with_move(
                &mut fixture.conn,
                &action(Some(id.clone()), "undo"),
                |source, target, published| {
                    fs::write(
                        target,
                        if identical {
                            bytes.as_slice()
                        } else {
                            b"racing occupant"
                        },
                    )?;
                    crate::util::file_move::move_no_clobber(source, target, published)
                },
            );
            assert!(result.is_err());
            assert_eq!(fs::read(&output).unwrap(), bytes);
            let (_, receipts, state) = load(&fixture.conn, &id).unwrap();
            assert_eq!(state, "completed");
            assert_eq!(receipts[0].recovery_published, Some(false));
            let derived = receipts[0].derived_id.unwrap();
            let target = Path::new(receipts[0].recovery_path.as_deref().unwrap());
            assert_eq!(
                fs::read(target).unwrap(),
                if identical {
                    bytes.as_slice()
                } else {
                    b"racing occupant"
                }
            );
            assert!(execute(&mut fixture.conn, &action(Some(id.clone()), "undo")).is_err());
            assert!(output.exists());
            assert_eq!(
                fixture
                    .conn
                    .query_row("SELECT COUNT(*) FROM catalog_assets", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                1
            );
            fs::remove_file(&output).unwrap();
            fixture.reopen();
            assert!(execute(&mut fixture.conn, &action(Some(id.clone()), "undo")).is_err());
            let (_, after, state) = load(&fixture.conn, &id).unwrap();
            assert_eq!(state, "completed");
            assert_eq!(after[0].output.state, "completed");
            assert_eq!(after[0].recovery_published, Some(false));
            assert_eq!(
                fixture
                    .conn
                    .query_row(
                        "SELECT COUNT(*) FROM catalog_assets WHERE original_id=1 AND derived_id=?1",
                        [derived],
                        |row| row.get::<_, i64>(0)
                    )
                    .unwrap(),
                1
            );
            assert_eq!(
                fixture
                    .conn
                    .query_row("SELECT COUNT(*) FROM files WHERE id=?1", [derived], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                1
            );
            assert_eq!(
                fs::read(target).unwrap(),
                if identical {
                    bytes.as_slice()
                } else {
                    b"racing occupant"
                }
            );
            assert_eq!(hash(&fixture.source).unwrap(), original);
        }
    }
    #[test]
    fn undo_legacy_missing_marker_cannot_adopt_unowned_identical_recovery() {
        let (mut fixture, id, output) = exported_fixture();
        let bytes = fs::read(&output).unwrap();
        let original = hash(&fixture.source).unwrap();
        assert!(execute_with_move(
            &mut fixture.conn,
            &action(Some(id.clone()), "undo"),
            |source, target, published| {
                fs::write(target, &bytes)?;
                crate::util::file_move::move_no_clobber(source, target, published)
            }
        )
        .is_err());
        let (_, mut receipts, state) = load(&fixture.conn, &id).unwrap();
        let derived = receipts[0].derived_id.unwrap();
        let recovery = std::path::PathBuf::from(receipts[0].recovery_path.as_deref().unwrap());
        receipts[0].recovery_published = None;
        persist(&fixture.conn, &id, &receipts, &state).unwrap();
        let inverse: String = fixture
            .conn
            .query_row(
                "SELECT inverse_json FROM catalog_operations WHERE id=?1",
                [&id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(serde_json::from_str::<serde_json::Value>(&inverse).unwrap()[0]
            .get("recoveryPublished")
            .is_none());
        fs::remove_file(&output).unwrap();
        fixture.reopen();
        assert!(execute(&mut fixture.conn, &action(Some(id.clone()), "undo"))
            .unwrap_err()
            .to_string()
            .contains("verified owned recovery"));
        let (_, after, state) = load(&fixture.conn, &id).unwrap();
        assert_eq!(state, "completed");
        assert_eq!(after[0].output.state, "completed");
        assert_eq!(after[0].recovery_published, None);
        assert_eq!(fs::read(&recovery).unwrap(), bytes);
        assert_eq!(
            fixture
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM catalog_assets WHERE original_id=1 AND derived_id=?1",
                    [derived],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        assert_eq!(
            fixture
                .conn
                .query_row("SELECT COUNT(*) FROM files WHERE id=?1", [derived], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(hash(&fixture.source).unwrap(), original);
    }
    #[test]
    fn undo_verified_owned_missing_source_recovers_after_restart() {
        let (mut fixture, id, output) = exported_fixture();
        let bytes = fs::read(&output).unwrap();
        let original = hash(&fixture.source).unwrap();
        assert!(execute_with_move(
            &mut fixture.conn,
            &action(Some(id.clone()), "undo"),
            |source, target, published| {
                crate::util::file_move::move_no_clobber(source, target, published)?;
                Err(std::io::Error::other("Simulated interruption after file movement"))
            }
        )
        .is_err());
        let (_, receipts, state) = load(&fixture.conn, &id).unwrap();
        assert_eq!(state, "completed");
        assert_eq!(receipts[0].recovery_published, Some(true));
        let recovery = std::path::PathBuf::from(receipts[0].recovery_path.as_deref().unwrap());
        assert!(!output.exists());
        assert_eq!(fs::read(&recovery).unwrap(), bytes);
        fixture.reopen();
        let result = execute(&mut fixture.conn, &action(Some(id.clone()), "undo")).unwrap();
        assert_eq!(result.outputs[0].state, "undone");
        assert_eq!(load(&fixture.conn, &id).unwrap().2, "undone");
        assert_eq!(fs::read(&recovery).unwrap(), bytes);
        assert_eq!(
            fixture
                .conn
                .query_row("SELECT COUNT(*) FROM catalog_assets", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(hash(&fixture.source).unwrap(), original);
    }
    #[test]
    fn undo_revalidates_source_after_publication_and_keeps_changed_bytes() {
        let (mut fixture, id, output) = exported_fixture();
        let original = hash(&fixture.source).unwrap();
        let result = execute_with_move(
            &mut fixture.conn,
            &action(Some(id.clone()), "undo"),
            |source, target, published| {
                fs::write(source, b"edited during undo")?;
                crate::util::file_move::move_no_clobber(source, target, published)
            },
        );
        assert!(result.is_err());
        assert_eq!(fs::read(&output).unwrap(), b"edited during undo");
        let (_, receipts, state) = load(&fixture.conn, &id).unwrap();
        assert_eq!(state, "completed");
        assert_eq!(receipts[0].recovery_published, Some(true));
        assert_eq!(
            fs::read(receipts[0].recovery_path.as_deref().unwrap()).unwrap(),
            b"edited during undo"
        );
        assert!(execute(&mut fixture.conn, &action(Some(id), "undo")).is_err());
        assert_eq!(hash(&fixture.source).unwrap(), original);
    }
    #[test]
    fn undo_cross_device_copy_error_preserves_receipt_source_and_retry() {
        let (mut fixture, id, output) = exported_fixture();
        let bytes = fs::read(&output).unwrap();
        let original = hash(&fixture.source).unwrap();
        let io = CrossDeviceUndo {
            copy_error: true,
            delete_error: false,
            cleanup_error: false,
        };
        assert!(execute_with_move(
            &mut fixture.conn,
            &action(Some(id.clone()), "undo"),
            |source, target, published| crate::util::file_move::move_with_io(source, target, &io, published)
        )
        .is_err());
        let (_, receipts, state) = load(&fixture.conn, &id).unwrap();
        assert_eq!(state, "completed");
        assert_eq!(receipts[0].recovery_published, Some(false));
        assert!(!Path::new(receipts[0].recovery_path.as_deref().unwrap()).exists());
        assert_eq!(fs::read(&output).unwrap(), bytes);
        assert_eq!(
            execute(&mut fixture.conn, &action(Some(id), "undo")).unwrap().outputs[0].state,
            "undone"
        );
        assert!(!output.exists());
        assert_eq!(hash(&fixture.source).unwrap(), original);
    }
    #[test]
    fn undo_published_receipts_recover_after_restart_and_cleanup_or_unlink_failure() {
        for cleanup in [false, true] {
            let (mut fixture, id, output) = exported_fixture();
            let bytes = fs::read(&output).unwrap();
            let original = hash(&fixture.source).unwrap();
            let io = CrossDeviceUndo {
                copy_error: false,
                delete_error: !cleanup,
                cleanup_error: cleanup,
            };
            assert!(execute_with_move(
                &mut fixture.conn,
                &action(Some(id.clone()), "undo"),
                |source, target, published| crate::util::file_move::move_with_io(source, target, &io, published)
            )
            .is_err());
            let (_, receipts, state) = load(&fixture.conn, &id).unwrap();
            assert_eq!(state, "completed");
            assert_eq!(receipts[0].output.state, "completed");
            assert_eq!(receipts[0].recovery_published, Some(true));
            let recovery = std::path::PathBuf::from(receipts[0].recovery_path.as_deref().unwrap());
            assert_eq!(fs::read(&output).unwrap(), bytes);
            assert_eq!(fs::read(&recovery).unwrap(), bytes);
            assert_eq!(
                fixture
                    .conn
                    .query_row("SELECT COUNT(*) FROM catalog_assets", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                1
            );
            let conn = std::mem::replace(&mut fixture.conn, Connection::open_in_memory().unwrap());
            conn.close().unwrap();
            fixture.conn = crate::db::open_writer(&fixture.root.join("catalog.sqlite")).unwrap();
            let result = execute(&mut fixture.conn, &action(Some(id), "undo")).unwrap();
            assert_eq!(result.outputs[0].state, "undone");
            assert!(!output.exists());
            assert_eq!(fs::read(&recovery).unwrap(), bytes);
            assert_eq!(
                fixture
                    .conn
                    .query_row("SELECT COUNT(*) FROM catalog_assets", [], |row| row.get::<_, i64>(0))
                    .unwrap(),
                0
            );
            assert_eq!(hash(&fixture.source).unwrap(), original);
        }
    }
    #[test]
    fn undo_changed_published_recovery_is_not_deleted_or_claimed_complete() {
        let (mut fixture, id, output) = exported_fixture();
        let bytes = fs::read(&output).unwrap();
        let io = CrossDeviceUndo {
            copy_error: false,
            delete_error: true,
            cleanup_error: false,
        };
        assert!(execute_with_move(
            &mut fixture.conn,
            &action(Some(id.clone()), "undo"),
            |source, target, published| crate::util::file_move::move_with_io(source, target, &io, published)
        )
        .is_err());
        let (_, receipts, _) = load(&fixture.conn, &id).unwrap();
        let recovery = Path::new(receipts[0].recovery_path.as_deref().unwrap());
        fs::write(recovery, b"edited recovery").unwrap();
        assert!(execute(&mut fixture.conn, &action(Some(id.clone()), "undo")).is_err());
        assert_eq!(fs::read(&output).unwrap(), bytes);
        assert_eq!(fs::read(recovery).unwrap(), b"edited recovery");
        assert_eq!(load(&fixture.conn, &id).unwrap().1[0].output.state, "completed");
    }

    #[test]
    fn chapter_export_escapes_cues_and_uses_nonzero_intervals() {
        let c = CatalogChapter {
            id: "gift".into(),
            file_id: 1,
            start_seconds: 1.234,
            end_seconds: 1.234,
            title: "Gift <opening> -->\nGrandma".into(),
            summary: String::new(),
            source_revision: "fixture".into(),
            model_version: "user".into(),
            confidence: 1.0,
            user_edited: true,
            stale: false,
        };
        let text = String::from_utf8(export_chapters(std::slice::from_ref(&c), "vtt").unwrap()).unwrap();
        assert!(text.contains("00:00:01.234 --> 00:00:01.235"));
        assert!(text.contains("Gift &lt;opening&gt; → Grandma"));
        let decoded: Vec<CatalogChapter> = serde_json::from_slice(&export_chapters(&[c], "json").unwrap()).unwrap();
        assert_eq!(decoded[0].source_revision, "fixture");
    }

    #[test]
    fn a_chapter_export_refuses_markers_edited_added_or_deleted_after_its_preview() {
        let mut fixture = Fixture::new();
        fixture
            .conn
            .execute("INSERT INTO catalog_chapters(id,file_id,start_seconds,end_seconds,title,summary,source_revision,model_version,confidence,user_edited,stale) VALUES('gift',1,1,2,'Gift','','r','user',1,1,0)", [])
            .unwrap();
        let mut request = fixture.request("vtt");
        request.recipe.as_mut().unwrap().kind = "chapters".into();
        let edits = [
            "UPDATE catalog_chapters SET title='Cake' WHERE id='gift'",
            "INSERT INTO catalog_chapters(id,file_id,start_seconds,end_seconds,title,summary,source_revision,model_version,confidence,user_edited,stale) VALUES('song',1,3,4,'Song','','r','user',1,1,0)",
            "DELETE FROM catalog_chapters WHERE id='song'",
        ];
        for edit in edits {
            let preview = execute(&mut fixture.conn, &request).unwrap();
            let output = std::path::PathBuf::from(&preview.outputs[0].output_path);
            fixture.conn.execute(edit, []).unwrap();
            let message = match execute(&mut fixture.conn, &action(preview.operation_id.clone(), "execute")) {
                Ok(result) => {
                    assert_eq!(result.status, "error", "{edit}");
                    result.message
                }
                Err(error) => error.to_string(),
            };
            assert!(message.contains("Chapter markers changed"), "{edit}: {message}");
            assert!(!output.exists(), "{edit}: nothing is written");
        }
        let preview = execute(&mut fixture.conn, &request).unwrap();
        let exported = execute(&mut fixture.conn, &action(preview.operation_id, "execute")).unwrap();
        assert_eq!(exported.status, "ok", "{}", exported.message);
        let text = fs::read_to_string(&exported.outputs[0].output_path).unwrap();
        assert!(text.contains("Cake") && !text.contains("Song"), "{text}");
    }

    /// A JPEG whose EXIF says "rotate 90 degrees clockwise to display" and carries a GPS latitude.
    fn rotated_jpeg_with_location(path: &Path) {
        let mut jpeg = Vec::new();
        image::RgbImage::from_fn(32, 16, |x, _| if x < 8 { image::Rgb([255, 255, 255]) } else { image::Rgb([0, 0, 0]) })
            .write_to(&mut std::io::Cursor::new(&mut jpeg), ImageFormat::Jpeg)
            .unwrap();
        let mut tiff: Vec<u8> = b"II*\0".to_vec();
        tiff.extend(8u32.to_le_bytes());
        tiff.extend(2u16.to_le_bytes());
        tiff.extend([0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0]);
        tiff.extend([0x25, 0x88, 4, 0, 1, 0, 0, 0]);
        tiff.extend(38u32.to_le_bytes());
        tiff.extend(0u32.to_le_bytes());
        tiff.extend(1u16.to_le_bytes());
        tiff.extend([1, 0, 2, 0, 2, 0, 0, 0, b'N', 0, 0, 0]);
        tiff.extend(0u32.to_le_bytes());
        let mut segment = vec![0xFF, 0xE1];
        segment.extend(((2 + 6 + tiff.len()) as u16).to_be_bytes());
        segment.extend(b"Exif\0\0");
        segment.extend(tiff);
        let mut bytes = jpeg[..2].to_vec();
        bytes.extend(segment);
        bytes.extend(&jpeg[2..]);
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn a_photo_export_applies_the_exif_orientation_and_keeps_no_location() {
        let mut fixture = Fixture::new();
        let source = fixture.root.join("Rotated.jpg");
        rotated_jpeg_with_location(&source);
        let mut decoder = ImageReader::open(&source).unwrap().with_guessed_format().unwrap().into_decoder().unwrap();
        assert_eq!(decoder.orientation().unwrap(), image::metadata::Orientation::Rotate90, "the fixture carries its EXIF");
        fixture.conn.execute("UPDATE files SET path_text=?1 WHERE id=1", [source.to_str().unwrap()]).unwrap();
        let original = fs::read(&source).unwrap();
        for format in ["jpeg", "png"] {
            let request = fixture.request(format);
            let preview = execute(&mut fixture.conn, &request).unwrap();
            let exported = execute(&mut fixture.conn, &action(preview.operation_id, "execute")).unwrap();
            assert_eq!(exported.status, "ok", "{}", exported.message);
            let output = fs::read(&exported.outputs[0].output_path).unwrap();
            let image = image::load_from_memory(&output).unwrap();
            assert_eq!((image.width(), image.height()), (8, 16), "{format}: turned upright, then fitted in 16 pixels");
            let pixels = image.to_rgb8();
            assert!(pixels.get_pixel(4, 1)[0] > 200 && pixels.get_pixel(4, 14)[0] < 60, "{format}: the white band is turned to the top");
            assert!(!output.windows(4).any(|window| window == b"Exif"), "{format}: no EXIF block, so no location");
        }
        assert_eq!(fs::read(&source).unwrap(), original, "the source keeps its bytes");
    }

    #[test]
    fn an_export_stores_the_recipe_it_ran_beside_its_lineage() {
        let mut fixture = Fixture::new();
        let request = fixture.request("png");
        let preview = execute(&mut fixture.conn, &request).unwrap();
        let exported = execute(&mut fixture.conn, &action(preview.operation_id, "execute")).unwrap();
        assert_eq!(exported.status, "ok", "{}", exported.message);
        let (original, role, recipe): (i64, String, String) = fixture
            .conn
            .query_row("SELECT original_id,role,recipe_json FROM catalog_assets", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap();
        assert_eq!((original, role.as_str()), (1, "export"));
        let stored: serde_json::Value = serde_json::from_str(&recipe).unwrap();
        assert_eq!(stored, serde_json::to_value(request.recipe.as_ref().unwrap()).unwrap());
        assert_eq!(serde_json::from_value::<ToolRecipe>(stored).unwrap().max_dimension, 16);
    }

    use media_recipes::{MediaProbe, MediaRuntime};

    struct FakeMedia {
        source_path: std::path::PathBuf,
        source: MediaProbe,
        output: MediaProbe,
        args: Mutex<Vec<Vec<String>>>,
    }
    impl MediaRuntime for FakeMedia {
        fn probe(&self, path: &Path) -> Result<MediaProbe> {
            Ok(if path == self.source_path { self.source.clone() } else { self.output.clone() })
        }
        fn run(&self, args: &[String], mut stage: &File, cancelled: &AtomicBool) -> Result<()> {
            if cancelled.load(Ordering::Acquire) {
                bail!("Export cancelled before publication")
            }
            self.args.lock().push(args.to_vec());
            stage.write_all(b"generated media")?;
            Ok(())
        }
    }
    struct InstalledMedia;
    impl InstalledMedia {
        fn new(fake: Arc<FakeMedia>) -> Self {
            media_recipes::TEST_RUNTIME.with(|slot| *slot.borrow_mut() = Some(fake));
            Self
        }
    }
    impl Drop for InstalledMedia {
        fn drop(&mut self) {
            media_recipes::TEST_RUNTIME.with(|slot| *slot.borrow_mut() = None);
        }
    }

    fn media_fixture() -> Fixture {
        let fixture = Fixture::new();
        let clip = fixture.root.join("Clip.mov");
        fs::write(&clip, b"source clip bytes").unwrap();
        fixture
            .conn
            .execute("UPDATE files SET path_text=?1,kind='video',extension='mov' WHERE id=1", [clip.to_str().unwrap()])
            .unwrap();
        fixture
    }

    fn media_request(fixture: &Fixture, recipe: ToolRecipe) -> ToolRequest {
        ToolRequest { recipe: Some(recipe), ..fixture.request("png") }
    }

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn media_recipes_refuse_before_any_output_without_the_vetted_runtime() {
        if media_recipes::runtime().is_some() {
            eprintln!("a media runtime is available; the unavailable path cannot be exercised");
            return;
        }
        let mut fixture = media_fixture();
        let invalid = ToolRecipe { rotation_degrees: Some(45), ..media_recipe("rotate", "mov") };
        let request = media_request(&fixture, invalid);
        let error = execute(&mut fixture.conn, &request).unwrap_err();
        assert_eq!(error.to_string(), media_recipes::message::ROTATION);
        let request = media_request(&fixture, media_recipe("remux", "mp4"));
        let error = execute(&mut fixture.conn, &request).unwrap_err();
        assert_eq!(error.to_string(), media_recipes::UNAVAILABLE);
        let capabilities = execute(&mut fixture.conn, &action(None, "capabilities")).unwrap().capabilities;
        let media = capabilities.iter().filter(|c| media_recipes::is_media(&c.id)).collect::<Vec<_>>();
        assert_eq!(media.len(), 5);
        assert!(media.iter().all(|c| !c.available && c.detail == media_recipes::UNAVAILABLE));
        assert_eq!(count(&fixture.conn, "SELECT COUNT(*) FROM catalog_operations"), 0);

        let clip = fixture.root.join("Clip.mov");
        fixture.conn.execute("UPDATE files SET path_text=?1,kind='image',extension='png' WHERE id=1", [fixture.source.to_str().unwrap()]).unwrap();
        let request = fixture.request("png");
        let preview = execute(&mut fixture.conn, &request).unwrap();
        let id = preview.operation_id.clone().unwrap();
        fixture.conn.execute("UPDATE catalog_operations SET plan_json=json_set(plan_json,'$.recipe.kind','remux','$.recipe.format','mp4') WHERE id=?1", [&id]).unwrap();
        let error = execute(&mut fixture.conn, &action(Some(id.clone()), "execute")).unwrap_err();
        assert_eq!(error.to_string(), media_recipes::UNAVAILABLE);
        assert_eq!(load(&fixture.conn, &id).unwrap().2, "preview");
        assert!(!Path::new(&preview.outputs[0].output_path).exists());
        assert_eq!(fs::read(clip).unwrap(), b"source clip bytes");
    }

    #[test]
    fn media_recipes_publish_verified_outputs_with_lineage_and_undo() {
        let source = clip_probe();
        let trimmed = MediaProbe { duration: 1.5, audio_duration: Some(1.5), ..source.clone() };
        let audio_only = MediaProbe { video_count: 0, video_codec: None, video_start: None, ..source.clone() };
        let proxy = MediaProbe { width: 64.0, height: 36.0, ..source.clone() };
        let rotated = MediaProbe { rotation: 90, width: 54.0, height: 96.0, ..source.clone() };
        let cases = [
            (media_recipe("remux", "mp4"), source.clone(), "Clip - Remux.mp4", "video", "conversion", "-c copy"),
            (
                ToolRecipe { trim_start_seconds: Some(1.0), trim_end_seconds: Some(2.5), trim_mode: Some("keyframe".into()), ..media_recipe("trim", "mov") },
                trimmed,
                "Clip - Trim.mov",
                "video",
                "conversion",
                "-ss 1.000 -t 1.500",
            ),
            (media_recipe("audio", "m4a"), audio_only, "Clip - Audio.m4a", "audio", "conversion", "-f ipod"),
            (ToolRecipe { max_dimension: 640, ..media_recipe("proxy", "mp4") }, proxy, "Clip - Proxy.mp4", "video", "proxy", "libopenh264"),
            (ToolRecipe { rotation_degrees: Some(90), ..media_recipe("rotate", "mov") }, rotated, "Clip - Rotated 90.mov", "video", "conversion", "-display_rotation:v:0 -90"),
        ];
        for (recipe, output, name, kind, role, flag) in cases {
            let mut fixture = media_fixture();
            let clip = fixture.root.join("Clip.mov");
            let fake = Arc::new(FakeMedia { source_path: clip.clone(), source: source.clone(), output, args: Mutex::new(vec![]) });
            let _installed = InstalledMedia::new(fake.clone());
            let request = media_request(&fixture, recipe.clone());
            let preview = execute(&mut fixture.conn, &request).unwrap();
            assert_eq!(preview.outputs[0].message, media_recipes::summary(&recipe));
            let planned = std::path::PathBuf::from(&preview.outputs[0].output_path);
            assert_eq!(planned.file_name().unwrap().to_str().unwrap(), name);
            assert!(!planned.exists());
            let exported = execute(&mut fixture.conn, &action(preview.operation_id.clone(), "execute")).unwrap();
            assert_eq!(exported.status, "ok", "{name}: {}", exported.message);
            assert!(exported.outputs[0].message.contains(media_recipes::summary(&recipe)));
            assert_eq!(fs::read(&planned).unwrap(), b"generated media");
            assert!(fake.args.lock()[0].join(" ").contains(flag), "{name}: {:?}", fake.args.lock());
            let catalogued: (String, String) = fixture.conn.query_row(
                "SELECT f.kind, a.role FROM catalog_assets a JOIN files f ON f.id=a.derived_id WHERE a.original_id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            ).unwrap();
            assert_eq!(catalogued, (kind.to_owned(), role.to_owned()));
            assert_eq!(fs::read(&clip).unwrap(), b"source clip bytes");
            let undone = execute(&mut fixture.conn, &action(preview.operation_id, "undo")).unwrap();
            assert_eq!(undone.outputs[0].state, "undone");
            assert!(!planned.exists());
            assert_eq!(count(&fixture.conn, "SELECT COUNT(*) FROM catalog_assets"), 0);
        }
    }

    #[test]
    fn media_verification_failure_never_publishes() {
        let mut fixture = media_fixture();
        let clip = fixture.root.join("Clip.mov");
        let source = clip_probe();
        let reencoded = MediaProbe { video_codec: Some("mpeg4".into()), ..source.clone() };
        let fake = Arc::new(FakeMedia { source_path: clip.clone(), source, output: reencoded, args: Mutex::new(vec![]) });
        let _installed = InstalledMedia::new(fake);
        let request = media_request(&fixture, media_recipe("remux", "mp4"));
        let preview = execute(&mut fixture.conn, &request).unwrap();
        let output = std::path::PathBuf::from(&preview.outputs[0].output_path);
        let failed = execute(&mut fixture.conn, &action(preview.operation_id.clone(), "execute")).unwrap();
        assert_eq!(failed.status, "error");
        assert_eq!(failed.message, "Output validation rejected re-encoded streams.");
        assert!(!output.exists());
        assert_eq!(load(&fixture.conn, preview.operation_id.as_deref().unwrap()).unwrap().2, "failed");
        assert_eq!(count(&fixture.conn, "SELECT COUNT(*) FROM catalog_assets"), 0);
        let leftovers = fs::read_dir(&fixture.root).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with(".FileIDExport-")).count();
        assert_eq!(leftovers, 0);
        assert_eq!(fs::read(&clip).unwrap(), b"source clip bytes");
    }

    #[test]
    fn a_killed_exports_writer_sibling_is_removed_with_its_part_file() {
        let dir = std::env::temp_dir().join(format!("fileid-orphan-stage-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let stage = dir.join(".FileIDExport-be29b13a-0000-4000-8000-000000000000.part");
        let sibling = dir.join(".FileIDExport-be29b13a-0000-4000-8000-000000000000.part.sb-3771399d-ceVlkX");
        let other_export = dir.join(".FileIDExport-ffffffff-0000-4000-8000-000000000000.part.sb-1");
        let user_file = dir.join("Clip.mp4");
        for file in [&stage, &sibling, &other_export, &user_file] {
            fs::write(file, b"bytes").unwrap();
        }
        remove_stage_files(&stage);
        assert!(!stage.exists() && !sibling.exists(), "the part file and its writer sibling are gone");
        assert!(other_export.exists() && user_file.exists(), "nothing else in the folder is touched");

        let lone = dir.join(".FileIDExport-aaaaaaaa-0000-4000-8000-000000000000.part");
        let lone_sibling = dir.join(".FileIDExport-aaaaaaaa-0000-4000-8000-000000000000.part.sb-9");
        fs::write(&lone_sibling, b"bytes").unwrap();
        remove_stage_files(&lone);
        assert!(!lone_sibling.exists(), "the sibling goes even when the part file itself is already gone");
        let _ = fs::remove_dir_all(&dir);
    }
}

pub(crate) fn recover(conn: &Connection) -> Result<()> {
    conn.execute("UPDATE catalog_operations SET state='failed' WHERE state='running' AND json_extract(plan_json,'$.type')='export'",[])?;
    let mut statement = conn.prepare(
        "SELECT plan_json FROM catalog_operations WHERE state='failed' AND json_extract(plan_json,'$.type')='export'",
    )?;
    let plans = statement
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter_map(|j| serde_json::from_str::<Plan>(&j).ok())
        .collect::<Vec<_>>();
    if plans.is_empty() {
        return Ok(());
    }
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(2));
        for plan in plans {
            for path in plan.stage_paths.unwrap_or_default() {
                let stage = Path::new(&path);
                let name = stage.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
                if stage.extension().and_then(|s| s.to_str()) != Some("part")
                    || name
                        .strip_prefix(".FileIDExport-")
                        .is_none_or(|s| uuid::Uuid::parse_str(s).is_err())
                {
                    continue;
                }
                if !plan
                    .items
                    .iter()
                    .any(|i| Path::new(&i.output.output_path).parent() == stage.parent())
                    || read_only::require_writable(stage).is_err()
                {
                    continue;
                }
                remove_stage_files(stage);
            }
        }
    });
    Ok(())
}

/// Removes a killed export's `.part` file and the `<name>.part.sb-<id>` sibling the Apple media writer keeps beside it.
fn remove_stage_files(stage: &Path) {
    let removable = |path: &Path| fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && !m.file_type().is_symlink());
    if removable(stage) {
        let _ = fs::remove_file(stage);
    }
    let (Some(parent), Some(name)) = (stage.parent(), stage.file_name().and_then(|n| n.to_str())) else {
        return;
    };
    let sibling_prefix = format!("{name}.sb-");
    let Ok(entries) = fs::read_dir(parent) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_name().to_str().is_some_and(|n| n.starts_with(&sibling_prefix)) && removable(&path) {
            let _ = fs::remove_file(path);
        }
    }
}
