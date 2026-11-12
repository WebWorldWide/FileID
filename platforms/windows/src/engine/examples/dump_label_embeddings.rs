//! Prompt-ensembled CLIP text embeddings for a label list (one label per
//! line) as raw little-endian f32 rows: the matrix behind the RAM++ x CLIP
//! agreement experiment (DECISIONS 2026-11-12, not adopted) and the
//! `--matrix` option of `shared/scripts/eval/tag_eval.py`. Reads the CLIP
//! text model from `FILEID_MODELS_DIR`.
//!   cargo run --example dump_label_embeddings -- <labels.txt> <out.f32>

use fileid_engine::models::clip_text::{self, ClipText};
use fileid_engine::models::scene_vocab::PROMPT_TEMPLATES;
use fileid_engine::models::ClipTokenizer;

const BATCH: usize = 64;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
        .init();
    let args: Vec<String> = std::env::args().collect();
    let [_, labels_path, out_path] = args.as_slice() else {
        anyhow::bail!("usage: dump_label_embeddings <labels.txt> <out.f32>");
    };
    let labels: Vec<String> = std::fs::read_to_string(labels_path)?
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    let weights = clip_text::default_weights_path()?;
    let dir = weights.parent().expect("clip_text weights have a parent dir");
    let vocab = std::fs::read_to_string(dir.join("vocab.json"))?;
    let merges = std::fs::read_to_string(dir.join("merges.txt"))?;
    let mut model = ClipText::load(&weights, ClipTokenizer::new(&vocab, &merges)?)?;

    let started = std::time::Instant::now();
    let prompts: Vec<String> = labels
        .iter()
        .flat_map(|label| PROMPT_TEMPLATES.iter().map(move |t| t.replace("{}", label)))
        .collect();
    let mut embeddings: Vec<Vec<f32>> = Vec::with_capacity(prompts.len());
    for chunk in prompts.chunks(BATCH) {
        match model.embed_batch(chunk) {
            Ok(embs) => embeddings.extend(embs),
            Err(_) => {
                for prompt in chunk {
                    embeddings.push(model.embed(prompt)?);
                }
            }
        }
    }
    let mut bytes = Vec::with_capacity(labels.len() * 512 * 4);
    for group in embeddings.chunks(PROMPT_TEMPLATES.len()) {
        let dim = group[0].len();
        let mut acc = vec![0f32; dim];
        for emb in group {
            for (a, v) in acc.iter_mut().zip(emb) {
                *a += *v;
            }
        }
        let norm = acc.iter().map(|v| v * v).sum::<f32>().sqrt().max(f32::EPSILON);
        for v in acc {
            bytes.extend_from_slice(&(v / norm).to_le_bytes());
        }
    }
    std::fs::write(out_path, &bytes)?;
    println!("wrote {} rows to {out_path} in {:.1} s", labels.len(), started.elapsed().as_secs_f64());
    Ok(())
}
