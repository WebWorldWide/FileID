//! Pure-Rust keyword extraction (RAKE-style). Splits text into phrases at
//! stopword boundaries, scores each phrase by sum of word degree / frequency
//! (the classic RAKE metric), returns the top-N as content tags. No ML
//! model, no new dependency — complements the (heavier) BGE/GLiNER text
//! pipeline added later.
#![allow(dead_code)] // wired into the Phase 4 document tagging integration.

use std::collections::{HashMap, HashSet};

const MAX_TAGS: usize = 8;
const MIN_PHRASE_BYTES: usize = 3;
const MAX_PHRASE_BYTES: usize = 60;

const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "from", "has", "have", "he",
    "her", "his", "i", "if", "in", "into", "is", "it", "its", "just", "of", "on", "or", "our",
    "she", "so", "than", "that", "the", "their", "them", "then", "there", "these", "they", "this",
    "those", "to", "was", "we", "were", "what", "when", "where", "which", "who", "why", "will",
    "with", "you", "your", "about", "after", "all", "also", "any", "because", "been", "before",
    "between", "both", "can", "could", "do", "does", "done", "each", "few", "had", "having", "how",
    "more", "most", "no", "nor", "not", "now", "off", "once", "only", "other", "out", "over",
    "own", "same", "should", "some", "such", "through", "too", "under", "until", "up", "very",
    "while", "would",
];

/// Extract up to MAX_TAGS keyword tags from `text`. Returns `(tag, score)`
/// pairs in descending salience order. Each tag is lowercase. Empty input
/// or text shorter than a phrase returns `[]`.
pub(crate) fn extract(text: &str) -> Vec<(String, f32)> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    let stops: HashSet<&str> = STOPWORDS.iter().copied().collect();
    let phrases = split_into_phrases(text, &stops);
    if phrases.is_empty() {
        return Vec::new();
    }

    // RAKE word scoring: degree = total co-occurrence count (Σ phrase length),
    // freq = how often the word appears, score(word) = degree / freq.
    let mut word_freq: HashMap<String, u32> = HashMap::new();
    let mut word_degree: HashMap<String, u32> = HashMap::new();
    for phrase in &phrases {
        let n = u32::try_from(phrase.len()).unwrap_or(u32::MAX);
        for w in phrase {
            *word_freq.entry(w.clone()).or_insert(0) += 1;
            *word_degree.entry(w.clone()).or_insert(0) += n;
        }
    }

    // Score each unique phrase by Σ score(word). Multiple occurrences of the
    // same phrase keep the higher score (a phrase that appears multiple times
    // is automatically boosted via its constituent word frequencies anyway).
    let mut scores: HashMap<String, f32> = HashMap::new();
    for phrase in &phrases {
        if phrase.is_empty() {
            continue;
        }
        let key = phrase.join(" ");
        let bytes = key.len();
        if !(MIN_PHRASE_BYTES..=MAX_PHRASE_BYTES).contains(&bytes) {
            continue;
        }
        #[allow(clippy::cast_precision_loss)]
        let score: f32 = phrase
            .iter()
            .map(|w| {
                let d = *word_degree.get(w).unwrap_or(&1) as f32;
                let f = *word_freq.get(w).unwrap_or(&1) as f32;
                d / f
            })
            .sum();
        scores
            .entry(key)
            .and_modify(|s| *s = s.max(score))
            .or_insert(score);
    }

    let mut out: Vec<(String, f32)> = scores.into_iter().collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1));
    out.truncate(MAX_TAGS);
    out
}

fn split_into_phrases(text: &str, stops: &HashSet<&str>) -> Vec<Vec<String>> {
    let mut phrases = Vec::new();
    let mut cur: Vec<String> = Vec::new();
    let flush = |cur: &mut Vec<String>, dst: &mut Vec<Vec<String>>| {
        if !cur.is_empty() {
            dst.push(std::mem::take(cur));
        }
    };
    for raw in text.split(|c: char| !c.is_alphanumeric()) {
        let w = raw.trim().to_ascii_lowercase();
        if w.is_empty() {
            flush(&mut cur, &mut phrases);
            continue;
        }
        if stops.contains(w.as_str()) {
            flush(&mut cur, &mut phrases);
            continue;
        }
        cur.push(w);
    }
    flush(&mut cur, &mut phrases);
    phrases
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_yields_no_tags() {
        assert!(extract("").is_empty());
        assert!(extract("   \n\t  ").is_empty());
    }

    #[test]
    fn stopwords_split_phrases() {
        let stops: HashSet<&str> = STOPWORDS.iter().copied().collect();
        let phrases = split_into_phrases("the cat and the dog", &stops);
        assert_eq!(phrases, vec![vec!["cat".to_string()], vec!["dog".to_string()]]);
    }

    #[test]
    fn extracts_a_salient_phrase() {
        let text = "Golden retriever puppies playing on the beach at sunset. Golden retriever \
                    photos make great desktop wallpapers. The beach was sunny.";
        let tags = extract(text);
        let labels: Vec<&str> = tags.iter().map(|t| t.0.as_str()).collect();
        assert!(
            labels.iter().any(|l| l.contains("golden retriever") || l.contains("beach")),
            "expected a salient phrase, got: {labels:?}"
        );
    }

    #[test]
    fn tag_count_is_capped() {
        // Generate a long unique-vocab paragraph; expect MAX_TAGS at most.
        let words: Vec<String> = (0..200).map(|i| format!("word{i:03}")).collect();
        let text = words.join(" ");
        let tags = extract(&text);
        assert!(tags.len() <= MAX_TAGS);
    }

    #[test]
    fn scores_are_descending() {
        let tags = extract("alpha beta. gamma alpha. delta alpha gamma.");
        for w in tags.windows(2) {
            assert!(w[0].1 >= w[1].1, "tags must be descending by score: {tags:?}");
        }
    }
}
