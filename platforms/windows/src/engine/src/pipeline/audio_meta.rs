//! Phase 5 — audio metadata extraction (artist / album / title / genre / year)
//! via `symphonia`. Pure-Rust, no Java/ffmpeg system dependency, supports
//! mp3 (ID3v1/v2), flac (Vorbis comments), ogg/vorbis, wav, m4a/aac (iso-mp4),
//! and plain PCM containers.
//!
//! Returns `(label, score)` pairs ready to push onto `TaggedFile.tags`. Each
//! tag's score is `None` — these are metadata-derived facts, not model
//! probabilities. The full YAMNet sound-event tagger + Whisper transcription
//! are documented Phase-5b follow-ups; both need a publicly downloadable ONNX
//! release per the no-self-host policy.
#![allow(dead_code)] // wired into run_decoder_thread for FileKind::Audio.

use std::path::Path;

use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::{MetadataOptions, StandardTagKey, Value};
use symphonia::core::probe::Hint;

/// Cap tags per audio file. Most files yield 2-4 useful tags; the cap stops
/// a pathological ID3 dump from drowning the Library chip row.
const MAX_TAGS: usize = 6;

/// Extract artist / album / title / genre / year tags from `path`. Returns
/// an empty Vec on any decode/probe failure — audio metadata is best-effort.
///
/// `bytes` is an optional pre-read buffer (decoder thread reads the file once
/// for hashing + extraction on files ≤ FULL_HASH_MAX_BYTES). When supplied,
/// symphonia probes the in-memory bytes via a tiny MediaSource adapter and
/// skips a second file open. When `None`, the existing path-based open runs.
pub(crate) fn extract(path: &Path, bytes: Option<&[u8]>) -> Vec<(String, Option<f32>)> {
    let mss = if let Some(b) = bytes {
        // symphonia owns the source for the stream's lifetime; copy once into
        // the adapter. Cost is bounded by FULL_HASH_MAX_BYTES (16 MB) per file
        // and amortizes against the file open + read it replaces.
        MediaSourceStream::new(
            Box::new(BytesMediaSource::new(b.to_vec())),
            MediaSourceStreamOptions::default(),
        )
    } else {
        let p = crate::util::path_safety::to_extended_length(path);
        let file = match std::fs::File::open(&p) {
            Ok(f) => f,
            Err(_) => return Vec::new(),
        };
        MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default())
    };
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut probed = match symphonia::default::get_probe().format(
        &hint,
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    ) {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    let mut format = probed.format;
    let mut out: Vec<(String, Option<f32>)> = Vec::new();

    // Duration chip — present even when there's no ID3/Vorbis metadata
    // (voice memos, raw wav). Computed from the default track's n_frames /
    // sample_rate. Emitted FIRST so it ends up before lower-priority chips
    // when MAX_TAGS truncates a metadata-rich file.
    if let Some(label) = duration_label(format.as_ref()) {
        out.push((label, None));
    }

    // Some formats (FLAC, Vorbis, M4A) carry metadata on the FormatReader;
    // others (MP3 with ID3v2) carry it on the probe's MetadataLog. Read
    // both, dedup at the end.
    if let Some(rev) = probed.metadata.get().as_ref().and_then(|m| m.current()) {
        push_metadata(&mut out, rev);
    }
    if let Some(rev) = format.metadata().current() {
        push_metadata(&mut out, rev);
    }

    dedup_preserve_order(&mut out);
    out.truncate(MAX_TAGS);
    out
}

/// Format the audio's total duration as a Library chip ("12 min" / "1 h 5 min").
/// Returns None when symphonia can't expose enough info (some streamed formats).
fn duration_label(format: &dyn symphonia::core::formats::FormatReader) -> Option<String> {
    let track = format.default_track().or_else(|| format.tracks().first())?;
    let cp = &track.codec_params;
    let n_frames = cp.n_frames?;
    let sample_rate = cp.sample_rate?;
    if sample_rate == 0 {
        return None;
    }
    let total_secs = (n_frames as f64) / (sample_rate as f64);
    if total_secs < 1.0 {
        return None;
    }
    let total_secs = total_secs.round() as u64;
    let hours = total_secs / 3600;
    let mins = (total_secs % 3600) / 60;
    let secs = total_secs % 60;
    Some(if hours > 0 {
        format!("{hours} h {mins:02} min")
    } else if mins > 0 {
        format!("{mins} min")
    } else {
        format!("{secs} sec")
    })
}

fn push_metadata(out: &mut Vec<(String, Option<f32>)>, rev: &symphonia::core::meta::MetadataRevision) {
    for tag in rev.tags() {
        let value = match &tag.value {
            Value::String(s) => s.trim().to_string(),
            Value::UnsignedInt(n) => n.to_string(),
            Value::SignedInt(n) => n.to_string(),
            _ => continue,
        };
        if value.is_empty() {
            continue;
        }
        let label = match tag.std_key {
            Some(StandardTagKey::Artist | StandardTagKey::AlbumArtist) => value,
            Some(StandardTagKey::Album) => value,
            Some(StandardTagKey::TrackTitle) => value,
            Some(StandardTagKey::Genre) => value,
            Some(StandardTagKey::Date | StandardTagKey::OriginalDate) => {
                // Keep only the year (first 4 digits) so different date formats
                // collapse to a single tag (mm/dd/yyyy, yyyy-mm-dd, yyyy).
                value.chars().take_while(|c| c.is_ascii_digit()).take(4).collect()
            }
            _ => continue,
        };
        if !label.is_empty() {
            out.push((label, None));
        }
    }
}

fn dedup_preserve_order(v: &mut Vec<(String, Option<f32>)>) {
    let mut seen = std::collections::HashSet::new();
    v.retain(|(label, _)| seen.insert(label.to_lowercase()));
}

/// MediaSource over an owned `Vec<u8>` so symphonia can probe a pre-read
/// buffer without a file handle. `Cursor<Vec<u8>>` already gives us
/// `Read + Seek + Send + Sync`; symphonia just needs us to declare seekability
/// and the byte length.
struct BytesMediaSource(std::io::Cursor<Vec<u8>>);

impl BytesMediaSource {
    fn new(bytes: Vec<u8>) -> Self {
        Self(std::io::Cursor::new(bytes))
    }
}

impl std::io::Read for BytesMediaSource {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        std::io::Read::read(&mut self.0, buf)
    }
}

impl std::io::Seek for BytesMediaSource {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        std::io::Seek::seek(&mut self.0, pos)
    }
}

impl symphonia::core::io::MediaSource for BytesMediaSource {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        Some(self.0.get_ref().len() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_returns_empty_for_nonexistent_path() {
        let tags = extract(Path::new("Z:/fileid-nonexistent-audio.mp3"), None);
        assert!(tags.is_empty());
    }

    #[test]
    fn extract_returns_empty_for_non_audio_file() {
        let tmp = std::env::temp_dir().join(format!("fileid-not-audio-{}.txt", std::process::id()));
        std::fs::write(&tmp, b"this is not an audio file").unwrap();
        let tags = extract(&tmp, None);
        assert!(tags.is_empty());
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn extract_bytes_path_equivalent_for_non_audio() {
        let tmp = std::env::temp_dir().join(format!("fileid-not-audio-bytes-{}.txt", std::process::id()));
        let body = b"also not an audio file";
        std::fs::write(&tmp, body).unwrap();
        let via_path = extract(&tmp, None);
        let via_bytes = extract(&tmp, Some(body));
        assert_eq!(via_path, via_bytes);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn bytes_media_source_reports_seekable_with_length() {
        use symphonia::core::io::MediaSource;
        let src = BytesMediaSource::new(vec![1, 2, 3, 4, 5]);
        assert!(src.is_seekable());
        assert_eq!(src.byte_len(), Some(5));
    }

    #[test]
    fn dedup_keeps_first_occurrence_case_insensitive() {
        let mut v = vec![
            ("Beatles".to_string(), None),
            ("beatles".to_string(), None),
            ("Pink Floyd".to_string(), None),
        ];
        dedup_preserve_order(&mut v);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].0, "Beatles");
        assert_eq!(v[1].0, "Pink Floyd");
    }
}
