//! Florence-2-base grounded-region detection (Phase 7 — foundation only).
//!
//! Microsoft's Florence-2 (MIT, 0.23B params) ships as a 4-ONNX split on
//! `onnx-community/Florence-2-base`. Its non-redundant capability vs. the
//! existing FileID model stack is **phrase-grounded object detection** —
//! the `<OD>` and `<CAPTION_TO_PHRASE_GROUNDING>` prompt heads emit bounding
//! boxes tied to text phrases, which nothing else here produces:
//! - Captioning + tags: SmolVLM / Qwen2.5-VL / Gemma 3 (Deep Analyze).
//! - OCR: Windows.Media.Ocr (built-in).
//! - Image tags: MobileCLIP zero-shot + Deep Analyze VLM refinement.
//! - Documents: pipeline::doc_extract + util::keywords (Phase 4).
//!
//! ## What this module ships today
//!
//! - The `default_weights_dir()` resolver for the install location, so the
//!   welcome / Settings install row can render progress against the same
//!   canonical layout the future loader will read.
//! - A real registry arm (`"florence2_base"`) in `models::registry` pointing
//!   at the community ONNX export — users can install the model now.
//!
//! ## What it does NOT do yet (Phase 7b)
//!
//! - 4 ORT sessions (vision encoder → token embed → BART-style encoder →
//!   KV-cache decoder) wired through `models::variants::resolve_model_path`.
//! - BART tokenizer integration (requires the `tokenizers` crate; gated
//!   behind a Cargo feature when it lands).
//! - A Rust autoregressive generation loop that threads `past_key_values`
//!   between decoder steps.
//! - A `modelKind: "florence2_base"` selectable backend in `Deep Analyze`
//!   for grounded-region tagging.
//!
//! Wire those when grounded OD becomes a concrete product requirement; the
//! plan deliberately ranked Phase 7 last and called it defer-able.
#![allow(dead_code)] // implementation is the documented Phase 7b sub-task.

use std::path::PathBuf;

use anyhow::Result;

/// The directory `florence2_base` installs into:
/// `%LOCALAPPDATA%\FileID\Models\florence2\`. Stable so the future loader
/// and the install UI agree on the layout from day one.
pub(crate) fn default_weights_dir() -> Result<PathBuf> {
    Ok(crate::paths::models_dir()?.join("florence2"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_weights_dir_ends_in_florence2() {
        let p = default_weights_dir().unwrap();
        assert!(
            p.ends_with("florence2"),
            "weights dir should end in `florence2`: {}",
            p.display()
        );
    }
}
