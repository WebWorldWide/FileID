//! Per-execution-provider model-variant selection.
//!
//! NPU/QNN execution providers need *different* model files than the
//! fp32/fp16 graph CUDA / DirectML / CPU consume: OpenVINO on an Intel NPU
//! wants a statically-INT8-quantized graph; QNN on the Snapdragon HTP wants a
//! w8a8 context binary built via Qualcomm AI Hub. Rather than couple every
//! model wrapper to the active EP, a model registers a base filename stem and
//! asks this resolver for the right variant — which **falls back to the base
//! `.onnx` when the EP-specific variant isn't on disk**, so untested hardware
//! (and any box without the matching Performance Pack) always runs on the
//! universal graph (DirectML → CPU) instead of failing the stage.
//!
//! Wired into the BGE text-embedding wrapper today; any future accelerated
//! model adopts the same convention.

use std::path::{Path, PathBuf};

use super::runtime::{active_provider, ExecutionProvider};

/// Filename suffix for the active EP's preferred variant, or `None` when the
/// EP consumes the base (fp32/fp16) graph.
fn variant_suffix(ep: ExecutionProvider) -> Option<&'static str> {
    match ep {
        // Intel NPU via OpenVINO: static INT8.
        ExecutionProvider::OpenVino => Some("_int8"),
        // Snapdragon HTP via QNN: w8a8 QNN context binary.
        ExecutionProvider::Qnn => Some("_qnn"),
        // CUDA / TensorRT / DirectML / CPU all run the base graph.
        _ => None,
    }
}

/// Resolve the on-disk path for `base_stem` in `model_dir`, preferring the
/// active EP's quantized variant but falling back to the base `.onnx` when
/// that variant isn't installed. `base_stem` is the filename without
/// extension, e.g. `"bge"` → `bge.onnx` / `bge_int8.onnx`
/// (OpenVINO) / `bge_qnn.bin` (QNN).
pub fn resolve_model_path(model_dir: &Path, base_stem: &str) -> PathBuf {
    resolve_for(model_dir, base_stem, active_provider())
}

/// Testable core: `resolve_model_path` with the EP injected.
fn resolve_for(model_dir: &Path, base_stem: &str, ep: ExecutionProvider) -> PathBuf {
    if let Some(suffix) = variant_suffix(ep) {
        // QNN ships a context binary (.bin); OpenVINO ships an .onnx variant.
        let ext = if ep == ExecutionProvider::Qnn { "bin" } else { "onnx" };
        let variant = model_dir.join(format!("{base_stem}{suffix}.{ext}"));
        if variant.exists() {
            return variant;
        }
    }
    model_dir.join(format!("{base_stem}.onnx"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn falls_back_to_base_when_variant_absent() {
        // Nothing on disk → the base .onnx for every EP, including the NPUs.
        let dir = Path::new("Z:/fileid-nonexistent-models");
        for ep in [
            ExecutionProvider::Cuda,
            ExecutionProvider::TensorRt,
            ExecutionProvider::DirectMl,
            ExecutionProvider::OpenVino,
            ExecutionProvider::Qnn,
            ExecutionProvider::Cpu,
        ] {
            assert_eq!(resolve_for(dir, "bge", ep), dir.join("bge.onnx"));
        }
    }

    #[test]
    fn base_eps_never_use_a_suffix() {
        assert_eq!(variant_suffix(ExecutionProvider::Cuda), None);
        assert_eq!(variant_suffix(ExecutionProvider::TensorRt), None);
        assert_eq!(variant_suffix(ExecutionProvider::DirectMl), None);
        assert_eq!(variant_suffix(ExecutionProvider::Cpu), None);
    }

    #[test]
    fn npu_eps_request_quantized_variants() {
        assert_eq!(variant_suffix(ExecutionProvider::OpenVino), Some("_int8"));
        assert_eq!(variant_suffix(ExecutionProvider::Qnn), Some("_qnn"));
    }

    #[test]
    fn picks_variant_when_present_else_base() {
        let tmp = std::env::temp_dir().join(format!("fileid-variants-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let base = tmp.join("bge.onnx");
        let int8 = tmp.join("bge_int8.onnx");
        std::fs::write(&base, b"x").unwrap();
        std::fs::write(&int8, b"x").unwrap();
        // OpenVINO sees its INT8 variant; CPU takes the base; QNN's variant is
        // absent so it also falls back to the base.
        assert_eq!(resolve_for(&tmp, "bge", ExecutionProvider::OpenVino), int8);
        assert_eq!(resolve_for(&tmp, "bge", ExecutionProvider::Cpu), base);
        assert_eq!(resolve_for(&tmp, "bge", ExecutionProvider::Qnn), base);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
