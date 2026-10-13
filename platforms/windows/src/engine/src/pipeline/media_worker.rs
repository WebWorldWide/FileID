//! Isolated file-only media primitives. `run` owns transient admission; callers must not
//! reserve decoder RAM/CPU/I/O again. Resident inference budgets remain caller-owned.
use crate::resource_scheduler::{Demand, Priority, ResourceScheduler};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, UNIX_EPOCH},
};
#[cfg(windows)]
#[path = "native_media.rs"]
mod adapter;
#[cfg(not(windows))]
#[path = "linux_media.rs"]
mod adapter;
#[cfg(windows)]
#[path = "media_timing.rs"]
mod timing;

#[cfg(windows)]
#[allow(dead_code)]
pub const VIDEO_CAPABILITY_DETAIL: &str = "Native H.264/AAC SDR MP4 export, 1280 or 1920 pixels. One video and at most one audio track. HDR, ambiguous transfer metadata, auxiliary tracks and sync-changing inputs are refused. Some positive audio edit-list offsets cannot be preserved by the Windows adapter; not all SDR inputs convert. Original files remain unchanged.";
#[cfg(not(windows))]
#[allow(dead_code)]
pub const VIDEO_CAPABILITY_DETAIL: &str = "H.264/AAC SDR MP4 export through explicitly configured installed FFmpeg/FFprobe, 1280 or 1920 pixels. HDR, ambiguous transfer metadata, auxiliary tracks and timing changes are refused. No bundled or downloaded media runtime. Original files remain unchanged.";
#[cfg(windows)]
#[allow(dead_code)]
pub const PHOTO_INPUT_FORMATS: &[&str] = &["png", "jpeg", "tiff", "bmp", "heic"];
#[cfg(not(windows))]
#[allow(dead_code)]
pub const PHOTO_INPUT_FORMATS: &[&str] = &["png", "jpeg", "tiff", "bmp"];

/// Configuration readiness, not a guarantee that a particular source/codec converts.
#[allow(dead_code)]
pub fn video_backend_available() -> bool {
    #[cfg(windows)]
    {
        true
    }
    #[cfg(not(windows))]
    {
        adapter::available()
    }
}

#[allow(dead_code)]
const MAX_INPUT: u64 = 8 * 1024 * 1024 * 1024;
pub const MAX_OUTPUT: u64 = 512 * 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Operation {
    Probe,
    Frame {
        seconds: f64,
        max_dimension: u32,
    },
    Audio {
        start_seconds: f64,
        duration_seconds: f64,
    },
    Photo {
        format: PhotoFormat,
        max_dimension: u32,
    },
    Video {
        max_dimension: u32,
    },
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PhotoFormat {
    Png,
    Jpeg,
    Tiff,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Revision {
    pub size: u64,
    pub modified_ns: u128,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Probe {
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub video_codec: String,
    pub video_start: f64,
    pub audio_count: u32,
    pub audio_codec: Option<String>,
    pub audio_start: Option<f64>,
    pub audio_duration: Option<f64>,
    pub hdr: bool,
    pub rotation: u32,
    pub frame_rate: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    pub revision: Revision,
    pub probe: Option<Probe>,
    pub actual_seconds: Option<f64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub sample_rate: Option<u32>,
    pub samples: Option<u64>,
    pub bytes: u64,
}
/// Owns only a generated private staging directory. Publication remains the caller's
/// existing no-clobber protocol; dropping this value removes unconsumed staging bytes.
#[allow(dead_code)]
pub struct StagedMedia {
    pub report: Report,
    pub artifact: Option<PathBuf>,
    directory: PathBuf,
}
impl Drop for StagedMedia {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    source: PathBuf,
    operation: Operation,
    directory: PathBuf,
    token: String,
    timeout_ms: u64,
}

#[allow(dead_code)]
pub fn run(
    source: &Path,
    operation: Operation,
    cancelled: &AtomicBool,
    timeout: Duration,
) -> Result<StagedMedia> {
    validate_operation(&operation)?;
    sweep_abandoned_staging()?;
    if timeout.is_zero() || timeout > Duration::from_secs(3600) {
        bail!("Invalid media deadline");
    }
    let source = source.canonicalize()?;
    let _source_handle = open_source(&source)?;
    let revision = revision(&source)?;
    let deadline = std::time::Instant::now() + timeout;
    let _lease = ResourceScheduler::shared().reserve(
        Demand {
            memory_mb: 1024,
            cpu: 1,
            io: 1,
        },
        Priority::Interactive,
        || cancelled.load(Ordering::Relaxed) || std::time::Instant::now() >= deadline,
    )?;
    let token = uuid::Uuid::new_v4().to_string();
    let directory = std::env::temp_dir().join(format!("FileIDMedia-{token}"));
    crate::util::read_only::require_source_mutation(&directory)?;
    #[allow(unused_mut)]
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&directory)?;
    let mut staged = StagedMedia {
        report: empty_report(revision.clone()),
        artifact: None,
        directory: directory.clone(),
    };
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join("owner"))?
        .write_all(token.as_bytes())?;
    let request = Request {
        source,
        operation,
        directory,
        token,
        timeout_ms: deadline
            .saturating_duration_since(std::time::Instant::now())
            .as_millis()
            .min(3_600_000) as u64,
    };
    let executable = std::env::current_exe()?;
    #[cfg(test)]
    let executable = std::env::var_os("FILEID_MEDIA_TEST_ENGINE").map_or(executable, PathBuf::from);
    let mut command = std::process::Command::new(executable);
    command.args(["--media-worker", &std::process::id().to_string()]);
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    let output = crate::util::child_process::run(
        &mut command,
        &serde_json::to_vec(&request)?,
        cancelled,
        remaining,
        16 * 1024,
        32 * 1024,
    )?;
    let report: Report =
        serde_json::from_slice(&output.stdout).context("Invalid media worker response")?;
    if report.revision != revision
        || crate::pipeline::media_worker::revision(&request.source)? != revision
    {
        bail!("Source revision changed during media processing");
    }
    let artifact = artifact_path(&request);
    if let Some(path) = &artifact {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.file_type().is_file()
            || metadata.len() == 0
            || metadata.len() > MAX_OUTPUT
            || metadata.len() != report.bytes
        {
            bail!("Invalid staged media output");
        }
    } else if report.bytes != 0 {
        bail!("Probe unexpectedly reported output bytes");
    }
    staged.report = report;
    staged.artifact = artifact;
    Ok(staged)
}

/// Returns None for ordinary engine startup; worker mode never opens logging/state/DB.
pub fn dispatch_cli(arguments: &[String]) -> Option<Result<()>> {
    if arguments.get(1).map(String::as_str) != Some("--media-worker") {
        return None;
    }
    Some((|| {
        if arguments.len() != 3 {
            bail!("Invalid media worker arguments");
        }
        let parent: u32 = arguments[2].parse()?;
        if parent == 0 || parent == std::process::id() {
            bail!("Invalid worker parent");
        }
        watch_parent(parent)?;
        let awaiting_request = std::sync::Arc::new(AtomicBool::new(true));
        let waiting = awaiting_request.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(30));
            if waiting.load(Ordering::Relaxed) {
                std::process::exit(124);
            }
        });
        let mut bytes = Vec::new();
        std::io::stdin()
            .take(16 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        awaiting_request.store(false, Ordering::Relaxed);
        if bytes.len() > 16 * 1024 {
            bail!("Media request exceeded cap");
        }
        let request: Request = serde_json::from_slice(&bytes)?;
        validate_request(&request)?;
        watch_limits(&request);
        let _source_handle = open_source(&request.source)?;
        let before = revision(&request.source)?;
        let mut report = empty_report(before.clone());
        let result = match &request.operation {
            Operation::Photo {
                format,
                max_dimension,
            } => photo(&request, *format, *max_dimension, &mut report),
            _ => adapter::execute(
                &request.source,
                &request.operation,
                artifact_path(&request).as_deref(),
                &mut report,
            ),
        };
        if result.is_err() || revision(&request.source)? != before {
            if let Some(path) = artifact_path(&request) {
                let _ = fs::remove_file(path);
            }
            result?;
            bail!("Source revision changed during media processing");
        }
        if let Some(path) = artifact_path(&request) {
            report.bytes = fs::metadata(&path)?.len();
            if report.bytes == 0 || report.bytes > MAX_OUTPUT {
                let _ = fs::remove_file(path);
                bail!("Media output exceeded cap");
            }
        }
        std::io::stdout().write_all(&serde_json::to_vec(&report)?)?;
        Ok(())
    })())
}
#[allow(dead_code)]
pub fn sweep_abandoned_staging() -> Result<()> {
    let root = std::env::temp_dir();
    crate::util::read_only::require_writable(&root)?;
    for entry in fs::read_dir(root)?.take(10_000) {
        let entry = entry?;
        let name = entry.file_name();
        let Some(token) = name.to_str().and_then(|s| s.strip_prefix("FileIDMedia-")) else {
            continue;
        };
        if uuid::Uuid::parse_str(token)
            .map(|u| u.to_string())
            .as_deref()
            != Ok(token)
        {
            continue;
        }
        if !entry.file_type()?.is_dir()
            || entry.metadata()?.modified()?.elapsed().unwrap_or_default()
                <= Duration::from_secs(7200)
        {
            continue;
        }
        let path = entry.path();
        let owner = path.join("owner");
        if crate::util::read_only::require_source_mutation(&path).is_ok()
            && fs::symlink_metadata(&owner).is_ok_and(|m| m.file_type().is_file() && m.len() == 36)
            && fs::read_to_string(owner).is_ok_and(|s| s == token)
        {
            let _ = fs::remove_dir_all(path);
        }
    }
    Ok(())
}
fn empty_report(revision: Revision) -> Report {
    Report {
        revision,
        probe: None,
        actual_seconds: None,
        width: None,
        height: None,
        sample_rate: None,
        samples: None,
        bytes: 0,
    }
}
fn artifact_path(request: &Request) -> Option<PathBuf> {
    let name = match request.operation {
        Operation::Probe => return None,
        Operation::Frame { .. } => "artifact.png",
        Operation::Audio { .. } => "artifact.pcm",
        Operation::Video { .. } => "artifact.mp4",
        Operation::Photo { format, .. } => match format {
            PhotoFormat::Png => "artifact.png",
            PhotoFormat::Jpeg => "artifact.jpg",
            PhotoFormat::Tiff => "artifact.tiff",
        },
    };
    Some(request.directory.join(name))
}
fn validate_request(request: &Request) -> Result<()> {
    validate_operation(&request.operation)?;
    if request.timeout_ms == 0 || request.timeout_ms > 3_600_000 {
        bail!("Invalid worker deadline");
    }
    if !request.source.is_absolute() || !request.directory.is_absolute() {
        bail!("Worker paths must be absolute");
    }
    let uuid = uuid::Uuid::parse_str(&request.token)?;
    if uuid.to_string() != request.token
        || request.directory.file_name().and_then(|s| s.to_str())
            != Some(&format!("FileIDMedia-{}", request.token))
    {
        bail!("Unowned worker directory");
    }
    if !fs::symlink_metadata(&request.directory)?
        .file_type()
        .is_dir()
        || request.directory.canonicalize()?.parent()
            != Some(std::env::temp_dir().canonicalize()?.as_path())
    {
        bail!("Unsafe worker directory");
    }
    crate::util::read_only::require_source_mutation(&request.directory)?;
    let owner = request.directory.join("owner");
    if !fs::symlink_metadata(&owner)?.file_type().is_file()
        || fs::metadata(&owner)?.len() > 64
        || fs::read_to_string(owner)? != request.token
    {
        bail!("Unowned worker directory");
    }
    if let Some(path) = artifact_path(request) {
        crate::util::read_only::require_source_mutation(&path)?;
        if fs::symlink_metadata(path).is_ok() {
            bail!("Staged output is occupied");
        }
    }
    Ok(())
}
fn validate_operation(operation: &Operation) -> Result<()> {
    let valid = match operation {
        Operation::Probe => true,
        Operation::Frame {
            seconds,
            max_dimension,
        } => {
            seconds.is_finite()
                && *seconds >= 0.0
                && *seconds <= 21600.0
                && (1..=768).contains(max_dimension)
        }
        Operation::Audio {
            start_seconds,
            duration_seconds,
        } => {
            start_seconds.is_finite()
                && *start_seconds >= 0.0
                && *start_seconds <= 21600.0
                && duration_seconds.is_finite()
                && *duration_seconds > 0.0
                && *duration_seconds <= 60.0
        }
        Operation::Photo { max_dimension, .. } => (1..=8192).contains(max_dimension),
        Operation::Video { max_dimension } => [1280, 1920].contains(max_dimension),
    };
    if !valid {
        bail!("Invalid bounded media operation");
    }
    Ok(())
}
fn open_source(path: &Path) -> Result<fs::File> {
    #[cfg(windows)]
    if path.components().any(|component| matches!(component,std::path::Component::Normal(name) if name.to_string_lossy().contains(':'))) {
        bail!("Alternate data streams are not regular media inputs");
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.len() == 0 || metadata.len() > MAX_INPUT {
        bail!("Media source must be a bounded regular file");
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1);
    }
    options.open(path).context("Opening read-only media source")
}
fn revision(path: &Path) -> Result<Revision> {
    let metadata = fs::metadata(path)?;
    Ok(Revision {
        size: metadata.len(),
        modified_ns: metadata.modified()?.duration_since(UNIX_EPOCH)?.as_nanos(),
    })
}
#[cfg(any(windows, test))]
pub(super) fn write_frame_png(image: &image::DynamicImage, destination: impl Write) -> Result<()> {
    // PNG drop-finalization cannot hide disk errors when encoding into an infallible memory sink.
    let mut encoded = std::io::Cursor::new(Vec::new());
    image.write_to(&mut encoded, image::ImageFormat::Png)?;
    let mut writer = std::io::BufWriter::new(destination);
    writer.write_all(encoded.get_ref())?;
    writer.flush()?;
    Ok(())
}

fn photo(
    request: &Request,
    format: PhotoFormat,
    dimension: u32,
    report: &mut Report,
) -> Result<()> {
    #[cfg(windows)]
    let image = adapter::decode_photo(&request.source)?;
    #[cfg(not(windows))]
    let image = decode_photo(&request.source)?;
    let image = if image.width().max(image.height()) > dimension {
        image.resize(dimension, dimension, image::imageops::FilterType::Lanczos3)
    } else {
        image
    };
    let rgba = image.to_rgba8();
    let image = match format {
        PhotoFormat::Jpeg => image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(
            rgba.width(),
            rgba.height(),
            |x, y| {
                let pixel = rgba.get_pixel(x, y);
                let alpha = u32::from(pixel[3]);
                image::Rgb([0, 1, 2].map(|channel| {
                    ((u32::from(pixel[channel]) * alpha + 255 * (255 - alpha) + 127) / 255) as u8
                }))
            },
        )),
        PhotoFormat::Png | PhotoFormat::Tiff => image::DynamicImage::ImageRgba8(rgba),
    };
    let path = artifact_path(request).context("Missing photo artifact")?;
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let mut writer = std::io::BufWriter::new(file);
    image.write_to(
        &mut writer,
        match format {
            PhotoFormat::Png => image::ImageFormat::Png,
            PhotoFormat::Jpeg => image::ImageFormat::Jpeg,
            PhotoFormat::Tiff => image::ImageFormat::Tiff,
        },
    )?;
    writer.flush()?;
    report.width = Some(image.width());
    report.height = Some(image.height());
    Ok(())
}
#[cfg(not(windows))]
fn decode_photo(source: &Path) -> Result<image::DynamicImage> {
    let mut reader = image::ImageReader::new(std::io::BufReader::new(open_source(source)?))
        .with_guessed_format()?;
    if !matches!(
        reader.format(),
        Some(
            image::ImageFormat::Png
                | image::ImageFormat::Jpeg
                | image::ImageFormat::Bmp
                | image::ImageFormat::Tiff
        )
    ) {
        bail!("Photo input must be a non-animated PNG, JPEG, BMP or single-page TIFF");
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    if reader.format() == Some(image::ImageFormat::Png) {
        let png =
            image::codecs::png::PngDecoder::new(std::io::BufReader::new(open_source(source)?))?;
        if png.is_apng()? {
            bail!("Animated photos are unsupported");
        }
    }
    if reader.format() == Some(image::ImageFormat::Tiff) {
        validate_single_page_tiff(source)?;
    }
    use image::ImageDecoder;
    let mut decoder = reader.into_decoder()?;
    let (width, height) = decoder.dimensions();
    if u64::from(width) * u64::from(height) > 32_000_000 {
        bail!("Photo dimensions exceeded cap");
    }
    if decoder.icc_profile()?.is_some() {
        bail!("Linux photo export cannot guarantee embedded ICC color conversion");
    }
    let orientation = decoder.orientation()?;
    let mut image = image::DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    Ok(image)
}
#[cfg(not(windows))]
fn validate_single_page_tiff(path: &Path) -> Result<()> {
    use std::io::{Seek, SeekFrom};
    let mut file = open_source(path)?;
    let mut header = [0u8; 8];
    file.read_exact(&mut header)?;
    let little = &header[..2] == b"II";
    let word = |b: [u8; 2]| {
        if little {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        }
    };
    let long = |b: [u8; 4]| {
        if little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        }
    };
    if !matches!(&header[..2], b"II" | b"MM") || word(header[2..4].try_into()?) != 42 {
        bail!("Unsupported TIFF layout");
    }
    let offset = u64::from(long(header[4..8].try_into()?));
    file.seek(SeekFrom::Start(offset))?;
    let mut entries = [0u8; 2];
    file.read_exact(&mut entries)?;
    file.seek(SeekFrom::Start(offset + 2 + u64::from(word(entries)) * 12))?;
    let mut next = [0u8; 4];
    file.read_exact(&mut next)?;
    if long(next) != 0 {
        bail!("Multi-page TIFF export is unsupported");
    }
    Ok(())
}
fn watch_limits(request: &Request) {
    let deadline = std::time::Instant::now() + Duration::from_millis(request.timeout_ms);
    let artifact = artifact_path(request);
    std::thread::spawn(move || loop {
        if std::time::Instant::now() >= deadline
            || artifact
                .as_ref()
                .is_some_and(|p| fs::metadata(p).is_ok_and(|m| m.len() > MAX_OUTPUT))
        {
            std::process::exit(124);
        }
        std::thread::sleep(Duration::from_millis(10));
    });
}
fn watch_parent(parent: u32) -> Result<()> {
    if crate::platform::get_parent_pid() != Some(parent) {
        bail!("Worker parent does not match spawning process");
    }
    #[cfg(windows)]
    {
        use windows::Win32::{
            Foundation::CloseHandle,
            System::Threading::{OpenProcess, WaitForSingleObject, INFINITE, PROCESS_SYNCHRONIZE},
        };
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, parent)? };
        let raw = handle.0 as usize;
        std::thread::spawn(move || unsafe {
            let handle = windows::Win32::Foundation::HANDLE(raw as *mut _);
            WaitForSingleObject(handle, INFINITE);
            let _ = CloseHandle(handle);
            std::process::exit(125);
        });
    }
    #[cfg(unix)]
    {
        if unsafe { libc::getppid() } != parent as i32 {
            bail!("Worker parent changed");
        }
        unsafe {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                bail!("Cannot bind worker lifetime");
            }
        }
        std::thread::spawn(move || loop {
            if unsafe { libc::getppid() } != parent as i32 {
                std::process::exit(125);
            }
            std::thread::sleep(Duration::from_millis(100));
        });
    }
    Ok(())
}
pub(super) fn validate_probe(probe: &Probe) -> Result<()> {
    if !probe.duration.is_finite()
        || !(0.0..=21600.0).contains(&probe.duration)
        || probe.duration == 0.0
        || probe.width == 0
        || probe.height == 0
        || probe.width.max(probe.height) > 8192
        || u64::from(probe.width) * u64::from(probe.height) > 32_000_000
        || probe.audio_count > 1
        || !probe.video_start.is_finite()
        || !probe.frame_rate.is_finite()
        || probe.frame_rate <= 0.0
        || probe.frame_rate > 240.0
    {
        bail!("Unsupported media duration, streams or dimensions");
    }
    if probe.audio_count == 1
        && (!probe.audio_start.is_some_and(f64::is_finite)
            || !probe
                .audio_duration
                .is_some_and(|d| d.is_finite() && d > 0.0))
    {
        bail!("Invalid audio timing");
    }
    Ok(())
}
pub(super) fn validate_export(before: &Probe, after: &Probe, dimension: u32) -> Result<()> {
    validate_probe(after)?;
    if after.hdr
        || after.video_codec != "h264"
        || after.audio_count != before.audio_count
        || (after.duration - before.duration).abs() > 0.25
        || after.width.max(after.height) > dimension + 2
        || (f64::from(after.width) / f64::from(after.height)
            - f64::from(before.width) / f64::from(before.height))
        .abs()
            > 0.02
    {
        bail!("Export changed duration, aspect, streams or codec");
    }
    if before.audio_count == 1
        && (after.audio_codec.as_deref() != Some("aac")
            || ((after.audio_start.unwrap() - after.video_start)
                - (before.audio_start.unwrap() - before.video_start))
                .abs()
                > 0.1
            || (after.audio_duration.unwrap() - before.audio_duration.unwrap()).abs() > 0.25)
    {
        bail!("Export changed audio codec or synchronization");
    }
    Ok(())
}

/// Refuse playlists/URLs by magic and fixed demuxer; MP4 external data references
/// are rejected before either native Windows or installed Linux decoders see them.
pub(super) fn validate_container(path: &Path) -> Result<&'static str> {
    use std::io::{Seek, SeekFrom};
    let mut file = open_source(path)?;
    let mut magic = [0u8; 12];
    file.read_exact(&mut magic)?;
    if &magic[4..8] == b"ftyp" || &magic[4..8] == b"moov" || &magic[4..8] == b"wide" {
        let length = file.metadata()?.len();
        file.seek(SeekFrom::Start(0))?;
        inspect_boxes(&mut file, 0, length, 0, &mut 0, &mut ColorInfo::default())?;
        return Ok("mov");
    }
    if &magic[..4] == b"RIFF" && &magic[8..12] == b"WAVE" {
        return Ok("wav");
    }
    if &magic[..4] == b"fLaC" {
        return Ok("flac");
    }
    if &magic[..3] == b"ID3" || (magic[0] == 0xff && magic[1] & 0xe0 == 0xe0) {
        return Ok("mp3");
    }
    bail!("Unsupported file-only media container; playlists and URL protocols are forbidden")
}
#[derive(Default)]
pub(super) struct ColorInfo {
    pub hdr: bool,
    pub high_bit_depth: bool,
    pub transfer_known: bool,
    pub requires_transfer_metadata: bool,
}
pub(super) fn source_color(path: &Path) -> Result<ColorInfo> {
    use std::io::{Seek, SeekFrom};
    let mut file = open_source(path)?;
    let mut magic = [0u8; 8];
    file.read_exact(&mut magic)?;
    let mut color = ColorInfo::default();
    if matches!(&magic[4..8], b"ftyp" | b"moov" | b"wide") {
        let length = file.metadata()?.len();
        file.seek(SeekFrom::Start(0))?;
        inspect_boxes(&mut file, 0, length, 0, &mut 0, &mut color)?;
    }
    if (color.high_bit_depth || color.requires_transfer_metadata) && !color.transfer_known {
        bail!("Video has unspecified transfer metadata; SDR export cannot be guaranteed");
    }
    Ok(color)
}
fn inspect_boxes(
    file: &mut fs::File,
    start: u64,
    end: u64,
    depth: usize,
    count: &mut usize,
    color: &mut ColorInfo,
) -> Result<()> {
    use std::io::{Seek, SeekFrom};
    if depth > 8 {
        bail!("Nested MP4 box cap exceeded");
    }
    let mut position = start;
    while position < end {
        *count += 1;
        if *count > 100_000 || end - position < 8 {
            bail!("Invalid bounded MP4 boxes");
        }
        file.seek(SeekFrom::Start(position))?;
        let mut header = [0u8; 8];
        file.read_exact(&mut header)?;
        let mut size = u64::from(u32::from_be_bytes(header[..4].try_into()?));
        let mut header_length = 8;
        if size == 1 {
            let mut wide = [0u8; 8];
            file.read_exact(&mut wide)?;
            size = u64::from_be_bytes(wide);
            header_length = 16;
        }
        if size == 0 {
            size = end - position;
        }
        if size < header_length || size > end - position {
            bail!("Invalid MP4 box extent");
        }
        let payload = position + header_length;
        match &header[4..] {
            b"moov" | b"trak" | b"mdia" | b"minf" | b"dinf" | b"stbl" => {
                inspect_boxes(file, payload, position + size, depth + 1, count, color)?;
            }
            b"stsd" => {
                if size < header_length + 8 {
                    bail!("Invalid MP4 sample description");
                }
                inspect_boxes(file, payload + 8, position + size, depth + 1, count, color)?;
            }
            b"avc1" | b"avc3" | b"hvc1" | b"hev1" | b"vp09" | b"av01" | b"encv" => {
                if size < header_length + 78 {
                    bail!("Invalid MP4 visual sample entry");
                }
                if &header[4..] != b"avc1" {
                    color.requires_transfer_metadata = true;
                }
                inspect_boxes(file, payload + 78, position + size, depth + 1, count, color)?;
            }
            b"dvcC" | b"dvvC" | b"mdcv" | b"clli" => {
                color.hdr = true;
            }
            b"colr" => {
                if size < header_length + 10 {
                    bail!("Invalid MP4 color metadata");
                }
                let mut description = [0u8; 10];
                file.seek(SeekFrom::Start(payload))?;
                file.read_exact(&mut description)?;
                if matches!(&description[..4], b"nclx" | b"nclc") {
                    let transfer = u16::from_be_bytes(description[6..8].try_into()?);
                    color.hdr |= [16, 18].contains(&transfer);
                    color.transfer_known |= matches!(transfer, 1 | 4..=18);
                } else {
                    bail!("Unrecognized video color profile cannot guarantee SDR");
                }
            }
            b"hvcC" => {
                if size < header_length + 23 {
                    bail!("Invalid HEVC description");
                }
                let mut description = [0u8; 23];
                file.seek(SeekFrom::Start(payload))?;
                file.read_exact(&mut description)?;
                color.high_bit_depth |= description[17] & 7 != 0 || description[18] & 7 != 0;
            }
            b"avcC" => {
                if size < header_length + 7 {
                    bail!("Invalid AVC description");
                }
                let mut description = [0u8; 7];
                file.seek(SeekFrom::Start(payload))?;
                file.read_exact(&mut description)?;
                color.high_bit_depth |= [110, 122, 244].contains(&description[1]);
                let sps_count = description[5] & 31;
                if sps_count == 0 {
                    bail!("AVC parameter sets must be self-contained");
                }
                // Byte 6 starts the first 16-bit SPS length, so resume after byte 5.
                let mut cursor = payload + 6;
                for _ in 0..sps_count {
                    if cursor + 2 > position + size {
                        bail!("Invalid AVC SPS extent");
                    }
                    file.seek(SeekFrom::Start(cursor))?;
                    let mut length = [0u8; 2];
                    file.read_exact(&mut length)?;
                    let length = usize::from(u16::from_be_bytes(length));
                    cursor += 2;
                    if length == 0 || cursor + length as u64 > position + size {
                        bail!("Invalid AVC SPS extent");
                    }
                    let mut sps = vec![0u8; length];
                    file.read_exact(&mut sps)?;
                    inspect_avc_sps(&sps, color)?;
                    cursor += length as u64;
                }
            }
            b"cmov" | b"rmra" | b"rmda" | b"rdrf" => {
                bail!("Referenced/compressed QuickTime inputs are unsupported")
            }
            b"dref" => {
                if size != header_length + 20 {
                    bail!("External or multiple MP4 data references are forbidden");
                }
                file.seek(SeekFrom::Start(payload))?;
                let mut reference = [0u8; 20];
                file.read_exact(&mut reference)?;
                if reference[..8] != [0, 0, 0, 0, 0, 0, 0, 1]
                    || reference[8..12] != [0, 0, 0, 12]
                    || &reference[12..16] != b"url "
                    || reference[16..] != [0, 0, 0, 1]
                {
                    bail!("External MP4 data references are forbidden");
                }
            }
            b"url " | b"urn " => {
                if size != header_length + 4 {
                    bail!("External media references are forbidden");
                }
                file.seek(SeekFrom::Start(payload))?;
                let mut flags = [0u8; 4];
                file.read_exact(&mut flags)?;
                if flags != [0, 0, 0, 1] {
                    bail!("External media references are forbidden");
                }
            }
            _ => {}
        }
        position += size;
    }
    Ok(())
}

struct Bits {
    bytes: Vec<u8>,
    position: usize,
}
impl Bits {
    fn new(nal: &[u8]) -> Self {
        let mut bytes = Vec::new();
        let mut zeros = 0;
        for &byte in nal {
            if zeros >= 2 && byte == 3 {
                zeros = 0;
                continue;
            }
            bytes.push(byte);
            zeros = if byte == 0 { zeros + 1 } else { 0 };
        }
        Self { bytes, position: 0 }
    }
    fn read(&mut self, count: usize) -> Result<u32> {
        if count > 32 || self.position + count > self.bytes.len() * 8 {
            bail!("Invalid AVC bitstream");
        }
        let mut value = 0;
        for _ in 0..count {
            value = (value << 1)
                | u32::from((self.bytes[self.position / 8] >> (7 - self.position % 8)) & 1);
            self.position += 1;
        }
        Ok(value)
    }
    fn ue(&mut self) -> Result<u32> {
        let mut zeros = 0;
        while self.read(1)? == 0 {
            zeros += 1;
            if zeros >= 31 {
                bail!("Invalid AVC Golomb code");
            }
        }
        Ok((1u32 << zeros) - 1 + self.read(zeros)?)
    }
    fn se(&mut self) -> Result<i64> {
        let code = self.ue()?;
        Ok(if code & 1 == 0 {
            -i64::from(code / 2)
        } else {
            i64::from(code / 2 + 1)
        })
    }
}
fn inspect_avc_sps(nal: &[u8], color: &mut ColorInfo) -> Result<()> {
    let mut bits = Bits::new(nal);
    if bits.read(8)? & 31 != 7 {
        bail!("Invalid AVC SPS");
    }
    let profile = bits.read(8)?;
    bits.read(16)?;
    bits.ue()?;
    if [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135].contains(&profile) {
        let chroma = bits.ue()?;
        if chroma > 3 {
            bail!("Invalid AVC chroma format");
        }
        if chroma == 3 {
            bits.read(1)?;
        }
        color.high_bit_depth |= bits.ue()? > 0;
        color.high_bit_depth |= bits.ue()? > 0;
        bits.read(1)?;
        if bits.read(1)? != 0 {
            for index in 0..if chroma == 3 { 12 } else { 8 } {
                if bits.read(1)? == 0 {
                    continue;
                }
                let mut last = 8;
                let mut next = 8;
                for _ in 0..if index < 6 { 16 } else { 64 } {
                    if next != 0 {
                        next = (last + bits.se()?).rem_euclid(256);
                    }
                    if next != 0 {
                        last = next;
                    }
                }
            }
        }
    }
    bits.ue()?;
    match bits.ue()? {
        0 => {
            bits.ue()?;
        }
        1 => {
            bits.read(1)?;
            bits.se()?;
            bits.se()?;
            let count = bits.ue()?;
            if count > 255 {
                bail!("AVC reference cap exceeded");
            }
            for _ in 0..count {
                bits.se()?;
            }
        }
        2 => {}
        _ => bail!("Invalid AVC picture order"),
    }
    bits.ue()?;
    bits.read(1)?;
    bits.ue()?;
    bits.ue()?;
    if bits.read(1)? == 0 {
        bits.read(1)?;
    }
    bits.read(1)?;
    if bits.read(1)? != 0 {
        for _ in 0..4 {
            bits.ue()?;
        }
    }
    if bits.read(1)? == 0 {
        return Ok(());
    }
    if bits.read(1)? != 0 && bits.read(8)? == 255 {
        bits.read(32)?;
    }
    if bits.read(1)? != 0 {
        bits.read(1)?;
    }
    if bits.read(1)? != 0 {
        bits.read(4)?;
        if bits.read(1)? != 0 {
            bits.read(8)?;
            let transfer = bits.read(8)?;
            bits.read(8)?;
            color.hdr |= [16, 18].contains(&transfer);
            color.transfer_known |= matches!(transfer, 1 | 4..=18);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_media_frame_png_propagates_final_write_and_flush_failures() {
        struct FailingOutput {
            bytes: Vec<u8>,
            fail_after: usize,
            fail_flush: bool,
        }
        impl Write for FailingOutput {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.bytes.len() == self.fail_after {
                    return Err(std::io::Error::other("injected final write failure"));
                }
                let count = bytes.len().min(self.fail_after - self.bytes.len());
                self.bytes.extend_from_slice(&bytes[..count]);
                Ok(count)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                if self.fail_flush {
                    return Err(std::io::Error::other("injected final flush failure"));
                }
                Ok(())
            }
        }
        let image = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            4,
            4,
            image::Rgb([200, 20, 10]),
        ));
        let mut output = FailingOutput {
            bytes: Vec::new(),
            fail_after: 32,
            fail_flush: false,
        };
        use image::ImageEncoder;
        let unchecked =
            image::codecs::png::PngEncoder::new(&mut std::io::BufWriter::new(&mut output))
                .write_image(
                    image.as_bytes(),
                    image.width(),
                    image.height(),
                    image.color().into(),
                );
        assert!(unchecked.is_ok());
        assert_eq!(output.bytes.len(), 32);
        assert!(image::load_from_memory(&output.bytes).is_err());
        output.bytes.clear();
        let error = write_frame_png(&image, &mut output).unwrap_err();
        assert!(error.to_string().contains("injected final write failure"));
        assert!(!output.bytes.is_empty());
        output = FailingOutput {
            bytes: Vec::new(),
            fail_after: usize::MAX,
            fail_flush: true,
        };
        assert!(write_frame_png(&image, &mut output)
            .unwrap_err()
            .to_string()
            .contains("injected final flush failure"));
        let mut valid = Vec::new();
        write_frame_png(&image, &mut valid).unwrap();
        assert!(valid.ends_with(&[0, 0, 0, 0, b'I', b'E', b'N', b'D', 174, 66, 96, 130]));
        assert_eq!(
            image::load_from_memory(&valid).unwrap().to_rgb8(),
            image.to_rgb8()
        );
    }
    #[test]
    fn media_operation_rejects_nonfinite_and_unbounded_requests() {
        assert!(validate_operation(&Operation::Frame {
            seconds: f64::NAN,
            max_dimension: 768
        })
        .is_err());
        assert!(validate_operation(&Operation::Audio {
            start_seconds: 0.0,
            duration_seconds: 61.0
        })
        .is_err());
        assert!(validate_operation(&Operation::Photo {
            format: PhotoFormat::Png,
            max_dimension: 9000
        })
        .is_err());
    }
    #[test]
    fn media_export_rejects_sync_and_hdr_changes() {
        let probe = Probe {
            duration: 2.0,
            width: 320,
            height: 240,
            video_codec: "h264".into(),
            video_start: 0.0,
            audio_count: 1,
            audio_codec: Some("aac".into()),
            audio_start: Some(0.0),
            audio_duration: Some(2.0),
            hdr: false,
            rotation: 0,
            frame_rate: 30.0,
        };
        assert!(validate_export(&probe, &probe, 1280).is_ok());
        let mut changed = probe.clone();
        changed.audio_start = Some(1.0);
        assert!(validate_export(&probe, &changed, 1280).is_err());
        changed = probe.clone();
        changed.hdr = true;
        assert!(validate_export(&probe, &changed, 1280).is_err());
    }
    #[test]
    #[ignore = "requires built FileIDEngine and generated fixtures in FILEID_MEDIA_TEST_FIXTURES"]
    fn media_worker_generated_metadata_source_preservation_and_cleanup() {
        let fixtures = PathBuf::from(std::env::var_os("FILEID_MEDIA_TEST_FIXTURES").unwrap());
        let video = fixtures.join("sdr.mp4");
        let original = fs::read(&video).unwrap();
        let cancel = AtomicBool::new(false);
        let before = run(&video, Operation::Probe, &cancel, Duration::from_secs(30)).unwrap();
        let probe = before.report.probe.as_ref().unwrap();
        assert!((probe.duration - 3.0).abs() < 0.1);
        assert_eq!(
            (probe.width, probe.height, probe.audio_count),
            (320, 240, 1)
        );
        let frame = run(
            &video,
            Operation::Frame {
                seconds: 1.05,
                max_dimension: 768,
            },
            &cancel,
            Duration::from_secs(30),
        )
        .unwrap();
        let actual = frame.report.actual_seconds.unwrap();
        assert!((1.05..1.2).contains(&actual));
        assert!(frame.report.width.unwrap() <= 768);
        let directory = frame.directory.clone();
        assert!(frame.artifact.as_ref().unwrap().exists());
        drop(frame);
        assert!(!directory.exists());
        let audio = run(
            &video,
            Operation::Audio {
                start_seconds: 0.5,
                duration_seconds: 1.0,
            },
            &cancel,
            Duration::from_secs(30),
        )
        .unwrap();
        assert_eq!(audio.report.sample_rate, Some(16000));
        assert!((15990..=16000).contains(&audio.report.samples.unwrap()));
        assert!((audio.report.actual_seconds.unwrap() - 0.5).abs() < 0.001);
        let photo = run(
            &fixtures.join("still.png"),
            Operation::Photo {
                format: PhotoFormat::Tiff,
                max_dimension: 200,
            },
            &cancel,
            Duration::from_secs(30),
        )
        .unwrap();
        assert_eq!(
            (photo.report.width, photo.report.height),
            (Some(200), Some(150))
        );
        let export = run(
            &video,
            Operation::Video {
                max_dimension: 1280,
            },
            &cancel,
            Duration::from_secs(30),
        )
        .unwrap();
        validate_export(probe, export.report.probe.as_ref().unwrap(), 1280).unwrap();
        assert_eq!(fs::read(video).unwrap(), original);
    }
    #[test]
    #[ignore = "requires built FileIDEngine and generated fixtures in FILEID_MEDIA_TEST_FIXTURES"]
    fn media_worker_timeout_cancel_malformed_and_occupied_output() {
        let fixtures = PathBuf::from(std::env::var_os("FILEID_MEDIA_TEST_FIXTURES").unwrap());
        let video = fixtures.join("sdr.mp4");
        let original = fs::read(&video).unwrap();
        let cancel = AtomicBool::new(true);
        assert!(run(&video, Operation::Probe, &cancel, Duration::from_secs(30)).is_err());
        cancel.store(false, Ordering::Relaxed);
        assert!(run(
            &video,
            Operation::Video {
                max_dimension: 1280
            },
            &cancel,
            Duration::from_millis(1)
        )
        .is_err());
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(50));
                cancel.store(true, Ordering::Relaxed);
            });
            assert!(run(
                &fixtures.join("long.mp4"),
                Operation::Video {
                    max_dimension: 1280
                },
                &cancel,
                Duration::from_secs(30)
            )
            .is_err());
        });
        cancel.store(false, Ordering::Relaxed);
        let malformed = fixtures.join(format!("malformed-{}.mp4", uuid::Uuid::new_v4()));
        fs::write(
            &malformed,
            b"#EXTM3U
https://example.invalid/media
",
        )
        .unwrap();
        assert!(run(
            &malformed,
            Operation::Probe,
            &cancel,
            Duration::from_secs(30)
        )
        .is_err());
        assert_eq!(
            fs::read(&malformed).unwrap(),
            b"#EXTM3U
https://example.invalid/media
"
        );
        fs::remove_file(malformed).unwrap();
        let token = uuid::Uuid::new_v4().to_string();
        let directory = std::env::temp_dir().join(format!("FileIDMedia-{token}"));
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("owner"), &token).unwrap();
        fs::write(directory.join("artifact.png"), b"existing").unwrap();
        let request = Request {
            source: video.clone(),
            operation: Operation::Frame {
                seconds: 1.0,
                max_dimension: 768,
            },
            directory: directory.clone(),
            token,
            timeout_ms: 30000,
        };
        assert!(validate_request(&request).is_err());
        assert_eq!(
            fs::read(directory.join("artifact.png")).unwrap(),
            b"existing"
        );
        fs::remove_dir_all(directory).unwrap();
        assert_eq!(fs::read(video).unwrap(), original);
    }
    #[test]
    fn media_worker_rejects_external_mp4_references() {
        let path = std::env::temp_dir().join(format!(
            "FileIDMedia-malformed-{}.mp4",
            uuid::Uuid::new_v4()
        ));
        let mut bytes = Vec::new();
        for (size, kind, payload) in [
            (12u32, b"ftyp", &b"mp42"[..]),
            (13u32, b"url ", &b"    x"[..]),
        ] {
            bytes.extend_from_slice(&size.to_be_bytes());
            bytes.extend_from_slice(kind);
            bytes.extend_from_slice(payload);
        }
        fs::write(&path, bytes).unwrap();
        assert!(validate_container(&path).is_err());
        fs::remove_file(path).unwrap();
    }

    #[test]
    #[ignore = "requires built FileIDEngine and generated fixtures in FILEID_MEDIA_TEST_FIXTURES"]
    fn media_worker_hdr_refusal_rotation_fidelity_and_huge_photo() {
        let fixtures = PathBuf::from(std::env::var_os("FILEID_MEDIA_TEST_FIXTURES").unwrap());
        let cancelled = AtomicBool::new(false);
        let hdr = fixtures.join("hdr.mp4");
        let original = fs::read(&hdr).unwrap();
        assert!(run(
            &hdr,
            Operation::Video {
                max_dimension: 1280
            },
            &cancelled,
            Duration::from_secs(30)
        )
        .is_err());
        assert_eq!(fs::read(hdr).unwrap(), original);
        assert!(run(
            &fixtures.join("hdr-avc.mp4"),
            Operation::Video {
                max_dimension: 1280
            },
            &cancelled,
            Duration::from_secs(30)
        )
        .is_err());
        let rotated = fixtures.join("rotated.mp4");
        let before = run(
            &rotated,
            Operation::Probe,
            &cancelled,
            Duration::from_secs(30),
        )
        .unwrap();
        assert_eq!(
            (
                before.report.probe.as_ref().unwrap().width,
                before.report.probe.as_ref().unwrap().height
            ),
            (240, 320)
        );
        let export = run(
            &rotated,
            Operation::Video {
                max_dimension: 1280,
            },
            &cancelled,
            Duration::from_secs(30),
        )
        .unwrap();
        validate_export(
            before.report.probe.as_ref().unwrap(),
            export.report.probe.as_ref().unwrap(),
            1280,
        )
        .unwrap();
        assert!(run(
            &fixtures.join("huge.png"),
            Operation::Photo {
                format: PhotoFormat::Png,
                max_dimension: 200
            },
            &cancelled,
            Duration::from_secs(30)
        )
        .is_err());
    }

    #[test]
    #[ignore = "requires built FileIDEngine and generated fixtures in FILEID_MEDIA_TEST_FIXTURES"]
    fn media_worker_response_cap_kills_and_reaps_worker() {
        let source =
            PathBuf::from(std::env::var_os("FILEID_MEDIA_TEST_FIXTURES").unwrap()).join("sdr.mp4");
        let token = uuid::Uuid::new_v4().to_string();
        let directory = std::env::temp_dir().join(format!("FileIDMedia-{token}"));
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("owner"), &token).unwrap();
        let request = Request {
            source,
            operation: Operation::Probe,
            directory: directory.clone(),
            token,
            timeout_ms: 30000,
        };
        let executable = PathBuf::from(std::env::var_os("FILEID_MEDIA_TEST_ENGINE").unwrap());
        let mut command = std::process::Command::new(executable);
        command.args(["--media-worker", &std::process::id().to_string()]);
        let result = crate::util::child_process::run(
            &mut command,
            &serde_json::to_vec(&request).unwrap(),
            &AtomicBool::new(false),
            Duration::from_secs(30),
            1,
            32 * 1024,
        );
        assert!(result.unwrap_err().to_string().contains("cap"));
        fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    #[ignore = "requires built FileIDEngine and generated fixtures in FILEID_MEDIA_TEST_FIXTURES"]
    fn media_worker_delayed_audio_truthful_ranges_and_metadata_stripping() {
        let fixtures = PathBuf::from(std::env::var_os("FILEID_MEDIA_TEST_FIXTURES").unwrap());
        let cancelled = AtomicBool::new(false);
        let source = fixtures.join("delayed.mp4");
        let original = fs::read(&source).unwrap();
        let before = run(
            &source,
            Operation::Probe,
            &cancelled,
            Duration::from_secs(30),
        )
        .unwrap();
        assert!((before.report.probe.as_ref().unwrap().audio_start.unwrap() - 0.378).abs() < 0.001);
        let audio = run(
            &source,
            Operation::Audio {
                start_seconds: 0.5,
                duration_seconds: 1.0,
            },
            &cancelled,
            Duration::from_secs(30),
        )
        .unwrap();
        assert!((audio.report.actual_seconds.unwrap() - 0.5).abs() < 0.001);
        let result = run(
            &source,
            Operation::Video {
                max_dimension: 1280,
            },
            &cancelled,
            Duration::from_secs(30),
        );
        #[cfg(windows)]
        assert!(result.is_err());
        #[cfg(not(windows))]
        validate_export(
            before.report.probe.as_ref().unwrap(),
            result.unwrap().report.probe.as_ref().unwrap(),
            1280,
        )
        .unwrap();
        assert_eq!(fs::read(&source).unwrap(), original);
        let export = run(
            &fixtures.join("tagged.mp4"),
            Operation::Video {
                max_dimension: 1280,
            },
            &cancelled,
            Duration::from_secs(30),
        )
        .unwrap();
        let bytes = fs::read(export.artifact.as_ref().unwrap()).unwrap();
        assert!(!bytes
            .windows(b"fixture-private".len())
            .any(|b| b == b"fixture-private"));
        assert!(!bytes
            .windows(b"fixture-title".len())
            .any(|b| b == b"fixture-title"));
    }
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires built FileIDEngine and independent references in FILEID_MEDIA_TEST_PTS_FIXTURES"]
    fn linux_media_source_pts_matches_exact_frame_and_pcm_references() {
        let fixtures = PathBuf::from(std::env::var_os("FILEID_MEDIA_TEST_PTS_FIXTURES").unwrap());
        let source = fixtures.join("positive-common-offset.mp4");
        let original = fs::read(&source).unwrap();
        let cancelled = AtomicBool::new(false);
        let staging_before: Vec<_> = fs::read_dir(std::env::temp_dir())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        for (name, seconds) in [
            ("frame-before-origin", 1.9),
            ("frame-red", 2.5),
            ("frame-blue", 4.5),
            ("frame-tail", 5.8),
        ] {
            let staged = run(
                &source,
                Operation::Frame {
                    seconds,
                    max_dimension: 160,
                },
                &cancelled,
                Duration::from_secs(30),
            )
            .unwrap();
            assert_eq!(
                fs::read(staged.artifact.as_ref().unwrap()).unwrap(),
                fs::read(fixtures.join(format!("{name}.reference.png"))).unwrap(),
                "{name}"
            );
            let expected_seconds = seconds.max(2.0);
            assert!((staged.report.actual_seconds.unwrap() - expected_seconds).abs() < 0.000001);
            if name == "frame-tail" {
                assert!(
                    staged.report.actual_seconds.unwrap()
                        > staged.report.probe.as_ref().unwrap().duration
                );
            }
        }
        assert!(run(
            &source,
            Operation::Frame {
                seconds: 6.0,
                max_dimension: 160
            },
            &cancelled,
            Duration::from_secs(30)
        )
        .is_err());
        for (name, start, duration) in [
            ("audio-overlap-origin", 1.5, 1.0),
            ("audio-red", 2.5, 0.4),
            ("audio-blue", 4.5, 0.4),
            ("audio-tail", 5.8, 0.4),
        ] {
            let staged = run(
                &source,
                Operation::Audio {
                    start_seconds: start,
                    duration_seconds: duration,
                },
                &cancelled,
                Duration::from_secs(30),
            )
            .unwrap();
            assert_eq!(
                fs::read(staged.artifact.as_ref().unwrap()).unwrap(),
                fs::read(fixtures.join(format!("{name}.reference.pcm"))).unwrap(),
                "{name}"
            );
            let actual = staged.report.actual_seconds.unwrap();
            assert!((actual - start.max(1.936)).abs() < 0.000001);
            assert!(
                actual + staged.report.samples.unwrap() as f64 / 16000.0
                    <= start + duration + 1.0 / 16000.0
            );
            assert_eq!(staged.report.sample_rate, Some(16000));
        }
        for (start, duration) in [(1.0, 0.4), (6.3, 0.2)] {
            assert!(run(
                &source,
                Operation::Audio {
                    start_seconds: start,
                    duration_seconds: duration
                },
                &cancelled,
                Duration::from_secs(30)
            )
            .is_err());
        }
        assert_eq!(fs::read(source).unwrap(), original);
        let staging_after: Vec<_> = fs::read_dir(std::env::temp_dir())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(staging_after, staging_before);
    }
    proptest::proptest! {
        #[test]
        fn media_worker_malformed_sps_is_bounded(bytes in proptest::collection::vec(proptest::num::u8::ANY,0..128)) {
            let _ = inspect_avc_sps(&bytes,&mut ColorInfo::default());
        }
    }
    #[test]
    #[ignore = "requires built FileIDEngine and generated fixtures in FILEID_MEDIA_TEST_FIXTURES"]
    fn media_worker_photo_jpeg_white_alpha_and_eight_bit_tiff() {
        let fixtures = PathBuf::from(std::env::var_os("FILEID_MEDIA_TEST_FIXTURES").unwrap());
        let source = fixtures.join(format!("transparent-{}.png", uuid::Uuid::new_v4()));
        image::RgbaImage::from_pixel(2, 2, image::Rgba([40, 80, 120, 0]))
            .save(&source)
            .unwrap();
        let original = fs::read(&source).unwrap();
        let cancelled = AtomicBool::new(false);
        let jpeg = run(
            &source,
            Operation::Photo {
                format: PhotoFormat::Jpeg,
                max_dimension: 200,
            },
            &cancelled,
            Duration::from_secs(30),
        )
        .unwrap();
        assert!(image::open(jpeg.artifact.as_ref().unwrap())
            .unwrap()
            .to_rgb8()
            .pixels()
            .all(|p| p.0.iter().all(|v| *v >= 250)));
        let tiff = run(
            &source,
            Operation::Photo {
                format: PhotoFormat::Tiff,
                max_dimension: 200,
            },
            &cancelled,
            Duration::from_secs(30),
        )
        .unwrap();
        use image::ImageDecoder;
        let decoder = image::ImageReader::open(tiff.artifact.as_ref().unwrap())
            .unwrap()
            .with_guessed_format()
            .unwrap()
            .into_decoder()
            .unwrap();
        assert_eq!(decoder.color_type(), image::ColorType::Rgba8);
        assert_eq!(fs::read(&source).unwrap(), original);
        fs::remove_file(source).unwrap();
    }
}
