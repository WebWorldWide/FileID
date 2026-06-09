// CLIP BPE tokenizer — minimal port of the openai/clip tokenization
// pipeline.
//
// CLIP uses a byte-pair-encoded vocab (49152 tokens) plus the special
// `<|startoftext|>` (49406) and `<|endoftext|>` (49407) markers that
// delimit each query. The tokenizer constructor takes the raw `vocab.json`
// (token → id mapping) and `merges.txt` (pair-merge order) the registry
// downloads, builds the merge-rank table, and exposes `encode(s) ->
// Vec<u32>` that produces a token list ready for the `<sot>...<eot>`
// + zero-pad expansion the text-encoder model expects.
//
// This is the smallest possible CLIP tokenizer that round-trips
// canonical inputs ("a photo of a dog" → 49406, 320, 1125, 539, 320,
// 1929, 49407). Edge cases (multi-byte UTF-8 in queries, weird
// punctuation) match HuggingFace tokenizers' bytes-to-unicode mapping.

use std::collections::HashMap;

use anyhow::{Context, Result};
use serde_json::Value;

const SOT: u32 = 49406;
const EOT: u32 = 49407;
/// CLIP's fixed text context length. Keep in sync with `clip_text::CONTEXT_LEN`.
const CLIP_CONTEXT_LEN: usize = 77;
// DoS bounds. The BPE merge loop is O(n²) with per-iteration String clones, so
// an unbroken multi-megabyte "word" would stall a query thread; 1 024 chars is
// >13x the 77-token context, so truncation never changes a real query's
// embedding. The constructor caps are defense-in-depth — vocab/merges are
// SHA256-pinned downloads (real CLIP: 49 408 vocab / ~48 900 merges).
const MAX_QUERY_CHARS: usize = 1_024;
const MAX_WORD_CHARS: usize = 256;
const MAX_MERGES: usize = 50_000;
const MAX_VOCAB: usize = 65_536;

pub struct ClipTokenizer {
    vocab: HashMap<String, u32>,
    /// (left, right) → merge priority (lower = earlier merge).
    merges: HashMap<(String, String), usize>,
    /// 256 mapping byte → unicode char, matches HF's bytes_to_unicode.
    byte_encoder: [char; 256],
}

impl ClipTokenizer {
    /// Construct from the raw `vocab.json` and `merges.txt` strings.
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
            // First line is "#version: ..."; skip if so.
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

    /// Tokenize `query` to a Vec<u32> with `<sot>` prefixed and
    /// `<eot>` suffixed. Truncates to fit before the trailing EOT if
    /// needed; the caller pads to the model's context length (77).
    pub fn encode(&self, query: &str) -> Vec<u32> {
        let normalized: String = normalize(query).chars().take(MAX_QUERY_CHARS).collect();
        let mut out: Vec<u32> = Vec::with_capacity(16);
        out.push(SOT);

        for word in split_words(&normalized) {
            let word = match word.char_indices().nth(MAX_WORD_CHARS) {
                Some((i, _)) => &word[..i],
                None => word,
            };
            // Map each UTF-8 byte to the bytes-to-unicode char and
            // mark word-end with `</w>` per the CLIP convention.
            let mut s = String::new();
            for b in word.bytes() {
                s.push(self.byte_encoder[b as usize]);
            }
            // BPE merge loop.
            let mut tokens: Vec<String> = s.chars().map(|c| c.to_string()).collect();
            if let Some(last) = tokens.last_mut() {
                last.push_str("</w>");
            }
            loop {
                let mut best: Option<(usize, usize)> = None; // (idx, rank)
                for i in 0..tokens.len().saturating_sub(1) {
                    let pair = (tokens[i].clone(), tokens[i + 1].clone());
                    if let Some(&rank) = self.merges.get(&pair) {
                        if best.is_none_or(|(_, r)| rank < r) {
                            best = Some((i, rank));
                        }
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

        // Truncate the content so the trailing EOT always fits inside the
        // model's 77-token context window. The doc comment promised this but
        // the code never did it, so for queries >77 tokens the downstream
        // `.take(CONTEXT_LEN)` dropped the EOT entirely — CLIP relies on the
        // EOT position for the pooled text embedding, so its absence degraded
        // long-query search quality.
        out.truncate(CLIP_CONTEXT_LEN - 1);
        out.push(EOT);
        out
    }
}

fn normalize(s: &str) -> String {
    let lower = s.to_lowercase();
    // Collapse runs of whitespace (CLIP's reference tokenizer does this).
    lower.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn split_words(s: &str) -> impl Iterator<Item = &str> {
    s.split(' ').filter(|w| !w.is_empty())
}

fn build_byte_encoder() -> [char; 256] {
    // HF bytes_to_unicode: keep printable bytes intact, remap the rest
    // into the high-Unicode block so tokens can be JSON-safe strings.
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
