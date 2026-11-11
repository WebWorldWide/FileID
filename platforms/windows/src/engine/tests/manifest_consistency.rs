// The registry serves every shared-engine artifact in
// shared/models/manifest.json exactly once, with its manifest URL, hash and
// size, and no download the manifest lacks.
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
    "qwen3_vl_4b",
    "qwen3_vl_8b",
    "gemma_3_4b",
    "bge_text",
    "face_mesh",
];

/// The engine the Mac app hosts serves the Mac's ONNX set; its VLMs are MLX
/// repos (`vlmRepos`), resolved in `models::mlx`.
const MAC_APP_KINDS: &[&str] = &["yunet_sface", "mobileclip_s2", "clip_text", "ram_plus", "bge_text", "face_mesh"];

fn platform() -> &'static str {
    if fileid_engine::paths::APP_HOSTED { "macos" } else { "windows" }
}

fn manifest() -> serde_json::Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../shared/models/manifest.json");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&raw).expect("manifest.json must parse")
}

/// url -> (sha256, approxBytes) for every artifact of this engine's platform.
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
                .any(|p| p == platform())
        })
        .map(|a| {
            (
                format!(
                    "https://huggingface.co/{}/resolve/{}/{}",
                    a["repo"].as_str().expect("repo"),
                    a["revision"].as_str().expect("revision"),
                    a["path"].as_str().expect("path"),
                ),
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

    let kinds = if fileid_engine::paths::APP_HOSTED { MAC_APP_KINDS } else { CANONICAL_KINDS };
    for kind in kinds {
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
        }
    }

    assert!(
        manifest_entries.is_empty(),
        "manifest lists windows artifacts the registry doesn't serve: {:?}",
        manifest_entries.keys().collect::<Vec<_>>()
    );
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
