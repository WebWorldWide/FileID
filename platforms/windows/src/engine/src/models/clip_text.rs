// CLIP text encoder — query-time semantic search. Tokenize a string
// via `ClipTokenizer`, run the ONNX session, L2-normalize, return 512
// floats. Held inside a process-static `OnceLock` in main.rs so
// back-to-back queries reuse the session (avoids recompiling
// the model on every keystroke).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ndarray::Array2;

use super::clip_tokenizer::ClipTokenizer;
use super::inference::{load_session, Dim, InferenceSession, InputSpec};

const CONTEXT_LEN: usize = 77;

pub struct ClipText {
    session: InferenceSession,
    /// The ONNX's single input tensor name, read once at load and reused on
    /// every forward instead of re-walking `session.inputs.first()`.
    input_name: String,
    tokenizer: ClipTokenizer,
}

impl ClipText {
    pub fn load<P: AsRef<Path>>(weights: P, tokenizer: ClipTokenizer) -> Result<Self> {
        let path = weights.as_ref();
        if !path.exists() {
            anyhow::bail!("CLIP text weights missing at {}", path.display());
        }
        let directory = path.parent().ok_or_else(|| anyhow::anyhow!("CLIP weights have no directory"))?;
        let (vocab, merges) = (directory.join("vocab.json"), directory.join("merges.txt"));
        let checks = [(path, "clip_text", 0), (vocab.as_path(), "clip_text", 1), (merges.as_path(), "clip_text", 2)];
        let (session, input_name) = super::clip_space::load_verified(&checks, || session(path))?;
        Ok(Self { session, input_name, tokenizer })
    }

    pub fn embed(&mut self, query: &str) -> Result<Vec<f32>> {
        let tokens = self.tokenizer.encode(query);
        let mut padded = vec![0i64; CONTEXT_LEN];
        for (i, t) in tokens.iter().take(CONTEXT_LEN).enumerate() {
            padded[i] = *t as i64;
        }
        // ENG-65: CLIP pools the sentence embedding at the EOT token (the
        // highest-id token). A query longer than the 77-token context had EOT
        // truncated off the end, so the model pooled at a content token → a
        // wrong embedding. Force the original EOT into the last slot when
        // truncated so the pooling position is correct.
        if tokens.len() > CONTEXT_LEN
            && let Some(&eot) = tokens.last() {
                padded[CONTEXT_LEN - 1] = eot as i64;
            }
        let input = Array2::<i64>::from_shape_vec((1, CONTEXT_LEN), padded)
            .context("CLIP text input shape")?;
        let input_name = self.input_name.clone();
        let output = self
            .session
            .run(vec![(input_name.as_str(), input.into())])
            .context("CLIP text session.run")?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("CLIP text produced no outputs"))?;
        let data = &output.data;
        if data.len() != crate::models::mobileclip::CLIP_EMBED_DIM {
            anyhow::bail!(
                "CLIP text produced a {}-d embedding, expected {} (wrong or corrupt model? text + image encoders must share the space)",
                data.len(),
                crate::models::mobileclip::CLIP_EMBED_DIM
            );
        }
        let mut emb: Vec<f32> = output.data;
        l2_normalize(&mut emb);
        if !super::clip_space::valid(&emb) { anyhow::bail!("Invalid CLIP text embedding") }
        Ok(emb)
    }

    /// Batched variant of [`ClipText::embed`] — tokenize + encode every query
    /// in one ONNX run with a `(B, 77)` input, returning one L2-normalized
    /// embedding per query. Used to build the zero-shot scene-label matrix at
    /// startup; far cheaper than B separate session runs. Assumes the exported
    /// text model has a dynamic batch axis (the Xenova MobileCLIP-S2 export
    /// does).
    pub fn embed_batch(&mut self, queries: &[String]) -> Result<Vec<Vec<f32>>> {
        if queries.is_empty() {
            return Ok(Vec::new());
        }
        let batch = queries.len();
        let mut flat = vec![0i64; batch * CONTEXT_LEN];
        for (qi, q) in queries.iter().enumerate() {
            let tokens = self.tokenizer.encode(q);
            for (i, t) in tokens.iter().take(CONTEXT_LEN).enumerate() {
                flat[qi * CONTEXT_LEN + i] = *t as i64;
            }
            // ENG-65: preserve EOT for an over-length query (see embed()).
            if tokens.len() > CONTEXT_LEN
                && let Some(&eot) = tokens.last() {
                    flat[qi * CONTEXT_LEN + (CONTEXT_LEN - 1)] = eot as i64;
                }
        }
        let input = Array2::<i64>::from_shape_vec((batch, CONTEXT_LEN), flat)
            .context("CLIP text batch input shape")?;
        let input_name = self.input_name.clone();
        let output = self
            .session
            .run(vec![(input_name.as_str(), input.into())])
            .context("CLIP text batch session.run")?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("CLIP text produced no outputs"))?;
        let data = &output.data;
        if data.len() % batch != 0 {
            anyhow::bail!(
                "CLIP text batch output len {} not divisible by batch {batch}",
                data.len()
            );
        }
        let dim = data.len() / batch;
        if dim != crate::models::mobileclip::CLIP_EMBED_DIM {
            anyhow::bail!(
                "CLIP text produced {dim}-d embeddings, expected {} (wrong or corrupt model?)",
                crate::models::mobileclip::CLIP_EMBED_DIM
            );
        }
        let mut out = Vec::with_capacity(batch);
        for i in 0..batch {
            let mut emb = data[i * dim..(i + 1) * dim].to_vec();
            l2_normalize(&mut emb);
            if !super::clip_space::valid(&emb) { anyhow::bail!("Invalid CLIP text embedding") }
            out.push(emb);
        }
        Ok(out)
    }
}

fn session(path: &Path) -> Result<(InferenceSession, String)> {
    load_session("CLIP text", path, &[InputSpec::i64(&[Dim::Batch, Dim::Fixed(CONTEXT_LEN)])])
}

fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-8);
    for x in v.iter_mut() {
        *x /= norm;
    }
}

pub fn default_weights_path() -> Result<PathBuf> {
    Ok(crate::paths::models_dir()?
        .join("clip_text")
        .join("clip_text.onnx"))
}

#[cfg(test)]
mod golden_tests {
    use super::*;
    use crate::models::golden::{check, model_file, Probe, Tolerance};

    const PATH: &str = "clip_text/clip_text.onnx";

    #[test]
    fn embedding_matches_the_golden_vector() {
        let Some(path) = model_file(PATH) else { return };
        let directory = path.parent().unwrap();
        let tokenizer = ClipTokenizer::new(
            &std::fs::read_to_string(directory.join("vocab.json")).unwrap(),
            &std::fs::read_to_string(directory.join("merges.txt")).unwrap(),
        )
        .unwrap();
        let mut model = ClipText::load(&path, tokenizer).unwrap();
        let embedding = model.embed("a photo of a dog running on a beach at sunset").unwrap();
        check(
            "clip_text",
            PATH,
            "the query \"a photo of a dog running on a beach at sunset\" through the CLIP BPE tokenizer; L2-normalized 512-d embedding",
            Tolerance { max_abs: 1e-4, rel_norm: 1e-4 },
            &[Probe { name: "embedding", shape: vec![512], values: &embedding }],
        );
    }
}
