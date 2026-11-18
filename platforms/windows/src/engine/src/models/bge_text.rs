// BGE-small-en-v1.5 document embeddings: 384-d, L2-normalized, stored in `text_embeddings` (a different vector space
// from CLIP). Some exports drop token_type_ids, so it is bound only when advertised.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ndarray::Array2;
use super::inference::{Dim, Dtype, InferenceSession, Input, InputSpec};
use super::wordpiece_tokenizer::WordPieceTokenizer;

const HIDDEN: usize = 384;
const MAX_SEQ: usize = 256;

pub struct BgeText {
    session: InferenceSession,
    input_names: Vec<String>,
    tokenizer: WordPieceTokenizer,
}

impl BgeText {
    pub fn load<P: AsRef<Path>>(weights: P, vocab_txt: P) -> Result<Self> {
        let weights = weights.as_ref();
        if !weights.exists() {
            anyhow::bail!("BGE weights missing at {}", weights.display());
        }
        let tokenizer = WordPieceTokenizer::from_vocab_file(vocab_txt.as_ref(), true)
            .context("loading BGE vocab.txt")?;

        // Tiny encoder over short strings: must not contend on the accelerator queue at scan start.
        let sequence = [Dim::Fixed(1), Dim::Symbol("S")];
        let session = InferenceSession::load(
            "BGE",
            weights,
            &["input_ids", "attention_mask", "token_type_ids"].map(|name| InputSpec::named(name, Dtype::I64, &sequence)),
        )?;
        tracing::info!(model = "BGE-small-en-v1.5", backend = super::inference::TRACT_CPU, "loaded");
        let input_names: Vec<String> = session.input_names().to_vec();

        let mut model = Self { session, input_names, tokenizer };
        let _ = model.embed("warmup")?;
        tracing::info!(model = "BGE-small-en-v1.5", "warmup complete");
        Ok(model)
    }

    /// An empty/whitespace string still yields a deterministic vector (the `[CLS]`/`[SEP]` pair alone).
    pub fn embed(&mut self, text: &str) -> Result<Vec<f32>> {
        let enc = self.tokenizer.encode(text, MAX_SEQ);
        let n = enc.ids.len();
        let mut ids = Array2::<i64>::zeros((1, n));
        let mut mask = Array2::<i64>::zeros((1, n));
        let mut type_ids = Array2::<i64>::zeros((1, n));
        for i in 0..n {
            ids[[0, i]] = enc.ids[i];
            mask[[0, i]] = enc.attention_mask[i];
            type_ids[[0, i]] = enc.type_ids[i];
        }
        let mut ids_opt = Some(Input::from(ids));
        let mut mask_opt = Some(Input::from(mask));
        let mut type_opt = Some(Input::from(type_ids));
        let inputs: Vec<(&str, Input)> = self
            .input_names
            .iter()
            .filter_map(|name| match name.as_str() {
                "input_ids" => ids_opt.take().map(|v| (name.as_str(), v)),
                "attention_mask" => mask_opt.take().map(|v| (name.as_str(), v)),
                "token_type_ids" => type_opt.take().map(|v| (name.as_str(), v)),
                _ => None,
            })
            .collect();

        let output = self
            .session
            .run(inputs)
            .context("BGE session.run")?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("BGE produced no outputs"))?;
        let (shape, data) = (&output.shape, &output.data);
        if shape.len() != 3 || shape[2] != HIDDEN {
            anyhow::bail!("BGE output shape {:?} != (1, seq, {HIDDEN})", shape);
        }
        // A malformed or wrong-variant ONNX can report a sequence length that disagrees with the tokens fed; validate
        // so it is a per-file error, not an index-OOB panic that kills the scan worker.
        let seq = shape[1];
        if shape[0] != 1 || seq != enc.attention_mask.len() || data.len() < seq * HIDDEN {
            anyhow::bail!(
                "BGE output shape {:?} inconsistent with {} input tokens (HIDDEN={HIDDEN})",
                shape,
                enc.attention_mask.len()
            );
        }

        let mut emb = vec![0f32; HIDDEN];
        let mut total: f32 = 0.0;
        for t in 0..seq {
            let m = enc.attention_mask[t] as f32;
            if m == 0.0 {
                continue;
            }
            total += m;
            for h in 0..HIDDEN {
                emb[h] += data[t * HIDDEN + h] * m;
            }
        }
        if total > 0.0 {
            for x in &mut emb {
                *x /= total;
            }
        }
        l2_normalize(&mut emb);
        // l2_normalize's max(1e-8) floor passes NaN through; never persist one.
        anyhow::ensure!(emb.iter().all(|v| v.is_finite()), "BGE produced a non-finite embedding");
        Ok(emb)
    }
}

fn l2_normalize(v: &mut [f32]) {
    let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-8);
    for x in v.iter_mut() {
        *x /= n;
    }
}

pub fn default_weights_path() -> Result<PathBuf> {
    Ok(crate::paths::models_dir()?.join("bge_text").join("bge_small.onnx"))
}

pub fn default_vocab_path() -> Result<PathBuf> {
    Ok(crate::paths::models_dir()?.join("bge_text").join("vocab.txt"))
}

#[cfg(test)]
mod golden_tests {
    use super::*;
    use crate::models::golden::{check, model_file, Probe, Tolerance};

    const PATH: &str = "bge_text/bge_small.onnx";

    #[test]
    fn embeddings_match_the_golden_vectors() {
        let (Some(path), Some(vocab)) = (model_file(PATH), model_file("bge_text/vocab.txt")) else { return };
        let mut model = BgeText::load(&path, &vocab).unwrap();
        let short = model.embed("invoice").unwrap();
        let long = model.embed("Quarterly invoice for the office printer, paid by bank transfer on 3 March").unwrap();
        check(
            "bge_text",
            PATH,
            "the texts \"invoice\" and \"Quarterly invoice for the office printer, paid by bank transfer on 3 March\" through the BGE WordPiece tokenizer; mean-pooled L2-normalized 384-d embeddings",
            Tolerance { max_abs: 1e-4, rel_norm: 1e-4 },
            &[
                Probe { name: "short", shape: vec![384], values: &short },
                Probe { name: "long", shape: vec![384], values: &long },
            ],
        );
    }
}
