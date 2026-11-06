// Describe Files model picker: the curated description models first, then the
// shared engine's portable catalog vision models that run here, fit this
// machine's memory (engine `listModels` fit: ~60% of RAM or VRAM with
// headroom) and are licensed for commercial use.

use fileid_engine::ipc::ModelCatalogItem;

pub(super) const LEADING_CATALOG_ROWS: usize = 6;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct VlmChoice {
    pub id: &'static str,
    pub name: String,
    pub license: String,
    pub terms_required: bool,
    pub resident_gb: f64,
    pub download_gb: f64,
    pub installed: bool,
    pub curated: bool,
    pub fits: bool,
    pub measured_secs_per_image: Option<f64>,
}

impl VlmChoice {
    /// Measured on this machine when benchmarked; otherwise a display estimate
    /// that scales with the resident size (the curated estimates run ~0.85 s/GB).
    pub fn secs_per_image(&self) -> f64 {
        self.measured_secs_per_image
            .unwrap_or_else(|| (self.resident_gb * 0.85).max(2.0))
    }

    pub fn stats(&self) -> String {
        let speed = match self.measured_secs_per_image {
            Some(secs) => format!("measured {secs:.1} s a photo"),
            None if self.curated => format!("about {:.0} s a photo", self.secs_per_image()),
            None => "speed not measured yet".to_owned(),
        };
        let terms = if self.terms_required {
            " · terms shown before download"
        } else {
            ""
        };
        format!(
            "Needs ≈ {:.1} GB memory · {speed} · {}{terms}",
            self.resident_gb, self.license
        )
    }

    pub fn badge(&self) -> String {
        if self.installed {
            "Ready".to_owned()
        } else {
            format!("{:.1} GB download", self.download_gb)
        }
    }
}

/// Apache-2.0 and MIT weights are offered as is. Weights under separately
/// accepted terms are offered only when FileID has a reviewed policy to show
/// before download; every other license is never offered.
pub(super) fn offerable(item: &ModelCatalogItem, has_terms_policy: impl Fn(&str) -> bool) -> bool {
    if item.terms_required {
        return has_terms_policy(&item.id);
    }
    matches!(item.license.as_str(), "Apache-2.0" | "MIT")
}

/// Choices for the picker, curated first in the engine's order, then the
/// catalog: installed, then measured (fastest first), then smallest. Models
/// that don't fit are dropped unless they are the active choice.
pub(super) fn choices(
    models: &[ModelCatalogItem],
    active: &str,
    resolve: impl Fn(&str) -> Option<&'static str>,
    has_terms_policy: impl Fn(&str) -> bool,
) -> Vec<VlmChoice> {
    let mut shown: Vec<VlmChoice> = models
        .iter()
        .filter(|item| item.role == "vision" && item.runnable)
        .filter(|item| item.fits || item.id == active)
        .filter(|item| offerable(item, &has_terms_policy))
        .filter_map(|item| {
            Some(VlmChoice {
                id: resolve(&item.id)?,
                name: item.name.clone(),
                license: item.license.clone(),
                terms_required: item.terms_required,
                resident_gb: item.estimated_resident_gb,
                download_gb: item.download_bytes as f64 / 1e9,
                installed: item.installed,
                curated: item.status == "curated",
                fits: item.fits,
                measured_secs_per_image: item.benchmark.as_ref().and_then(|b| b.seconds_per_image),
            })
        })
        .collect();
    let speed = |choice: &VlmChoice| choice.measured_secs_per_image.unwrap_or(f64::INFINITY);
    shown.sort_by(|a, b| {
        b.curated.cmp(&a.curated).then_with(|| {
            if a.curated {
                std::cmp::Ordering::Equal
            } else {
                b.installed
                    .cmp(&a.installed)
                    .then(speed(a).total_cmp(&speed(b)))
                    .then(a.resident_gb.total_cmp(&b.resident_gb))
                    .then(a.id.cmp(b.id))
            }
        })
    });
    shown
}

/// How many choices go in the main list; the rest sit behind "More models".
pub(super) fn leading_count(choices: &[VlmChoice], active: &str) -> usize {
    let curated = choices.iter().filter(|choice| choice.curated).count();
    let mut count = (curated + LEADING_CATALOG_ROWS).min(choices.len());
    if let Some(position) = choices.iter().position(|choice| choice.id == active) {
        count = count.max(position + 1);
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use fileid_engine::ipc::ModelBenchmark;

    const IDS: [&str; 9] = [
        "qwen2_5_vl_7b",
        "gemma_3_4b",
        "cat_small",
        "cat_tiny",
        "cat_huge",
        "cat_gemma",
        "cat_nc",
        "cat_measured",
        "cat_text",
    ];

    fn resolve(id: &str) -> Option<&'static str> {
        IDS.iter().copied().find(|known| *known == id)
    }

    fn item(id: &str, status: &str, license: &str, resident: f64, fits: bool) -> ModelCatalogItem {
        ModelCatalogItem {
            id: id.into(),
            name: id.into(),
            family: "f".into(),
            upstream: format!("org/{id}"),
            role: "vision".into(),
            format: "gguf".into(),
            quantization: "Q4_K_M".into(),
            license: license.into(),
            terms_required: license == "Gemma",
            parameters_billion: None,
            download_bytes: 3_000_000_000,
            estimated_resident_gb: resident,
            tier: "small".into(),
            status: status.into(),
            runnable: true,
            installed: false,
            fits,
            benchmark: None,
        }
    }

    fn catalog() -> Vec<ModelCatalogItem> {
        let mut measured = item("cat_measured", "verified", "MIT", 9.0, true);
        measured.benchmark = Some(ModelBenchmark {
            measured_at: "t".into(),
            hardware: "h".into(),
            load_seconds: 1.0,
            prompt_tokens_per_second: None,
            generated_tokens_per_second: Some(30.0),
            seconds_per_image: Some(2.5),
            peak_resident_gb: None,
            json_valid_rate: None,
            probe_accuracy: None,
            tuned_arguments: None,
        });
        let mut text = item("cat_text", "candidate", "Apache-2.0", 1.0, true);
        text.role = "text".into();
        let mut unrunnable = item("cat_tiny", "candidate", "Apache-2.0", 0.5, true);
        unrunnable.runnable = false;
        vec![
            item("qwen2_5_vl_7b", "curated", "Apache-2.0", 8.0, true),
            item("gemma_3_4b", "curated", "Gemma", 5.0, true),
            item("cat_small", "candidate", "Apache-2.0", 4.0, true),
            unrunnable,
            item("cat_huge", "candidate", "Apache-2.0", 60.0, false),
            item("cat_gemma", "candidate", "Gemma", 3.0, true),
            item("cat_nc", "candidate", "CC-BY-NC-4.0", 2.0, true),
            measured,
            text,
            item("cat_unknown", "candidate", "MIT", 2.0, true),
        ]
    }

    fn ids(choices: &[VlmChoice]) -> Vec<&'static str> {
        choices.iter().map(|choice| choice.id).collect()
    }

    #[test]
    fn curated_lead_then_measured_then_smallest_that_fit() {
        let shown = choices(&catalog(), "qwen2_5_vl_7b", resolve, |id| id.contains("gemma"));
        assert_eq!(
            ids(&shown),
            ["qwen2_5_vl_7b", "gemma_3_4b", "cat_measured", "cat_gemma", "cat_small"]
        );
        assert!(shown[0].curated && shown[1].curated && !shown[2].curated);
    }

    #[test]
    fn non_commercial_and_unreviewed_terms_are_never_offered() {
        let shown = choices(&catalog(), "qwen2_5_vl_7b", resolve, |_| false);
        assert!(!ids(&shown).contains(&"cat_nc"));
        assert!(!ids(&shown).contains(&"gemma_3_4b"), "terms without a policy to show");
        assert!(!ids(&shown).contains(&"cat_gemma"));
        let mut llama = item("cat_small", "candidate", "Llama-3.2", 1.0, true);
        assert!(!offerable(&llama, |_| true));
        llama.license = "MIT".into();
        assert!(offerable(&llama, |_| false));
    }

    #[test]
    fn the_active_model_stays_even_when_it_does_not_fit() {
        let shown = choices(&catalog(), "cat_huge", resolve, |_| true);
        let huge = shown.iter().find(|choice| choice.id == "cat_huge").unwrap();
        assert!(!huge.fits);
        assert!(!ids(&choices(&catalog(), "qwen2_5_vl_7b", resolve, |_| true)).contains(&"cat_huge"));
    }

    #[test]
    fn the_main_list_holds_curated_plus_a_few_and_always_the_active_one() {
        let many: Vec<VlmChoice> = (0..12)
            .map(|index| VlmChoice {
                id: IDS[index % IDS.len()],
                name: String::new(),
                license: "MIT".into(),
                terms_required: false,
                resident_gb: 1.0,
                download_gb: 1.0,
                installed: false,
                curated: index < 2,
                fits: true,
                measured_secs_per_image: None,
            })
            .collect();
        assert_eq!(leading_count(&many, "qwen2_5_vl_7b"), 2 + LEADING_CATALOG_ROWS);
        assert_eq!(leading_count(&many[..3], "x"), 3);
        let mut tail = many.clone();
        tail[11].id = "late";
        assert_eq!(leading_count(&tail, "late"), 12);
    }

    #[test]
    fn rows_state_memory_speed_license_and_terms() {
        let shown = choices(&catalog(), "qwen2_5_vl_7b", resolve, |_| true);
        let by = |id: &str| shown.iter().find(|choice| choice.id == id).unwrap().clone();
        assert_eq!(by("qwen2_5_vl_7b").stats(), "Needs ≈ 8.0 GB memory · about 7 s a photo · Apache-2.0");
        assert_eq!(by("cat_measured").stats(), "Needs ≈ 9.0 GB memory · measured 2.5 s a photo · MIT");
        assert_eq!(by("cat_measured").secs_per_image(), 2.5);
        assert_eq!(
            by("cat_gemma").stats(),
            "Needs ≈ 3.0 GB memory · speed not measured yet · Gemma · terms shown before download"
        );
        assert_eq!(by("cat_small").badge(), "3.0 GB download");
        let ready = VlmChoice { installed: true, ..by("cat_small") };
        assert_eq!(ready.badge(), "Ready");
    }
}
