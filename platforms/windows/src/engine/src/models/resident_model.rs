//! Which resident vision-language model serves Deep Analyze, Chat answers,
//! selection checks, goal ranking and timelines: llama.cpp GGUF weights, or, in
//! the engine the Mac app hosts, the Swift engine's MLX sessions in
//! FileIDAppleKit (`crate::apple::vlm`, Stage 7). Model kinds are shared; this
//! answers whether a kind can run here.

/// True when the engine runs MLX models instead of llama.cpp.
pub(crate) const MLX: bool = cfg!(all(target_os = "macos", feature = "apple-adapters"));

/// Whether `kind` is installed for this engine's backend.
pub(crate) fn installed(kind: &str) -> bool {
    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    {
        crate::apple::vlm::installed(kind)
    }
    #[cfg(not(all(target_os = "macos", feature = "apple-adapters")))]
    {
        super::vlm::find_weights(kind).is_some()
    }
}
