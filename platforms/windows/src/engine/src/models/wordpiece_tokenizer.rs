//! Minimal BERT-family WordPiece tokenizer (pure Rust, no `tokenizers` dep).
//!
//! Shared by the upcoming BGE-small text-embedding and GLiNER NER wrappers,
//! which are BERT-architecture models needing `[CLS]`/`[SEP]`, a `vocab.txt`
//! map, an attention mask, and token-type ids — unlike the BPE
//! `clip_tokenizer`. Greedy longest-match-first WordPiece over a `vocab.txt`,
//! mirroring HuggingFace `BertTokenizer(do_lower_case=True)` for the common
//! English case. Avoids pulling the heavy `tokenizers` crate (onig + a big
//! build) for what these models actually need.
#![allow(dead_code)] // wired into the Phase 4 document/text models (GLiNER, BGE).

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};

const CLS: &str = "[CLS]";
const SEP: &str = "[SEP]";
const UNK: &str = "[UNK]";
const PAD: &str = "[PAD]";
/// HF caps a single whitespace token at 100 chars before emitting `[UNK]`.
const MAX_INPUT_CHARS_PER_WORD: usize = 100;

pub struct WordPieceTokenizer {
    vocab: HashMap<String, i64>,
    cls_id: i64,
    sep_id: i64,
    unk_id: i64,
    pad_id: i64,
    lower_case: bool,
}

/// The three parallel input tensors a BERT ONNX graph expects.
pub struct Encoding {
    pub ids: Vec<i64>,
    pub attention_mask: Vec<i64>,
    pub type_ids: Vec<i64>,
}

impl WordPieceTokenizer {
    /// Load from a HuggingFace-style `vocab.txt` (one token per line; the
    /// line number is the token id).
    pub fn from_vocab_file<P: AsRef<Path>>(path: P, lower_case: bool) -> Result<Self> {
        let text = std::fs::read_to_string(path.as_ref())
            .with_context(|| format!("reading vocab.txt at {}", path.as_ref().display()))?;
        let mut vocab = HashMap::new();
        for (i, line) in text.lines().enumerate() {
            // A Windows checkout may leave a trailing \r; tokens never carry
            // surrounding whitespace.
            let tok = line.trim_end_matches(['\r', '\n']);
            vocab.insert(tok.to_string(), i as i64);
        }
        Self::from_vocab(vocab, lower_case)
    }

    fn from_vocab(vocab: HashMap<String, i64>, lower_case: bool) -> Result<Self> {
        let cls_id = *vocab.get(CLS).context("vocab.txt missing [CLS]")?;
        let sep_id = *vocab.get(SEP).context("vocab.txt missing [SEP]")?;
        let unk_id = *vocab.get(UNK).context("vocab.txt missing [UNK]")?;
        let pad_id = vocab.get(PAD).copied().unwrap_or(0);
        Ok(Self { vocab, cls_id, sep_id, unk_id, pad_id, lower_case })
    }

    pub fn pad_id(&self) -> i64 {
        self.pad_id
    }

    /// Encode one string into `[CLS] … [SEP]`, truncated to `max_len`
    /// (inclusive of the two special tokens). Returns ids + attention mask +
    /// all-zero token-type ids.
    pub fn encode(&self, text: &str, max_len: usize) -> Encoding {
        let max_len = max_len.max(2);
        let mut ids = Vec::with_capacity(max_len);
        ids.push(self.cls_id);
        'outer: for word in self.basic_tokenize(text, max_len) {
            for piece in self.wordpiece(&word) {
                if ids.len() >= max_len - 1 {
                    break 'outer;
                }
                ids.push(piece);
            }
        }
        ids.push(self.sep_id);
        let attention_mask = vec![1i64; ids.len()];
        let type_ids = vec![0i64; ids.len()];
        Encoding { ids, attention_mask, type_ids }
    }

    /// BERT "basic tokenizer" stage: whitespace split, optional lowercasing,
    /// and punctuation broken into its own tokens. English-first; good enough
    /// for the models we ship. Bounded to `max_words` outputs (the punctuation
    /// branch may overshoot by one): every word yields at least one wordpiece,
    /// so the encoder can never consume more —
    /// without the bound a multi-hundred-MB extracted document would be
    /// materialized as a Vec of tiny Strings before truncation. The char
    /// pre-slice bounds the `to_lowercase` allocation the same way; 64 chars
    /// per word is generous (words past 100 chars collapse to `[UNK]`).
    fn basic_tokenize(&self, text: &str, max_words: usize) -> Vec<String> {
        let text = match text.char_indices().nth(max_words.saturating_mul(64)) {
            Some((i, _)) => &text[..i],
            None => text,
        };
        let mut out = Vec::new();
        let mut cur = String::new();
        // Full Unicode lowering, not ASCII-only: lowercasing only the ASCII
        // range leaves accented/non-Latin text in its original case so it never
        // matches the (lowercased) vocab and collapses to [UNK]. Matches HF
        // BertTokenizer(do_lower_case=True). (audit E8)
        let lowered;
        let src: &str = if self.lower_case {
            lowered = text.to_lowercase();
            &lowered
        } else {
            text
        };
        for c in src.chars() {
            if out.len() >= max_words {
                return out;
            }
            if c.is_whitespace() {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            } else if c.is_ascii_punctuation() {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                out.push(c.to_string());
            } else {
                cur.push(c);
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
        out
    }

    /// Greedy longest-match-first WordPiece over a single whitespace token.
    /// Returns the `[UNK]` id for a token over the length cap or with any
    /// unmatchable prefix, matching HF behavior.
    fn wordpiece(&self, word: &str) -> Vec<i64> {
        let chars: Vec<char> = word.chars().collect();
        if chars.len() > MAX_INPUT_CHARS_PER_WORD {
            return vec![self.unk_id];
        }
        let mut out = Vec::new();
        let mut start = 0;
        while start < chars.len() {
            let mut end = chars.len();
            let mut matched: Option<i64> = None;
            while start < end {
                let mut sub: String = chars[start..end].iter().collect();
                if start > 0 {
                    sub.insert_str(0, "##");
                }
                if let Some(&id) = self.vocab.get(&sub) {
                    matched = Some(id);
                    break;
                }
                end -= 1;
            }
            match matched {
                Some(id) => {
                    out.push(id);
                    start = end;
                }
                None => return vec![self.unk_id],
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toy() -> WordPieceTokenizer {
        // id == insertion order.
        let toks = [
            "[PAD]", "[UNK]", "[CLS]", "[SEP]", "play", "##ing", "##ed", "love", "!",
        ];
        let vocab = toks
            .iter()
            .enumerate()
            .map(|(i, t)| ((*t).to_string(), i as i64))
            .collect();
        WordPieceTokenizer::from_vocab(vocab, true).unwrap()
    }

    #[test]
    fn wraps_with_cls_and_sep() {
        let e = toy().encode("play", 16);
        assert_eq!(e.ids.first(), Some(&2)); // [CLS]
        assert_eq!(e.ids.last(), Some(&3)); // [SEP]
        assert_eq!(e.attention_mask.len(), e.ids.len());
        assert!(e.attention_mask.iter().all(|&m| m == 1));
        assert!(e.type_ids.iter().all(|&t| t == 0));
    }

    #[test]
    fn greedy_splits_into_wordpieces() {
        // playing → play ##ing → [CLS] play ##ing [SEP] == 2,4,5,3
        assert_eq!(toy().encode("playing", 16).ids, vec![2, 4, 5, 3]);
    }

    #[test]
    fn unknown_word_becomes_unk() {
        assert_eq!(toy().encode("xyzzy", 16).ids, vec![2, 1, 3]);
    }

    #[test]
    fn lowercases_and_splits_punctuation() {
        // "LOVE!" → love ! → [CLS] love ! [SEP] == 2,7,8,3
        assert_eq!(toy().encode("LOVE!", 16).ids, vec![2, 7, 8, 3]);
    }

    #[test]
    fn truncates_to_max_len_keeping_cls_sep() {
        let e = toy().encode("play play play play", 4);
        assert_eq!(e.ids.len(), 4);
        assert_eq!(e.ids.first(), Some(&2));
        assert_eq!(e.ids.last(), Some(&3));
    }

    #[test]
    fn missing_special_tokens_is_an_error() {
        let mut v = HashMap::new();
        v.insert("hello".to_string(), 0i64);
        assert!(WordPieceTokenizer::from_vocab(v, true).is_err());
    }

    #[test]
    fn ten_megabyte_document_completes_to_max_len() {
        let text = "play ".repeat(2_097_152);
        let started = std::time::Instant::now();
        let e = toy().encode(&text, 256);
        assert!(started.elapsed().as_secs() < 5);
        assert_eq!(e.ids.len(), 256);
        assert_eq!(e.ids.first(), Some(&2));
        assert_eq!(e.ids.last(), Some(&3));
    }

    #[test]
    fn word_at_max_input_chars_boundary_tokenizes() {
        let word = format!("play{}", "ing".repeat(32));
        assert_eq!(word.chars().count(), MAX_INPUT_CHARS_PER_WORD);
        let e = toy().encode(&word, 256);
        assert_eq!(e.ids.len(), 35); // [CLS] play ##ing*32 [SEP]
        assert!(!e.ids.contains(&1));
    }

    #[test]
    fn word_past_max_input_chars_becomes_unk() {
        let word = format!("play{}g", "ing".repeat(32));
        assert_eq!(word.chars().count(), MAX_INPUT_CHARS_PER_WORD + 1);
        assert_eq!(toy().encode(&word, 256).ids, vec![2, 1, 3]);
    }

    #[test]
    fn all_punctuation_input_splits_per_char() {
        assert_eq!(toy().encode("!!!", 16).ids, vec![2, 8, 8, 8, 3]);
        assert_eq!(toy().encode("???", 16).ids, vec![2, 1, 1, 1, 3]);
    }
}
