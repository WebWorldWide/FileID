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
pub(crate) fn extract(path: &Path) -> Vec<(String, Option<f32>)> {
    let p = crate::util::path_safety::to_extended_length(path);
    let file = match std::fs::File::open(&p) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let mss = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_returns_empty_for_nonexistent_path() {
        let tags = extract(Path::new("Z:/fileid-nonexistent-audio.mp3"));
        assert!(tags.is_empty());
    }

    #[test]
    fn extract_returns_empty_for_non_audio_file() {
        let tmp = std::env::temp_dir().join(format!("fileid-not-audio-{}.txt", std::process::id()));
        std::fs::write(&tmp, b"this is not an audio file").unwrap();
        let tags = extract(&tmp);
        assert!(tags.is_empty());
        let _ = std::fs::remove_file(&tmp);
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
