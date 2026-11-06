//! The curated model download set, embedded from `shared/models/manifest.json`
//! at compile time. Each artifact's URL is derived from its Hugging Face repo,
//! revision and path. A document that fails validation yields an empty set, so
//! nothing downloads from a malformed manifest.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Deserialize;

const MANIFEST_JSON: &str = include_str!("../../../../../../shared/models/manifest.json");
const SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LicensePolicy {
    pub spdx: String,
    pub license_url: String,
    pub terms_required: bool,
    pub reviewed_at: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Artifact {
    pub id: String,
    pub kind: String,
    pub platforms: Vec<String>,
    pub repo: String,
    pub revision: String,
    pub path: String,
    pub sha256: String,
    pub approx_bytes: u64,
    pub license: String,
}

impl Artifact {
    pub fn url(&self) -> String {
        format!("https://huggingface.co/{}/resolve/{}/{}", self.repo, self.revision, self.path)
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VlmRepo {
    pub repo: String,
    pub kind: String,
    pub platforms: Vec<String>,
    pub revision: String,
    pub approx_bytes: u64,
    pub license: String,
}

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    #[serde(default, rename = "comment")]
    _comment: String,
    schema_version: u32,
    pub license_policies: BTreeMap<String, LicensePolicy>,
    pub artifacts: Vec<Artifact>,
    pub vlm_repos: Vec<VlmRepo>,
}

pub fn manifest() -> &'static Manifest {
    static MANIFEST: OnceLock<Manifest> = OnceLock::new();
    MANIFEST.get_or_init(|| parse(MANIFEST_JSON).unwrap_or_default())
}

pub fn artifact(id: &str) -> Option<&'static Artifact> {
    manifest().artifacts.iter().find(|artifact| artifact.id == id)
}

/// The terms-required policy key governing any manifest artifact or VLM repo
/// of `kind`.
pub fn terms_policy_for_kind(kind: &str) -> Option<&'static str> {
    let manifest = manifest();
    manifest
        .artifacts
        .iter()
        .filter(|artifact| artifact.kind == kind)
        .map(|artifact| artifact.license.as_str())
        .chain(manifest.vlm_repos.iter().filter(|repo| repo.kind == kind).map(|repo| repo.license.as_str()))
        .find(|license| manifest.license_policies.get(*license).is_some_and(|policy| policy.terms_required))
}

fn parse(json: &str) -> Option<Manifest> {
    let manifest: Manifest = serde_json::from_str(json).ok()?;
    valid(&manifest).then_some(manifest)
}

fn valid(manifest: &Manifest) -> bool {
    let license_ok = |key: &str| {
        manifest.license_policies.get(key).is_some_and(|policy| policy.terms_required || matches!(key, "Apache-2.0" | "MIT"))
    };
    let mut ids = std::collections::HashSet::new();
    let mut repos = std::collections::HashSet::new();
    manifest.schema_version == SCHEMA_VERSION
        && !manifest.artifacts.is_empty()
        && manifest.artifacts.iter().all(|artifact| {
            ids.insert(artifact.id.as_str())
                && !artifact.id.is_empty()
                && !artifact.kind.is_empty()
                && !artifact.platforms.is_empty()
                && repo_ok(&artifact.repo)
                && revision_ok(&artifact.revision)
                && path_ok(&artifact.path)
                && hex(&artifact.sha256, 64)
                && artifact.approx_bytes > 0
                && license_ok(&artifact.license)
        })
        && manifest.vlm_repos.iter().all(|repo| {
            repos.insert(repo.repo.as_str())
                && !repo.kind.is_empty()
                && !repo.platforms.is_empty()
                && repo_ok(&repo.repo)
                && hex(&repo.revision, 40)
                && repo.approx_bytes > 0
                && license_ok(&repo.license)
        })
}

fn hex(value: &str, len: usize) -> bool {
    value.len() == len && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn name_ok(part: &str) -> bool {
    !part.is_empty() && part != "." && part != ".." && part.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn repo_ok(repo: &str) -> bool {
    let mut parts = repo.split('/');
    matches!((parts.next(), parts.next(), parts.next()), (Some(owner), Some(name), None) if name_ok(owner) && name_ok(name))
}

fn revision_ok(revision: &str) -> bool {
    hex(revision, 40) || revision == "main"
}

fn path_ok(path: &str) -> bool {
    path.split('/').all(name_ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_manifest_is_valid_and_hugging_face_only() {
        let manifest = parse(MANIFEST_JSON).expect("embedded manifest must validate");
        assert!(!manifest.artifacts.is_empty());
        for artifact in &manifest.artifacts {
            assert!(artifact.url().starts_with("https://huggingface.co/"), "{}", artifact.id);
        }
        assert_eq!(manifest, *super::manifest());
    }

    #[test]
    fn malformed_documents_are_rejected() {
        let good: serde_json::Value = serde_json::from_str(MANIFEST_JSON).unwrap();
        let mutate = |edit: &dyn Fn(&mut serde_json::Value)| {
            let mut value = good.clone();
            edit(&mut value);
            parse(&value.to_string())
        };
        assert!(mutate(&|_| {}).is_some());
        assert!(parse("not json").is_none());
        assert!(mutate(&|v| v["schemaVersion"] = 1.into()).is_none());
        assert!(mutate(&|v| v["artifacts"] = serde_json::json!([])).is_none());
        assert!(mutate(&|v| v["artifacts"][0]["sha256"] = "abc".into()).is_none());
        assert!(mutate(&|v| v["artifacts"][0]["sha256"] = serde_json::Value::Null).is_none());
        assert!(mutate(&|v| v["artifacts"][0]["repo"] = "evil.example/x/y".into()).is_none());
        assert!(mutate(&|v| v["artifacts"][0]["path"] = "../escape.onnx".into()).is_none());
        assert!(mutate(&|v| v["artifacts"][0]["path"] = "x?y=1".into()).is_none());
        assert!(mutate(&|v| v["artifacts"][0]["revision"] = "v1".into()).is_none());
        assert!(mutate(&|v| v["artifacts"][0]["license"] = "CC-BY-NC-4.0".into()).is_none());
        assert!(mutate(&|v| v["artifacts"][1]["id"] = v["artifacts"][0]["id"].clone()).is_none());
        assert!(mutate(&|v| v["artifacts"][0]["url"] = "https://evil.example/x".into()).is_none());
        assert!(mutate(&|v| v["vlmRepos"][0]["revision"] = "main".into()).is_none());
        assert!(mutate(&|v| v["vlmRepos"][0]["license"] = "Unknown".into()).is_none());
    }

    #[test]
    fn terms_policies_follow_manifest_licenses() {
        assert_eq!(terms_policy_for_kind("gemma_3_4b"), Some("Gemma"));
        assert_eq!(terms_policy_for_kind("paligemma_3b"), Some("Gemma"));
        assert_eq!(terms_policy_for_kind("qwen2_5_vl_7b"), None);
        assert_eq!(terms_policy_for_kind("unknown"), None);
    }
}
