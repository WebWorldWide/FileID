// Query-time CLIP text encoder, held in a process-static OnceLock so back-to-back keystroke queries reuse one session.

use crate::util::vector_math::l2_normalize;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ndarray::Array2;

use super::clip_tokenizer::ClipTokenizer;
use super::inference::{load_session, Dim, InferenceSession, InputSpec};

const CONTEXT_LEN: usize = 77;

pub struct ClipText {
    session: InferenceSession,
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
        // CLIP pools at the EOT token: when truncating, keep EOT in the last slot.
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

    /// Batched `embed` (`(B, 77)` input) used to build the scene-label matrix at startup; needs the export's dynamic
    /// batch axis (Xenova MobileCLIP-S2 has one).
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

    fn tokenizer_next_to(path: &std::path::Path) -> ClipTokenizer {
        let directory = path.parent().unwrap();
        ClipTokenizer::new(
            &std::fs::read_to_string(directory.join("vocab.json")).unwrap(),
            &std::fs::read_to_string(directory.join("merges.txt")).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn embedding_matches_the_golden_vector() {
        let Some(path) = model_file(PATH) else { return };
        let tokenizer = tokenizer_next_to(&path);
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

    #[test]
    fn batched_embeddings_match_the_golden_vectors() {
        let Some(path) = model_file(PATH) else { return };
        let mut model = ClipText::load(&path, tokenizer_next_to(&path)).unwrap();
        let queries: Vec<String> = [
            "a photo of a dog running on a beach at sunset",
            "invoice for the office printer",
            "a birthday cake with candles",
        ]
        .map(String::from)
        .into();
        let batch = model.embed_batch(&queries).unwrap();
        let single = model.embed(&queries[0]).unwrap();
        let gap = single.iter().zip(&batch[0]).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
        assert!(gap < 1e-4, "row 0 of the batch drifts from the single-query embedding by {gap}");
        let flat: Vec<f32> = batch.concat();
        check(
            "clip_text_batch",
            PATH,
            "the queries \"a photo of a dog running on a beach at sunset\", \"invoice for the office printer\" and \"a birthday cake with candles\" through the CLIP BPE tokenizer as one (3, 77) batch; L2-normalized 512-d embedding per row",
            Tolerance { max_abs: 1e-4, rel_norm: 1e-4 },
            &[Probe { name: "batch", shape: vec![3, 512], values: &flat }],
        );
    }
}
