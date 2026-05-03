// Deep Analyze — VLM-powered captioning + smart-rename.
//
// Mirror of macOS engine/Sources/FileIDEngine/Pipeline/DeepAnalyze.swift.
// Pipeline:
//   1. Pick a model (Qwen2.5-VL 3B / 7B / Gemma 3 4B / SmolVLM).
//   2. Load via llama.cpp (Vulkan / CUDA / DirectML / CPU backend by EP).
//   3. Per file: render the image / extract a video keyframe / pdfium
//      first-page render → resize to model context → caption + smart name.
//   4. Persist to `deep_analyze_results` (migration v3).
//   5. Emit `deepAnalyzeProgress` IPC events on every N files.
//
// Phase 6 cut: API contract + the model registry (model id → file path
// + SHA256 + size). The actual llama.cpp binding lights up alongside
// the 12-way downloader once it's verified end-to-end.

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VlmModelKind {
    QwenVl3B,
    QwenVl7B,
    Gemma3_4B,
    SmolVlm,
}

impl VlmModelKind {
    pub fn id(self) -> &'static str {
        match self {
            VlmModelKind::QwenVl3B => "qwen2.5-vl-3b",
            VlmModelKind::QwenVl7B => "qwen2.5-vl-7b",
            VlmModelKind::Gemma3_4B => "gemma-3-4b",
            VlmModelKind::SmolVlm => "smolvlm",
        }
    }

    pub fn human_name(self) -> &'static str {
        match self {
            VlmModelKind::QwenVl3B => "Qwen2.5-VL 3B",
            VlmModelKind::QwenVl7B => "Qwen2.5-VL 7B (recommended)",
            VlmModelKind::Gemma3_4B => "Gemma 3 4B",
            VlmModelKind::SmolVlm => "SmolVLM 256M",
        }
    }

    /// Approximate disk size, in MB, for the Q4_K_M quant + mmproj.
    /// Drives the install-disk-budget warning in the model picker UI.
    pub fn approx_size_mb(self) -> u32 {
        match self {
            VlmModelKind::QwenVl3B => 1900,
            VlmModelKind::QwenVl7B => 4500,
            VlmModelKind::Gemma3_4B => 2500,
            VlmModelKind::SmolVlm => 700,
        }
    }

    /// Approximate runtime VRAM/RAM ceiling in MB at Q4_K_M.
    pub fn approx_ram_mb(self) -> u32 {
        match self {
            VlmModelKind::QwenVl3B => 3500,
            VlmModelKind::QwenVl7B => 7500,
            VlmModelKind::Gemma3_4B => 4500,
            VlmModelKind::SmolVlm => 900,
        }
    }
}

#[derive(Debug, Clone)]
pub struct VlmModelFiles {
    pub kind: VlmModelKind,
    /// Main GGUF weights file.
    pub gguf_path: PathBuf,
    /// Vision projection file (vision adapter for the multi-modal head).
    pub mmproj_path: PathBuf,
}

impl VlmModelFiles {
    pub fn default_paths(kind: VlmModelKind) -> anyhow::Result<Self> {
        let dir = crate::paths::models_dir()?.join("VLM").join(kind.id());
        Ok(Self {
            kind,
            gguf_path: dir.join("model-q4_k_m.gguf"),
            mmproj_path: dir.join("mmproj.gguf"),
        })
    }

    pub fn ready(&self) -> bool {
        self.gguf_path.exists() && self.mmproj_path.exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_kinds_have_unique_ids() {
        let kinds = [
            VlmModelKind::QwenVl3B,
            VlmModelKind::QwenVl7B,
            VlmModelKind::Gemma3_4B,
            VlmModelKind::SmolVlm,
        ];
        let mut seen = std::collections::HashSet::new();
        for k in kinds {
            assert!(seen.insert(k.id()), "duplicate id for {:?}", k);
        }
    }

    #[test]
    fn size_estimates_increase_with_capability() {
        assert!(VlmModelKind::SmolVlm.approx_size_mb() < VlmModelKind::QwenVl3B.approx_size_mb());
        assert!(VlmModelKind::QwenVl3B.approx_size_mb() < VlmModelKind::QwenVl7B.approx_size_mb());
    }
}
