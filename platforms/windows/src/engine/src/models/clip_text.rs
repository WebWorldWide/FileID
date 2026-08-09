// CLIP text encoder — query-time semantic search. Tokenize a string
// via `ClipTokenizer`, run the ONNX session, L2-normalize, return 512
// floats. Held inside a process-static `OnceLock` in main.rs so
// back-to-back queries reuse the session (avoids the 100-300 ms ORT
// session-create on every keystroke).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ndarray::Array2;
use ort::session::{Session, SessionInputValue, SessionOutputs};
use ort::value::Tensor;

use super::clip_tokenizer::ClipTokenizer;
use super::runtime::{classify_inference_error, configure_session_builder, execution_providers_for_chain, priority_chain, RuntimeProbe};

const CONTEXT_LEN: usize = 77;

pub struct ClipText {
    session: Session,
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
        let probe = RuntimeProbe::shared();
        let chain = priority_chain(probe.vendor);
        let builder = Session::builder().context("ORT session builder")?;
        let mut builder = configure_session_builder(builder)
            .context("configure session (CLIP text)")?;
        let chain_labels: Vec<&'static str> = chain.iter().map(|e| e.as_str()).collect();
        let providers = execution_providers_for_chain(&chain, probe.adapter_index);
        if !providers.is_empty() {
            builder = builder
                .with_execution_providers(providers)
                .context("register execution providers (CLIP text)")?;
        }
        tracing::info!(model = "CLIP text", chain = ?chain_labels, "EP priority chain registered");
        let session = builder
            .commit_from_file(path)
            .context("ORT session commit (CLIP text)")?;
        let input_name = session
            .inputs
            .first()
            .ok_or_else(|| anyhow::anyhow!("CLIP text ONNX has no inputs"))?
            .name
            .clone();
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
        if tokens.len() > CONTEXT_LEN {
            if let Some(&eot) = tokens.last() {
                padded[CONTEXT_LEN - 1] = eot as i64;
            }
        }
        let input = Array2::<i64>::from_shape_vec((1, CONTEXT_LEN), padded)
            .context("CLIP text input shape")?;
        let tensor = Tensor::from_array(input).context("CLIP text input tensor")?;
        let input_name = self.input_name.clone();
        let outputs: SessionOutputs = self
            .session
            .run(vec![(input_name, SessionInputValue::from(tensor))])
            .context("CLIP text session.run")
            .map_err(classify_inference_error)?;
        let (_, value) = outputs
            .iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("CLIP text produced no outputs"))?;
        let (_shape, data) = value
            .try_extract_tensor::<f32>()
            .context("extract CLIP text output as f32")?;
        let mut emb: Vec<f32> = data.to_vec();
        l2_normalize(&mut emb);
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
            if tokens.len() > CONTEXT_LEN {
                if let Some(&eot) = tokens.last() {
                    flat[qi * CONTEXT_LEN + (CONTEXT_LEN - 1)] = eot as i64;
                }
            }
        }
        let input = Array2::<i64>::from_shape_vec((batch, CONTEXT_LEN), flat)
            .context("CLIP text batch input shape")?;
        let tensor = Tensor::from_array(input).context("CLIP text batch input tensor")?;
        let input_name = self.input_name.clone();
        let outputs: SessionOutputs = self
            .session
            .run(vec![(input_name, SessionInputValue::from(tensor))])
            .context("CLIP text batch session.run")
            .map_err(classify_inference_error)?;
        let (_, value) = outputs
            .iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("CLIP text produced no outputs"))?;
        let (_shape, data) = value
            .try_extract_tensor::<f32>()
            .context("extract CLIP text batch output as f32")?;
        if data.len() % batch != 0 {
            anyhow::bail!(
                "CLIP text batch output len {} not divisible by batch {batch}",
                data.len()
            );
        }
        let dim = data.len() / batch;
        let mut out = Vec::with_capacity(batch);
        for i in 0..batch {
            let mut emb = data[i * dim..(i + 1) * dim].to_vec();
            l2_normalize(&mut emb);
            out.push(emb);
        }
        Ok(out)
    }
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
