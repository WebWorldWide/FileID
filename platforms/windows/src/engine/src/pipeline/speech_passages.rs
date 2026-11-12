//! Speech chunks and passages for on-device recognisers that return timed
//! words (Apple Speech in the Mac app's engine): the rules of the Swift
//! engine's `TimelineSpeechTranscription`, so both engines write the same
//! coverage and passage rows and resume each other's jobs chunk by chunk.

use serde::Deserialize;

/// One recognised word or phrase; `timestamp` is relative to the chunk's
/// extract start.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Segment {
    pub text: String,
    pub timestamp: f64,
    pub duration: f64,
    pub confidence: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Passage {
    pub start_seconds: f64,
    pub end_seconds: f64,
    pub text: String,
    pub confidence: f64,
}

/// A chunk owns `[owned_start, owned_end)`; it is extracted with overlap so a
/// word on a boundary is heard whole, and each word is kept by the chunk that
/// owns its midpoint.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Chunk {
    pub extract_start: f64,
    pub extract_end: f64,
    pub owned_start: f64,
    pub owned_end: f64,
}

pub const MAXIMUM_CORE_SECONDS: f64 = 45.0;
pub const OVERLAP_SECONDS: f64 = 1.5;

pub fn chunks(duration: f64) -> Vec<Chunk> {
    chunks_with(duration, MAXIMUM_CORE_SECONDS, OVERLAP_SECONDS)
}

pub fn chunks_with(duration: f64, maximum_core: f64, overlap: f64) -> Vec<Chunk> {
    if !(duration.is_finite() && duration > 0.0 && maximum_core.is_finite() && maximum_core > 0.0 && overlap.is_finite() && overlap >= 0.0) {
        return Vec::new();
    }
    let mut result = Vec::new();
    let mut core_start = 0.0;
    while core_start < duration {
        let core_end = (core_start + maximum_core).min(duration);
        result.push(Chunk {
            extract_start: (core_start - overlap).max(0.0),
            extract_end: duration.min(core_end + overlap),
            owned_start: core_start,
            owned_end: core_end,
        });
        core_start = core_end;
    }
    result
}

/// Groups a chunk's words into passages: a new passage after a 1.25 s pause,
/// 8 s of span or 280 characters.
pub fn passages(segments: &[Segment], chunk: &Chunk, media_duration: f64) -> Vec<Passage> {
    if !(media_duration.is_finite() && media_duration > 0.0) {
        return Vec::new();
    }
    let mut sorted: Vec<Segment> = segments
        .iter()
        .filter_map(|segment| {
            if !(segment.timestamp.is_finite() && segment.duration.is_finite() && segment.confidence.is_finite()) {
                return None;
            }
            let text = segment.text.trim();
            if text.is_empty() {
                return None;
            }
            let start = chunk.extract_start + segment.timestamp.max(0.0);
            let end = media_duration.min(chunk.extract_start + segment.timestamp.max(segment.timestamp + segment.duration));
            let midpoint = start + (end - start) / 2.0;
            if midpoint < chunk.owned_start || midpoint >= chunk.owned_end || start >= media_duration || end < start {
                return None;
            }
            Some(Segment { text: text.to_owned(), timestamp: start, duration: (end - start).max(0.0), confidence: segment.confidence.clamp(0.0, 1.0) })
        })
        .collect();
    sorted.sort_by(|a, b| a.timestamp.total_cmp(&b.timestamp));

    let mut passages = Vec::new();
    let mut group: Vec<Segment> = Vec::new();
    let flush = |group: &mut Vec<Segment>, passages: &mut Vec<Passage>| {
        let (Some(first), Some(last)) = (group.first(), group.last()) else { return };
        let mut text = String::new();
        for segment in group.iter() {
            if text.is_empty() {
                text.push_str(&segment.text);
            } else if segment.text.chars().next().is_some_and(is_punctuation) {
                text.push_str(&segment.text);
            } else {
                text.push(' ');
                text.push_str(&segment.text);
            }
        }
        let end = media_duration.min(last.timestamp + last.duration.max(0.001));
        passages.push(Passage {
            start_seconds: first.timestamp,
            end_seconds: (first.timestamp + 0.001).max(end),
            text,
            confidence: group.iter().map(|s| s.confidence).sum::<f64>() / group.len() as f64,
        });
        group.clear();
    };
    for segment in sorted {
        if let Some(previous) = group.last() {
            let gap = segment.timestamp - (previous.timestamp + previous.duration);
            let span = segment.timestamp - group[0].timestamp;
            let length = group.iter().map(|s| s.text.chars().count()).sum::<usize>() + segment.text.chars().count();
            if gap > 1.25 || span > 8.0 || length > 280 {
                flush(&mut group, &mut passages);
            }
        }
        group.push(segment);
    }
    flush(&mut group, &mut passages);
    passages
}

/// Swift's `Character.isPunctuation` for the characters recognisers emit.
fn is_punctuation(c: char) -> bool {
    c.is_ascii_punctuation()
        || matches!(c, '\u{2010}'..='\u{2027}' | '\u{3001}' | '\u{3002}' | '\u{FF01}' | '\u{FF0C}' | '\u{FF1F}' | '\u{00A1}' | '\u{00BF}')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, timestamp: f64, duration: f64) -> Segment {
        Segment { text: text.into(), timestamp, duration, confidence: 0.9 }
    }

    #[test]
    fn chunks_own_consecutive_spans_and_overlap_their_extracts() {
        let chunks = chunks(100.0);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0], Chunk { extract_start: 0.0, extract_end: 46.5, owned_start: 0.0, owned_end: 45.0 });
        assert_eq!(chunks[1], Chunk { extract_start: 43.5, extract_end: 91.5, owned_start: 45.0, owned_end: 90.0 });
        assert_eq!(chunks[2], Chunk { extract_start: 88.5, extract_end: 100.0, owned_start: 90.0, owned_end: 100.0 });
        assert!(super::chunks(0.0).is_empty() && super::chunks(f64::NAN).is_empty());
    }

    #[test]
    fn words_belong_to_the_chunk_that_owns_their_midpoint() {
        let chunk = chunks(100.0)[1];
        let kept = passages(&[word("early", 0.5, 0.4), word("hello", 2.0, 0.5), word("late", 47.0, 0.5)], &chunk, 100.0);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].text, "hello");
        assert!((kept[0].start_seconds - 45.5).abs() < 1e-9);
    }

    #[test]
    fn passages_break_on_pauses_span_and_length_and_join_punctuation() {
        let chunk = chunks(60.0)[0];
        let words = [word("Hello", 0.0, 0.4), word(",", 0.4, 0.05), word("world", 0.5, 0.4), word("again", 3.0, 0.4)];
        let found = passages(&words, &chunk, 60.0);
        assert_eq!(found.iter().map(|p| p.text.as_str()).collect::<Vec<_>>(), ["Hello, world", "again"]);
        let long: Vec<Segment> = (0..20).map(|i| word("word", f64::from(i) * 0.5, 0.4)).collect();
        assert_eq!(passages(&long, &chunk, 60.0).len(), 2, "a passage spans at most 8 seconds");
        assert!(passages(&[word("  ", 1.0, 0.2)], &chunk, 60.0).is_empty());
    }
}
