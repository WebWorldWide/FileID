//! The Mac app's MLX models as the engine it hosts (`apple-adapters`) sees
//! them: the favourite repos pinned in `shared/models/manifest.json`
//! (`vlmRepos`), the audited MLX catalog the app bundles
//! (`FileIDShared/Resources/model-catalog.json`), the Swift engine's install
//! locations, and its verified-install records (`.fileid-verified-<revision>`
//! plus the `-v2` file-identity record). Every rule here is a port of
//! `MLXModelCatalog.swift`, `AIModels.swift`, `VLMDownloader.swift` and
//! `ModelLicenseAcceptance.swift`, so a model either engine installed serves the
//! other without a download.
//!
//! Network access is not here: the file listing is fetched by
//! `downloader::fetch_repo_listing` and the files by the prewarm download path.

use std::collections::{HashMap, HashSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use super::registry::{FileEntry, Model};
use crate::ipc::{ModelBenchmark, ModelCatalogItem};

#[cfg(target_os = "macos")]
const MAC_CATALOG_JSON: &str = include_str!("../../../../../apple/shared/Sources/FileIDShared/Resources/model-catalog.json");
#[cfg(not(target_os = "macos"))]
const MAC_CATALOG_JSON: &str = "";

const RUNTIME: &str = "mlx-swift-lm 3.32.3";
const CATALOG_LICENSES: [&str; 5] = ["apache-2.0", "mit", "bsd-2-clause", "bsd-3-clause", "isc"];

/// `MLXModelCatalog.textArchitectures` / `visionArchitectures`.
const TEXT_ARCHITECTURES: &[&str] = &[
    "acereason", "afmoe", "apertus", "baichuan_m1", "bailing_moe", "bitnet", "cohere", "deepseek_v2", "deepseek_v3", "ernie4_5",
    "exaone4", "falcon_h1", "gemma", "gemma2", "gemma3", "gemma3_text", "gemma3n", "gemma4", "gemma4_text", "gemma4_unified", "glm4",
    "glm4_moe", "glm4_moe_lite", "gpt_oss", "granite", "granitemoehybrid", "helium", "hunyuan_v1_dense", "internlm2", "jamba", "lfm2",
    "lfm2_moe", "lille-130m", "llama", "mamba2", "mimo", "mimo_v2_flash", "minicpm", "minimax", "mistral", "mistral3", "mixtral",
    "nanbeige", "nanochat", "nemotron_h", "nemotron_labs_diffusion", "olmo2", "olmo3", "olmoe", "openelm", "phi", "phi3", "phimoe",
    "qwen2", "qwen3", "qwen3_5", "qwen3_5_moe", "qwen3_5_text", "qwen3_moe", "qwen3_next", "smollm3", "starcoder2",
];
const VISION_ARCHITECTURES: &[&str] = &[
    "fastvlm", "gemma3", "gemma4", "gemma4_unified", "glm_ocr", "idefics3", "lfm2-vl", "lfm2_vl", "llava_qwen2", "mistral3",
    "muse_glimmer", "paligemma", "pixtral", "qwen2_5_vl", "qwen2_vl", "qwen3_5", "qwen3_5_moe", "qwen3_vl", "qwen3_vl_moe", "smolvlm",
];
/// `MLXModelCatalog.runtimeIncompatibleRepos`.
const RUNTIME_INCOMPATIBLE: [&str; 2] = ["mlx-community/SmolVLM-256M-Instruct-4bit", "mlx-community/SmolVLM-500M-Instruct-4bit"];

/// `AIModelKind.favorites`: kind, display name, resident-memory budget (GB).
const FAVORITES: [(&str, &str, f64); 9] = [
    ("qwen2_5_vl_7b", "Qwen2.5-VL 7B (4-bit)", 7.0),
    ("qwen3_vl_2b", "Qwen3-VL 2B (4-bit)", 3.0),
    ("qwen3_vl_4b", "Qwen3-VL 4B (4-bit)", 5.0),
    ("qwen3_vl_8b", "Qwen3-VL 8B (4-bit)", 8.5),
    ("qwen3_5_2b", "Qwen3.5 2B (4-bit)", 3.0),
    ("gemma_3_4b", "Gemma 3 4B (QAT 4-bit)", 4.5),
    ("gemma_3_12b", "Gemma 3 12B (QAT 4-bit)", 9.0),
    ("mistral_small_3_2", "Mistral Small 3.2 24B (4-bit)", 16.0),
    ("paligemma_3b", "PaliGemma 3B (8-bit)", 4.0),
];

/// `ModelLicenseAcceptance.reviewedAt`.
const LICENSE_REVIEWED_AT: &str = "2026-07-16";

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Origin {
    pub repo: String,
    pub revision: String,
    pub license: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Weight {
    pub path: String,
    pub sha256: String,
    pub size: i64,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Quantization {
    pub bits: i64,
    pub group_size: i64,
    pub mode: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CatalogModel {
    pub repo: String,
    pub revision: String,
    pub license: String,
    pub architecture: String,
    pub role: String,
    pub weight_bytes: i64,
    pub bases: Vec<Origin>,
    pub files: Vec<Weight>,
    pub quantization: Quantization,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatalogDocument {
    format_version: i64,
    #[serde(rename = "auditedAt")]
    _audited_at: String,
    runtime: String,
    entries: Vec<CatalogModel>,
}

fn is_hex(value: &str, count: usize) -> bool {
    value.len() == count && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn is_repo(value: &str) -> bool {
    let mut parts = value.split('/');
    let valid = |part: Option<&str>| part.is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b)));
    valid(parts.next()) && valid(parts.next()) && parts.next().is_none() && !value.contains("..")
}

fn safe_relative(path: &str) -> bool {
    !path.is_empty() && !path.starts_with('/') && !path.split('/').any(|part| part == "..")
}

fn entry_is_valid(model: &CatalogModel) -> bool {
    let architectures = if model.role == "vision" { VISION_ARCHITECTURES } else { TEXT_ARCHITECTURES };
    let mut paths = HashSet::new();
    matches!(model.role.as_str(), "vision" | "text")
        && is_repo(&model.repo)
        && is_hex(&model.revision, 40)
        && CATALOG_LICENSES.contains(&model.license.as_str())
        && architectures.contains(&model.architecture.as_str())
        && !model.bases.is_empty()
        && model.bases.len() <= 20
        && model.bases.iter().all(|base| is_repo(&base.repo) && is_hex(&base.revision, 40) && CATALOG_LICENSES.contains(&base.license.as_str()))
        && [2, 3, 4, 5, 6, 8].contains(&model.quantization.bits)
        && [32, 64, 128].contains(&model.quantization.group_size)
        && model.quantization.mode == "affine"
        && !model.files.is_empty()
        && model.files.len() <= 128
        && model.files.iter().all(|file| paths.insert(file.path.as_str()))
        && model.files.iter().all(|file| {
            safe_relative(&file.path) && file.path.ends_with(".safetensors") && is_hex(&file.sha256, 64) && file.size > 0 && file.size < 1_000_000_000_000
        })
        && model.weight_bytes == model.files.iter().map(|file| file.size).sum::<i64>()
}

/// `MLXModelCatalog.load`: one invalid entry rejects the whole catalog.
pub(crate) fn parse_catalog(json: &str) -> Vec<CatalogModel> {
    let Ok(document) = serde_json::from_str::<CatalogDocument>(json) else {
        return Vec::new();
    };
    let unique = document.entries.iter().map(|entry| entry.repo.as_str()).collect::<HashSet<_>>().len() == document.entries.len();
    if document.format_version != 1 || document.runtime != RUNTIME || !unique || !document.entries.iter().all(entry_is_valid) {
        return Vec::new();
    }
    document.entries.into_iter().filter(|entry| !RUNTIME_INCOMPATIBLE.contains(&entry.repo.as_str())).collect()
}

/// One installable MLX model: a favourite or a catalog entry (`AIModelKind`).
#[derive(Debug)]
pub struct Repo {
    pub kind: &'static str,
    pub display_name: &'static str,
    pub repo: &'static str,
    pub revision: &'static str,
    pub approx_bytes: u64,
    /// `licensePolicyKey`: a manifest policy key, or the catalog's SPDX name.
    pub license: &'static str,
    pub ram_budget_gb: f64,
    pub vision: bool,
    pub favorite: bool,
    /// The catalog entry for this repo (`AIModelKind.catalogEntry`).
    pub catalog: Option<&'static CatalogModel>,
}

impl Repo {
    /// Catalog weights the listing and the verified record must match: only
    /// for catalog models, never for the manifest favourites (VLMDownloader).
    pub fn audited_weights(&self) -> Option<&'static CatalogModel> {
        if self.favorite { None } else { self.catalog }
    }

    fn quantization(&self) -> String {
        match self.catalog {
            Some(entry) => format!("{}bit", entry.quantization.bits),
            None if self.display_name.contains("8-bit") => "8bit".into(),
            None => "4bit".into(),
        }
    }
}

fn leak(value: String) -> &'static str {
    Box::leak(value.into_boxed_str())
}

fn spdx_policy(license: &str) -> &'static str {
    match license {
        "apache-2.0" => "Apache-2.0",
        "mit" => "MIT",
        "bsd-2-clause" => "BSD-2-Clause",
        "bsd-3-clause" => "BSD-3-Clause",
        "isc" => "ISC",
        _ => "Unknown",
    }
}

/// Every entry of the bundled Mac catalog, or none when it fails validation.
pub fn catalog_entries() -> &'static [CatalogModel] {
    static CATALOG: OnceLock<Vec<CatalogModel>> = OnceLock::new();
    CATALOG.get_or_init(|| parse_catalog(MAC_CATALOG_JSON))
}

fn repos() -> &'static [Repo] {
    static REPOS: OnceLock<Vec<Repo>> = OnceLock::new();
    REPOS.get_or_init(|| {
        let catalog = catalog_entries();
        let by_repo: HashMap<&str, &'static CatalogModel> = catalog.iter().map(|entry| (entry.repo.as_str(), entry)).collect();
        let mut repos = Vec::new();
        for (kind, display_name, ram_budget_gb) in FAVORITES {
            let pin = super::manifest::manifest().vlm_repos.iter().find(|pin| pin.kind == kind && pin.platforms.iter().any(|p| p == "macos"));
            let Some(pin) = pin else { continue };
            repos.push(Repo {
                kind,
                display_name,
                repo: &pin.repo,
                revision: &pin.revision,
                approx_bytes: pin.approx_bytes,
                license: &pin.license,
                ram_budget_gb,
                vision: true,
                favorite: true,
                catalog: by_repo.get(pin.repo.as_str()).copied(),
            });
        }
        // Names are leaked once so catalog kinds are `&'static` like favourites.
        for entry in catalog {
            let approx_bytes = u64::try_from(entry.weight_bytes).unwrap_or(0);
            repos.push(Repo {
                kind: leak(format!("hf:{}", entry.repo)),
                display_name: leak(entry.repo.rsplit('/').next().unwrap_or("Model").replace('-', " ")),
                repo: &entry.repo,
                revision: &entry.revision,
                approx_bytes,
                license: spdx_policy(&entry.license),
                ram_budget_gb: approx_bytes as f64 / 1_073_741_824.0 * 1.5 + 2.0,
                vision: entry.role != "text",
                favorite: false,
                catalog: Some(entry),
            });
        }
        repos
    })
}

/// Look a kind up whatever the build (tests and tooling).
pub fn lookup(kind: &str) -> Option<&'static Repo> {
    repos().iter().find(|repo| repo.kind == kind)
}

/// The MLX model a prewarm or catalog request names. Only the engine the Mac
/// app hosts serves MLX; every other build keeps its GGUF bundles.
pub fn repo(kind: &str) -> Option<&'static Repo> {
    if crate::paths::APP_HOSTED { lookup(kind) } else { None }
}

/// `AIModelKind.allCases`: favourites, then catalog models whose repo is not
/// already a favourite. A catalog copy of a favourite stays reachable by its
/// `hf:` kind, as `AIModelKind(rawValue:)` allows.
pub fn listed() -> impl Iterator<Item = &'static Repo> {
    let favorites: HashSet<&str> = repos().iter().filter(|repo| repo.favorite).map(|repo| repo.repo).collect();
    repos().iter().filter(move |repo| repo.favorite || !favorites.contains(repo.repo))
}

/// The user's home as CoreFoundation resolves it (`CFFIXED_USER_HOME` first).
fn home() -> Option<PathBuf> {
    ["CFFIXED_USER_HOME", "HOME"].iter().filter_map(std::env::var_os).find(|value| !value.is_empty()).map(PathBuf::from)
}

/// `ModelCachePaths.huggingFaceModels`: `FILEID_HF_CACHE_ROOT` when absolute;
/// inside the App Store sandbox `Application Support/FileID/Models/huggingface`;
/// otherwise `~/Documents/huggingface`; then `models`.
pub fn models_root() -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("FILEID_HF_CACHE_ROOT").filter(|root| Path::new(root).is_absolute()) {
        return Some(PathBuf::from(root).join("models"));
    }
    let home = home()?;
    let sandboxed = std::env::var_os("APP_SANDBOX_CONTAINER_ID").is_some_and(|id| !id.is_empty());
    let root = if sandboxed {
        home.join("Library/Application Support/FileID/Models/huggingface")
    } else {
        home.join("Documents/huggingface")
    };
    Some(root.join("models"))
}

pub fn model_dir(repo: &Repo) -> Option<PathBuf> {
    Some(repo.repo.split('/').fold(models_root()?, |path, part| path.join(part)))
}

/// `AIModelKind.isInstalledOnDisk()`: what `listModels` reports.
pub fn has_config(repo: &Repo) -> bool {
    model_dir(repo).is_some_and(|dir| dir.join("config.json").is_file())
}

fn tier(gigabytes: f64) -> &'static str {
    match gigabytes {
        g if g <= 3.0 => "tiny",
        g if g <= 6.0 => "small",
        g if g <= 12.0 => "medium",
        g if g <= 24.0 => "large",
        g if g <= 48.0 => "xlarge",
        _ => "workstation",
    }
}

/// The Swift engine's `listModels` reply: favourites, then the bundled catalog.
pub fn catalog_items(role: Option<&str>, physical_memory_gb: f64, benchmarks: &HashMap<String, ModelBenchmark>) -> Vec<ModelCatalogItem> {
    listed()
        .filter_map(|repo| {
            let kind_role = if repo.vision { "vision" } else { "text" };
            if role.is_some_and(|role| role != kind_role) {
                return None;
            }
            Some(ModelCatalogItem {
                id: repo.kind.into(),
                name: repo.display_name.into(),
                family: repo.repo.rsplit('/').next().unwrap_or(repo.kind).into(),
                upstream: repo.catalog.and_then(|entry| entry.bases.first()).map_or(repo.repo, |base| base.repo.as_str()).into(),
                role: kind_role.into(),
                format: "mlx".into(),
                quantization: repo.quantization(),
                license: repo.license.into(),
                terms_required: repo.license == "Gemma",
                parameters_billion: None,
                download_bytes: repo.approx_bytes,
                estimated_resident_gb: repo.ram_budget_gb,
                tier: tier(repo.ram_budget_gb).into(),
                status: if repo.favorite { "curated" } else { "candidate" }.into(),
                runnable: true,
                installed: has_config(repo),
                fits: repo.ram_budget_gb <= physical_memory_gb * 0.6,
                benchmark: benchmarks.get(repo.kind).cloned(),
            })
        })
        .collect()
}

/// `ModelLicenseAcceptance`: the policy terms a model needs, when any.
fn terms(repo: &Repo) -> Option<&'static str> {
    if !repo.favorite {
        return None;
    }
    let policy = super::manifest::manifest().license_policies.get(repo.license)?;
    policy.terms_required.then_some(policy.license_url.as_str())
}

pub fn terms_accepted(repo: &Repo) -> bool {
    let Some(url) = terms(repo) else {
        return true;
    };
    let Some(home) = home() else {
        return false;
    };
    let marker = home
        .join("Library/Application Support/FileID/LicenseAcceptances")
        .join(format!("{}-{LICENSE_REVIEWED_AT}.accepted", repo.license));
    let regular = std::fs::symlink_metadata(&marker).is_ok_and(|meta| meta.file_type().is_file());
    regular && read_capped(&marker, 4096).is_some_and(|bytes| bytes == format!("policy={}\nreviewedAt={LICENSE_REVIEWED_AT}\nterms={url}\n", repo.license).as_bytes())
}

pub fn license_name(repo: &Repo) -> &'static str {
    match repo.license {
        "Apache-2.0" if repo.favorite => "Apache License 2.0",
        "Gemma" if repo.favorite => "Gemma Terms of Use",
        other => other,
    }
}

// ---------------------------------------------------------------------------
// Verified-install records (`VLMDownloader.VerifiedSentinel` / `IdentitySentinel`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VerifiedFile {
    pub path: String,
    pub sha256: String,
    pub size: i64,
}

/// Field order is the Swift encoder's sorted-key order.
#[derive(Debug, Serialize, Deserialize)]
struct VerifiedManifest {
    files: Vec<VerifiedFile>,
    revision: String,
    version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FileIdentity {
    pub path: String,
    pub size: i64,
    pub device: i64,
    pub inode: u64,
    pub modified_seconds: i64,
    pub modified_nanoseconds: i64,
    pub changed_seconds: i64,
    pub changed_nanoseconds: i64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IdentityRecord {
    version: i64,
    revision: String,
    #[serde(rename = "manifestSHA256")]
    manifest_sha256: String,
    files: Vec<FileIdentity>,
}

pub fn verified_record_path(dir: &Path, revision: &str) -> PathBuf {
    dir.join(format!(".fileid-verified-{revision}"))
}

pub fn identity_record_path(dir: &Path, revision: &str) -> PathBuf {
    dir.join(format!(".fileid-verified-{revision}-v2"))
}

pub fn installed_marker_path(dir: &Path) -> PathBuf {
    dir.join(".fileid-installed")
}

fn model_file(dir: &Path, path: &str) -> Option<PathBuf> {
    safe_relative(path).then(|| path.split('/').filter(|part| !part.is_empty() && *part != ".").fold(dir.to_path_buf(), |p, part| p.join(part)))
}

#[cfg(unix)]
pub fn file_identity(file: &Path, path: &str) -> Option<FileIdentity> {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::symlink_metadata(file).ok()?;
    if !meta.file_type().is_file() {
        return None;
    }
    Some(FileIdentity {
        path: path.to_string(),
        size: i64::try_from(meta.size()).ok()?,
        // Darwin's st_dev is a signed 32-bit value; Swift widens it with sign.
        device: meta.dev() as i32 as i64,
        inode: meta.ino(),
        modified_seconds: meta.mtime(),
        modified_nanoseconds: meta.mtime_nsec(),
        changed_seconds: meta.ctime(),
        changed_nanoseconds: meta.ctime_nsec(),
    })
}

#[cfg(not(unix))]
pub fn file_identity(_file: &Path, _path: &str) -> Option<FileIdentity> {
    None
}

fn identities(dir: &Path, paths: &[String]) -> Option<Vec<FileIdentity>> {
    paths.iter().map(|path| file_identity(&model_file(dir, path)?, path)).collect()
}

fn read_capped(path: &Path, cap: u64) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path).ok()?.take(cap + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= cap).then_some(bytes)
}

pub fn sha256_file(path: &Path) -> Option<String> {
    use sha2::{Digest as _, Sha256};
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => hasher.update(&buffer[..count]),
            Err(_) => return None,
        }
    }
    Some(hex::encode(hasher.finalize()))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    hex::encode(Sha256::digest(bytes))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    crate::util::read_only::require_writable(path)?;
    let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

fn write_identity(dir: &Path, revision: &str, manifest: &[u8], files: Vec<FileIdentity>) {
    let record = IdentityRecord { version: 2, revision: revision.to_string(), manifest_sha256: sha256_bytes(manifest), files };
    if let Ok(bytes) = serde_json::to_vec(&record) {
        let _ = write_atomic(&identity_record_path(dir, revision), &bytes);
    }
}

fn identity_matches(dir: &Path, revision: &str, manifest: &[u8], paths: &[String]) -> bool {
    let Some(bytes) = read_capped(&identity_record_path(dir, revision), 16 << 20) else {
        return false;
    };
    let Ok(record) = serde_json::from_slice::<IdentityRecord>(&bytes) else {
        return false;
    };
    record.version == 2
        && record.revision == revision
        && record.manifest_sha256 == sha256_bytes(manifest)
        && identities(dir, paths).is_some_and(|current| current == record.files)
}

fn read_manifest(dir: &Path, revision: &str) -> Option<(Vec<u8>, VerifiedManifest)> {
    let bytes = read_capped(&verified_record_path(dir, revision), 16 << 20)?;
    let manifest = serde_json::from_slice::<VerifiedManifest>(&bytes).ok()?;
    Some((bytes, manifest))
}

/// `VLMDownloader.verifiedSentinelIsValid`: the identity record proves the
/// files untouched since they were hashed; otherwise every file is hashed
/// again and, when nothing changed meanwhile, the identity record rewritten.
pub fn verified_record_is_valid(dir: &Path, revision: &str) -> bool {
    let Some((bytes, manifest)) = read_manifest(dir, revision) else {
        return false;
    };
    if manifest.version != 1 || manifest.revision != revision || manifest.files.is_empty() {
        return false;
    }
    let paths: Vec<String> = manifest.files.iter().map(|file| file.path.clone()).collect();
    if identity_matches(dir, revision, &bytes, &paths) {
        return true;
    }
    let Some(before) = identities(dir, &paths) else {
        return false;
    };
    for (file, identity) in manifest.files.iter().zip(&before) {
        let matches = identity.size == file.size
            && model_file(dir, &file.path).and_then(|path| sha256_file(&path)).is_some_and(|actual| actual.eq_ignore_ascii_case(&file.sha256));
        if !matches {
            return false;
        }
    }
    if identities(dir, &paths).as_ref() == Some(&before) {
        write_identity(dir, revision, &bytes, before);
    }
    true
}

/// `VLMDownloader.catalogSentinelIsValid`: a catalog model's record must list
/// exactly the audited weights.
pub fn catalog_record_is_valid(dir: &Path, repo: &Repo) -> bool {
    let Some(entry) = repo.audited_weights() else {
        return true;
    };
    let Some((_, manifest)) = read_manifest(dir, repo.revision) else {
        return false;
    };
    let weights: Vec<&VerifiedFile> = manifest.files.iter().filter(|file| file.path.ends_with(".safetensors")).collect();
    manifest.revision == entry.revision
        && weights.len() == entry.files.len()
        && entry.files.iter().all(|expected| weights.iter().any(|file| file.path == expected.path && file.sha256 == expected.sha256 && file.size == expected.size))
}

/// A complete, verified install of `repo` that needs no network. A record
/// that no longer verifies is removed, as the Swift engine does, so the next
/// fetch re-proves every file.
pub fn installed(repo: &Repo) -> bool {
    let Some(dir) = model_dir(repo) else {
        return false;
    };
    let record = verified_record_path(&dir, repo.revision);
    if !record.is_file() {
        return false;
    }
    if verified_record_is_valid(&dir, repo.revision) && catalog_record_is_valid(&dir, repo) {
        return true;
    }
    if crate::util::read_only::require_writable(&record).is_ok() {
        let _ = std::fs::remove_file(&record);
    }
    false
}

/// `DeepAnalyze.writeInstalledSentinel`: the empty marker the app's Deep
/// Analyze cards read.
pub fn mark_installed(repo: &Repo) -> Result<()> {
    let dir = model_dir(repo).context("no Hugging Face model folder")?;
    let marker = installed_marker_path(&dir);
    crate::util::read_only::require_writable(&marker)?;
    std::fs::create_dir_all(&dir)?;
    std::fs::write(&marker, b"")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Repo listing and install plan
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct RepoFile {
    pub path: String,
    pub size: i64,
    /// The LFS oid, which is the file's SHA-256; small files have none.
    pub sha256: Option<String>,
}

#[derive(Deserialize)]
struct TreeEntry {
    #[serde(rename = "type")]
    kind: String,
    path: String,
    size: Option<i64>,
    lfs: Option<TreeLfs>,
}

#[derive(Deserialize)]
struct TreeLfs {
    size: Option<i64>,
    oid: Option<String>,
}

/// The `api/models/<repo>/tree/<revision>?recursive=true` reply's files.
pub fn parse_listing(bytes: &[u8]) -> Result<Vec<RepoFile>> {
    let entries: Vec<TreeEntry> = serde_json::from_slice(bytes).context("the model file listing could not be read")?;
    Ok(entries
        .into_iter()
        .filter(|entry| entry.kind == "file")
        .map(|entry| RepoFile {
            size: entry.size.or_else(|| entry.lfs.as_ref().and_then(|lfs| lfs.size)).unwrap_or(0),
            sha256: entry.lfs.and_then(|lfs| lfs.oid),
            path: entry.path,
        })
        .collect())
}

/// `VLMDownloader.shouldDownload`.
fn should_download(file: &RepoFile) -> bool {
    let path = file.path.to_lowercase();
    !(path == "readme.md" || path == ".gitattributes" || path.ends_with(".png") || path.ends_with(".jpg"))
}

fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// What a prewarm still has to fetch, and everything the verified record will
/// attest once it has.
#[derive(Debug)]
pub struct Plan {
    pub repo: &'static Repo,
    pub dir: PathBuf,
    pub files: Vec<RepoFile>,
    pub fetch: Model,
}

/// `VLMDownloader.fetchRepo` from the listing on: catalog weights must match
/// the audit; files already on disk at the listed size whose SHA-256 matches
/// their LFS oid are kept. Blocking (hashes files).
pub fn plan(repo: &'static Repo, listing: Vec<RepoFile>) -> Result<Plan> {
    let dir = model_dir(repo).context("no Hugging Face model folder")?;
    crate::util::read_only::require_writable(&dir)?;
    if let Some(entry) = repo.audited_weights() {
        let weights: Vec<&RepoFile> = listing.iter().filter(|file| file.path.ends_with(".safetensors")).collect();
        let matches = weights.len() == entry.files.len()
            && entry.files.iter().all(|expected| {
                weights.iter().any(|file| file.path == expected.path && file.sha256.as_deref() == Some(expected.sha256.as_str()) && file.size == expected.size)
            });
        anyhow::ensure!(matches, "Model weights do not match the audited catalog.");
    }
    let files: Vec<RepoFile> = listing.into_iter().filter(should_download).collect();
    anyhow::ensure!(!files.is_empty(), "The model repository lists no files.");
    anyhow::ensure!(files.iter().all(|file| safe_relative(&file.path)), "The model repository lists an unsafe file path.");
    let mut fetch = Vec::new();
    for file in &files {
        let dest = model_file(&dir, &file.path).context("unsafe model file path")?;
        let on_disk = std::fs::symlink_metadata(&dest).ok().filter(|meta| meta.file_type().is_file()).map(|meta| meta.len());
        let kept = file.size > 0
            && on_disk == u64::try_from(file.size).ok()
            && file.sha256.as_deref().is_some_and(|expected| sha256_file(&dest).is_some_and(|actual| actual.eq_ignore_ascii_case(expected)));
        if !kept {
            fetch.push(FileEntry {
                url: format!("https://huggingface.co/{}/resolve/{}/{}", repo.repo, repo.revision, encode_path(&file.path)),
                dest,
                sha256: file.sha256.as_ref().map(|sha| sha.to_lowercase()),
                approx_bytes: u64::try_from(file.size).unwrap_or(0),
            });
        }
    }
    Ok(Plan { repo, dir, files, fetch: Model { id: repo.kind, display_name: repo.display_name, files: fetch } })
}

/// `VLMDownloader.writeVerifiedSentinel` plus the `.fileid-installed` marker:
/// hash every listed file, refuse a mismatch, and record identities when no
/// file changed while hashing. Blocking.
pub fn finish(plan: &Plan) -> Result<()> {
    let mut sorted: Vec<&RepoFile> = plan.files.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let paths: Vec<String> = sorted.iter().map(|file| file.path.clone()).collect();
    let before = identities(&plan.dir, &paths);
    let mut entries = Vec::with_capacity(sorted.len());
    for file in sorted {
        let path = model_file(&plan.dir, &file.path).context("unsafe model file path")?;
        let meta = std::fs::symlink_metadata(&path).with_context(|| format!("{} is missing", file.path))?;
        anyhow::ensure!(meta.file_type().is_file(), "{} is not a regular file", file.path);
        let size = i64::try_from(meta.len())?;
        anyhow::ensure!(file.size <= 0 || size == file.size, "{} has the wrong size", file.path);
        let actual = sha256_file(&path).with_context(|| format!("{} could not be read", file.path))?;
        if let Some(expected) = &file.sha256 {
            anyhow::ensure!(actual.eq_ignore_ascii_case(expected), "{} failed its SHA-256 check", file.path);
        }
        entries.push(VerifiedFile { path: file.path.clone(), sha256: actual, size });
    }
    let manifest = serde_json::to_vec(&VerifiedManifest { files: entries, revision: plan.repo.revision.to_string(), version: 1 })?;
    let after = identities(&plan.dir, &paths);
    write_atomic(&verified_record_path(&plan.dir, plan.repo.revision), &manifest)?;
    if let Some(before) = before.filter(|before| Some(before) == after.as_ref()) {
        write_identity(&plan.dir, plan.repo.revision, &manifest, before);
    }
    mark_installed(plan.repo)
}

#[cfg(test)]
#[path = "mlx_tests.rs"]
mod tests;
