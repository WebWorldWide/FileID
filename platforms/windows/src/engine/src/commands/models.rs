//! `listModels`: the curated defaults plus the extended catalog, with
//! install state, hardware fit and any local benchmark for this machine.

use crate::ipc::sink::Sink;
use crate::ipc::{EngineError, EventPayload, IpcEvent, ListModelsPayload, ModelCatalog, ModelCatalogItem, Wrap};
use crate::models::{catalog, registry};

struct Curated {
    kind: &'static str,
    name: &'static str,
    family: &'static str,
    upstream: &'static str,
    license: &'static str,
    parameters: f64,
}

const CURATED_VISION: [Curated; 5] = [
    Curated { kind: "qwen2_5_vl_7b", name: "Qwen2.5-VL 7B", family: "Qwen2.5", upstream: "Qwen/Qwen2.5-VL-7B-Instruct", license: "Apache-2.0", parameters: 7.0 },
    Curated { kind: "qwen3_vl_4b", name: "Qwen3-VL 4B", family: "Qwen3", upstream: "Qwen/Qwen3-VL-4B-Instruct", license: "Apache-2.0", parameters: 4.0 },
    Curated { kind: "qwen3_vl_8b", name: "Qwen3-VL 8B", family: "Qwen3", upstream: "Qwen/Qwen3-VL-8B-Instruct", license: "Apache-2.0", parameters: 8.0 },
    Curated { kind: "gemma_3_4b", name: "Gemma 3 4B", family: "gemma", upstream: "google/gemma-3-4b-it", license: "Gemma", parameters: 4.0 },
    Curated { kind: "mistral_small_3_2", name: "Mistral Small 3.2 24B", family: "Mistral", upstream: "mistralai/Mistral-Small-3.2-24B-Instruct-2506", license: "Apache-2.0", parameters: 24.0 },
];

/// Resident-memory estimate shared with the catalog generator: weights plus
/// ~15% runtime overhead plus a fixed context/projector allowance.
pub(crate) fn resident_estimate_gb(download_bytes: u64) -> f64 {
    (((download_bytes as f64 / 1e9) * 1.15 + 0.6) * 100.0).round() / 100.0
}

pub(crate) fn tier_for(resident_gb: f64) -> &'static str {
    match resident_gb {
        g if g <= 3.0 => "tiny",
        g if g <= 6.0 => "small",
        g if g <= 12.0 => "medium",
        g if g <= 24.0 => "large",
        g if g <= 48.0 => "xlarge",
        _ => "workstation",
    }
}

/// Conservative fit: a model fits when it can stay resident in about 60% of
/// system memory, or entirely in dedicated VRAM with 10% headroom.
pub(crate) fn fits(resident_gb: f64, physical_memory_gb: f64, vram_mb: u64) -> bool {
    resident_gb <= physical_memory_gb * 0.6 || resident_gb * 1.1 <= vram_mb as f64 / 1024.0
}

fn platform() -> &'static str {
    if cfg!(windows) { "windows" } else if cfg!(target_os = "macos") { "macos" } else { "linux" }
}

pub(crate) fn build(role: Option<&str>, physical_memory_gb: f64, vram_mb: u64) -> Vec<ModelCatalogItem> {
    let mut items = Vec::new();
    let measured = crate::models::benchmark::local_results();
    if role.is_none_or(|r| r == "vision") {
        for model in &CURATED_VISION {
            let registry::LookupResult::Found(found) = registry::lookup_full(model.kind) else { continue };
            let download: u64 = found.files.iter().map(|f| f.approx_bytes).sum();
            let resident = resident_estimate_gb(download);
            items.push(ModelCatalogItem {
                id: model.kind.into(),
                name: model.name.into(),
                family: model.family.into(),
                upstream: model.upstream.into(),
                role: "vision".into(),
                format: "gguf".into(),
                quantization: "Q4_K_M".into(),
                license: model.license.into(),
                terms_required: model.license == "Gemma",
                parameters_billion: Some(model.parameters),
                download_bytes: download,
                estimated_resident_gb: resident,
                tier: tier_for(resident).into(),
                status: "curated".into(),
                runnable: true,
                installed: crate::models::vlm::find_weights(model.kind).is_some(),
                fits: fits(resident, physical_memory_gb, vram_mb),
                benchmark: measured.get(model.kind).cloned(),
            });
        }
    }
    for entry in catalog::entries() {
        if role.is_some_and(|r| r != entry.role) || !entry.platforms.iter().any(|p| p == platform()) {
            continue;
        }
        let runnable = catalog::runnable_vision(&entry.id).is_some();
        items.push(ModelCatalogItem {
            id: entry.id.clone(),
            name: entry.name.clone(),
            family: entry.family.clone(),
            upstream: entry.upstream.clone(),
            role: entry.role.clone(),
            format: entry.format.clone(),
            quantization: entry.quantization.chars().take(40).collect(),
            license: entry.license.clone(),
            terms_required: entry.terms_required,
            parameters_billion: entry.parameters_billion,
            download_bytes: entry.download_bytes,
            estimated_resident_gb: entry.estimated_resident_gb,
            tier: entry.tier.clone(),
            // A local benchmark is this machine's verification of a candidate.
            status: if measured.contains_key(&entry.id) { "verified".into() } else { entry.status.clone() },
            runnable,
            installed: runnable && crate::models::vlm::find_weights(&entry.id).is_some(),
            fits: fits(entry.estimated_resident_gb, physical_memory_gb, vram_mb),
            benchmark: measured.get(&entry.id).cloned(),
        });
    }
    items
}

pub async fn handle(sink: Sink, payload: ListModelsPayload) {
    if !payload.is_valid() {
        sink.send(IpcEvent::now(EventPayload::Error(Wrap::new(EngineError {
            kind: "invalid_list_models_request".into(),
            message: "listModels needs a 1–128 character ASCII request ID and a known role.".into(),
            path: None,
            model_kind: None,
        }))))
        .await;
        return;
    }
    let catalog = tokio::task::spawn_blocking(move || {
        let physical_memory_gb = crate::platform::physical_memory_gb();
        let vram_mb = crate::platform::dedicated_vram_mb().unwrap_or(0);
        ModelCatalog { request_id: payload.request_id, physical_memory_gb, vram_mb, models: build(payload.role.as_deref(), physical_memory_gb, vram_mb) }
    })
    .await;
    if let Ok(catalog) = catalog {
        sink.send(IpcEvent::now(EventPayload::ModelCatalog(Wrap::new(catalog)))).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_uses_memory_or_vram_headroom() {
        assert!(fits(9.0, 16.0, 0));
        assert!(!fits(10.0, 16.0, 0));
        assert!(fits(10.0, 16.0, 12 * 1024));
        assert!(!fits(10.0, 8.0, 6 * 1024));
    }

    #[test]
    fn tiers_follow_resident_estimate() {
        assert_eq!([1.0, 5.0, 8.0, 20.0, 40.0, 90.0].map(tier_for), ["tiny", "small", "medium", "large", "xlarge", "workstation"]);
    }

    #[test]
    fn curated_models_lead_and_role_filter_applies() {
        let all = build(None, 16.0, 6144);
        let vision = build(Some("vision"), 16.0, 6144);
        assert!(vision.iter().all(|m| m.role == "vision"));
        assert!(all.len() >= vision.len());
        if let Some(first) = vision.first() {
            assert_eq!(first.status, "curated");
        }
        assert!(build(Some("speech"), 16.0, 0).iter().all(|m| m.status != "curated"));
        let ids: std::collections::HashSet<_> = all.iter().map(|m| &m.id).collect();
        assert_eq!(ids.len(), all.len(), "ids are unique across curated and catalog models");
    }
}
