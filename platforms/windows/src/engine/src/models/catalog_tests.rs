use super::*;

fn sample() -> CatalogEntry {
    let revision = "0123456789abcdef0123456789abcdef01234567".to_owned();
    let file = |role: &str, path: &str| CatalogFile {
        role: role.into(),
        path: path.into(),
        sha256: Some("a".repeat(64)),
        bytes: Some(10),
        url: format!("https://huggingface.co/org/model-GGUF/resolve/{revision}/{path}"),
    };
    CatalogEntry {
        id: "model_gguf_q4_k_m".into(),
        name: "model".into(),
        family: "model".into(),
        upstream: "org/model".into(),
        repo: "org/model-GGUF".into(),
        revision: revision.clone(),
        format: "gguf".into(),
        role: "vision".into(),
        quantization: "Q4_K_M".into(),
        license: "Apache-2.0".into(),
        terms_required: false,
        parameters_billion: Some(4.0),
        active_parameters_billion: None,
        download_bytes: 20,
        estimated_resident_gb: 1.0,
        tier: "tiny".into(),
        platforms: vec!["windows".into(), "linux".into()],
        files: vec![file("model", "model-Q4_K_M.gguf"), file("projector", "mmproj-F16.gguf")],
        status: "candidate".into(),
    }
}

#[test]
fn validation_accepts_pinned_permissive_entries() {
    assert!(valid(&sample()));
    let gemma = CatalogEntry { license: "Gemma".into(), terms_required: true, ..sample() };
    assert!(valid(&gemma));
}

#[test]
fn validation_rejects_unsafe_or_unlicensed_entries() {
    let mut unpinned = sample();
    unpinned.files[0].sha256 = None;
    let mut foreign = sample();
    foreign.files[1].url = foreign.files[1].url.replace("org/model-GGUF", "other/model-GGUF");
    let mut off_host = sample();
    off_host.files[1].url = format!("https://{}/org/model-GGUF/resolve/{}/mmproj.gguf", "mirror.invalid", off_host.revision);
    let mut moved = sample();
    moved.files[0].url = moved.files[0].url.replace(&moved.revision, "main");
    let mut traversal = sample();
    traversal.files[0].path = "../escape.gguf".into();
    let cases = [
        unpinned,
        foreign,
        off_host,
        moved,
        traversal,
        CatalogEntry { license: "cc-by-nc-4.0".into(), ..sample() },
        CatalogEntry { license: "Gemma".into(), terms_required: false, ..sample() },
        CatalogEntry { id: "Bad Id".into(), ..sample() },
        CatalogEntry { revision: "main".into(), ..sample() },
        CatalogEntry { files: vec![], ..sample() },
    ];
    for case in cases {
        assert!(!valid(&case), "{case:?}");
    }
}

#[test]
fn shipped_catalog_entries_are_unique_pinned_and_hugging_face_only() {
    let raw: serde_json::Value = serde_json::from_str(CATALOG_JSON).unwrap();
    let declared = raw["entries"].as_array().unwrap().len();
    assert_eq!(entries().len(), declared, "every shipped entry must pass validation");
    let mut ids = std::collections::HashSet::new();
    for entry in entries() {
        assert!(ids.insert(entry.id.as_str()), "duplicate id {}", entry.id);
        assert!(["candidate", "verified"].contains(&entry.status.as_str()));
        assert!(entry.files.iter().all(|f| f.url.starts_with("https://huggingface.co/")));
        assert!(entry.estimated_resident_gb.is_finite() && entry.estimated_resident_gb > 0.0);
    }
}

#[test]
fn runnable_vision_models_resolve_through_the_registry() {
    let Some(entry) = entries().iter().find(|e| runnable_vision(&e.id).is_some()) else { return };
    let dir = directory(&entry.id).unwrap();
    assert_eq!(dir, format!("catalog-{}", entry.id));
    assert_eq!(crate::models::registry::vlm_dir_name(&entry.id), Some(dir));
    if let crate::models::registry::LookupResult::Found(model) = crate::models::registry::lookup_full(&entry.id) {
        assert_eq!(model.id, entry.id);
        let names: Vec<_> = model.files.iter().map(|f| f.dest.file_name().unwrap().to_str().unwrap().to_owned()).collect();
        assert_eq!(names, ["model.gguf", "mmproj.gguf"]);
        assert!(model.files.iter().all(|f| f.sha256.is_some() && f.dest.parent().unwrap().ends_with(dir)));
    }
}

#[test]
fn curated_models_are_never_shadowed_by_the_catalog() {
    assert_eq!(crate::models::registry::vlm_dir_name("qwen3_vl_4b"), Some("qwen3-vl-4b"));
    assert!(runnable_vision("not-a-model").is_none());
}
