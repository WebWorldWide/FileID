// CLIP BPE tokenizer (openai/clip pre-tokenization and merges, HF-compatible bytes-to-unicode); vocab.json and
// merges.txt are SHA-256 pinned downloads.

use std::collections::HashMap;

use anyhow::{Context, Result};
use serde_json::Value;

const SOT: u32 = 49406;
const EOT: u32 = 49407;
/// Keep in sync with `clip_text::CONTEXT_LEN`.
const CLIP_CONTEXT_LEN: usize = 77;
// DoS bounds: the BPE merge loop is O(n²) with String clones, so an unbroken multi-megabyte "word" would stall a query
// thread; 1 024 chars is >13x the 77-token context, so truncation never changes a real query.
const MAX_QUERY_CHARS: usize = 1_024;
const MAX_WORD_CHARS: usize = 256;
const MAX_MERGES: usize = 50_000;
const MAX_VOCAB: usize = 65_536;

pub struct ClipTokenizer {
    vocab: HashMap<String, u32>,
    merges: HashMap<(String, String), usize>,
    byte_encoder: [char; 256],
}

impl ClipTokenizer {
    pub fn new(vocab_json: &str, merges_txt: &str) -> Result<Self> {
        let value: Value = serde_json::from_str(vocab_json).context("parsing vocab.json")?;
        let obj = value.as_object().ok_or_else(|| anyhow::anyhow!("vocab.json not an object"))?;
        if obj.len() > MAX_VOCAB {
            anyhow::bail!("vocab.json has {} entries (max {MAX_VOCAB})", obj.len());
        }
        let mut vocab = HashMap::with_capacity(obj.len());
        for (k, v) in obj {
            let id = v.as_u64().ok_or_else(|| anyhow::anyhow!("vocab id not int"))? as u32;
            vocab.insert(k.clone(), id);
        }

        let mut merges = HashMap::new();
        for (rank, line) in merges_txt.lines().enumerate() {
            if rank == 0 && line.starts_with("#") {
                continue;
            }
            if merges.len() >= MAX_MERGES {
                anyhow::bail!("merges.txt exceeds {MAX_MERGES} merge rules");
            }
            let mut parts = line.split_whitespace();
            let l = parts.next();
            let r = parts.next();
            if let (Some(l), Some(r)) = (l, r) {
                merges.insert((l.to_string(), r.to_string()), rank);
            }
        }

        Ok(Self { vocab, merges, byte_encoder: build_byte_encoder() })
    }

    pub fn encode(&self, query: &str) -> Vec<u32> {
        let normalized: String = normalize(query).chars().take(MAX_QUERY_CHARS).collect();
        let mut out: Vec<u32> = Vec::with_capacity(16);
        out.push(SOT);

        for word in split_words(&normalized) {
            let word = match word.char_indices().nth(MAX_WORD_CHARS) {
                Some((i, _)) => &word[..i],
                None => word,
            };
            let mut s = String::new();
            for b in word.bytes() {
                s.push(self.byte_encoder[b as usize]);
            }
            let mut tokens: Vec<String> = s.chars().map(|c| c.to_string()).collect();
            if let Some(last) = tokens.last_mut() {
                last.push_str("</w>");
            }
            loop {
                let mut best: Option<(usize, usize)> = None;
                for i in 0..tokens.len().saturating_sub(1) {
                    let pair = (tokens[i].clone(), tokens[i + 1].clone());
                    if let Some(&rank) = self.merges.get(&pair)
                        && best.is_none_or(|(_, r)| rank < r) {
                            best = Some((i, rank));
                        }
                }
                let Some((i, _)) = best else { break };
                let merged = format!("{}{}", tokens[i], tokens[i + 1]);
                tokens[i] = merged;
                tokens.remove(i + 1);
            }
            for tok in &tokens {
                let id = self.vocab.get(tok).copied().unwrap_or(EOT);
                out.push(id);
            }
        }

        // CLIP pools the text embedding at EOT, so keep it inside the 77-token context.
        out.truncate(CLIP_CONTEXT_LEN - 1);
        out.push(EOT);
        out
    }
}

fn normalize(s: &str) -> String {
    let lower = s.to_lowercase();
    lower.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// OpenAI CLIP's pre-tokenization (Swift `CLIPTokenizer`'s regex), so "dog." splits as "dog" then ".".
fn split_words(s: &str) -> Vec<&str> {
    let mut words = Vec::new();
    let mut rest = s;
    while let Some(first) = rest.chars().next() {
        if first.is_whitespace() {
            rest = &rest[first.len_utf8()..];
            continue;
        }
        let special = ["<|startoftext|>", "<|endoftext|>", "'re", "'ve", "'ll", "'s", "'t", "'m", "'d"];
        let end = if let Some(token) = special.iter().find(|token| rest.starts_with(**token)) {
            token.len()
        } else if first.is_alphabetic() {
            rest.find(|c: char| !c.is_alphabetic()).unwrap_or(rest.len())
        } else if first.is_numeric() {
            first.len_utf8()
        } else {
            rest.find(|c: char| c.is_whitespace() || c.is_alphabetic() || c.is_numeric()).unwrap_or(rest.len())
        };
        words.push(&rest[..end]);
        rest = &rest[end..];
    }
    words
}

fn build_byte_encoder() -> [char; 256] {
    let mut bs: Vec<u32> = Vec::new();
    for b in '!' as u32..='~' as u32 {
        bs.push(b);
    }
    for b in '¡' as u32..='¬' as u32 {
        bs.push(b);
    }
    for b in '®' as u32..='ÿ' as u32 {
        bs.push(b);
    }
    let mut cs: Vec<u32> = bs.clone();
    let mut n: u32 = 0;
    let mut out = ['\0'; 256];
    for b in 0u32..256 {
        if !bs.contains(&b) {
            bs.push(b);
            cs.push(256 + n);
            n += 1;
        }
    }
    for (b, c) in bs.iter().zip(cs.iter()) {
        if (*b as usize) < 256 {
            out[*b as usize] = char::from_u32(*c).unwrap_or('?');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn toy() -> ClipTokenizer {
        let vocab = r#"{"a": 9, "aa": 10, "a</w>": 320}"#;
        let merges = "#version: 0.2\na a\n";
        ClipTokenizer::new(vocab, merges).unwrap()
    }

    #[test]
    fn long_query_keeps_trailing_eot_inside_context() {
        let out = toy().encode(&"a ".repeat(200));
        assert_eq!(out.len(), CLIP_CONTEXT_LEN);
        assert_eq!(out[0], SOT);
        assert_eq!(out.iter().rposition(|&t| t == EOT), Some(CLIP_CONTEXT_LEN - 1));
    }

    #[test]
    fn punctuation_and_contractions_split_like_openai() {
        assert_eq!(split_words("a photo of dog."), ["a", "photo", "of", "dog", "."]);
        assert_eq!(split_words("kid's 2nd b-day?!"), ["kid", "'s", "2", "nd", "b", "-", "day", "?!"]);
        assert_eq!(split_words("<|startoftext|>we're  café 12"), ["<|startoftext|>", "we", "'re", "café", "1", "2"]);
    }

    #[test]
    fn short_query_round_trips() {
        let out = toy().encode("A  a");
        assert_eq!(out, vec![SOT, 320, 320, EOT]);
    }

    #[test]
    fn one_megabyte_single_word_completes() {
        let started = Instant::now();
        let out = toy().encode(&"a".repeat(1_048_576));
        assert!(started.elapsed().as_secs() < 5);
        assert!(out.len() <= CLIP_CONTEXT_LEN);
        assert_eq!(out.last(), Some(&EOT));
    }

    #[test]
    fn emoji_run_completes() {
        let started = Instant::now();
        let out = toy().encode(&"🔥".repeat(100_000));
        assert!(started.elapsed().as_secs() < 5);
        assert!(out.len() <= CLIP_CONTEXT_LEN);
        assert_eq!(out.last(), Some(&EOT));
    }

    #[test]
    fn combining_char_flood_completes() {
        let started = Instant::now();
        let out = toy().encode(&format!("e{}", "\u{0301}".repeat(500_000)));
        assert!(started.elapsed().as_secs() < 5);
        assert!(out.len() <= CLIP_CONTEXT_LEN);
        assert_eq!(out.last(), Some(&EOT));
    }

    #[test]
    fn oversized_vocab_is_rejected() {
        let entries: Vec<String> = (0..=MAX_VOCAB).map(|i| format!("\"t{i}\": {i}")).collect();
        let vocab = format!("{{{}}}", entries.join(","));
        assert!(ClipTokenizer::new(&vocab, "").is_err());
    }

    #[test]
    fn oversized_merges_are_rejected() {
        use std::fmt::Write;
        let mut merges = String::new();
        for i in 0..=MAX_MERGES {
            writeln!(merges, "a{i} b{i}").unwrap();
        }
        assert!(ClipTokenizer::new(r#"{"a</w>": 320}"#, &merges).is_err());
    }
}
