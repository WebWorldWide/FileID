use anyhow::{bail, Result};
use std::path::Path;

pub(crate) fn rejection(total_mb: u64, available_mb: u64, requested_mb: u64) -> Option<&'static str> {
    if total_mb == 0 || available_mb == 0 || requested_mb == 0 { return Some("Memory availability could not be established. Retry after checking system resources."); }
    let reserve = (total_mb / 4).clamp(2048,8192);
    if total_mb <= reserve || requested_mb > total_mb - reserve { return Some("This model exceeds the memory budget after reserving space for the system and app. Choose a smaller model."); }
    let free_floor = (total_mb / 16).clamp(512,2048);
    let available = available_mb.min(total_mb);
    if available < free_floor || requested_mb > available - free_floor { return Some("Available memory is too low to load this model safely. Finish other work or choose a smaller model."); }
    None
}

pub(crate) async fn require_model_headroom(gguf: &Path, projection: &Path) -> Result<()> {
    let weights = tokio::fs::metadata(gguf).await?;
    let vision = tokio::fs::metadata(projection).await?;
    if !weights.is_file() || !vision.is_file() || weights.len() == 0 || vision.len() == 0 { bail!("Model weights must be nonempty regular files") }
    let requested = estimate_mb(weights.len(),vision.len())?;
    let total = (crate::platform::physical_memory_gb() * 1024.0) as u64;
    let available = crate::platform::available_memory_mb();
    if let Some(reason) = rejection(total,available,requested) { bail!("{reason}") }
    Ok(())
}

fn estimate_mb(weights: u64, projection: u64) -> Result<u64> {
    let bytes = weights.checked_add(projection).ok_or_else(||anyhow::anyhow!("Model size is too large"))?;
    let mb = bytes.div_ceil(1_048_576);
    mb.checked_add(mb.div_ceil(4)).and_then(|size|size.checked_add(1024)).ok_or_else(||anyhow::anyhow!("Model memory estimate is too large"))
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
}
