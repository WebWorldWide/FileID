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

/// The memory a load of each installed model asks of admission.
fn installed_requests() -> Vec<(String, u64)> {
    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    {
        crate::apple::vlm::installed_budgets()
    }
    #[cfg(not(all(target_os = "macos", feature = "apple-adapters")))]
    {
        ["qwen2_5_vl_7b", "qwen3_vl_4b", "qwen3_vl_8b", "gemma_3_4b", "mistral_small_3_2"]
            .into_iter()
            .filter_map(|kind| {
                let (weights, projector) = super::vlm::find_weights(kind)?;
                let bytes = std::fs::metadata(weights).ok()?.len().checked_add(std::fs::metadata(projector).ok()?.len())?;
                Some((kind.to_owned(), crate::util::model_memory::estimate_download_mb(bytes)))
            })
            .collect()
    }
}

/// Installed models smaller than `requested`, largest first: what to try when `requested` is refused for memory.
#[cfg(all(target_os = "macos", feature = "apple-adapters"))]
pub(crate) fn smaller_installed(requested: &str) -> Vec<String> {
    smaller_than(requested, &installed_requests())
}

#[cfg(any(all(target_os = "macos", feature = "apple-adapters"), test))]
fn smaller_than(requested: &str, installed: &[(String, u64)]) -> Vec<String> {
    let Some((_, wanted)) = installed.iter().find(|(kind, _)| kind == requested) else { return Vec::new() };
    let mut smaller: Vec<&(String, u64)> = installed.iter().filter(|(_, mb)| mb < wanted).collect();
    smaller.sort_by_key(|(_, mb)| std::cmp::Reverse(*mb));
    smaller.into_iter().map(|(kind, _)| kind.clone()).collect()
}

fn pick_runnable(requested: &str, installed: &[(String, u64)], total_mb: u64, available_mb: u64) -> String {
    use crate::util::model_memory::fits_now;
    let Some((_, wanted)) = installed.iter().find(|(kind, _)| kind == requested) else { return requested.to_owned() };
    if fits_now(total_mb, available_mb, *wanted) {
        return requested.to_owned();
    }
    installed
        .iter()
        .filter(|(_, mb)| mb < wanted && fits_now(total_mb, available_mb, *mb))
        .max_by_key(|(_, mb)| *mb)
        .map_or_else(|| requested.to_owned(), |(kind, _)| kind.clone())
}

/// `requested` when it is already resident or the engine can load it with the memory free right now; otherwise the largest installed model that can, so a busy machine runs a smaller model instead of failing. `requested` again when nothing smaller is installed: admission then reports how much memory is missing.
pub(crate) async fn runnable(requested: &str) -> String {
    if is_resident(requested).await {
        return requested.to_owned();
    }
    pick_runnable(requested, &installed_requests(), (crate::platform::physical_memory_gb() * 1024.0) as u64, crate::platform::available_memory_mb())
}

#[cfg(all(target_os = "macos", feature = "apple-adapters"))]
async fn is_resident(kind: &str) -> bool {
    crate::apple::vlm::status().await.is_ok_and(|status| status.resident() && status.loaded.as_deref() == Some(kind))
}

#[cfg(not(all(target_os = "macos", feature = "apple-adapters")))]
fn is_resident(_kind: &str) -> std::future::Ready<bool> {
    std::future::ready(false)
}

#[cfg(test)]
mod tests {
    use super::pick_runnable;

    fn installed(kinds: &[(&str, u64)]) -> Vec<(String, u64)> {
        kinds.iter().map(|(kind, mb)| ((*kind).to_owned(), *mb)).collect()
    }

    #[test]
    fn a_busy_16_gb_mac_runs_the_installed_smaller_model_instead_of_failing() {
        let all = installed(&[("qwen3_vl_8b", 8704), ("qwen3_vl_4b", 5120), ("qwen3_vl_2b", 3072)]);
        assert_eq!(pick_runnable("qwen3_vl_8b", &all, 16384, 13000), "qwen3_vl_8b");
        assert_eq!(pick_runnable("qwen3_vl_8b", &all, 16384, 6500), "qwen3_vl_4b");
        assert_eq!(pick_runnable("qwen3_vl_8b", &all, 16384, 4500), "qwen3_vl_2b");
        assert_eq!(pick_runnable("qwen3_vl_8b", &all, 16384, 2000), "qwen3_vl_8b");
    }

    #[test]
    fn smaller_installed_models_come_largest_first() {
        let all = installed(&[("qwen3_vl_2b", 3072), ("qwen3_vl_8b", 8704), ("qwen3_vl_4b", 5120)]);
        assert_eq!(super::smaller_than("qwen3_vl_8b", &all), ["qwen3_vl_4b", "qwen3_vl_2b"]);
        assert!(super::smaller_than("qwen3_vl_2b", &all).is_empty());
        assert!(super::smaller_than("missing", &all).is_empty());
    }

    #[test]
    fn nothing_smaller_installed_keeps_the_request_for_admission_to_report() {
        let only_big = installed(&[("qwen3_vl_8b", 8704)]);
        assert_eq!(pick_runnable("qwen3_vl_8b", &only_big, 16384, 5000), "qwen3_vl_8b");
        assert_eq!(pick_runnable("qwen3_vl_4b", &only_big, 16384, 5000), "qwen3_vl_4b");
    }

    #[test]
    fn a_smaller_request_never_grows_into_a_bigger_model() {
        let all = installed(&[("qwen3_vl_8b", 8704), ("qwen3_vl_4b", 5120)]);
        assert_eq!(pick_runnable("qwen3_vl_4b", &all, 16384, 3000), "qwen3_vl_4b");
    }
}
