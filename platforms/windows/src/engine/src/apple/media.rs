//! Stage 8: AVFoundation media tools, timeline frames and on-device Apple
//! Speech through FileIDAppleKit. Probes, the trim/remux/audio/proxy/rotate
//! exports and speech run in process, as in the Swift engine; frame decoding,
//! the frame pool and the video export run in this binary's own worker
//! processes (`models::native_process::self_worker`), so a decoder crash on a
//! damaged file ends a worker, not the engine.

use super::ffi;
use crate::models::native_process::self_worker::{self, Worker};
use anyhow::{bail, ensure, Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::ffi::{CStr, CString, OsStr, OsString, c_char};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

const REPLY_LIMIT: usize = 64 * 1024;
const EXIT_INVALID: i32 = 2;
const EXIT_FAILED: i32 = 3;
const EXIT_SOURCE_CHANGED: i32 = 4;

fn c_path(path: &Path) -> Result<CString> {
    CString::new(path.as_os_str().as_bytes()).context("Invalid media path")
}

fn flag(cancel: &AtomicBool) -> *const bool {
    cancel.as_ptr().cast_const()
}

/// Takes ownership of a JSON reply from the adapter.
fn reply(pointer: *mut c_char) -> Result<Value> {
    ensure!(!pointer.is_null(), "The media adapter returned no reply");
    // SAFETY: the adapter returned a NUL-terminated string it allocated for us.
    let text = unsafe { CStr::from_ptr(pointer) }.to_string_lossy().into_owned();
    // SAFETY: released exactly once, with the adapter's allocator.
    unsafe { ffi::fileid_apple_free(pointer.cast()) };
    let value: Value = serde_json::from_str(&text).context("The media adapter returned invalid JSON")?;
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        bail!("{error}")
    }
    Ok(value)
}

/// AVFoundation facts in the adapter's probe shape.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Probe {
    pub(crate) duration: f64,
    pub(crate) video_count: usize,
    pub(crate) audio_count: usize,
    pub(crate) other_count: usize,
    pub(crate) width: f64,
    pub(crate) height: f64,
    pub(crate) rotation: u32,
    pub(crate) video_codec: Option<String>,
    pub(crate) audio_codec: Option<String>,
    pub(crate) video_start: Option<f64>,
    pub(crate) audio_start: Option<f64>,
    pub(crate) audio_duration: Option<f64>,
    pub(crate) hdr: bool,
    #[serde(default)]
    pub(crate) frame_rate: Option<f64>,
}

/// Probes `path`. `strict` applies the media tools' refusals; `mime_type` types
/// a staged `.part` file.
pub(crate) fn probe(path: &Path, mime_type: Option<&str>, strict: bool) -> Result<Probe> {
    let path = c_path(path)?;
    let mime = mime_type.map(CString::new).transpose().context("Invalid media type")?;
    let mime_pointer = mime.as_ref().map_or(std::ptr::null(), |mime| mime.as_ptr());
    // SAFETY: both strings outlive the call; the reply is owned by `reply`.
    let value = reply(unsafe { ffi::fileid_apple_media_probe(path.as_ptr(), mime_pointer, strict) })?;
    serde_json::from_value(value).context("The media adapter returned an invalid probe")
}

/// Runs a trim/remux/audio/proxy/rotate export in process; returns the source
/// probe the adapter checked before exporting.
pub(crate) fn export_common(recipe: &crate::ipc::ToolRecipe, source: &Path, output: &Path, cancel: &AtomicBool) -> Result<Probe> {
    let request = serde_json::json!({
        "recipe": recipe,
        "source": source.to_str().context("Invalid media path")?,
        "output": output.to_str().context("Invalid media path")?,
    });
    let request = CString::new(request.to_string())?;
    // SAFETY: the request and the flag outlive the synchronous call.
    let value = reply(unsafe { ffi::fileid_apple_media_export(request.as_ptr(), flag(cancel)) })?;
    serde_json::from_value(value["probe"].clone()).context("The media adapter returned an invalid probe")
}

/// The planned or finished video export's framing and stabilization summary;
/// the worker verified the output before replying.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Converted {
    #[serde(default)]
    pub(crate) summary: String,
}

/// The fit/pad/stabilize/follow-people video export in a worker process;
/// `output == None` plans without writing.
pub(crate) fn convert_video(recipe: &crate::ipc::ToolRecipe, source: &Path, output: Option<&Path>, cancel: &AtomicBool) -> Result<Converted> {
    let mut request = serde_json::json!({ "recipe": recipe, "source": source.to_str().context("Invalid media path")? });
    if let Some(output) = output {
        request["output"] = Value::from(output.to_str().context("Invalid media path")?);
    }
    let planning = output.is_none() && recipe.video_framing.as_deref() != Some("auto");
    let timeout = Duration::from_secs(if planning { 30 } else { 3_600 });
    let request = request.to_string();
    let result = self_worker::run(Worker::ConvertVideo, &[OsStr::new(&request)], timeout, REPLY_LIMIT, cancel)?;
    let value: Value = serde_json::from_slice(&result.stdout).context("The native video worker did not return a valid result")?;
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        bail!("{error}")
    }
    ensure!(result.status == 0, "Native video export failed");
    serde_json::from_value(value).context("The native video worker did not return a valid result")
}

/// Decodes one frame at `seconds` into a JPEG at `output` in a worker process;
/// returns the decoded frame's time.
pub(crate) fn sample_frame(source: &Path, seconds: f64, (width, height): (u32, u32), output: &Path, cancel: &AtomicBool) -> Result<f64> {
    let (seconds, width, height) = (seconds.to_string(), width.to_string(), height.to_string());
    let args = [source.as_os_str(), OsStr::new(&seconds), OsStr::new(&width), OsStr::new(&height), output.as_os_str()];
    let result = self_worker::run(Worker::SampleVideo, &args, Duration::from_secs(20), REPLY_LIMIT, cancel)?;
    if result.status != 0 {
        let _ = std::fs::remove_file(output);
        bail!("The video worker could not decode a frame at {seconds} seconds")
    }
    let value: Value = serde_json::from_slice(&result.stdout).context("The video worker returned an invalid reply")?;
    value["seconds"].as_f64().filter(|seconds| seconds.is_finite()).context("The video worker returned an invalid reply")
}

/// One pool frame from `manifest.json`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub(crate) struct PoolFrame {
    pub(crate) seconds: f64,
    pub(crate) file: String,
    pub(crate) signature: Vec<u8>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PoolManifest {
    pub(crate) duration: f64,
    pub(crate) frames: Vec<PoolFrame>,
}

/// The frame pool for a whole video (one sequential hardware decode) in a
/// worker process: JPEGs and signatures every `interval` seconds in `directory`.
pub(crate) fn sample_pool(source: &Path, interval: f64, max_side: u32, directory: &Path, duration_hint: f64, cancel: &AtomicBool) -> Result<PoolManifest> {
    std::fs::create_dir_all(directory)?;
    let (interval, max_side) = (interval.to_string(), max_side.to_string());
    let args = [source.as_os_str(), OsStr::new(&interval), OsStr::new(&max_side), directory.as_os_str()];
    let timeout = Duration::from_secs_f64((60.0 + duration_hint.max(0.0) * 2.0).min(3_600.0));
    let result = self_worker::run(Worker::SampleVideoPool, &args, timeout, REPLY_LIMIT, cancel)?;
    match result.status {
        0 => {}
        EXIT_SOURCE_CHANGED => bail!("The source changed during analysis. Rescan it before continuing."),
        _ => bail!("The video worker could not decode a stable source frame."),
    }
    let manifest: PoolManifest = serde_json::from_slice(&std::fs::read(directory.join("manifest.json"))?)
        .context("The video worker wrote an invalid frame manifest")?;
    ensure!(
        manifest.duration.is_finite()
            && manifest.duration > 0.0
            && !manifest.frames.is_empty()
            && manifest.frames.iter().all(|frame| frame.seconds.is_finite()
                && frame.seconds >= 0.0
                && !frame.file.contains('/')
                && frame.signature.len() == crate::pipeline::timeline_analysis::SIGNAL_WIDTH * crate::pipeline::timeline_analysis::SIGNAL_HEIGHT),
        "The video worker wrote an invalid frame manifest"
    );
    Ok(manifest)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpeechStatus {
    Ready,
    Unavailable,
    /// The host bundle has no speech usage description, so macOS would end the
    /// process on a request (a bare engine binary outside FileID.app).
    CannotAsk,
}

/// On-device speech availability; with `ask`, an undecided user sees the
/// system permission prompt for FileID.app.
pub(crate) fn speech_status(ask: bool) -> SpeechStatus {
    // SAFETY: plain value call.
    match unsafe { ffi::fileid_apple_speech_status(ask) } {
        1 => SpeechStatus::Ready,
        -1 => SpeechStatus::CannotAsk,
        _ => SpeechStatus::Unavailable,
    }
}

pub(crate) fn speech_model_version() -> String {
    // SAFETY: no arguments; the reply is a string the adapter allocated for us.
    let pointer = unsafe { ffi::fileid_apple_speech_model_version() };
    if pointer.is_null() {
        return "apple-speech-ondevice-v1/unknown".into();
    }
    // SAFETY: NUL-terminated adapter string, freed once below.
    let text = unsafe { CStr::from_ptr(pointer) }.to_string_lossy().into_owned();
    // SAFETY: released exactly once, with the adapter's allocator.
    unsafe { ffi::fileid_apple_free(pointer.cast()) };
    text
}

pub(crate) use crate::pipeline::speech_passages::Segment as SpeechSegment;

/// Recognised words of `[start, end)` seconds, timestamps relative to `start`.
pub(crate) fn transcribe(source: &Path, start: f64, end: f64, cancel: &AtomicBool) -> Result<Vec<SpeechSegment>> {
    let path = c_path(source)?;
    // SAFETY: the path and the flag outlive the synchronous call.
    let value = reply(unsafe { ffi::fileid_apple_speech_transcribe(path.as_ptr(), start, end, flag(cancel)) })?;
    serde_json::from_value(value["segments"].clone()).context("The speech adapter returned invalid segments")
}

/// Whether this process was started as one of the engine's media workers.
pub(crate) fn is_worker(args: &[OsString]) -> bool {
    args.first().is_some_and(|flag| Worker::from_flag(flag).is_some())
}

/// The worker side of a re-executed media worker; `None` when `args` (without
/// the program name) do not start with a worker flag.
pub(crate) fn run_worker(args: &[OsString]) -> Option<i32> {
    let worker = Worker::from_flag(args.first()?)?;
    watch_parent();
    let args = &args[1..];
    Some(match worker {
        Worker::SampleVideo => frame_worker(args),
        Worker::SampleVideoPool => pool_worker(args),
        Worker::ConvertVideo => convert_worker(args),
    })
}

/// Ends the worker when the engine that started it goes away.
fn watch_parent() {
    // SAFETY: getppid has no preconditions.
    let parent = unsafe { libc::getppid() };
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(1));
        // SAFETY: as above; _exit ends only this worker.
        if unsafe { libc::getppid() } != parent {
            unsafe { libc::_exit(9) }
        }
    });
}

fn print(value: &Value) {
    use std::io::Write;
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(value.to_string().as_bytes());
    let _ = stdout.flush();
}

/// `<source> <seconds> <width> <height> <output.jpg>`.
fn frame_worker(args: &[OsString]) -> i32 {
    let [source, seconds, width, height, output] = args else { return EXIT_INVALID };
    let (Some(seconds), Some(width), Some(height)) = (
        seconds.to_str().and_then(|s| s.parse::<f64>().ok()).filter(|s| s.is_finite() && *s >= 0.0),
        width.to_str().and_then(|s| s.parse::<u32>().ok()).filter(|w| (2..=8192).contains(w)),
        height.to_str().and_then(|s| s.parse::<u32>().ok()).filter(|h| (2..=8192).contains(h)),
    ) else {
        return EXIT_INVALID;
    };
    let output = PathBuf::from(output);
    if output.exists() {
        return EXIT_INVALID;
    }
    let Ok(path) = c_path(Path::new(source)) else { return EXIT_INVALID };
    let mut actual = f64::NAN;
    // SAFETY: the path outlives the call; `actual` is a valid out-pointer.
    let pixels = unsafe { ffi::fileid_apple_video_frame(path.as_ptr(), seconds, width, height, &raw mut actual) };
    if pixels.is_null() {
        return EXIT_FAILED;
    }
    let length = width as usize * height as usize * 3;
    // SAFETY: the adapter returned `length` bytes it allocated for us.
    let rgb = unsafe { std::slice::from_raw_parts(pixels, length) }.to_vec();
    // SAFETY: released exactly once, with the adapter's allocator.
    unsafe { ffi::fileid_apple_free(pixels.cast()) };
    let Some(image) = image::RgbImage::from_raw(width, height, rgb) else { return EXIT_FAILED };
    let written = std::fs::OpenOptions::new().write(true).create_new(true).open(&output).map_err(anyhow::Error::from).and_then(|file| {
        let mut writer = std::io::BufWriter::new(file);
        image.write_to(&mut writer, image::ImageFormat::Jpeg)?;
        Ok(())
    });
    if written.is_err() {
        let _ = std::fs::remove_file(&output);
        return EXIT_FAILED;
    }
    print(&serde_json::json!({ "seconds": actual }));
    0
}

/// `<source> <interval> <maxPixelSize> <directory>`, the Swift engine's contract.
fn pool_worker(args: &[OsString]) -> i32 {
    let [source, interval, max_side, directory] = args else { return EXIT_INVALID };
    let (Some(interval), Some(max_side)) = (
        interval.to_str().and_then(|s| s.parse::<f64>().ok()),
        max_side.to_str().and_then(|s| s.parse::<u32>().ok()),
    ) else {
        return EXIT_INVALID;
    };
    let (Ok(path), Ok(directory)) = (c_path(Path::new(source)), c_path(Path::new(directory))) else { return EXIT_INVALID };
    // SAFETY: both strings outlive the call; no cancellation flag (the engine kills the worker).
    unsafe { ffi::fileid_apple_video_pool(path.as_ptr(), interval, max_side, directory.as_ptr(), std::ptr::null()) }
}

/// `<request JSON>`; prints the adapter's reply.
fn convert_worker(args: &[OsString]) -> i32 {
    let [request] = args else { return EXIT_INVALID };
    let Ok(request) = CString::new(request.as_bytes()) else { return EXIT_INVALID };
    // SAFETY: the request outlives the call; the reply is freed below.
    let pointer = unsafe { ffi::fileid_apple_media_export(request.as_ptr(), std::ptr::null()) };
    if pointer.is_null() {
        return EXIT_FAILED;
    }
    // SAFETY: NUL-terminated adapter string, freed once below.
    let text = unsafe { CStr::from_ptr(pointer) }.to_string_lossy().into_owned();
    // SAFETY: released exactly once, with the adapter's allocator.
    unsafe { ffi::fileid_apple_free(pointer.cast()) };
    let value: Value = serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({ "error": "Native video export failed." }));
    print(&value);
    if value.get("error").is_some() { EXIT_FAILED } else { 0 }
}
