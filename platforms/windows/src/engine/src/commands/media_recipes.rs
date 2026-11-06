use crate::ipc::ToolRecipe;
use anyhow::{bail, ensure, Context, Result};
use serde_json::Value;
use std::{
    fs::File,
    path::Path,
    sync::{atomic::AtomicBool, Arc},
};

pub(super) const KINDS: [&str; 5] = ["trim", "remux", "audio", "proxy", "rotate"];

pub(super) const UNAVAILABLE: &str = "Trim, remux, audio, proxy and rotate need a reviewed FFmpeg media launcher that this engine does not include yet. Nothing was created.";

pub(super) mod message {
    pub const VIDEO_ONLY_FIELDS: &str = "Upscaling, framing and stabilization apply only to video export.";
    pub const TRIM_ONLY_FIELDS: &str = "Trim times and mode apply only to trim recipes.";
    pub const ROTATE_ONLY_FIELDS: &str = "Rotation applies only to rotate recipes.";
    pub const TRIM_FORMAT: &str = "Trim outputs must be MP4 or MOV.";
    pub const TRIM_MODE: &str = "Choose keyframe or exact trimming.";
    pub const EXACT_TRIM_FORMAT: &str = "Exact trims re-encode to H.264/AAC MP4; choose MP4.";
    pub const TRIM_TIMES: &str = "Choose a trim start and end time.";
    pub const TRIM_NEGATIVE: &str = "Trim times must be zero or later.";
    pub const TRIM_RANGE: &str = "The trim end must be at least 0.1 seconds after its start.";
    pub const REMUX_FORMAT: &str = "Remux outputs must be MP4 or MOV.";
    pub const AUDIO_FORMAT: &str = "Audio extraction writes AAC in an M4A file.";
    pub const PROXY_FORMAT: &str = "Proxies are 640 or 960 pixel H.264 MP4 files.";
    pub const ROTATION: &str = "Choose a 90, 180 or 270 degree clockwise rotation.";
    pub const ROTATE_FORMAT: &str = "Rotated outputs must be MP4 or MOV.";
    pub const INPUT: &str = "Media tools accept MP4, MOV or M4V inputs.";
    pub const NO_VIDEO: &str = "This file has no video track.";
    pub const NO_AUDIO: &str = "This file has no audio track.";
    pub const TRACKS: &str = "Media tools support one video track and at most one audio track; other tracks are refused rather than dropped.";
    pub const COPY_CODECS: &str = "Lossless copy supports H.264 or HEVC video with AAC audio; these codecs would need re-encoding, which this recipe does not do.";
    pub const TRIM_PAST_END: &str = "The trim end is after the end of this video.";
    pub const HDR: &str = "HDR sources need a color-managed adapter; this recipe would re-encode them to SDR.";
    #[cfg_attr(not(test), expect(dead_code, reason = "ffprobe parsing serves the reviewed media launcher, which is not wired yet"))]
    pub const LIMITS: &str = "Unsupported media duration or dimensions.";
    pub fn already_container(format: &str) -> String {
        format!("This file is already {}; remuxing would only copy it.", format.to_uppercase())
    }
}

const KEYFRAME_LEAD_SECONDS: f64 = 10.0;

pub(super) fn is_media(kind: &str) -> bool {
    KINDS.contains(&kind)
}

pub(super) fn validate(recipe: &ToolRecipe) -> Result<()> {
    ensure!(is_media(&recipe.kind), "Unsupported media recipe.");
    ensure!(
        recipe.allow_upscale.is_none()
            && recipe.video_aspect_ratio.is_none()
            && recipe.video_framing.is_none()
            && recipe.video_stabilization.is_none(),
        message::VIDEO_ONLY_FIELDS
    );
    ensure!(
        recipe.kind == "trim"
            || (recipe.trim_start_seconds.is_none() && recipe.trim_end_seconds.is_none() && recipe.trim_mode.is_none()),
        message::TRIM_ONLY_FIELDS
    );
    ensure!(recipe.kind == "rotate" || recipe.rotation_degrees.is_none(), message::ROTATE_ONLY_FIELDS);
    ensure!((1..=8192).contains(&recipe.max_dimension), "Unsupported media recipe.");
    let format = recipe.format.as_str();
    match recipe.kind.as_str() {
        "trim" => {
            ensure!(["mp4", "mov"].contains(&format), message::TRIM_FORMAT);
            let mode = recipe.trim_mode.as_deref().filter(|mode| ["keyframe", "exact"].contains(mode));
            let mode = mode.context(message::TRIM_MODE)?;
            ensure!(mode == "keyframe" || format == "mp4", message::EXACT_TRIM_FORMAT);
            let (Some(start), Some(end)) = (recipe.trim_start_seconds, recipe.trim_end_seconds) else {
                bail!(message::TRIM_TIMES)
            };
            ensure!(start.is_finite() && end.is_finite(), message::TRIM_TIMES);
            ensure!(start >= 0.0, message::TRIM_NEGATIVE);
            ensure!(end - start >= 0.1, message::TRIM_RANGE);
        }
        "remux" => ensure!(["mp4", "mov"].contains(&format), message::REMUX_FORMAT),
        "audio" => ensure!(format == "m4a", message::AUDIO_FORMAT),
        "proxy" => ensure!(format == "mp4" && [640, 960].contains(&recipe.max_dimension), message::PROXY_FORMAT),
        _ => {
            ensure!(matches!(recipe.rotation_degrees, Some(90 | 180 | 270)), message::ROTATION);
            ensure!(["mp4", "mov"].contains(&format), message::ROTATE_FORMAT);
        }
    }
    Ok(())
}

pub(super) fn label(recipe: &ToolRecipe) -> String {
    match recipe.kind.as_str() {
        "trim" => " - Trim".into(),
        "remux" => " - Remux".into(),
        "audio" => " - Audio".into(),
        "proxy" => " - Proxy".into(),
        _ => format!(" - Rotated {}", recipe.rotation_degrees.unwrap_or_default()),
    }
}

pub(super) fn output_kind(recipe: &ToolRecipe) -> &'static str {
    if recipe.kind == "audio" { "audio" } else { "video" }
}

pub(super) fn role(recipe: &ToolRecipe) -> &'static str {
    if recipe.kind == "proxy" { "proxy" } else { "conversion" }
}

fn copies_streams(recipe: &ToolRecipe) -> bool {
    matches!(recipe.kind.as_str(), "remux" | "rotate") || (recipe.kind == "trim" && recipe.trim_mode.as_deref() == Some("keyframe"))
}

fn source_container(source: &Path) -> Option<&'static str> {
    match source.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "mp4" | "m4v" => Some("mp4"),
        "mov" => Some("mov"),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct MediaProbe {
    pub(super) duration: f64,
    pub(super) video_count: usize,
    pub(super) audio_count: usize,
    pub(super) other_count: usize,
    pub(super) width: f64,
    pub(super) height: f64,
    pub(super) rotation: u32,
    pub(super) video_codec: Option<String>,
    pub(super) audio_codec: Option<String>,
    pub(super) video_start: Option<f64>,
    pub(super) audio_start: Option<f64>,
    pub(super) audio_duration: Option<f64>,
    pub(super) hdr: bool,
}

#[cfg_attr(not(test), expect(dead_code, reason = "ffprobe parsing serves the reviewed media launcher, which is not wired yet"))]
fn number(value: &Value) -> Option<f64> {
    value.as_f64().or_else(|| value.as_str()?.parse().ok()).filter(|value: &f64| value.is_finite())
}

#[cfg_attr(not(test), expect(dead_code, reason = "ffprobe parsing serves the reviewed media launcher, which is not wired yet"))]
pub(super) fn parse_probe(data: &[u8]) -> Result<MediaProbe> {
    let document: Value = serde_json::from_slice(data).context("Invalid media metadata")?;
    let streams = document["streams"].as_array().context("Missing media streams")?;
    let duration = number(&document["format"]["duration"]).context("Missing media duration")?;
    ensure!(duration > 0.0 && duration <= 21_600.0, message::LIMITS);
    let mut probe = MediaProbe {
        duration, video_count: 0, audio_count: 0, other_count: 0, width: 0.0, height: 0.0, rotation: 0,
        video_codec: None, audio_codec: None, video_start: None, audio_start: None, audio_duration: None, hdr: false,
    };
    for stream in streams {
        match stream["codec_type"].as_str() {
            Some("video") if stream["disposition"]["attached_pic"] != 1 => {
                probe.video_count += 1;
                if probe.video_count > 1 { continue; }
                let width = stream["width"].as_u64().context("Missing video width")?;
                let height = stream["height"].as_u64().context("Missing video height")?;
                ensure!(width > 0 && height > 0 && width <= 8192 && height <= 8192 && width * height <= 32_000_000, message::LIMITS);
                let counter_clockwise = stream["side_data_list"].as_array()
                    .and_then(|items| items.iter().find_map(|item| number(&item["rotation"])))
                    .unwrap_or(0.0);
                probe.rotation = ((((-counter_clockwise / 90.0).round() as i64) * 90).rem_euclid(360)) as u32;
                (probe.width, probe.height) = if probe.rotation % 180 == 90 { (height as f64, width as f64) } else { (width as f64, height as f64) };
                probe.video_codec = stream["codec_name"].as_str().map(str::to_owned);
                probe.video_start = Some(number(&stream["start_time"]).context("Invalid video timestamps")?);
                let sdr = ["unknown", "bt709", "bt470bg", "smpte170m"];
                probe.hdr = !(sdr.contains(&stream["color_transfer"].as_str().unwrap_or("unknown"))
                    && sdr.contains(&stream["color_primaries"].as_str().unwrap_or("unknown"))
                    && ["yuv420p", "yuvj420p", "nv12"].contains(&stream["pix_fmt"].as_str().unwrap_or("")));
            }
            Some("video" | "data") => {}
            Some("audio") => {
                probe.audio_count += 1;
                if probe.audio_count > 1 { continue; }
                probe.audio_codec = stream["codec_name"].as_str().map(str::to_owned);
                probe.audio_start = Some(number(&stream["start_time"]).context("Invalid audio timestamps")?);
                let length = number(&stream["duration"]).context("Invalid audio timestamps")?;
                ensure!(length > 0.0, "Invalid audio timestamps");
                probe.audio_duration = Some(length);
            }
            _ => probe.other_count += 1,
        }
    }
    Ok(probe)
}

pub(super) fn check_source(recipe: &ToolRecipe, source: &Path, probe: &MediaProbe) -> Result<()> {
    let container = source_container(source).context(message::INPUT)?;
    ensure!(probe.other_count == 0 && probe.video_count <= 1 && probe.audio_count <= 1, message::TRACKS);
    if recipe.kind == "audio" {
        ensure!(probe.audio_count == 1, message::NO_AUDIO);
        return Ok(());
    }
    ensure!(probe.video_count == 1, message::NO_VIDEO);
    if copies_streams(recipe) {
        ensure!(
            matches!(probe.video_codec.as_deref(), Some("h264" | "hevc"))
                && (probe.audio_count == 0 || probe.audio_codec.as_deref() == Some("aac")),
            message::COPY_CODECS
        );
    } else {
        ensure!(!probe.hdr, message::HDR);
    }
    if recipe.kind == "remux" && container == recipe.format {
        bail!(message::already_container(&recipe.format))
    }
    if recipe.kind == "trim" && recipe.trim_end_seconds.is_some_and(|end| end > probe.duration + 0.05) {
        bail!(message::TRIM_PAST_END)
    }
    Ok(())
}

fn trim_range(recipe: &ToolRecipe, source: &MediaProbe) -> (f64, f64) {
    match (recipe.kind.as_str(), recipe.trim_start_seconds, recipe.trim_end_seconds) {
        ("trim", Some(start), Some(end)) => (start, end.min(source.duration)),
        _ => (0.0, source.duration),
    }
}

fn expected_audio_duration(recipe: &ToolRecipe, source: &MediaProbe) -> Option<f64> {
    let (start, end) = trim_range(recipe, source);
    let (audio_start, audio_length) = (source.audio_start?, source.audio_duration?);
    Some(((audio_start + audio_length).min(end) - audio_start.max(start)).max(0.0))
}

pub(super) fn ffmpeg_args(recipe: &ToolRecipe, source: &Path, probe: &MediaProbe) -> Result<Vec<String>> {
    let owned = |items: &[&str]| items.iter().map(|item| (*item).to_owned()).collect::<Vec<_>>();
    let mut args = owned(&["-hide_banner", "-loglevel", "error", "-nostdin"]);
    if recipe.kind == "trim" {
        let (start, end) = trim_range(recipe, probe);
        args.extend(["-ss".to_owned(), format!("{start:.3}"), "-t".to_owned(), format!("{:.3}", end - start)]);
    }
    if recipe.kind == "rotate" {
        let clockwise = (probe.rotation + recipe.rotation_degrees.context(message::ROTATION)?) % 360;
        args.extend(["-display_rotation:v:0".to_owned(), format!("{}", -(clockwise as i64))]);
    }
    args.extend(owned(&["-protocol_whitelist", "file", "-f", "mov", "-enable_drefs", "0", "-use_absolute_path", "0", "-i"]));
    args.push(source.to_str().context("Invalid media path")?.to_owned());
    if recipe.kind == "audio" {
        args.extend(owned(&["-map", "0:a:0", "-vn"]));
    } else {
        args.extend(owned(&["-map", "0:v:0", "-map", "0:a:0?"]));
    }
    args.extend(owned(&["-map_metadata", "-1", "-map_metadata:s", "-1", "-map_chapters", "-1"]));
    match recipe.kind.as_str() {
        "audio" => args.extend(owned(&["-c:a", "aac", "-b:a", "192k"])),
        "proxy" => {
            let side = recipe.max_dimension;
            args.extend(["-vf".to_owned(), format!(
                "scale='if(gte(iw,ih),min(iw,{side}),-2)':'if(gte(iw,ih),-2,min(ih,{side}))',format=yuv420p"
            )]);
            args.extend(owned(&["-c:v", "libopenh264", "-b:v", if side == 640 { "1M" } else { "2M" },
                "-c:a", "aac", "-b:a", "128k", "-fps_mode", "passthrough"]));
        }
        "trim" if recipe.trim_mode.as_deref() == Some("exact") => args.extend(owned(&[
            "-vf", "format=yuv420p", "-c:v", "libopenh264", "-b:v", "8M", "-c:a", "aac", "-b:a", "192k", "-fps_mode", "passthrough",
        ])),
        _ => args.extend(owned(&["-c", "copy"])),
    }
    if recipe.kind == "trim" {
        args.extend(owned(&["-avoid_negative_ts", "make_zero"]));
    }
    let muxer = match recipe.format.as_str() {
        "mov" => "mov",
        "m4a" => "ipod",
        _ => "mp4",
    };
    args.extend(owned(&["-movflags", "frag_keyframe+empty_moov+default_base_moof", "-f", muxer, "pipe:1"]));
    Ok(args)
}

pub(super) fn verify(recipe: &ToolRecipe, source: &MediaProbe, output: &MediaProbe) -> Result<()> {
    let (start, end) = trim_range(recipe, source);
    let keyframe_trim = recipe.kind == "trim" && recipe.trim_mode.as_deref() == Some("keyframe");
    let tolerance = if matches!(recipe.kind.as_str(), "remux" | "rotate") { 0.1 } else { 0.25 };
    if recipe.kind == "audio" {
        ensure!(output.video_count == 0 && output.audio_count == 1 && output.other_count == 0 && output.audio_codec.as_deref() == Some("aac"),
            "Output validation rejected the audio streams or codec.");
        let expected = expected_audio_duration(recipe, source).context(message::NO_AUDIO)?;
        ensure!(output.audio_duration.is_some_and(|length| (length - expected).abs() <= tolerance),
            "Output validation rejected changed audio duration.");
        return Ok(());
    }
    ensure!(output.video_count == 1 && output.audio_count == source.audio_count && output.other_count == 0,
        "Output validation rejected changed stream counts.");
    if copies_streams(recipe) {
        ensure!(output.video_codec == source.video_codec && output.audio_codec == source.audio_codec,
            "Output validation rejected re-encoded streams.");
    } else {
        ensure!(output.video_codec.as_deref() == Some("h264") && (source.audio_count == 0 || output.audio_codec.as_deref() == Some("aac")),
            "Output validation rejected the codec.");
    }
    let length = end - start;
    let lead = if keyframe_trim { KEYFRAME_LEAD_SECONDS } else { 0.0 };
    ensure!(output.duration >= length - tolerance && output.duration <= length + tolerance + lead,
        "Output validation rejected changed duration.");
    if let (Some(video), Some(audio)) = (source.video_start, source.audio_start) {
        let expected = (audio - start).max(0.0) - (video - start).max(0.0);
        let (Some(out_video), Some(out_audio), Some(out_length)) = (output.video_start, output.audio_start, output.audio_duration) else {
            bail!("Output validation rejected audio/video sync.")
        };
        let offset = out_audio - out_video;
        let extra = (output.duration - length).max(0.0);
        let synced = if keyframe_trim { offset >= expected - 0.1 && offset <= expected + extra + 0.1 } else { (offset - expected).abs() <= 0.1 };
        let expected_length = expected_audio_duration(recipe, source).unwrap_or(out_length);
        ensure!(synced && (out_length - expected_length).abs() <= 0.25, "Output validation rejected audio/video sync.");
    }
    let rotation = if recipe.kind == "rotate" { (source.rotation + recipe.rotation_degrees.unwrap_or(0)) % 360 } else { source.rotation };
    let quarter_turn = (rotation + 360 - source.rotation) % 180 == 90;
    let ratio = if quarter_turn { source.height / source.width } else { source.width / source.height };
    let baked = !copies_streams(recipe) && output.rotation == 0;
    ensure!((output.rotation == rotation || baked) && (output.width / output.height - ratio).abs() <= 0.02 * ratio,
        "Output validation rejected changed orientation or aspect.");
    if recipe.kind == "proxy" {
        ensure!(output.width.max(output.height) <= f64::from(recipe.max_dimension) + 1.0, "Output validation rejected the proxy dimensions.");
    } else if copies_streams(recipe) {
        ensure!((output.width.max(output.height) - source.width.max(source.height)).abs() <= 1.0, "Output validation rejected changed dimensions.");
    }
    Ok(())
}

pub(super) fn summary(recipe: &ToolRecipe) -> &'static str {
    match (recipe.kind.as_str(), recipe.trim_mode.as_deref()) {
        ("trim", Some("exact")) => "Re-encoded to H.264/AAC for a frame-accurate cut.",
        ("trim", _) => "Streams copied without re-encoding; the output may start at the keyframe before the requested time.",
        ("remux", _) => "Streams copied into a new container without re-encoding.",
        ("audio", _) => "Audio converted to AAC.",
        ("proxy", _) => "Small H.264 preview copy linked to its source.",
        _ => "Orientation changed; streams copied without re-encoding.",
    }
}

pub(super) trait MediaRuntime: Send + Sync {
    fn probe(&self, path: &Path) -> Result<MediaProbe>;
    fn run(&self, args: &[String], stage: &File, cancelled: &AtomicBool) -> Result<()>;
}

#[cfg(test)]
thread_local! {
    pub(super) static TEST_RUNTIME: std::cell::RefCell<Option<Arc<dyn MediaRuntime>>> = const { std::cell::RefCell::new(None) };
}

pub(super) fn runtime() -> Option<Arc<dyn MediaRuntime>> {
    #[cfg(test)]
    if let Some(runtime) = TEST_RUNTIME.with(|slot| slot.borrow().clone()) {
        return Some(runtime);
    }
    None
}

pub(super) fn preview(runtime: &dyn MediaRuntime, recipe: &ToolRecipe, source: &Path) -> Result<&'static str> {
    validate(recipe)?;
    source_container(source).context(message::INPUT)?;
    check_source(recipe, source, &runtime.probe(source)?)?;
    Ok(summary(recipe))
}

pub(super) fn export(runtime: &dyn MediaRuntime, recipe: &ToolRecipe, source: &Path, stage: &File, cancelled: &AtomicBool) -> Result<MediaProbe> {
    validate(recipe)?;
    let before = runtime.probe(source)?;
    check_source(recipe, source, &before)?;
    runtime.run(&ffmpeg_args(recipe, source, &before)?, stage, cancelled)?;
    stage.sync_all()?;
    Ok(before)
}

pub(super) fn verify_stage(runtime: &dyn MediaRuntime, recipe: &ToolRecipe, before: &MediaProbe, stage: &Path) -> Result<()> {
    verify(recipe, before, &runtime.probe(stage)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recipe(kind: &str, format: &str) -> ToolRecipe {
        ToolRecipe {
            kind: kind.into(), format: format.into(), max_dimension: 1920, allow_upscale: None, video_aspect_ratio: None,
            video_framing: None, video_stabilization: None, trim_start_seconds: None, trim_end_seconds: None, trim_mode: None, rotation_degrees: None,
        }
    }

    fn trim(start: Option<f64>, end: Option<f64>, mode: Option<&str>, format: &str) -> ToolRecipe {
        ToolRecipe { trim_start_seconds: start, trim_end_seconds: end, trim_mode: mode.map(str::to_owned), ..recipe("trim", format) }
    }

    fn landscape() -> MediaProbe {
        MediaProbe {
            duration: 3.0, video_count: 1, audio_count: 1, other_count: 0, width: 96.0, height: 54.0, rotation: 0,
            video_codec: Some("h264".into()), audio_codec: Some("aac".into()), video_start: Some(0.0), audio_start: Some(0.0),
            audio_duration: Some(3.0), hdr: false,
        }
    }

    #[test]
    fn validation_refuses_with_messages_shared_with_the_mac_engine() {
        let cases = [
            (ToolRecipe { video_aspect_ratio: Some("9:16".into()), ..trim(Some(0.0), Some(1.0), Some("keyframe"), "mp4") }, message::VIDEO_ONLY_FIELDS),
            (ToolRecipe { trim_start_seconds: Some(0.0), ..recipe("remux", "mp4") }, message::TRIM_ONLY_FIELDS),
            (ToolRecipe { rotation_degrees: Some(90), ..recipe("audio", "m4a") }, message::ROTATE_ONLY_FIELDS),
            (trim(Some(0.0), Some(1.0), Some("keyframe"), "m4a"), message::TRIM_FORMAT),
            (trim(Some(0.0), Some(1.0), None, "mp4"), message::TRIM_MODE),
            (trim(Some(0.0), Some(1.0), Some("exact"), "mov"), message::EXACT_TRIM_FORMAT),
            (trim(Some(1.0), None, Some("keyframe"), "mp4"), message::TRIM_TIMES),
            (trim(Some(-1.0), Some(1.0), Some("keyframe"), "mp4"), message::TRIM_NEGATIVE),
            (trim(Some(2.0), Some(2.05), Some("exact"), "mp4"), message::TRIM_RANGE),
            (recipe("remux", "m4a"), message::REMUX_FORMAT),
            (recipe("audio", "mp4"), message::AUDIO_FORMAT),
            (ToolRecipe { max_dimension: 1280, ..recipe("proxy", "mp4") }, message::PROXY_FORMAT),
            (ToolRecipe { rotation_degrees: Some(45), ..recipe("rotate", "mp4") }, message::ROTATION),
            (ToolRecipe { rotation_degrees: Some(90), ..recipe("rotate", "m4a") }, message::ROTATE_FORMAT),
        ];
        for (recipe, expected) in cases {
            assert_eq!(validate(&recipe).unwrap_err().to_string(), expected, "{recipe:?}");
        }
        for recipe in [
            trim(Some(0.5), Some(1.0), Some("keyframe"), "mov"),
            recipe("remux", "mp4"),
            recipe("audio", "m4a"),
            ToolRecipe { max_dimension: 640, ..recipe("proxy", "mp4") },
            ToolRecipe { rotation_degrees: Some(270), ..recipe("rotate", "mov") },
        ] {
            validate(&recipe).unwrap();
        }
    }

    #[test]
    fn probe_reads_codecs_timing_rotation_and_refuses_other_tracks() {
        let portrait = br#"{"format":{"duration":"2.0"},"streams":[
            {"codec_type":"video","codec_name":"h264","width":96,"height":54,"start_time":"0","pix_fmt":"yuv420p","side_data_list":[{"rotation":-90}]},
            {"codec_type":"audio","codec_name":"aac","start_time":"0.021","duration":"1.98"},
            {"codec_type":"data","codec_tag_string":"tmcd"}]}"#;
        let probe = parse_probe(portrait).unwrap();
        assert_eq!((probe.width, probe.height, probe.rotation), (54.0, 96.0, 90));
        assert_eq!((probe.video_codec.as_deref(), probe.audio_codec.as_deref(), probe.other_count), (Some("h264"), Some("aac"), 0));
        assert!(!probe.hdr);
        let subtitled = br#"{"format":{"duration":"2.0"},"streams":[
            {"codec_type":"video","codec_name":"hevc","width":96,"height":54,"start_time":"0","pix_fmt":"yuv420p10le","color_transfer":"smpte2084"},
            {"codec_type":"subtitle","codec_name":"mov_text"}]}"#;
        let probe = parse_probe(subtitled).unwrap();
        assert!(probe.hdr);
        let error = check_source(&recipe("remux", "mp4"), Path::new("/clip.mov"), &probe).unwrap_err();
        assert_eq!(error.to_string(), message::TRACKS);
    }

    #[test]
    fn source_checks_refuse_what_cannot_be_done_faithfully() {
        let source = landscape();
        let check = |recipe: &ToolRecipe, path: &str, probe: &MediaProbe| check_source(recipe, Path::new(path), probe).err().map(|error| error.to_string());
        assert_eq!(check(&recipe("remux", "mov"), "/clip.mov", &source), Some(message::already_container("mov")));
        assert_eq!(check(&recipe("remux", "mp4"), "/clip.m4v", &source), Some(message::already_container("mp4")));
        assert_eq!(check(&recipe("remux", "mp4"), "/clip.mkv", &source).as_deref(), Some(message::INPUT));
        let pcm = MediaProbe { audio_codec: Some("pcm_s16le".into()), ..source.clone() };
        assert_eq!(check(&recipe("remux", "mp4"), "/clip.mov", &pcm).as_deref(), Some(message::COPY_CODECS));
        assert_eq!(check(&recipe("proxy", "mp4"), "/clip.mov", &pcm), None);
        let silent = MediaProbe { audio_count: 0, audio_codec: None, audio_start: None, audio_duration: None, ..source.clone() };
        assert_eq!(check(&recipe("audio", "m4a"), "/clip.mov", &silent).as_deref(), Some(message::NO_AUDIO));
        let hdr = MediaProbe { hdr: true, ..source.clone() };
        assert_eq!(check(&trim(Some(0.0), Some(1.0), Some("exact"), "mp4"), "/clip.mov", &hdr).as_deref(), Some(message::HDR));
        assert_eq!(check(&trim(Some(0.0), Some(1.0), Some("keyframe"), "mp4"), "/clip.mov", &hdr), None);
        assert_eq!(check(&trim(Some(1.0), Some(9.0), Some("keyframe"), "mp4"), "/clip.mov", &source).as_deref(), Some(message::TRIM_PAST_END));
    }

    #[test]
    fn arguments_copy_losslessly_and_re_encode_only_when_asked() {
        let source = landscape();
        let path = Path::new("/media/clip.mov");
        let args = |recipe: &ToolRecipe| ffmpeg_args(recipe, path, &source).unwrap().join(" ");
        let remux = args(&recipe("remux", "mp4"));
        assert!(remux.contains("-c copy") && remux.contains("-map_metadata -1") && remux.ends_with("-f mp4 pipe:1"), "{remux}");
        assert!(remux.contains("-enable_drefs 0") && remux.contains("-protocol_whitelist file"));
        let keyframe = args(&trim(Some(1.0), Some(2.5), Some("keyframe"), "mov"));
        assert!(keyframe.starts_with("-hide_banner -loglevel error -nostdin -ss 1.000 -t 1.500 ") && keyframe.contains("-c copy") && keyframe.ends_with("-f mov pipe:1"), "{keyframe}");
        let exact = args(&trim(Some(1.0), Some(2.5), Some("exact"), "mp4"));
        assert!(exact.contains("-c:v libopenh264") && exact.contains("-c:a aac") && !exact.contains("-c copy"), "{exact}");
        let audio = args(&recipe("audio", "m4a"));
        assert!(audio.contains("-map 0:a:0 -vn") && audio.contains("-c:a aac") && audio.ends_with("-f ipod pipe:1"), "{audio}");
        let proxy = args(&ToolRecipe { max_dimension: 640, ..recipe("proxy", "mp4") });
        assert!(proxy.contains("min(iw,640)") && proxy.contains("-c:v libopenh264"), "{proxy}");
        let portrait = MediaProbe { rotation: 90, ..source.clone() };
        let rotate = ffmpeg_args(&ToolRecipe { rotation_degrees: Some(270), ..recipe("rotate", "mp4") }, path, &portrait).unwrap().join(" ");
        assert!(rotate.contains("-display_rotation:v:0 0 -protocol_whitelist") && rotate.contains("-c copy"), "{rotate}");
        let rotate = ffmpeg_args(&ToolRecipe { rotation_degrees: Some(90), ..recipe("rotate", "mp4") }, path, &source).unwrap().join(" ");
        assert!(rotate.contains("-display_rotation:v:0 -90 "), "{rotate}");
    }

    #[test]
    fn verification_rejects_changed_streams_timing_sync_and_orientation() {
        let source = landscape();
        let remux = recipe("remux", "mp4");
        verify(&remux, &source, &source).unwrap();
        for output in [
            MediaProbe { video_codec: Some("hevc".into()), ..source.clone() },
            MediaProbe { duration: 2.5, ..source.clone() },
            MediaProbe { audio_start: Some(0.3), ..source.clone() },
            MediaProbe { audio_count: 0, audio_codec: None, ..source.clone() },
            MediaProbe { rotation: 90, width: 54.0, height: 96.0, ..source.clone() },
        ] {
            assert!(verify(&remux, &source, &output).is_err(), "{output:?}");
        }
        let rotate = ToolRecipe { rotation_degrees: Some(90), ..recipe("rotate", "mov") };
        assert!(verify(&rotate, &source, &source).is_err());
        verify(&rotate, &source, &MediaProbe { rotation: 90, width: 54.0, height: 96.0, ..source.clone() }).unwrap();
        let exact = trim(Some(1.0), Some(2.5), Some("exact"), "mp4");
        let trimmed = MediaProbe { duration: 1.5, audio_duration: Some(1.5), ..source.clone() };
        verify(&exact, &source, &trimmed).unwrap();
        assert!(verify(&exact, &source, &MediaProbe { duration: 3.0, ..trimmed.clone() }).is_err());
        let keyframe = trim(Some(1.0), Some(2.5), Some("keyframe"), "mp4");
        verify(&keyframe, &source, &MediaProbe { duration: 2.0, video_start: Some(0.0), audio_start: Some(0.5), ..trimmed.clone() }).unwrap();
        assert!(verify(&keyframe, &source, &MediaProbe { duration: 2.0, audio_start: Some(1.2), ..trimmed.clone() }).is_err());
        let proxy = ToolRecipe { max_dimension: 640, ..recipe("proxy", "mp4") };
        let portrait = MediaProbe { rotation: 90, width: 1080.0, height: 1920.0, ..source.clone() };
        verify(&proxy, &portrait, &MediaProbe { rotation: 0, width: 360.0, height: 640.0, ..portrait.clone() }).unwrap();
        assert!(verify(&proxy, &portrait, &MediaProbe { rotation: 0, width: 640.0, height: 360.0, ..portrait.clone() }).is_err());
        assert!(verify(&proxy, &portrait, &MediaProbe { width: 540.0, height: 960.0, ..portrait.clone() }).is_err());
        let audio = recipe("audio", "m4a");
        let extracted = MediaProbe { video_count: 0, video_codec: None, video_start: None, ..source.clone() };
        verify(&audio, &source, &extracted).unwrap();
        assert!(verify(&audio, &source, &MediaProbe { audio_duration: Some(2.0), ..extracted }).is_err());
    }
}
