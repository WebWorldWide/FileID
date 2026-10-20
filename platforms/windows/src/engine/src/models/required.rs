//! The models a scan cannot start without. `clip_text` is query-time only and
//! RAM++ tags fall back to CLIP scene tags, so neither is required.

use super::registry::{installation_complete, lookup_full, LookupResult};

pub const REQUIRED_SCAN_MODELS: [&str; 2] = ["mobileclip_s2", "arcface"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingModel {
    pub kind: &'static str,
    pub display_name: String,
    pub approx_bytes: u64,
}

/// Required scan models that are not fully installed in the engine's model
/// directory — the exact gate `startScan` enforces.
pub fn missing_required() -> Vec<MissingModel> {
    REQUIRED_SCAN_MODELS
        .iter()
        .filter_map(|&kind| match lookup_full(kind) {
            LookupResult::Found(model) if installation_complete(&model) => None,
            LookupResult::Found(model) => Some(MissingModel {
                kind,
                display_name: model.display_name.to_string(),
                approx_bytes: model.files.iter().map(|file| file.approx_bytes).sum(),
            }),
            LookupResult::Unknown => Some(MissingModel {
                kind,
                display_name: kind.to_string(),
                approx_bytes: 0,
            }),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_models_are_a_subset_of_the_required_set() {
        for missing in missing_required() {
            assert!(REQUIRED_SCAN_MODELS.contains(&missing.kind));
            assert!(!missing.display_name.is_empty());
        }
    }
}
