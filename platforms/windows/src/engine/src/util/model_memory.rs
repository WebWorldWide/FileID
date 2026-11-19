use anyhow::{bail, Result};
use std::path::Path;

/// A load refused for memory alone: another, smaller model may still run.
#[derive(Debug)]
pub(crate) struct MemoryShortfall(pub String);

impl std::fmt::Display for MemoryShortfall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for MemoryShortfall {}

pub(crate) fn reserve_mb(total_mb: u64) -> u64 {
    (total_mb / 4).clamp(2048, 8192)
}

fn free_floor_mb(total_mb: u64) -> u64 {
    (total_mb / 16).clamp(512, 2048)
}

fn gb(mb: u64) -> String {
    format!("{:.1} GB", mb as f64 / 1024.0)
}

/// The machine could ever run this model: it fits the installed memory after the system reserve, whatever is free now.
pub fn fits_budget(total_mb: u64, requested_mb: u64) -> bool {
    requested_mb > 0 && total_mb > reserve_mb(total_mb) && requested_mb <= total_mb - reserve_mb(total_mb)
}

/// The machine can load this model this moment: `rejection` finds nothing to object to.
pub fn fits_now(total_mb: u64, available_mb: u64, requested_mb: u64) -> bool {
    rejection(total_mb, available_mb, requested_mb).is_none()
}

pub(crate) fn rejection(total_mb: u64, available_mb: u64, requested_mb: u64) -> Option<String> {
    if total_mb == 0 || available_mb == 0 || requested_mb == 0 { return Some("Memory availability could not be established. Retry after checking system resources.".into()); }
    if !fits_budget(total_mb, requested_mb) {
        return Some(format!("This model needs about {} and this machine has {} after reserving space for the system and app. Choose a smaller model.", gb(requested_mb), gb(total_mb.saturating_sub(reserve_mb(total_mb)))));
    }
    let free_floor = free_floor_mb(total_mb);
    let available = available_mb.min(total_mb);
    if available < free_floor || requested_mb > available - free_floor {
        return Some(format!("Available memory is too low to load this model safely: it needs about {} free and {} is available. Finish other work or choose a smaller model.", gb(requested_mb + free_floor), gb(available)));
    }
    None
}

/// The first model of `ladder` (best first, `(id, requested_mb)`) that loads now; when none does, the smallest that the machine could ever run, which loads once memory frees.
pub(crate) fn recommend<'a>(ladder: &[(&'a str, u64)], total_mb: u64, available_mb: u64) -> Option<&'a str> {
    ladder
        .iter()
        .find(|(_, requested)| fits_now(total_mb, available_mb, *requested))
        .or_else(|| ladder.iter().filter(|(_, requested)| fits_budget(total_mb, *requested)).min_by_key(|(_, requested)| *requested))
        .map(|(id, _)| *id)
}

pub(crate) async fn require_model_headroom(gguf: &Path, projection: &Path) -> Result<u64> {
    let weights = tokio::fs::metadata(gguf).await?;
    let vision = tokio::fs::metadata(projection).await?;
    if !weights.is_file() || !vision.is_file() || weights.len() == 0 || vision.len() == 0 { bail!("Model weights must be nonempty regular files") }
    let requested = estimate_mb(weights.len(),vision.len())?;
    let total = (crate::platform::physical_memory_gb() * 1024.0) as u64;
    let available = crate::platform::available_memory_mb();
    if let Some(reason) = rejection(total,available,requested) { bail!("{reason}") }
    Ok(requested)
}

fn estimate_mb(weights: u64, projection: u64) -> Result<u64> {
    let bytes = weights.checked_add(projection).ok_or_else(||anyhow::anyhow!("Model size is too large"))?;
    let mb = bytes.div_ceil(1_048_576);
    mb.checked_add(mb.div_ceil(4)).and_then(|size|size.checked_add(1024)).ok_or_else(||anyhow::anyhow!("Model memory estimate is too large"))
}

/// A GGUF model's admission estimate from its download size (weights plus projector).
pub fn estimate_download_mb(download_bytes: u64) -> u64 {
    estimate_mb(download_bytes, 0).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_memory_admission_reserves_system_headroom() {
        #[derive(serde::Deserialize)]
        #[serde(rename_all="camelCase")]
        struct Fixture {
            #[serde(rename="totalMB")] total_mb:u64,
            #[serde(rename="availableMB")] available_mb:u64,
            #[serde(rename="requestedMB")] requested_mb:u64,
            accepted:bool,
        }
        let fixtures: Vec<Fixture> = serde_json::from_str(include_str!("../../../../../../shared/test-corpus/model-memory-admission.json")).unwrap();
        for fixture in fixtures { assert_eq!(rejection(fixture.total_mb,fixture.available_mb,fixture.requested_mb).is_none(),fixture.accepted); }
        assert_eq!(estimate_mb(4*1_048_576,4*1_048_576).unwrap(),1034);
        assert!(estimate_mb(u64::MAX,1).is_err());
    }

    #[test]
    fn static_fit_and_live_fit_share_one_budget() {
        for (total, requested) in [(8192, 5120), (8192, 6145), (16384, 12288), (16384, 12289), (65536, 57344), (0, 1024), (16384, 0)] {
            let reserve = reserve_mb(total);
            assert_eq!(fits_budget(total, requested), requested > 0 && total > reserve && requested <= total - reserve);
            if !fits_budget(total, requested) {
                assert!(!fits_now(total, u64::MAX / 2, requested), "a model over budget never loads, however much is free");
            }
        }
        assert!(fits_now(16384, 16384, 12288 - 2048));
        assert!(rejection(16384, 6144, 8704).unwrap().contains("needs about 9.5 GB free and 6.0 GB is available"));
    }

    #[test]
    fn recommendation_follows_the_shared_fixture() {
        #[derive(serde::Deserialize)]
        struct Candidate { id: String, #[serde(rename="requestedMB")] requested_mb: u64 }
        #[derive(serde::Deserialize)]
        struct Case {
            #[serde(rename="totalMB")] total_mb: u64,
            #[serde(rename="availableMB")] available_mb: u64,
            ladder: Vec<Candidate>,
            expected: Option<String>,
        }
        let cases: Vec<Case> = serde_json::from_str(include_str!("../../../../../../shared/test-corpus/model-recommendation.json")).unwrap();
        for case in cases {
            let ladder: Vec<(&str, u64)> = case.ladder.iter().map(|c| (c.id.as_str(), c.requested_mb)).collect();
            assert_eq!(recommend(&ladder, case.total_mb, case.available_mb), case.expected.as_deref(), "{}MB total, {}MB free", case.total_mb, case.available_mb);
        }
    }
}
