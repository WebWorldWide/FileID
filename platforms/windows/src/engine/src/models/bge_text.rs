// BGE-small-en-v1.5 text embedding wrapper. Maps a string to a 384-d
// L2-normalized float32 embedding via the BERT-family WordPiece tokenizer
// (`util::wordpiece_tokenizer`) + ONNX inference. Used for semantic search
// over document text (Phase 4b) — different vector space from MobileCLIP
// (which is image-only), so the dbwriter persists these into a parallel
// `text_embeddings` table keyed by model.
//
// Input names are read from the session so a re-export with different
// names still binds; token_type_ids is passed only when the session
// actually advertises it (some BGE exports drop it).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ndarray::Array2;
use ort::session::{Session, SessionInputValue, SessionOutputs};
use ort::value::Tensor;

use super::runtime::{
    classify_inference_error, configure_session_builder, execution_providers_for_chain,
    ExecutionProvider,
};
use super::wordpiece_tokenizer::WordPieceTokenizer;

const HIDDEN: usize = 384;
/// BERT max-length cap; longer text gets truncated by the tokenizer.
const MAX_SEQ: usize = 256;

pub struct BgeText {
    session: Session,
    /// The ONNX's input tensor names, read once at load. BGE binds up to three
    /// inputs (ids/mask/token_type) by name; caching the list avoids rebuilding
    /// a `Vec<String>` from `session.inputs` on every `embed`.
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

        // Pin BGE to the CPU EP. It is a tiny 384-d text encoder over short
        // document strings, off the GPU-bound image hot path, so on a small-VRAM
        // card (e.g. 4 GB DirectML) it must not consume VRAM or contend on the
        // DirectML command queue at scan start. CPU is ORT's implicit fallback,
        // so a CPU-only chain registers no GPU dispatch. Semantic doc-search is
        // unchanged (same weights, same output) — only the EP differs.
        let chain = vec![ExecutionProvider::Cpu];
        let builder = Session::builder().context("ORT session builder")?;
        let mut builder =
            configure_session_builder(builder).context("configure session (BGE)")?;
        // configure_session_builder tunes intra-op threads for the system's
        // DEFAULT EP — 1 on a GPU box, where the accelerator does the parallelism.
        // BGE is pinned to the CPU EP below, so on a GPU box it would otherwise run
        // SINGLE-THREADED on the CPU. Force the CPU performance-core count here so
        // this CPU-only encoder is multi-threaded regardless of the system EP.
        // Output is identical (same weights/graph); only throughput changes.
        builder = builder
            .with_intra_threads(crate::platform::cpu_topology().p_cores.max(1) as usize)
            .context("set BGE intra-op threads")?;
        let providers = execution_providers_for_chain(&chain, None);
        if !providers.is_empty() {
            builder = builder
                .with_execution_providers(providers)
                .context("register execution providers (BGE)")?;
        }
        tracing::info!(model = "BGE-small-en-v1.5", "pinned to CPU EP");
        let session = builder
            .commit_from_file(weights)
            .context("ORT session commit (BGE)")?;
        let input_names: Vec<String> = session.inputs.iter().map(|i| i.name.clone()).collect();

        let mut model = Self { session, input_names, tokenizer };
        let _ = model.embed("warmup")?;
        tracing::info!(model = "BGE-small-en-v1.5", "warmup complete");
        Ok(model)
    }

    /// Embed `text` to a HIDDEN-d L2-normalized vector. Returns `Err` only on
    /// genuine session failures; an empty / whitespace string still produces
    /// a deterministic vector (the `[CLS]`/`[SEP]` pair alone).
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
        let ids_tensor = Tensor::from_array(ids).context("BGE input_ids tensor")?;
        let mask_tensor = Tensor::from_array(mask).context("BGE attention_mask tensor")?;
        let type_tensor = Tensor::from_array(type_ids).context("BGE token_type_ids tensor")?;

        // Bind by name so a session missing `token_type_ids` (some exports
        // do) still runs with just ids + mask. Names are cached at load.
        let mut ids_opt = Some(SessionInputValue::from(ids_tensor));
        let mut mask_opt = Some(SessionInputValue::from(mask_tensor));
        let mut type_opt = Some(SessionInputValue::from(type_tensor));
        let inputs: Vec<(String, SessionInputValue)> = self
            .input_names
            .iter()
            .filter_map(|name| match name.as_str() {
                "input_ids" => ids_opt.take().map(|v| (name.clone(), v)),
                "attention_mask" => mask_opt.take().map(|v| (name.clone(), v)),
                "token_type_ids" => type_opt.take().map(|v| (name.clone(), v)),
                _ => None,
            })
            .collect();

        let outputs: SessionOutputs = self
            .session
            .run(inputs)
            .context("BGE session.run")
            .map_err(classify_inference_error)?;
        let (_, value) = outputs
            .iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("BGE produced no outputs"))?;
        let (shape, data) = value
            .try_extract_tensor::<f32>()
            .context("extract BGE last_hidden_state as f32")?;
        if shape.len() != 3 || shape[2] as usize != HIDDEN {
            anyhow::bail!("BGE output shape {:?} != (1, seq, {HIDDEN})", shape);
        }
        // The reported sequence length must match the tokens we fed (a malformed
        // or wrong-variant ONNX re-export can disagree). Validate before pooling
        // so a hostile/buggy model is a clean per-file error, not an index-OOB
        // panic that kills the scan worker. (audit E9 / S1)
        let seq = shape[1] as usize;
        if shape[0] != 1 || seq != enc.attention_mask.len() || data.len() < seq * HIDDEN {
            anyhow::bail!(
                "BGE output shape {:?} inconsistent with {} input tokens (HIDDEN={HIDDEN})",
                shape,
                enc.attention_mask.len()
            );
        }

        // Mean-pool over the sequence dim using the attention mask, then
        // L2-normalize. Matches BGE-small's canonical pooling.
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
        Ok(emb)
    }
}

fn l2_normalize(v: &mut [f32]) {
    let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-8);
    for x in v.iter_mut() {
        *x /= n;
    }
}

/// Per-EP variant-aware weights path: `bge_text/bge_small.onnx` (or
/// `_int8.onnx` / `_qnn.bin` on accelerated EPs when present).
pub fn default_weights_path() -> Result<PathBuf> {
    // BGE always binds the CPU EP (see `load`), so resolve the CPU variant
    // (= the base .onnx) explicitly rather than via active_provider(), which on
    // an Intel-NPU/Snapdragon box would pick an _int8/_qnn variant a CPU session
    // can't consume. (audit E6)
    Ok(super::variants::resolve_model_path_for(
        &crate::paths::models_dir()?.join("bge_text"),
        "bge_small",
        ExecutionProvider::Cpu,
    ))
}

pub fn default_vocab_path() -> Result<PathBuf> {
    Ok(crate::paths::models_dir()?.join("bge_text").join("vocab.txt"))
}
