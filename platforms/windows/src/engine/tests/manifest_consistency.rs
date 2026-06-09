// Locks the compiled model registry to the canonical cross-platform
// manifest (shared/models/manifest.json). Either side drifting — a URL,
// hash, or size changed in one place but not the other — fails this test
// on every platform's `cargo test`.
use std::collections::BTreeMap;
use std::path::Path;

use fileid_engine::models::registry::{lookup_full, LookupResult};

const CANONICAL_KINDS: &[&str] = &[
    "yunet_sface",
    "mobileclip_s2",
    "clip_text",
    "ram_plus",
    "mistral_small_3_2",
    "qwen2_5_vl_7b",
    "gemma_3_4b",
    "llama_runtime_x64",
    "cudnn_runtime_x64",
    "ort_cuda_x64",
    "ort_openvino_x64",
    "llama_runtime_cuda_x64",
    "bge_text",
    "florence2_base",
];

fn manifest() -> serde_json::Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../shared/models/manifest.json");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&raw).expect("manifest.json must parse")
}

/// url -> (sha256, approxBytes) for every windows-platform artifact.
fn manifest_windows_artifacts() -> BTreeMap<String, (String, u64)> {
    manifest()["artifacts"]
        .as_array()
        .expect("artifacts array")
        .iter()
        .filter(|a| {
            a["platforms"]
                .as_array()
                .expect("platforms array")
                .iter()
                .any(|p| p == "windows")
        })
        .map(|a| {
            (
                a["url"].as_str().expect("url").to_string(),
                (
                    a["sha256"].as_str().expect("sha256").to_string(),
                    a["approxBytes"].as_u64().expect("approxBytes"),
                ),
            )
        })
        .collect()
}

#[test]
fn registry_matches_manifest_exactly() {
    let mut manifest_entries = manifest_windows_artifacts();
    let mut registry_urls: Vec<String> = Vec::new();

    for kind in CANONICAL_KINDS {
        let LookupResult::Found(model) = lookup_full(kind) else {
            panic!("registry no longer knows kind {kind:?}");
        };
        for file in &model.files {
            let sha = file
                .sha256
                .as_deref()
                .unwrap_or_else(|| panic!("{kind}: {} has no sha256 pin", file.url));
            let Some((m_sha, m_bytes)) = manifest_entries.remove(&file.url) else {
                panic!("{kind}: {} is in the registry but NOT in shared/models/manifest.json", file.url);
            };
            assert_eq!(sha, m_sha, "{kind}: sha256 drift for {}", file.url);
            assert_eq!(
                file.approx_bytes, m_bytes,
                "{kind}: approxBytes drift for {}",
                file.url
            );
            registry_urls.push(file.url.clone());
        }
    }

    assert!(
        manifest_entries.is_empty(),
        "manifest lists windows artifacts the registry doesn't serve: {:?}",
        manifest_entries.keys().collect::<Vec<_>>()
    );
    assert_eq!(registry_urls.len(), 29, "expected 29 pinned artifacts");
}

#[test]
fn manifest_hashes_are_wellformed() {
    let m = manifest();
    for a in m["artifacts"].as_array().unwrap() {
        let sha = a["sha256"].as_str().unwrap();
        assert_eq!(sha.len(), 64, "{}: sha256 must be 64 hex chars", a["id"]);
        assert!(
            sha.chars().all(|c| c.is_ascii_hexdigit()),
            "{}: sha256 must be hex",
            a["id"]
        );
    }
    for r in m["vlmRepos"].as_array().unwrap() {
        let rev = r["revision"].as_str().unwrap();
        assert_eq!(rev.len(), 40, "{}: revision must be a 40-char commit sha", r["repo"]);
        assert!(rev.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
