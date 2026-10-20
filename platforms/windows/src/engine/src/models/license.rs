//! Opt-in model license terms: which kinds need acceptance, and the on-device
//! record of what the user accepted. Policy URLs and review dates come from
//! `shared/models/manifest.json` (`licensePolicies`). Acceptances live in
//! `<state root>/model-licenses.json` as a sorted JSON array of
//! `ModelLicenseAccepted:<policy>:<reviewedAt>` keys — the format WinUI writes,
//! so every front end shares one record. A re-review bumps `reviewedAt` and
//! therefore asks again.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context as _, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LicensePolicy {
    pub key: &'static str,
    pub display_name: &'static str,
    pub terms_url: &'static str,
    pub reviewed_at: &'static str,
}

impl LicensePolicy {
    pub fn acceptance_key(&self) -> String {
        format!("ModelLicenseAccepted:{}:{}", self.key, self.reviewed_at)
    }
}

fn policy_key(model_kind: &str) -> Option<&'static str> {
    match model_kind {
        "gemma_3_4b" | "gemma_3_12b" | "paligemma_3b" => Some("Gemma"),
        "cudnn_runtime_x64" => Some("NVIDIA-cuDNN"),
        // The ORT CUDA pack and CUDA llama runtime carry CUDA Toolkit
        // redistributables, so the CUDA EULA governs them.
        "ort_cuda_x64" | "llama_runtime_cuda_x64" => Some("NVIDIA-CUDA"),
        _ => None,
    }
}

fn display_name(key: &str) -> &'static str {
    match key {
        "Gemma" => "Google Gemma Terms of Use",
        "NVIDIA-cuDNN" => "NVIDIA cuDNN Software License Agreement",
        "NVIDIA-CUDA" => "NVIDIA CUDA Toolkit End User License Agreement",
        _ => "model license terms",
    }
}

/// `(key, licenseUrl, reviewedAt)` for every terms-required manifest policy.
fn manifest_policies() -> &'static [(String, String, String)] {
    static POLICIES: OnceLock<Vec<(String, String, String)>> = OnceLock::new();
    POLICIES.get_or_init(|| {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../../../../../../shared/models/manifest.json")).unwrap_or_default();
        let Some(policies) = manifest["licensePolicies"].as_object() else {
            return Vec::new();
        };
        policies
            .iter()
            .filter(|(_, policy)| policy["termsRequired"].as_bool() == Some(true))
            .filter_map(|(key, policy)| {
                Some((
                    key.clone(),
                    policy["licenseUrl"].as_str()?.to_owned(),
                    policy["reviewedAt"].as_str()?.to_owned(),
                ))
            })
            .collect()
    })
}

/// The terms a model kind needs accepted before download; `None` when its
/// license is permissive.
pub fn policy_for(model_kind: &str) -> Option<LicensePolicy> {
    let key = policy_key(model_kind)?;
    let (key, terms_url, reviewed_at) = manifest_policies().iter().find(|(name, _, _)| name == key)?;
    Some(LicensePolicy {
        key,
        display_name: display_name(key),
        terms_url,
        reviewed_at,
    })
}

pub fn store_path(state_root: &Path) -> PathBuf {
    state_root.join("model-licenses.json")
}

fn load(state_root: &Path) -> Result<BTreeSet<String>> {
    let path = store_path(state_root);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<Vec<String>>(&bytes)
            .map(|keys| keys.into_iter().collect())
            .with_context(|| format!("parsing {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeSet::new()),
        Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
    }
}

/// True when this exact policy revision was accepted on this device. An
/// unreadable record counts as not accepted.
pub fn is_accepted(state_root: &Path, policy: &LicensePolicy) -> bool {
    load(state_root).is_ok_and(|accepted| accepted.contains(&policy.acceptance_key()))
}

/// Persist acceptance of `policy`, keeping every other recorded key.
pub fn record(state_root: &Path, policy: &LicensePolicy) -> Result<()> {
    let mut accepted = load(state_root)?;
    if !accepted.insert(policy.acceptance_key()) {
        return Ok(());
    }
    std::fs::create_dir_all(state_root).with_context(|| format!("creating {}", state_root.display()))?;
    let path = store_path(state_root);
    let temp = path.with_extension(format!("json.tmp-{}", std::process::id()));
    std::fs::write(&temp, serde_json::to_vec(&accepted.into_iter().collect::<Vec<_>>())?)
        .with_context(|| format!("writing {}", temp.display()))?;
    if let Err(error) = std::fs::rename(&temp, &path) {
        let _ = std::fs::remove_file(&temp);
        return Err(error).with_context(|| format!("replacing {}", path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restricted_kinds_map_to_manifest_policies() {
        let gemma = policy_for("gemma_3_4b").unwrap();
        assert_eq!(gemma.key, "Gemma");
        assert_eq!(gemma.terms_url, "https://ai.google.dev/gemma/terms");
        assert_eq!(gemma.reviewed_at, "2026-07-16");
        assert_eq!(gemma.acceptance_key(), "ModelLicenseAccepted:Gemma:2026-07-16");
        assert_eq!(policy_for("paligemma_3b"), Some(gemma));
        assert_eq!(policy_for("ort_cuda_x64").unwrap().key, "NVIDIA-CUDA");
        assert_eq!(policy_for("cudnn_runtime_x64").unwrap().key, "NVIDIA-cuDNN");
        for open in ["qwen2_5_vl_7b", "arcface", "mobileclip_s2", ""] {
            assert!(policy_for(open).is_none(), "{open}");
        }
    }

    #[test]
    fn acceptance_is_versioned_merged_and_persisted() {
        let root = std::env::temp_dir().join(format!(
            "fileid-license-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let gemma = policy_for("gemma_3_4b").unwrap();
        let cuda = policy_for("ort_cuda_x64").unwrap();
        assert!(!is_accepted(&root, &gemma));
        record(&root, &gemma).unwrap();
        record(&root, &cuda).unwrap();
        record(&root, &gemma).unwrap();
        assert!(is_accepted(&root, &gemma) && is_accepted(&root, &cuda));
        let stored: Vec<String> = serde_json::from_slice(&std::fs::read(store_path(&root)).unwrap()).unwrap();
        assert_eq!(stored, ["ModelLicenseAccepted:Gemma:2026-07-16", "ModelLicenseAccepted:NVIDIA-CUDA:2026-07-16"]);
        let rereviewed = LicensePolicy { reviewed_at: "2099-01-01", ..gemma };
        assert!(!is_accepted(&root, &rereviewed));
        std::fs::write(store_path(&root), b"not json").unwrap();
        assert!(!is_accepted(&root, &gemma));
        assert!(record(&root, &gemma).is_err());
        std::fs::remove_dir_all(root).ok();
    }
}
