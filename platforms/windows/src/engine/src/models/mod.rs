// ML inference models for the Windows engine.
//
// One submodule per model + the runtime probe + the registry of
// downloadable artifacts. Mirrors the macOS layer's surface; the
// implementations differ because Windows uses ONNX Runtime (CUDA /
// OpenVINO / DirectML / QNN / CPU) instead of CoreML+Vision and a
// llama.cpp subprocess instead of MLX for the VLMs.
//
// Loading is fail-soft: every model's `load(path)` returns Err if the
// weights aren't on disk or the ORT session refuses to bind. The
// pipeline (`pipeline/tagging.rs::ModelStack::load_default`) wraps each
// load in `load_optional` so a missing model degrades that single stage
// to a no-op without failing the whole scan. Inference paths similarly
// return `anyhow::Error` on any failure; callers `tracing::warn!` and
// move on.
//
// The actual ORT session bind + tensor wrangling lives inside each
// submodule. Cross-cutting concerns (EP priority, DLL-search hardening,
// pack detection) live in `runtime.rs`.

pub mod arcface;
pub mod classifier;
pub mod clip_text;
pub mod clip_tokenizer;
pub mod mobileclip;
pub mod registry;
pub mod runtime;
pub mod scrfd;
pub mod vlm;

pub use clip_tokenizer::ClipTokenizer;
