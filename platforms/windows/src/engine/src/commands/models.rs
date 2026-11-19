use crate::ipc::sink::Sink;
use crate::ipc::{EngineError, EventPayload, IpcEvent, ListModelsPayload, ModelCatalog, ModelCatalogItem, Wrap};
use crate::models::{catalog, registry};
use crate::util::model_memory;

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

/// Shared with the catalog generator: weights plus ~15% runtime overhead plus a fixed context/projector allowance.
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

#[derive(Clone, Copy)]
pub(crate) struct Memory {
    pub total_mb: u64,
    pub available_mb: u64,
}

impl Memory {
    pub(crate) fn live() -> Self {
        Self { total_mb: (crate::platform::physical_memory_gb() * 1024.0) as u64, available_mb: crate::platform::available_memory_mb() }
    }
}

/// What the engine's memory admission will ask for when it loads this model.
fn requested_mb(format: &str, download_bytes: u64, resident_gb: f64) -> u64 {
    if format == "gguf" { model_memory::estimate_download_mb(download_bytes) } else { (resident_gb * 1024.0).ceil() as u64 }
}

pub(crate) fn fits(format: &str, download_bytes: u64, resident_gb: f64, memory: Memory) -> (bool, bool) {
    let requested = requested_mb(format, download_bytes, resident_gb);
    (model_memory::fits_budget(memory.total_mb, requested), model_memory::fits_now(memory.total_mb, memory.available_mb, requested))
}

/// Best first. Models under separate terms (Gemma) are opt-in, never a default.
fn default_ladder() -> &'static [&'static str] {
    if crate::paths::APP_HOSTED {
        &["mistral_small_3_2", "qwen3_vl_8b", "qwen3_vl_4b", "qwen3_vl_2b"]
    } else {
        &["qwen2_5_vl_7b", "qwen3_vl_8b", "qwen3_vl_4b"]
    }
}

pub(crate) fn recommended_vision(items: &[ModelCatalogItem], memory: Memory) -> Option<String> {
    let ladder: Vec<(&str, u64)> = default_ladder()
        .iter()
        .filter_map(|id| items.iter().find(|item| item.id == *id && item.role == "vision" && item.runnable && !item.terms_required))
        .map(|item| (item.id.as_str(), requested_mb(&item.format, item.download_bytes, item.estimated_resident_gb)))
        .collect();
    model_memory::recommend(&ladder, memory.total_mb, memory.available_mb).map(str::to_owned)
}

fn platform() -> &'static str {
    if cfg!(windows) { "windows" } else if cfg!(target_os = "macos") { "macos" } else { "linux" }
}

pub(crate) fn build(role: Option<&str>, memory: Memory) -> Vec<ModelCatalogItem> {
    let mut items = Vec::new();
    let measured = crate::models::benchmark::local_results();
    // The Mac-hosted engine serves the app's MLX models, as Swift does; GGUF models never run there.
    if crate::paths::APP_HOSTED {
        return crate::models::mlx::catalog_items(role, memory, &measured);
    }
    if role.is_none_or(|r| r == "vision") {
        for model in &CURATED_VISION {
            let registry::LookupResult::Found(found) = registry::lookup_full(model.kind) else { continue };
            let download: u64 = found.files.iter().map(|f| f.approx_bytes).sum();
            let resident = resident_estimate_gb(download);
            let (fits, fits_now) = fits("gguf", download, resident, memory);
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
                fits,
                fits_now,
                benchmark: measured.get(model.kind).cloned(),
            });
        }
    }
    for entry in catalog::entries() {
        if role.is_some_and(|r| r != entry.role) || !entry.platforms.iter().any(|p| p == platform()) {
            continue;
        }
        let runnable = catalog::runnable(&entry.id).is_some();
        let (fits, fits_now) = fits(&entry.format, entry.download_bytes, entry.estimated_resident_gb, memory);
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
            status: if measured.contains_key(&entry.id) { "verified".into() } else { entry.status.clone() },
            runnable,
            installed: runnable && crate::models::vlm::find_weights(&entry.id).is_some(),
            fits,
            fits_now,
            benchmark: measured.get(&entry.id).cloned(),
        });
    }
    items
}

pub async fn handle(sink: Sink, payload: ListModelsPayload) {
    if !payload.is_valid() {
        sink.send(EngineError::event(
            "invalid_list_models_request",
            "listModels needs a 1–128 character ASCII request ID and a known role.",
        ))
        .await;
        return;
    }
    let catalog = tokio::task::spawn_blocking(move || {
        let memory = Memory::live();
        let vram_mb = if crate::paths::APP_HOSTED { 0 } else { crate::platform::dedicated_vram_mb().unwrap_or(0) };
        let models = build(payload.role.as_deref(), memory);
        ModelCatalog {
            request_id: payload.request_id,
            physical_memory_gb: memory.total_mb as f64 / 1024.0,
            available_memory_mb: memory.available_mb,
            vram_mb,
            recommended_id: recommended_vision(&models, memory),
            models,
        }
    })
    .await;
    if let Ok(catalog) = catalog {
        sink.send(IpcEvent::now(EventPayload::ModelCatalog(Wrap::new(catalog)))).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIXTEEN_GB_IDLE: Memory = Memory { total_mb: 16384, available_mb: 12000 };

    #[test]
    fn fit_is_the_admission_rule_not_a_fraction_of_memory() {
        let download = 3_400_000_000;
        let resident = resident_estimate_gb(download);
        assert_eq!(fits("gguf", download, resident, SIXTEEN_GB_IDLE), (true, true));
        assert_eq!(fits("gguf", download, resident, Memory { total_mb: 16384, available_mb: 4000 }), (true, false));
        assert_eq!(fits("gguf", download, resident, Memory { total_mb: 8192, available_mb: 7000 }), (true, true));
        assert_eq!(fits("gguf", 4_400_000_000, resident, Memory { total_mb: 8192, available_mb: 7000 }), (false, false));
        assert_eq!(fits("gguf", 7_400_000_000, 9.1, Memory { total_mb: 8192, available_mb: 7000 }), (false, false));
        assert_eq!(fits("mlx", 0, 8.5, Memory { total_mb: 16384, available_mb: 6000 }), (true, false));
        assert_eq!(fits("mlx", 0, 8.5, Memory { total_mb: 8192, available_mb: 8000 }), (false, false));
        assert_eq!(fits("mlx", 0, 16.0, Memory { total_mb: 16384, available_mb: 16000 }), (false, false));
    }

    fn vision(id: &str, format: &str, resident_gb: f64) -> ModelCatalogItem {
        ModelCatalogItem {
            id: id.into(), name: id.into(), family: String::new(), upstream: String::new(), role: "vision".into(), format: format.into(),
            quantization: String::new(), license: "Apache-2.0".into(), terms_required: false, parameters_billion: None, download_bytes: 0,
            estimated_resident_gb: resident_gb, tier: "small".into(), status: "curated".into(), runnable: true, installed: false, fits: true, fits_now: true, benchmark: None,
        }
    }

    #[test]
    fn recommendation_falls_down_the_ladder_with_free_memory() {
        let mac = if crate::paths::APP_HOSTED {
            vec![vision("mistral_small_3_2", "mlx", 16.0), vision("qwen3_vl_8b", "mlx", 8.5), vision("qwen3_vl_4b", "mlx", 5.0), vision("qwen3_vl_2b", "mlx", 3.0)]
        } else {
            vec![vision("qwen2_5_vl_7b", "mlx", 7.0), vision("qwen3_vl_8b", "mlx", 8.5), vision("qwen3_vl_4b", "mlx", 5.0)]
        };
        let pick = |total_mb, available_mb| recommended_vision(&mac, Memory { total_mb, available_mb });
        let [best, second, third] = if crate::paths::APP_HOSTED { ["mistral_small_3_2", "qwen3_vl_8b", "qwen3_vl_4b"] } else { ["qwen2_5_vl_7b", "qwen3_vl_8b", "qwen3_vl_4b"] };
        assert_eq!(pick(16384, 13000).as_deref(), Some(if crate::paths::APP_HOSTED { second } else { best }));
        assert_eq!(pick(16384, 6500).as_deref(), Some(third));
        assert_eq!(pick(65536, 50000).as_deref(), Some(best));
        assert_eq!(pick(2048, 2000), None);
    }

    #[test]
    fn terms_gated_and_unrunnable_models_are_never_the_default() {
        let mut items = vec![vision("qwen3_vl_8b", "mlx", 8.5), vision("qwen3_vl_4b", "mlx", 5.0)];
        items[0].terms_required = true;
        assert_eq!(recommended_vision(&items, Memory { total_mb: 32768, available_mb: 30000 }).as_deref(), Some("qwen3_vl_4b"));
        items[1].runnable = false;
        assert_eq!(recommended_vision(&items, Memory { total_mb: 32768, available_mb: 30000 }), None);
    }

    #[test]
    fn tiers_follow_resident_estimate() {
        assert_eq!([1.0, 5.0, 8.0, 20.0, 40.0, 90.0].map(tier_for), ["tiny", "small", "medium", "large", "xlarge", "workstation"]);
    }

    #[test]
    fn curated_models_lead_and_role_filter_applies() {
        let all = build(None, SIXTEEN_GB_IDLE);
        let vision = build(Some("vision"), SIXTEEN_GB_IDLE);
        assert!(vision.iter().all(|m| m.role == "vision"));
        assert!(all.len() >= vision.len());
        if let Some(first) = vision.first() {
            assert_eq!(first.status, "curated");
        }
        assert!(build(Some("speech"), SIXTEEN_GB_IDLE).iter().all(|m| m.status != "curated"));
        let ids: std::collections::HashSet<_> = all.iter().map(|m| &m.id).collect();
        assert_eq!(ids.len(), all.len(), "ids are unique across curated and catalog models");
    }
}
