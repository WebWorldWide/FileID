//! BERT WordPiece tokenizer in pure Rust (avoids the `tokenizers` crate's onig and large build); matches HF
//! BertTokenizer(do_lower_case=True) for common English.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};

const CLS: &str = "[CLS]";
const SEP: &str = "[SEP]";
const UNK: &str = "[UNK]";
/// HF emits `[UNK]` for a whitespace token over 100 chars.
const MAX_INPUT_CHARS_PER_WORD: usize = 100;

pub struct WordPieceTokenizer {
    vocab: HashMap<String, i64>,
    cls_id: i64,
    sep_id: i64,
    unk_id: i64,
    lower_case: bool,
}

pub struct Encoding {
    pub ids: Vec<i64>,
    pub attention_mask: Vec<i64>,
    pub type_ids: Vec<i64>,
}

impl WordPieceTokenizer {
    pub fn from_vocab_file<P: AsRef<Path>>(path: P, lower_case: bool) -> Result<Self> {
        let text = std::fs::read_to_string(path.as_ref())
            .with_context(|| format!("reading vocab.txt at {}", path.as_ref().display()))?;
        let mut vocab = HashMap::new();
        for (i, line) in text.lines().enumerate() {
            // A Windows checkout may leave a trailing \r.
            let tok = line.trim_end_matches(['\r', '\n']);
            vocab.insert(tok.to_string(), i as i64);
        }
        Self::from_vocab(vocab, lower_case)
    }

    fn from_vocab(vocab: HashMap<String, i64>, lower_case: bool) -> Result<Self> {
        let cls_id = *vocab.get(CLS).context("vocab.txt missing [CLS]")?;
        let sep_id = *vocab.get(SEP).context("vocab.txt missing [SEP]")?;
        let unk_id = *vocab.get(UNK).context("vocab.txt missing [UNK]")?;
        Ok(Self { vocab, cls_id, sep_id, unk_id, lower_case })
    }

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

    /// Bounded to `max_words` so a huge extracted document is never materialized as tiny Strings; the char pre-slice
    /// bounds the lowercasing allocation.
    fn basic_tokenize(&self, text: &str, max_words: usize) -> Vec<String> {
        let text = match text.char_indices().nth(max_words.saturating_mul(64)) {
            Some((i, _)) => &text[..i],
            None => text,
        };
        let mut out = Vec::new();
        let mut cur = String::new();
        // Full Unicode lowering, not ASCII-only: otherwise accented/non-Latin text never matches the lowercased vocab
        // and collapses to [UNK].
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
            if is_dropped(c) || (self.lower_case && is_combining_mark(c)) {
                continue;
            }
            let c = if self.lower_case { fold_accent(c) } else { c };
            if c.is_whitespace() {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            } else if c.is_ascii_punctuation() || is_unicode_punctuation(c) || is_cjk(c) {
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

/// BERT's `_clean_text` drops NUL, U+FFFD and control/format characters (zero-width spaces and joiners, soft hyphen, BOM) so they never glue onto a word.
fn is_dropped(c: char) -> bool {
    if c.is_whitespace() {
        return false;
    }
    c == '\u{0}'
        || c == '\u{FFFD}'
        || c.is_control()
        || matches!(c, '\u{AD}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}')
}

fn is_combining_mark(c: char) -> bool {
    matches!(c, '\u{300}'..='\u{36F}')
}

/// The accent stripping `BertTokenizer` gets from NFD: Latin-1 and Latin Extended-A letters fold to their base letter; letters with no decomposition (æ, ø, ß, đ, ł) stay.
fn fold_accent(c: char) -> char {
    const LATIN_1: &str = "aaaaaaæceeeeiiiiðnooooo÷øuuuuyþy";
    const LATIN_EXTENDED_A: &str = "aaaaaaccccccccddđđeeeeeeeeeegggggggghhħħiiiiiiiiiıĳĳjjkkĸllllllŀŀłłnnnnnnŉŋŋooooooœœrrrrrrssssssssttttŧŧuuuuuuuuuuuuwwyyyzzzzzzſ";
    let code = c as u32;
    match code {
        0xE0..=0xFF => LATIN_1.chars().nth((code - 0xE0) as usize).unwrap_or(c),
        0x100..=0x17F => LATIN_EXTENDED_A.chars().nth((code - 0x100) as usize).unwrap_or(c),
        _ => c,
    }
}

/// Unicode punctuation outside ASCII (curly quotes, dashes, ellipsis, CJK and fullwidth marks): a word boundary and a token of its own, as in BERT.
fn is_unicode_punctuation(c: char) -> bool {
    matches!(
        c,
        '\u{A1}' | '\u{A7}' | '\u{AB}' | '\u{B6}' | '\u{B7}' | '\u{BB}' | '\u{BF}'
            | '\u{37E}' | '\u{387}' | '\u{55A}'..='\u{55F}' | '\u{589}' | '\u{58A}' | '\u{5BE}' | '\u{5C0}'
            | '\u{60C}' | '\u{60D}' | '\u{61B}' | '\u{61F}' | '\u{6D4}' | '\u{964}' | '\u{965}'
            | '\u{2010}'..='\u{2027}' | '\u{2030}'..='\u{2043}' | '\u{2045}'..='\u{2051}' | '\u{2053}'..='\u{205E}'
            | '\u{2E00}'..='\u{2E4F}' | '\u{3001}'..='\u{3003}' | '\u{3008}'..='\u{3011}' | '\u{3014}'..='\u{301F}'
            | '\u{30FB}' | '\u{FE10}'..='\u{FE19}' | '\u{FE30}'..='\u{FE52}' | '\u{FE54}'..='\u{FE61}' | '\u{FE63}' | '\u{FE68}' | '\u{FE6A}' | '\u{FE6B}'
            | '\u{FF01}'..='\u{FF03}' | '\u{FF05}'..='\u{FF0A}' | '\u{FF0C}'..='\u{FF0F}' | '\u{FF1A}' | '\u{FF1B}'
            | '\u{FF1F}' | '\u{FF20}' | '\u{FF3B}'..='\u{FF3D}' | '\u{FF3F}' | '\u{FF5B}' | '\u{FF5D}' | '\u{FF5F}'..='\u{FF65}'
    )
}

/// Ideographs are spaced out so each is its own word, as BertTokenizer does.
fn is_cjk(c: char) -> bool {
    matches!(
        c,
        '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}' | '\u{20000}'..='\u{2A6DF}' | '\u{2A700}'..='\u{2B73F}'
            | '\u{2B740}'..='\u{2B81F}' | '\u{2B820}'..='\u{2CEAF}' | '\u{F900}'..='\u{FAFF}' | '\u{2F800}'..='\u{2FA1F}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toy() -> WordPieceTokenizer {
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
        assert_eq!(e.ids.first(), Some(&2));
        assert_eq!(e.ids.last(), Some(&3));
        assert_eq!(e.attention_mask.len(), e.ids.len());
        assert!(e.attention_mask.iter().all(|&m| m == 1));
        assert!(e.type_ids.iter().all(|&t| t == 0));
    }

    #[test]
    fn greedy_splits_into_wordpieces() {
        assert_eq!(toy().encode("playing", 16).ids, vec![2, 4, 5, 3]);
    }

    #[test]
    fn unknown_word_becomes_unk() {
        assert_eq!(toy().encode("xyzzy", 16).ids, vec![2, 1, 3]);
    }

    #[test]
    fn lowercases_and_splits_punctuation() {
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
        assert_eq!(e.ids.len(), 35);
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

    #[test]
    fn unicode_punctuation_accents_cjk_and_invisible_characters_match_bert_basic_tokenizing() {
        let toks = [
            "[PAD]", "[UNK]", "[CLS]", "[SEP]", "don", "t", "cafe", "\u{4e2d}", "\u{6587}", "play", "\u{2019}", "\u{2014}", "'",
        ];
        let vocab = toks.iter().enumerate().map(|(i, t)| ((*t).to_string(), i as i64)).collect();
        let tok = WordPieceTokenizer::from_vocab(vocab, true).unwrap();
        assert_eq!(tok.encode("don\u{2019}t", 16).ids, vec![2, 4, 10, 5, 3], "a curly apostrophe splits the word");
        assert_eq!(tok.encode("play\u{2014}play", 16).ids, vec![2, 9, 11, 9, 3]);
        assert_eq!(tok.encode("Caf\u{e9}", 16).ids, vec![2, 6, 3], "accents fold to the base letter");
        assert_eq!(tok.encode("cafe\u{301}", 16).ids, vec![2, 6, 3], "combining marks are dropped");
        assert_eq!(tok.encode("\u{4e2d}\u{6587}", 16).ids, vec![2, 7, 8, 3], "each ideograph is its own word");
        assert_eq!(tok.encode("pl\u{200b}ay", 16).ids, vec![2, 9, 3], "zero-width characters vanish");
    }
}
