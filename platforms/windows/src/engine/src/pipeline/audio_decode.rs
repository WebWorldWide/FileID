//! Audio → 16 kHz mono PCM, the shared input both Whisper (transcription) and YAMNet
//! (sound classification) require. Decodes any `symphonia`-supported container (mp3,
//! flac, ogg/vorbis, wav, m4a/aac — the formats already enabled for `audio_meta`),
//! mixes to mono, linearly resamples to 16 kHz, and writes a 16-bit PCM WAV the
//! whisper.cpp CLI reads directly. No new dependency (reuses `symphonia`; the resampler
//! is hand-rolled — linear is plenty for speech, and a high-quality resampler crate
//! would be a new dep). The pure `resample_linear` + `write_wav16_mono` are unit-tested;
//! the decode path is build-verified (real-file decode is exercised on-hardware).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use symphonia::core::audio::{AudioBufferRef, Signal};
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Whisper + YAMNet both consume 16 kHz mono.
pub(crate) const TARGET_HZ: u32 = 16_000;

/// Cap the decoded duration so a multi-hour file can't materialize an unbounded sample
/// buffer (or a multi-minute transcription). The leading window carries the descriptive
/// content for a filename — the first couple of minutes is plenty — and bounding it here
/// keeps the whisper subprocess comfortably under its `TRANSCRIBE_TIMEOUT`.
const MAX_SECONDS: usize = 120;

/// Decode `path` to a 16 kHz mono 16-bit PCM WAV at `out_wav`. Best-effort transcoder
/// for the audio-AI pipeline; returns Err on an undecodable/empty stream.
pub(crate) fn decode_to_wav16_mono(
    path: &Path,
    out_wav: &Path,
    cancel: &AtomicBool,
) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        anyhow::bail!("cancelled");
    }
    let (samples, src_hz) = decode_mono_f32(path, cancel)?;
    if samples.is_empty() {
        anyhow::bail!("audio decode produced no samples");
    }
    let resampled = resample_linear(&samples, src_hz, TARGET_HZ);
    if cancel.load(Ordering::Relaxed) {
        anyhow::bail!("cancelled");
    }
    write_wav16_mono(&resampled, TARGET_HZ, out_wav)
}

/// Decode + downmix to mono f32 at the SOURCE sample rate. Returns (samples, src_hz).
fn decode_mono_f32(path: &Path, cancel: &AtomicBool) -> Result<(Vec<f32>, u32)> {
    let p = crate::util::path_safety::to_extended_length(path);
    let file = std::fs::File::open(&p).with_context(|| format!("open {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .context("probe audio format")?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.sample_rate.is_some())
        .or_else(|| format.tracks().first())
        .context("no audio track")?;
    let track_id = track.id;
    let src_hz = track.codec_params.sample_rate.unwrap_or(TARGET_HZ);
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .context("make decoder")?;

    let max_samples = MAX_SECONDS * src_hz as usize;
    let mut out: Vec<f32> = Vec::new();
    while let Ok(packet) = format.next_packet() {
        if cancel.load(Ordering::Relaxed) {
            anyhow::bail!("cancelled");
        }
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(symphonia::core::errors::Error::DecodeError(_)) => continue, // skip a bad packet
            Err(_) => break,
        };
        append_mono(&decoded, &mut out);
        if out.len() >= max_samples {
            out.truncate(max_samples);
            break;
        }
    }
    Ok((out, src_hz))
}

/// Downmix any channel layout to mono (average across channels) and append f32 samples.
fn append_mono(buf: &AudioBufferRef, out: &mut Vec<f32>) {
    macro_rules! mix {
        ($b:expr, $conv:expr) => {{
            let b = $b;
            let chans = b.spec().channels.count().max(1);
            let frames = b.frames();
            out.reserve(frames);
            for f in 0..frames {
                let mut acc = 0.0f32;
                for c in 0..chans {
                    acc += $conv(b.chan(c)[f]);
                }
                out.push(acc / chans as f32);
            }
        }};
    }
    match buf {
        AudioBufferRef::F32(b) => mix!(b, |s: f32| s),
        AudioBufferRef::F64(b) => mix!(b, |s: f64| s as f32),
        AudioBufferRef::S32(b) => mix!(b, |s: i32| s as f32 / i32::MAX as f32),
        AudioBufferRef::S16(b) => mix!(b, |s: i16| s as f32 / i16::MAX as f32),
        AudioBufferRef::U8(b) => mix!(b, |s: u8| (s as f32 - 128.0) / 128.0),
        _ => {} // other sample formats: skip this buffer (best-effort)
    }
}

/// Linear-interpolation resample of mono `samples` from `from_hz` to `to_hz`. Pure +
/// allocation-bounded (output ≈ len * to/from). Linear is adequate for speech → whisper.
pub(crate) fn resample_linear(samples: &[f32], from_hz: u32, to_hz: u32) -> Vec<f32> {
    if from_hz == to_hz || samples.len() < 2 || from_hz == 0 || to_hz == 0 {
        return samples.to_vec();
    }
    let ratio = to_hz as f64 / from_hz as f64;
    let out_len = ((samples.len() as f64) * ratio).floor() as usize;
    let mut out = Vec::with_capacity(out_len);
    let step = from_hz as f64 / to_hz as f64; // source samples per output sample
    let mut pos = 0.0f64;
    for _ in 0..out_len {
        let i = pos.floor() as usize;
        let frac = (pos - i as f64) as f32;
        let a = samples[i];
        let b = if i + 1 < samples.len() { samples[i + 1] } else { a };
        out.push(a + (b - a) * frac);
        pos += step;
    }
    out
}

/// Write mono f32 `samples` (clamped to [-1, 1]) as a 16-bit PCM WAV at `sample_hz`.
pub(crate) fn write_wav16_mono(samples: &[f32], sample_hz: u32, out: &Path) -> Result<()> {
    use std::io::Write;
    let data_len = (samples.len() * 2) as u32; // 16-bit mono
    let byte_rate = sample_hz * 2;
    let mut buf: Vec<u8> = Vec::with_capacity(44 + data_len as usize);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(36 + data_len).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM
    buf.extend_from_slice(&1u16.to_le_bytes()); // mono
    buf.extend_from_slice(&sample_hz.to_le_bytes());
    buf.extend_from_slice(&byte_rate.to_le_bytes());
    buf.extend_from_slice(&2u16.to_le_bytes()); // block align
    buf.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_len.to_le_bytes());
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        buf.extend_from_slice(&v.to_le_bytes());
    }
    let p = crate::util::path_safety::to_extended_length(out);
    let mut f = std::fs::File::create(&p).with_context(|| format!("create {}", out.display()))?;
    f.write_all(&buf).context("write wav")?;
    f.sync_all().ok();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_identity_when_rates_match() {
        let s = vec![0.1, -0.2, 0.3, -0.4];
        assert_eq!(resample_linear(&s, 16_000, 16_000), s);
    }

    #[test]
    fn resample_downsamples_length_by_ratio() {
        // 48 kHz → 16 kHz is a 1/3 ratio: 300 samples → ~100.
        let s: Vec<f32> = (0..300).map(|i| i as f32 / 300.0).collect();
        let out = resample_linear(&s, 48_000, 16_000);
        assert_eq!(out.len(), 100);
        // Endpoints are preserved/interpolated, monotonic ramp stays monotonic.
        assert!(out[0] <= out[out.len() - 1]);
        assert!(out[0].abs() < 1e-3);
    }

    #[test]
    fn resample_upsamples_length_by_ratio() {
        let s: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let out = resample_linear(&s, 8_000, 16_000);
        assert_eq!(out.len(), 200);
    }

    #[test]
    fn wav_header_is_well_formed_16k_mono() {
        let dir = std::env::temp_dir().join(format!("fileid-wav-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("t.wav");
        write_wav16_mono(&[0.0, 0.5, -0.5, 1.0, -1.0], TARGET_HZ, &p).unwrap();
        let bytes = std::fs::read(&p).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(&bytes[22..24], &1u16.to_le_bytes()); // mono
        assert_eq!(&bytes[24..28], &16_000u32.to_le_bytes()); // 16 kHz
        assert_eq!(&bytes[34..36], &16u16.to_le_bytes()); // 16-bit
        assert_eq!(bytes.len(), 44 + 5 * 2); // header + 5 samples × 2 bytes
        let _ = std::fs::remove_dir_all(&dir);
    }
}
