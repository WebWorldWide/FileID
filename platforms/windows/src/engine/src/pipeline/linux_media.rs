use super::{validate_export, validate_probe, Operation, Probe, Report};
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::AtomicBool,
    time::Duration,
};
fn installed(name: &str) -> Result<PathBuf> {
    let path = PathBuf::from(
        std::env::var_os(name)
            .context("Configure absolute installed FILEID_FFMPEG and FILEID_FFPROBE paths")?,
    );
    validate_installed(&path)
}
pub(super) fn available() -> bool {
    installed("FILEID_FFMPEG").is_ok() && installed("FILEID_FFPROBE").is_ok()
}
fn validate_installed(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() || !std::fs::metadata(path)?.is_file() {
        bail!("Media executable must be an absolute installed regular file");
    }
    let path = path.canonicalize()?;
    #[cfg(target_os = "linux")]
    {
        use std::{io::Read, os::unix::fs::MetadataExt};
        let mut file = std::fs::File::open(&path)?;
        let metadata = file.metadata()?;
        if metadata.mode() & 0o111 == 0
            || metadata.mode() & 0o022 != 0
            || (metadata.uid() != 0 && metadata.uid() != unsafe { libc::geteuid() })
        {
            bail!("Media executable must be executable and owned by root or this user, without group/world write access");
        }
        let mut header = [0u8; 20];
        file.read_exact(&mut header)?;
        let machine = u16::from_le_bytes([header[18], header[19]]);
        let host_matches = (cfg!(target_arch = "x86_64") && machine == 62)
            || (cfg!(target_arch = "aarch64") && machine == 183);
        if &header[..4] != b"\x7fELF"
            || header[4] != 2
            || header[5] != 1
            || header[6] != 1
            || !matches!(header[7], 0 | 3)
            || !matches!(u16::from_le_bytes([header[16], header[17]]), 2 | 3)
            || !host_matches
        {
            bail!("Media executable must be a native Linux ELF binary for this host; Windows executables and scripts are forbidden");
        }
    }
    #[cfg(not(target_os = "linux"))]
    bail!("Configured FFmpeg adapter requires native Linux");
    Ok(path)
}
fn invoke(
    name: &str,
    args: &[String],
    timeout: Duration,
) -> Result<crate::util::child_process::Output> {
    let mut command = Command::new(installed(name)?);
    command.args(args);
    crate::util::child_process::run(
        &mut command,
        &[],
        &AtomicBool::new(false),
        timeout,
        64 * 1024,
        32 * 1024,
    )
}
fn text(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_owned()).collect()
}
fn number(value: &Value, name: &str) -> Result<f64> {
    let value = value.get(name).context("Missing media timing")?;
    let n = value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
        .context("Invalid media timing")?;
    if !n.is_finite() {
        bail!("Nonfinite media timing");
    }
    Ok(n)
}
fn probe(source: &Path) -> Result<(Probe, f64)> {
    let demuxer = super::validate_container(source)?;
    let mut args = text(&[
        "-v",
        "error",
        "-protocol_whitelist",
        "file",
        "-f",
        demuxer,
        "-show_streams",
        "-show_format",
        "-of",
        "json",
    ]);
    args.push(source.to_str().context("Media path must be UTF-8")?.into());
    let output = invoke("FILEID_FFPROBE", &args, Duration::from_secs(20))?;
    let value: Value = serde_json::from_slice(&output.stdout)?;
    let streams = value["streams"].as_array().context("Missing streams")?;
    let mut video = None;
    let mut audio = None;
    for stream in streams {
        match stream["codec_type"].as_str() {
            Some("video") if video.is_none() => video = Some(stream),
            Some("audio") if audio.is_none() => audio = Some(stream),
            _ => bail!("Only one video and at most one audio track are supported"),
        }
    }
    let video = video.context("No video stream")?;
    let rotation = video["side_data_list"]
        .as_array()
        .and_then(|list| list.iter().find_map(|v| v["rotation"].as_i64()))
        .unwrap_or(0)
        .rem_euclid(360) as u32;
    if ![0, 90, 180, 270].contains(&rotation) {
        bail!("Unsupported track rotation");
    }
    if !matches!(
        video["sample_aspect_ratio"].as_str(),
        None | Some("1:1" | "N/A")
    ) {
        bail!("Non-square pixels are unsupported");
    }
    let rate = video["avg_frame_rate"]
        .as_str()
        .context("Missing frame rate")?;
    let (n, d) = rate.split_once('/').context("Invalid frame rate")?;
    let hdr = super::source_color(source)?.hdr
        || matches!(
            video["color_transfer"].as_str(),
            Some("smpte2084" | "arib-std-b67")
        );
    let mut probe = Probe {
        duration: number(&value["format"], "duration")?,
        width: video["width"]
            .as_u64()
            .context("Missing width")?
            .try_into()?,
        height: video["height"]
            .as_u64()
            .context("Missing height")?
            .try_into()?,
        video_codec: video["codec_name"]
            .as_str()
            .context("Missing codec")?
            .into(),
        video_start: number(video, "start_time")?,
        audio_count: u32::from(audio.is_some()),
        audio_codec: audio.and_then(|a| a["codec_name"].as_str().map(str::to_owned)),
        audio_start: audio.map(|a| number(a, "start_time")).transpose()?,
        audio_duration: audio.map(|a| number(a, "duration")).transpose()?,
        hdr,
        rotation,
        frame_rate: n.parse::<f64>()? / d.parse::<f64>()?,
    };
    if rotation == 90 || rotation == 270 {
        std::mem::swap(&mut probe.width, &mut probe.height);
    }
    validate_probe(&probe)?;
    let video_duration = number(video, "duration")?;
    let video_end = probe.video_start + video_duration;
    if video_duration <= 0.0 || !video_end.is_finite() {
        bail!("Invalid video stream extent");
    }
    Ok((probe, video_end))
}
pub(super) fn execute(
    source: &Path,
    operation: &Operation,
    output: Option<&Path>,
    report: &mut Report,
) -> Result<()> {
    let demuxer = super::validate_container(source)?;
    let source_text = source.to_str().context("Media path must be UTF-8")?;
    let mut args = text(&[
        "-nostdin",
        "-hide_banner",
        "-v",
        "error",
        "-n",
        "-protocol_whitelist",
        "file",
        "-threads",
        "1",
        "-filter_threads",
        "1",
        "-f",
        demuxer,
    ]);
    if demuxer == "mov" {
        args.extend(text(&["-enable_drefs", "0", "-use_absolute_path", "0"]));
    }
    // Sampling uses original source PTS; export retains its separately validated normalization.
    if matches!(operation, Operation::Frame { .. } | Operation::Audio { .. }) {
        args.push("-copyts".into());
    }
    args.extend(text(&["-i", source_text]));
    match operation {
        Operation::Probe => {
            report.probe = Some(probe(source)?.0);
            return Ok(());
        }
        Operation::Audio {
            start_seconds,
            duration_seconds,
        } => {
            let end_seconds = start_seconds + duration_seconds;
            let filter = format!("aresample=16000,aformat=sample_fmts=s16:channel_layouts=mono,atrim=start={start_seconds}:end={end_seconds}");
            let mut first = args.clone();
            first[3] = "info".into();
            first.extend(text(&["-map", "0:a:0", "-vn", "-sn", "-dn", "-af"]));
            first.push(format!("{filter},ashowinfo"));
            first.extend(text(&["-frames:a", "1", "-f", "null", "-"]));
            let first = invoke("FILEID_FFMPEG", &first, Duration::from_secs(120))?;
            let diagnostics = String::from_utf8_lossy(&first.stderr);
            let actual = diagnostics
                .lines()
                .find_map(|line| {
                    line.split_once("pts_time:")
                        .and_then(|(_, tail)| tail.split_whitespace().next())
                        .and_then(|s| s.parse::<f64>().ok())
                })
                .context("No decoded PCM in requested window")?;
            if !actual.is_finite() || actual < *start_seconds - 0.000001 || actual >= end_seconds {
                bail!("Invalid decoded PCM PTS");
            }
            args.extend(text(&["-map", "0:a:0", "-vn", "-sn", "-dn", "-af"]));
            args.push(filter);
            args.extend(text(&[
                "-ac",
                "1",
                "-ar",
                "16000",
                "-c:a",
                "pcm_s16le",
                "-f",
                "s16le",
            ]));
            args.push("-fs".into());
            args.push(((duration_seconds * 16000.0).ceil() as u64 * 2).to_string());
            args.push(
                output
                    .context("Missing PCM output")?
                    .to_str()
                    .context("Invalid PCM path")?
                    .into(),
            );
            invoke("FILEID_FFMPEG", &args, Duration::from_secs(120))?;
            let bytes = std::fs::metadata(output.unwrap())?.len();
            if bytes == 0
                || bytes % 2 != 0
                || bytes > (duration_seconds * 16000.0).ceil() as u64 * 2
                || actual + (bytes / 2) as f64 / 16000.0 > end_seconds + 1.0 / 16000.0
            {
                bail!("Invalid bounded PCM output");
            }
            report.actual_seconds = Some(actual);
            report.samples = Some(bytes / 2);
            report.sample_rate = Some(16000);
        }
        Operation::Frame {
            seconds,
            max_dimension,
        } => {
            let (before, video_end) = probe(source)?;
            if *seconds >= video_end {
                bail!("Requested frame is outside media duration");
            }
            // Select timestamps from the decoded stream, preserving PTS in showinfo.
            args[3] = "info".into();
            args.extend(text(&["-map", "0:v:0", "-an", "-sn", "-dn", "-vf"]));
            args.push(format!("select='gte(t,{seconds})',scale={max_dimension}:{max_dimension}:force_original_aspect_ratio=decrease,showinfo"));
            args.extend(text(&[
                "-frames:v",
                "1",
                "-fps_mode",
                "passthrough",
                "-c:v",
                "png",
                "-threads",
                "1",
                "-f",
                "image2",
            ]));
            args.push(
                output
                    .context("Missing PNG output")?
                    .to_str()
                    .context("Invalid PNG path")?
                    .into(),
            );
            let result = invoke("FILEID_FFMPEG", &args, Duration::from_secs(120))?;
            let diagnostic = String::from_utf8_lossy(&result.stderr);
            let actual = diagnostic
                .lines()
                .find_map(|line| {
                    line.split_once("pts_time:")
                        .and_then(|(_, tail)| tail.split_whitespace().next())
                        .and_then(|value| value.parse::<f64>().ok())
                })
                .context("Decoder did not report actual frame PTS")?;
            if !actual.is_finite()
                || actual < *seconds - 0.000001
                || actual < before.video_start - 0.000001
                || actual >= video_end
            {
                bail!("Invalid decoded frame PTS");
            }
            let (width, height) = image::image_dimensions(output.unwrap())?;
            if width.max(height) > *max_dimension {
                bail!("Frame dimensions exceeded cap");
            }
            report.actual_seconds = Some(actual);
            report.width = Some(width);
            report.height = Some(height);
            report.probe = Some(before);
        }
        Operation::Video { max_dimension } => {
            let before = probe(source)?.0;
            if before.hdr {
                bail!("HDR export is unsupported; use an SDR source");
            }
            args.extend(text(&[
                "-map",
                "0:v:0",
                "-map",
                "0:a:0?",
                "-sn",
                "-dn",
                "-map_metadata",
                "-1",
                "-vf",
            ]));
            args.push(format!("scale=w='min(iw,{max_dimension})':h='min(ih,{max_dimension})':force_original_aspect_ratio=decrease:force_divisible_by=2"));
            args.extend(text(&[
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-threads",
                "1",
                "-movflags",
                "+faststart",
                "-fs",
            ]));
            args.push(super::MAX_OUTPUT.to_string());
            args.extend(text(&["-f", "mp4"]));
            args.push(
                output
                    .context("Missing MP4 output")?
                    .to_str()
                    .context("Invalid MP4 path")?
                    .into(),
            );
            invoke("FILEID_FFMPEG", &args, Duration::from_secs(3600))?;
            let after = probe(output.unwrap())?.0;
            validate_export(&before, &after, *max_dimension)?;
            report.probe = Some(after);
        }
        Operation::Photo { .. } => bail!("Invalid installed media operation"),
    }
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn linux_media_config_rejects_wrong_host_scripts_and_writable_programs() {
        assert!(validate_installed(Path::new("ffmpeg")).is_err());
        let path =
            std::env::temp_dir().join(format!("FileIDMedia-executable-{}", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"MZWindows executable fixture").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(validate_installed(&path).is_err());
        std::fs::write(
            &path,
            b"#!/bin/sh
printf untrusted-fixture",
        )
        .unwrap();
        assert!(validate_installed(&path).is_err());
        let mut header = [0u8; 20];
        header[..4].copy_from_slice(b"ELF");
        header[4] = 2;
        header[5] = 1;
        header[6] = 1;
        header[16] = 2;
        let host = if cfg!(target_arch = "x86_64") {
            62u16
        } else {
            183u16
        };
        header[18..20].copy_from_slice(&host.to_le_bytes());
        std::fs::write(&path, header).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o722)).unwrap();
        assert!(validate_installed(&path).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        header[18..20].copy_from_slice(&0u16.to_le_bytes());
        std::fs::write(&path, header).unwrap();
        assert!(validate_installed(&path).is_err());
        std::fs::remove_file(path).unwrap();
        assert!(validate_installed(&std::env::current_exe().unwrap()).is_ok());
    }
}
