use crate::ipc::ToolRecipe;
use anyhow::{bail, ensure, Context, Result};
use serde_json::Value;
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
#[cfg(windows)]
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::HANDLE,
        System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        },
    },
};

const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
const EXPORT_TIMEOUT: Duration = Duration::from_secs(3600);

#[derive(Clone)]
struct Runtime {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
}

#[derive(Debug)]
pub(super) struct Probe {
    pub(super) duration: f64,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) video_start: f64,
    pub(super) audio_start: Option<f64>,
    pub(super) audio_duration: Option<f64>,
    pub(super) audio_count: usize,
    pub(super) video_codec: String,
    pub(super) audio_codec: Option<String>,
}

fn runtime() -> Result<Runtime> {
    let directory = if let Some(override_dir) = std::env::var_os("FILEID_FFMPEG_DIR") {
        let directory = PathBuf::from(override_dir);
        ensure!(directory.is_absolute(), "The configured FFmpeg directory must be absolute");
        directory
    } else {
        std::env::current_exe()?.parent().context("The engine has no executable directory")?.join("media")
    };
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    let ffmpeg = directory.join(format!("ffmpeg{suffix}"));
    let ffprobe = directory.join(format!("ffprobe{suffix}"));
    for executable in [&ffmpeg, &ffprobe] {
        ensure!(executable.is_file() && !executable.is_symlink(), "The vetted FFmpeg runtime is unavailable");
        let output = capture(executable, &["-version"], PROBE_TIMEOUT, 32 * 1024)?;
        let text = String::from_utf8(output).context("Invalid FFmpeg build information")?;
        ensure!(text.contains("version 9.0.2") && text.contains("--enable-libopenh264") && text.contains("--disable-network") &&
            !text.contains("--enable-gpl") && !text.contains("--enable-nonfree"),
            "The FFmpeg runtime is not the approved LGPL/OpenH264 configuration");
    }
    Ok(Runtime { ffmpeg, ffprobe })
}

pub(super) fn available() -> bool {
    runtime().is_ok()
}

fn wait(child: &mut Child, timeout: Duration, cancelled: Option<&AtomicBool>) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Export cancelled before publication");
        }
        if let Some(status) = child.try_wait()? {
            ensure!(status.success(), "The video worker could not process this media");
            return Ok(());
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("The video worker timed out");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn capture(executable: &Path, args: &[&str], timeout: Duration, maximum: usize) -> Result<Vec<u8>> {
    capture_cancellable(executable, args, timeout, maximum, None)
}

fn capture_cancellable(executable: &Path, args: &[&str], timeout: Duration, maximum: usize, cancelled: Option<&AtomicBool>) -> Result<Vec<u8>> {
    let mut child = command(executable)?;
    let (mut child, _lifetime) = spawn(child.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null()))?;
    let mut stdout = child.stdout.take().context("The video worker has no output")?;
    let reader = thread::spawn(move || -> std::io::Result<(Vec<u8>, bool)> {
        let mut collected = Vec::new();
        let mut truncated = false;
        let mut buffer = [0u8; 4096];
        loop {
            let count = stdout.read(&mut buffer)?;
            if count == 0 { break; }
            if collected.len() + count > maximum { truncated = true; }
            if collected.len() < maximum {
                collected.extend_from_slice(&buffer[..count.min(maximum - collected.len())]);
            }
        }
        Ok((collected, truncated))
    });
    let status = wait(&mut child, timeout, cancelled);
    let (output, truncated) = reader.join().map_err(|_| anyhow::anyhow!("The video worker output failed"))??;
    status?;
    ensure!(!truncated, "The video worker returned too much metadata");
    Ok(output)
}

fn command(executable: &Path) -> Result<Command> {
    let command = Command::new(executable);
    #[cfg(target_os = "linux")]
    let command = {
        let mut command = command;
        command.env("LD_LIBRARY_PATH", executable.parent().context("The video worker has no directory")?);
        let parent = unsafe { libc::getpid() };
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    return Err(std::io::Error::other("The engine exited before its video worker started"));
                }
                Ok(())
            });
        }
        command
    };
    Ok(command)
}

#[cfg(windows)]
struct ChildLifetime { _job: OwnedHandle }
#[cfg(not(windows))]
struct ChildLifetime;

fn spawn(command: &mut Command) -> Result<(Child, ChildLifetime)> {
    #[cfg(windows)]
    {
        let handle = unsafe { CreateJobObjectW(None, PCWSTR::null()) }
            .context("Could not create a video worker job")?;
        let job = unsafe { OwnedHandle::from_raw_handle(handle.0) };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        unsafe {
            SetInformationJobObject(
                HANDLE(job.as_raw_handle()),
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        }.context("Could not protect the video worker job")?;
        let mut child = command.spawn().context("Could not start the video worker")?;
        if let Err(error) = unsafe { AssignProcessToJobObject(HANDLE(job.as_raw_handle()), HANDLE(child.as_raw_handle())) } {
            if child.try_wait().ok().flatten().is_some() {
                return Ok((child, ChildLifetime { _job: job }));
            }
            let _ = child.kill();
            let _ = child.wait();
            return Err(error).context("Could not attach the video worker to its lifetime job");
        }
        Ok((child, ChildLifetime { _job: job }))
    }
    #[cfg(not(windows))]
    {
        Ok((command.spawn().context("Could not start the video worker")?, ChildLifetime))
    }
}

fn number(value: &Value) -> Option<f64> {
    value.as_f64().or_else(|| value.as_str()?.parse().ok()).filter(|value: &f64| value.is_finite())
}

fn parse_probe(data: &[u8]) -> Result<Probe> {
    let document: Value = serde_json::from_slice(data).context("Invalid video metadata")?;
    let streams = document["streams"].as_array().context("Missing video streams")?;
    ensure!((1..=2).contains(&streams.len()), "Video must have one video and at most one audio stream");
    let video = streams.iter().filter(|stream| stream["codec_type"] == "video").collect::<Vec<_>>();
    let audio = streams.iter().filter(|stream| stream["codec_type"] == "audio").collect::<Vec<_>>();
    ensure!(video.len() == 1 && audio.len() <= 1 && video.len() + audio.len() == streams.len(), "Video must have one video and at most one audio stream");
    let video = video[0];
    let width = video["width"].as_u64().context("Missing video width")?;
    let height = video["height"].as_u64().context("Missing video height")?;
    let duration = number(&document["format"]["duration"]).context("Missing video duration")?;
    ensure!(duration > 0.0 && duration <= 21_600.0 && width > 0 && height > 0 && width <= 8192 && height <= 8192 && width * height <= 32_000_000, "Unsupported video duration or dimensions");
    let transfer = video["color_transfer"].as_str().unwrap_or("unknown");
    let primaries = video["color_primaries"].as_str().unwrap_or("unknown");
    let pixel = video["pix_fmt"].as_str().unwrap_or("");
    ensure!(["unknown", "bt709", "bt470bg", "smpte170m"].contains(&transfer) &&
        ["unknown", "bt709", "bt470bg", "smpte170m"].contains(&primaries) &&
        ["yuv420p", "yuvj420p", "nv12"].contains(&pixel),
        "HDR or high-fidelity video needs a separate color-managed adapter");
    let mut display_width = width as u32;
    let mut display_height = height as u32;
    if let Some(side_data) = video["side_data_list"].as_array() {
        for item in side_data {
            if let Some(rotation) = number(&item["rotation"]) {
                if (rotation.abs() - 90.0).abs() <= 1.0 || (rotation.abs() - 270.0).abs() <= 1.0 {
                    std::mem::swap(&mut display_width, &mut display_height);
                }
            }
        }
    }
    let audio_stream = audio.first().copied();
    let audio_start = audio_stream.and_then(|stream| number(&stream["start_time"]));
    let audio_duration = audio_stream.and_then(|stream| number(&stream["duration"]));
    ensure!(audio_stream.is_none() || (audio_start.is_some() && audio_duration.is_some_and(|value| value > 0.0)), "Invalid audio timing");
    Ok(Probe {
        duration,
        width: display_width,
        height: display_height,
        video_start: number(&video["start_time"]).context("Invalid video timestamps")?,
        audio_start,
        audio_duration,
        audio_count: audio.len(),
        video_codec: video["codec_name"].as_str().unwrap_or("").to_owned(),
        audio_codec: audio_stream.and_then(|stream| stream["codec_name"].as_str()).map(str::to_owned),
    })
}

fn inspect(runtime: &Runtime, path: &Path) -> Result<Probe> {
    let output = capture(&runtime.ffprobe, &[
        "-v", "error", "-protocol_whitelist", "file", "-f", "mov",
        "-enable_drefs", "0", "-use_absolute_path", "0",
        "-show_streams", "-show_format", "-of", "json", "-i",
        path.to_str().context("Invalid video path")?,
    ], PROBE_TIMEOUT, 64 * 1024)?;
    parse_probe(&output)
}

pub(super) fn probe(source: &Path) -> Result<Probe> {
    ensure!(matches!(source.extension().and_then(|value| value.to_str()).map(str::to_ascii_lowercase).as_deref(), Some("mp4" | "mov" | "m4v")), "Video inputs must be MP4, MOV or M4V");
    inspect(&runtime()?, source)
}

pub(super) fn probe_stage(stage: &Path) -> Result<Probe> {
    inspect(&runtime()?, stage)
}

fn dimensions(recipe: &ToolRecipe, source: &Probe) -> Result<(u32, u32)> {
    let ratio = match recipe.video_aspect_ratio.as_deref().unwrap_or("source") {
        "source" => source.width as f64 / source.height as f64,
        "9:16" => 9.0 / 16.0,
        "16:9" => 16.0 / 9.0,
        "1:1" => 1.0,
        "4:5" => 4.0 / 5.0,
        _ => bail!("Unsupported video frame"),
    };
    let even = |value: f64| ((value.round() as u32).max(2) / 2) * 2;
    Ok(if ratio >= 1.0 {
        (recipe.max_dimension, even(f64::from(recipe.max_dimension) / ratio))
    } else {
        (even(f64::from(recipe.max_dimension) * ratio), recipe.max_dimension)
    })
}

pub(super) fn export(source: &Path, stage: &File, recipe: &ToolRecipe, before: &Probe, cancelled: &AtomicBool) -> Result<()> {
    ensure!(!cancelled.load(Ordering::Acquire), "Export cancelled before publication");
    let runtime = runtime()?;
    let (width, height) = dimensions(recipe, before)?;
    let filter = format!("scale={width}:{height}:force_original_aspect_ratio=decrease:force_divisible_by=2,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:black,format=yuv420p");
    let mut child = command(&runtime.ffmpeg)?;
    let (mut child, _lifetime) = spawn(child
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-protocol_whitelist", "file",
            "-f", "mov", "-enable_drefs", "0", "-use_absolute_path", "0", "-i"])
        .arg(source)
        .args(["-map", "0:v:0", "-map", "0:a:0?", "-map_metadata", "-1", "-map_metadata:s", "-1",
            "-map_chapters", "-1", "-vf", &filter,
            "-c:v", "libopenh264", "-b:v", if recipe.max_dimension == 1280 { "5M" } else { "8M" },
            "-c:a", "aac", "-b:a", "192k", "-fps_mode", "passthrough", "-movflags", "frag_keyframe+empty_moov+default_base_moof",
            "-f", "mp4", "pipe:1"])
        .stdin(Stdio::null())
        .stdout(Stdio::from(stage.try_clone()?))
        .stderr(Stdio::null()))?;
    wait(&mut child, EXPORT_TIMEOUT, Some(cancelled))?;
    stage.sync_all()?;
    Ok(())
}

pub(super) fn verify(source: &Probe, output: &Probe, recipe: &ToolRecipe) -> Result<()> {
    let (width, height) = dimensions(recipe, source)?;
    ensure!(output.video_codec == "h264" && output.width == width && output.height == height && output.audio_count == source.audio_count,
        "Export validation rejected the codec, dimensions or stream count");
    ensure!((output.duration - source.duration).abs() <= 0.25, "Export validation rejected changed duration");
    if source.audio_count == 1 {
        ensure!(output.audio_codec.as_deref() == Some("aac") &&
            ((output.audio_start.context("Missing output audio start")? - output.video_start) -
            (source.audio_start.context("Missing source audio start")? - source.video_start)).abs() <= 0.1 &&
            (output.audio_duration.context("Missing output audio duration")? - source.audio_duration.context("Missing source audio duration")?).abs() <= 0.25,
            "Export validation rejected audio timing or codec");
    }
    Ok(())
}

/// Read-only facts the timeline worker needs. Unlike `probe`, extra data or
/// subtitle tracks and wide-gamut sources are accepted: frames are only
/// decoded for analysis, never re-encoded into a user-visible output.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct AnalysisProbe {
    pub(crate) duration: f64,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

fn demuxer(source: &Path) -> Result<&'static str> {
    match source.extension().and_then(|value| value.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("mp4" | "mov" | "m4v" | "3gp") => Ok("mov"),
        Some("mkv" | "webm") => Ok("matroska"),
        Some("avi") => Ok("avi"),
        _ => bail!("Timeline analysis supports MP4, MOV, M4V, 3GP, MKV, WebM and AVI videos"),
    }
}

fn input_args(source: &Path) -> Result<Vec<String>> {
    let format = demuxer(source)?;
    let mut args: Vec<String> = ["-protocol_whitelist", "file", "-f", format].map(str::to_owned).into();
    if format == "mov" {
        args.extend(["-enable_drefs", "0", "-use_absolute_path", "0"].map(str::to_owned));
    }
    Ok(args)
}

fn parse_analysis_probe(data: &[u8]) -> Result<AnalysisProbe> {
    let document: Value = serde_json::from_slice(data).context("Invalid video metadata")?;
    let streams = document["streams"].as_array().context("Missing video streams")?;
    let video = streams.iter().find(|stream| stream["codec_type"] == "video" && stream["disposition"]["attached_pic"] != 1)
        .context("The file has no video stream")?;
    let width = video["width"].as_u64().context("Missing video width")?;
    let height = video["height"].as_u64().context("Missing video height")?;
    let duration = number(&document["format"]["duration"]).or_else(|| number(&video["duration"])).context("Missing video duration")?;
    ensure!(duration > 0.0 && duration <= crate::pipeline::timeline_analysis::MAX_DURATION_SECONDS && width > 0 && height > 0 && width <= 16_384 && height <= 16_384,
        "Unsupported video duration or dimensions");
    let rotated = video["side_data_list"].as_array().is_some_and(|items| items.iter().filter_map(|item| number(&item["rotation"]))
        .any(|rotation| (rotation.abs() - 90.0).abs() <= 1.0 || (rotation.abs() - 270.0).abs() <= 1.0));
    let (width, height) = if rotated { (height as u32, width as u32) } else { (width as u32, height as u32) };
    Ok(AnalysisProbe { duration, width, height })
}

pub(crate) fn analysis_probe(source: &Path, cancelled: &AtomicBool) -> Result<AnalysisProbe> {
    let runtime = runtime()?;
    let mut args = vec!["-v".to_owned(), "error".to_owned()];
    args.extend(input_args(source)?);
    args.extend(["-show_streams", "-show_format", "-of", "json", "-i"].map(str::to_owned));
    args.push(source.to_str().context("Invalid video path")?.to_owned());
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    parse_analysis_probe(&capture_cancellable(&runtime.ffprobe, &args, PROBE_TIMEOUT, 256 * 1024, Some(cancelled))?)
}

fn decode_args(source: &Path, start: f64, length: Option<f64>, filter: &str, pixel_format: &str) -> Result<Vec<String>> {
    ensure!(start.is_finite() && start >= 0.0, "Invalid frame time");
    let mut args: Vec<String> = ["-hide_banner", "-loglevel", "error", "-nostdin"].map(str::to_owned).into();
    args.extend(["-ss".to_owned(), format!("{start:.3}")]);
    if let Some(length) = length {
        ensure!(length.is_finite() && length > 0.0, "Invalid frame interval");
        args.extend(["-t".to_owned(), format!("{length:.3}")]);
    }
    args.extend(input_args(source)?);
    args.push("-i".to_owned());
    args.push(source.to_str().context("Invalid video path")?.to_owned());
    args.extend(["-map", "0:v:0", "-an", "-sn", "-dn", "-vf", filter, "-pix_fmt", pixel_format, "-f", "rawvideo", "pipe:1"].map(str::to_owned));
    Ok(args)
}

const SIGNAL_TIMEOUT: Duration = Duration::from_secs(90);

/// 1 fps grayscale signatures for `[start, end)`, keyed by media time.
pub(crate) fn signal_frames(source: &Path, start: f64, end: f64, cancelled: &AtomicBool) -> Result<Vec<(f64, Vec<u8>)>> {
    use crate::pipeline::timeline_analysis::{SIGNAL_HEIGHT, SIGNAL_WIDTH};
    ensure!(end > start, "Invalid signal interval");
    let runtime = runtime()?;
    let filter = format!("fps=1,scale={SIGNAL_WIDTH}:{SIGNAL_HEIGHT}:flags=area");
    let args = decode_args(source, start, Some(end - start), &filter, "gray")?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let frame = SIGNAL_WIDTH * SIGNAL_HEIGHT;
    let maximum = frame * ((end - start).ceil() as usize + 2);
    let data = capture_cancellable(&runtime.ffmpeg, &args, SIGNAL_TIMEOUT, maximum, Some(cancelled))?;
    ensure!(data.len() % frame == 0, "The video worker returned a partial signal frame");
    Ok(data.chunks_exact(frame).enumerate().map(|(index, pixels)| (start + index as f64, pixels.to_vec())).collect())
}

/// Even dimensions whose long side is at most `max_side`, preserving aspect.
pub(crate) fn fitted_size(width: u32, height: u32, max_side: u32) -> (u32, u32) {
    let scale = (f64::from(max_side) / f64::from(width.max(height))).min(1.0);
    let even = |value: f64| ((value.round() as u32).max(2) / 2) * 2;
    (even(f64::from(width) * scale), even(f64::from(height) * scale))
}

/// One RGB frame at `seconds`, scaled to `width`×`height`.
pub(crate) fn frame_rgb(source: &Path, seconds: f64, width: u32, height: u32, cancelled: &AtomicBool) -> Result<Vec<u8>> {
    let runtime = runtime()?;
    let filter = format!("scale={width}:{height}:flags=bicubic");
    let mut args = decode_args(source, seconds, None, &filter, "rgb24")?;
    let position = args.iter().position(|arg| arg == "-vf").context("Invalid decoder arguments")?;
    args.splice(position..position, ["-frames:v".to_owned(), "1".to_owned()]);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let expected = width as usize * height as usize * 3;
    let data = capture_cancellable(&runtime.ffmpeg, &args, PROBE_TIMEOUT, expected, Some(cancelled))?;
    ensure!(data.len() == expected, "The video worker could not decode a frame at {seconds:.3} seconds");
    Ok(data)
}

/// Silent clip from a lavfi source such as `testsrc2=size=64x36:rate=5:duration=6`.
#[cfg(test)]
pub(super) fn generate_silent_test_video(source: &Path, lavfi: &str) {
    let runtime = runtime().unwrap();
    let status = command(&runtime.ffmpeg).unwrap()
        .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", lavfi])
        .args(["-c:v", "libopenh264", "-b:v", "200k", "-y"])
        .arg(source)
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .status().unwrap();
    assert!(status.success(), "could not create the silent video test fixture");
}

#[cfg(test)]
pub(super) fn generate_test_video(source: &Path) {
    let runtime = runtime().unwrap();
    let status = command(&runtime.ffmpeg).unwrap()
        .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i",
            "testsrc2=size=180x320:rate=24:duration=2", "-f", "lavfi", "-i",
            "sine=frequency=440:duration=2", "-c:v", "libopenh264", "-b:v", "1M",
            "-c:a", "aac", "-b:a", "96k", "-metadata", "comment=private-location",
            "-metadata:s:v:0", "title=private-title", "-shortest", "-y"])
        .arg(source)
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .status().unwrap();
    assert!(status.success(), "could not create the video test fixture");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_rejects_auxiliary_streams_and_hdr() {
        let good = br#"{"format":{"duration":"2.0"},"streams":[{"codec_type":"video","codec_name":"h264","width":640,"height":360,"start_time":"0","pix_fmt":"yuv420p"}]}"#;
        let probe = parse_probe(good).unwrap();
        assert_eq!((probe.width, probe.height), (640, 360));
        let extra = br#"{"format":{"duration":"2.0"},"streams":[{"codec_type":"video","width":640,"height":360,"start_time":"0"},{"codec_type":"subtitle"}]}"#;
        assert!(parse_probe(extra).is_err());
        let hdr = br#"{"format":{"duration":"2.0"},"streams":[{"codec_type":"video","width":640,"height":360,"start_time":"0","pix_fmt":"yuv420p10le"}]}"#;
        assert!(parse_probe(hdr).is_err());
        let hdr_transfer = br#"{"format":{"duration":"2.0"},"streams":[{"codec_type":"video","width":640,"height":360,"start_time":"0","pix_fmt":"yuv420p","color_transfer":"arib-std-b67"}]}"#;
        assert!(parse_probe(hdr_transfer).is_err());
        let wide_gamut = br#"{"format":{"duration":"2.0"},"streams":[{"codec_type":"video","width":640,"height":360,"start_time":"0","pix_fmt":"yuv420p","color_primaries":"bt2020"}]}"#;
        assert!(parse_probe(wide_gamut).is_err());
        let subsampling = br#"{"format":{"duration":"2.0"},"streams":[{"codec_type":"video","width":640,"height":360,"start_time":"0","pix_fmt":"yuv422p"}]}"#;
        assert!(parse_probe(subsampling).is_err());
    }

    #[test]
    fn frame_dimensions_preserve_source_or_fit_selected_canvas() {
        let source = Probe { duration: 2.0, width: 640, height: 360, video_start: 0.0,
            audio_start: None, audio_duration: None, audio_count: 0, video_codec: "h264".into(), audio_codec: None };
        let mut recipe = ToolRecipe { kind: "video".into(), format: "mp4".into(), max_dimension: 1280,
            allow_upscale: None, video_aspect_ratio: None };
        assert_eq!(dimensions(&recipe, &source).unwrap(), (1280, 720));
        recipe.video_aspect_ratio = Some("9:16".into());
        assert_eq!(dimensions(&recipe, &source).unwrap(), (720, 1280));
        recipe.video_aspect_ratio = Some("1:1".into());
        assert_eq!(dimensions(&recipe, &source).unwrap(), (1280, 1280));
    }

    #[test]
    fn cancellation_stops_and_reaps_video_worker() {
        if std::env::var_os("FILEID_FFMPEG_DIR").is_none() {
            return;
        }
        let runtime = runtime().unwrap();
        let mut child = command(&runtime.ffmpeg).unwrap()
            .args(["-hide_banner", "-loglevel", "error", "-re", "-f", "lavfi", "-i",
                "testsrc2=size=64x64:rate=24:duration=30", "-f", "null", "-"])
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .spawn().unwrap();
        let cancelled = AtomicBool::new(false);
        thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(200));
                cancelled.store(true, Ordering::Release);
            });
            let start = Instant::now();
            let error = wait(&mut child, Duration::from_secs(5), Some(&cancelled)).unwrap_err();
            assert!(error.to_string().contains("cancelled"));
            assert!(start.elapsed() < Duration::from_secs(3));
        });
        assert!(child.try_wait().unwrap().is_some());
    }

    #[cfg(windows)]
    #[test]
    fn closing_worker_job_terminates_child() {
        if std::env::var_os("FILEID_FFMPEG_DIR").is_none() { return; }
        let runtime = runtime().unwrap();
        let mut command = command(&runtime.ffmpeg).unwrap();
        let (mut child, lifetime) = spawn(command.args(["-hide_banner", "-loglevel", "error",
            "-re", "-f", "lavfi", "-i", "testsrc2=size=64x64:rate=24:duration=30",
            "-f", "null", "-"])
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())).unwrap();
        thread::sleep(Duration::from_millis(100));
        assert!(child.try_wait().unwrap().is_none(), "The worker fixture ended before job closure");
        drop(lifetime);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            assert!(Instant::now() < deadline, "The video worker survived its parent job");
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parent_death_child_helper() {
        let Some(record) = std::env::var_os("FILEID_PARENT_DEATH_TEST") else { return; };
        let runtime = runtime().unwrap();
        let mut command = command(&runtime.ffmpeg).unwrap();
        let (child, _lifetime) = spawn(command.args(["-hide_banner", "-loglevel", "error",
            "-re", "-f", "lavfi", "-i", "testsrc2=size=64x64:rate=24:duration=30",
            "-f", "null", "-"])
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())).unwrap();
        std::fs::write(record, child.id().to_string()).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn engine_parent_death_terminates_video_worker() {
        if std::env::var_os("FILEID_FFMPEG_DIR").is_none() { return; }
        let record = std::env::temp_dir().join(format!("fileid-child-pid-{}", uuid::Uuid::new_v4()));
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "commands::video_export::tests::parent_death_child_helper"])
            .env("FILEID_PARENT_DEATH_TEST", &record)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .status().unwrap();
        assert!(status.success());
        let pid: u32 = std::fs::read_to_string(&record).unwrap().parse().unwrap();
        let _ = std::fs::remove_file(record);
        let deadline = Instant::now() + Duration::from_secs(3);
        while Path::new(&format!("/proc/{pid}/status")).exists() {
            let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
            if status.lines().any(|line| line.starts_with("State:") && line.contains("zombie")) { break; }
            assert!(Instant::now() < deadline, "The video worker survived its engine parent");
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn vetted_runtime_exports_tone_video_with_preserved_source() {
        if std::env::var_os("FILEID_FFMPEG_DIR").is_none() {
            return;
        }
        let runtime = runtime().unwrap();
        let directory = std::env::temp_dir().join(format!("fileid-video-qa-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
        }
        let _cleanup = Cleanup(directory.clone());
        let source = directory.join("portrait.mp4");
        generate_test_video(&source);
        let original = std::fs::read(&source).unwrap();
        let before = probe(&source).unwrap();
        let stage_path = directory.join("export.part");
        let stage = File::create(&stage_path).unwrap();
        let recipe = ToolRecipe { kind: "video".into(), format: "mp4".into(), max_dimension: 1280,
            allow_upscale: None, video_aspect_ratio: Some("16:9".into()) };
        export(&source, &stage, &recipe, &before, &AtomicBool::new(false)).unwrap();
        let after = probe_stage(&stage_path).unwrap();
        verify(&before, &after, &recipe).unwrap();
        assert_eq!((after.width, after.height), (1280, 720));
        let output_metadata = capture(&runtime.ffprobe, &["-v", "error", "-show_format", "-show_streams",
            stage_path.to_str().unwrap()], PROBE_TIMEOUT, 64 * 1024).unwrap();
        assert!(!String::from_utf8_lossy(&output_metadata).contains("private-"));
        let frame = capture(&runtime.ffmpeg, &["-v", "error", "-f", "mov", "-i",
            stage_path.to_str().unwrap(), "-frames:v", "1", "-pix_fmt", "rgb24", "-f", "rawvideo", "pipe:1"],
            PROBE_TIMEOUT, 4 * 1024 * 1024).unwrap();
        assert_eq!(frame.len(), 1280 * 720 * 3);
        let pixel = |x: usize, y: usize| &frame[(y * 1280 + x) * 3..(y * 1280 + x) * 3 + 3];
        assert_eq!(pixel(10, 360), &[0, 0, 0]);
        assert_ne!(pixel(640, 360), &[0, 0, 0]);
        assert_eq!(std::fs::read(&source).unwrap(), original);
    }
}
