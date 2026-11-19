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
        crate::commands::models::default_ladder()
            .iter()
            .copied()
            .chain(["gemma_3_4b", "mistral_small_3_2"])
            .filter_map(|kind| {
                let (weights, projector) = super::vlm::find_weights(kind)?;
                let bytes = std::fs::metadata(weights).ok()?.len().checked_add(std::fs::metadata(projector).ok()?.len())?;
                Some((kind.to_owned(), crate::util::model_memory::estimate_download_mb(bytes)))
            })
            .collect()
    }
}

/// Installed models smaller than `requested`, largest first: what to try when `requested` is refused for memory.
pub(crate) fn smaller_installed(requested: &str) -> Vec<String> {
    smaller_than(requested, &installed_requests())
}

fn smaller_than(requested: &str, installed: &[(String, u64)]) -> Vec<String> {
    let Some((_, wanted)) = installed.iter().find(|(kind, _)| kind == requested) else { return Vec::new() };
    let mut smaller: Vec<&(String, u64)> = installed.iter().filter(|(_, mb)| mb < wanted).collect();
    smaller.sort_by_key(|(_, mb)| std::cmp::Reverse(*mb));
    smaller.into_iter().map(|(kind, _)| kind.clone()).collect()
}

/// Loads `kinds[0]`, and while a load is refused for memory alone, the next one (smaller each time), saying so through `announce(refused, next)`. Returns the index of the kind that was tried last and its result; any other failure ends the walk.
pub(crate) async fn load_until_one_fits<T, Attempt, Loading, Announce, Announced>(
    kinds: &[String],
    mut attempt: Attempt,
    mut announce: Announce,
) -> (usize, anyhow::Result<T>)
where
    Attempt: FnMut(String) -> Loading,
    Loading: std::future::Future<Output = anyhow::Result<T>>,
    Announce: FnMut(String, String) -> Announced,
    Announced: std::future::Future<Output = ()>,
{
    let mut index = 0;
    loop {
        match attempt(kinds[index].clone()).await {
            Err(error) if index + 1 < kinds.len() && crate::util::model_memory::is_memory_shortfall(&error) => {
                announce(kinds[index].clone(), kinds[index + 1].clone()).await;
                index += 1;
            }
            result => return (index, result),
        }
    }
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
    use super::{load_until_one_fits, pick_runnable};
    use crate::models::native_admission::HostAdmissionError;
    use crate::util::model_memory::MemoryShortfall;

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

    fn kinds(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn short_of_memory() -> anyhow::Error {
        HostAdmissionError(MemoryShortfall("Available memory is too low".into()).into()).into()
    }

    #[tokio::test]
    async fn a_load_refused_for_memory_walks_down_to_the_model_that_loads() {
        let tried = std::sync::Mutex::new(Vec::new());
        let said = std::sync::Mutex::new(Vec::new());
        let (index, result) = load_until_one_fits(
            &kinds(&["qwen3_vl_4b", "qwen3_vl_2b_instruct_gguf_q8_0"]),
            |kind| {
                tried.lock().unwrap().push(kind.clone());
                let loads = kind == "qwen3_vl_2b_instruct_gguf_q8_0";
                async move { if loads { Ok(7) } else { Err(short_of_memory()) } }
            },
            |refused, next| {
                said.lock().unwrap().push(format!("{refused}->{next}"));
                async {}
            },
        )
        .await;
        assert_eq!((index, result.unwrap()), (1, 7));
        assert_eq!(*tried.lock().unwrap(), ["qwen3_vl_4b", "qwen3_vl_2b_instruct_gguf_q8_0"]);
        assert_eq!(*said.lock().unwrap(), ["qwen3_vl_4b->qwen3_vl_2b_instruct_gguf_q8_0"]);
    }

    #[tokio::test]
    async fn the_smallest_model_refused_for_memory_reports_that_refusal() {
        let (index, result) = load_until_one_fits(&kinds(&["a", "b"]), |_| async { Err::<(), _>(short_of_memory()) }, |_, _| async {}).await;
        assert_eq!(index, 1);
        let error = result.unwrap_err();
        assert!(crate::util::model_memory::is_memory_shortfall(&error));
        assert!(format!("{error:#}").contains("Available memory is too low"));
    }

    #[tokio::test]
    async fn only_a_memory_refusal_moves_on_to_a_smaller_model() {
        let tried = std::sync::Mutex::new(0);
        let (index, result) = load_until_one_fits(
            &kinds(&["a", "b"]),
            |_| {
                *tried.lock().unwrap() += 1;
                async { Err::<(), _>(HostAdmissionError(anyhow::anyhow!("Resident checkout cancelled")).into()) }
            },
            |_, _| async { panic!("no smaller model is announced for a non-memory failure") },
        )
        .await;
        assert_eq!((index, *tried.lock().unwrap()), (0, 1));
        assert!(result.is_err());
    }

    #[test]
    fn memory_shortfall_is_found_through_the_admission_wrapper() {
        assert!(crate::util::model_memory::is_memory_shortfall(&short_of_memory()));
        assert!(crate::util::model_memory::is_memory_shortfall(&anyhow::Error::new(MemoryShortfall("x".into()))));
        assert!(!crate::util::model_memory::is_memory_shortfall(&anyhow::anyhow!("Available memory is too low")));
    }
}
