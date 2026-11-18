//! Which resident vision-language model serves Deep Analyze, Chat, selection checks, goal ranking and timelines:
//! llama.cpp GGUF weights, or in the Mac-hosted engine the Swift engine's MLX sessions (`crate::apple::vlm`).

pub(crate) const MLX: bool = cfg!(all(target_os = "macos", feature = "apple-adapters"));

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
