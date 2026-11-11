use super::*;
use std::ffi::OsString;

const FIXTURE: &str = include_str!("../../../../../../shared/test-corpus/mlx-verified-install.json");

fn fixture() -> serde_json::Value {
    serde_json::from_str(FIXTURE).unwrap()
}

/// A temporary home and Hugging Face root; restores the environment on drop.
struct Scratch {
    dir: PathBuf,
    saved: Vec<(&'static str, Option<OsString>)>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let lock = crate::paths::TEST_ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = std::env::temp_dir().join(format!("fileid-mlx-{tag}-{}-{}", std::process::id(), uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("home")).unwrap();
        let keys = ["FILEID_HF_CACHE_ROOT", "HOME", "CFFIXED_USER_HOME", "APP_SANDBOX_CONTAINER_ID"];
        let saved = keys.iter().map(|key| (*key, std::env::var_os(key))).collect();
        unsafe {
            std::env::set_var("FILEID_HF_CACHE_ROOT", dir.join("hf"));
            std::env::set_var("HOME", dir.join("home"));
            std::env::remove_var("CFFIXED_USER_HOME");
            std::env::remove_var("APP_SANDBOX_CONTAINER_ID");
        }
        Scratch { dir, saved, _lock: lock }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        for (key, value) in &self.saved {
            match value {
                Some(value) => unsafe { std::env::set_var(key, value) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn test_repo(revision: &str, catalog: Option<&'static CatalogModel>) -> &'static Repo {
    Box::leak(Box::new(Repo {
        kind: "test_mlx_model",
        display_name: "Test model",
        repo: "test-org/Test-Model-4bit",
        revision: leak(revision.to_string()),
        approx_bytes: 4098,
        license: "Apache-2.0",
        ram_budget_gb: 3.0,
        vision: true,
        favorite: catalog.is_none(),
        catalog,
    }))
}

/// Writes the fixture's files into the repo folder and returns that folder.
fn write_fixture_files(repo: &Repo) -> PathBuf {
    let dir = model_dir(repo).unwrap();
    for file in fixture()["files"].as_array().unwrap() {
        let path = model_file(&dir, file["path"].as_str().unwrap()).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let bytes = match file["text"].as_str() {
            Some(text) => text.as_bytes().to_vec(),
            None => vec![file["byte"].as_u64().unwrap() as u8; file["count"].as_u64().unwrap() as usize],
        };
        std::fs::write(path, bytes).unwrap();
    }
    dir
}

fn fixture_listing() -> Vec<RepoFile> {
    let manifest: serde_json::Value = serde_json::from_str(fixture()["rustManifest"].as_str().unwrap()).unwrap();
    let mut listing: Vec<RepoFile> = manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| {
            let path = file["path"].as_str().unwrap().to_string();
            RepoFile {
                sha256: path.ends_with(".safetensors").then(|| file["sha256"].as_str().unwrap().to_string()),
                size: file["size"].as_i64().unwrap(),
                path,
            }
        })
        .collect();
    listing.push(RepoFile { path: "README.md".into(), size: 10, sha256: None });
    listing
}

#[cfg(target_os = "macos")]
#[test]
fn bundled_catalog_loads_like_the_swift_catalog() {
    let raw: serde_json::Value = serde_json::from_str(MAC_CATALOG_JSON).unwrap();
    let total = raw["entries"].as_array().unwrap().len();
    let entries = catalog_entries();
    assert_eq!(entries.len(), total - RUNTIME_INCOMPATIBLE.len());
    assert!(entries.iter().all(|entry| !RUNTIME_INCOMPATIBLE.contains(&entry.repo.as_str())));
}

#[test]
fn one_invalid_entry_rejects_the_catalog() {
    let entry = serde_json::json!({
        "repo": "mlx-community/Some-Model-4bit", "revision": "a".repeat(40), "license": "apache-2.0",
        "architecture": "qwen3", "role": "text", "weightBytes": 10,
        "bases": [{"repo": "org/Some-Model", "revision": "b".repeat(40), "license": "apache-2.0"}],
        "files": [{"path": "model.safetensors", "sha256": "c".repeat(64), "size": 10}],
        "quantization": {"bits": 4, "groupSize": 64, "mode": "affine"}
    });
    let document = |entries: Vec<serde_json::Value>| {
        serde_json::json!({"formatVersion": 1, "auditedAt": "2026-10-10", "runtime": RUNTIME, "entries": entries}).to_string()
    };
    assert_eq!(parse_catalog(&document(vec![entry.clone()])).len(), 1);
    let mut bad = entry.clone();
    bad["files"][0]["sha256"] = serde_json::json!("C".repeat(64));
    assert!(parse_catalog(&document(vec![entry.clone(), bad])).is_empty());
    let mut wrong_total = entry.clone();
    wrong_total["weightBytes"] = serde_json::json!(11);
    assert!(parse_catalog(&document(vec![wrong_total])).is_empty());
    let mut traversal = entry;
    traversal["files"][0]["path"] = serde_json::json!("../model.safetensors");
    assert!(parse_catalog(&document(vec![traversal])).is_empty());
}

fn swift_set(source: &str, name: &str) -> HashSet<String> {
    let start = source.find(&format!("{name}: Set<String> = [")).unwrap_or_else(|| panic!("{name} not found"));
    let rest = &source[start..];
    let body = &rest[rest.find('[').unwrap() + 1..rest.find(']').unwrap()];
    body.split(',').map(|item| item.trim().trim_matches('"').to_string()).filter(|item| !item.is_empty()).collect()
}

#[test]
fn architectures_and_exclusions_match_mlx_model_catalog_swift() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apple/shared/Sources/FileIDShared/MLXModelCatalog.swift");
    let source = std::fs::read_to_string(path).unwrap();
    let set = |items: &[&str]| items.iter().map(|item| item.to_string()).collect::<HashSet<_>>();
    assert_eq!(swift_set(&source, "textArchitectures"), set(TEXT_ARCHITECTURES));
    assert_eq!(swift_set(&source, "visionArchitectures"), set(VISION_ARCHITECTURES));
    assert_eq!(swift_set(&source, "runtimeIncompatibleRepos"), set(&RUNTIME_INCOMPATIBLE));
    assert!(source.contains(&format!("document.runtime == \"{RUNTIME}\"")));
}

#[test]
fn favorites_follow_the_manifest_in_swift_order() {
    let pins = &super::super::manifest::manifest().vlm_repos;
    let listed: Vec<&Repo> = listed().collect();
    for (index, (kind, name, budget)) in FAVORITES.iter().enumerate() {
        let pin = pins.iter().find(|pin| pin.kind == *kind).unwrap();
        let repo = lookup(kind).unwrap();
        assert_eq!((repo.repo, repo.revision, repo.license), (pin.repo.as_str(), pin.revision.as_str(), pin.license.as_str()));
        assert_eq!((repo.display_name, repo.ram_budget_gb, repo.approx_bytes), (*name, *budget, pin.approx_bytes));
        assert_eq!(listed[index].kind, *kind);
    }
    let kinds: HashSet<&str> = listed.iter().map(|repo| repo.kind).collect();
    let repos: HashSet<&str> = listed.iter().map(|repo| repo.repo).collect();
    assert_eq!(kinds.len(), listed.len());
    assert_eq!(repos.len(), listed.len(), "a favourite's catalog copy is not listed twice");
    if !crate::paths::APP_HOSTED {
        assert!(repo("qwen3_vl_4b").is_none(), "only the app-hosted engine serves MLX");
    }
}

#[test]
fn list_items_follow_the_swift_engine() {
    let _scratch = Scratch::new("list");
    let items = catalog_items(None, 16.0, &HashMap::new());
    assert_eq!(items.len(), listed().count());
    let first = &items[0];
    assert_eq!((first.id.as_str(), first.name.as_str(), first.format.as_str()), ("qwen2_5_vl_7b", "Qwen2.5-VL 7B (4-bit)", "mlx"));
    assert_eq!((first.status.as_str(), first.role.as_str(), first.tier.as_str()), ("curated", "vision", "medium"));
    assert_eq!(first.family, "Qwen2.5-VL-7B-Instruct-4bit");
    assert!(first.fits && first.runnable && !first.installed && first.parameters_billion.is_none());
    let gemma = items.iter().find(|item| item.id == "gemma_3_4b").unwrap();
    assert!(gemma.terms_required);
    assert_eq!((gemma.license.as_str(), gemma.quantization.as_str(), gemma.upstream.as_str()), ("Gemma", "4bit", "mlx-community/gemma-3-4b-it-qat-4bit"));
    let paligemma = items.iter().find(|item| item.id == "paligemma_3b").unwrap();
    assert_eq!(paligemma.quantization, "8bit");
    assert!(!items.iter().find(|item| item.id == "mistral_small_3_2").unwrap().fits);
    for item in items.iter().filter(|item| item.status == "candidate") {
        let entry = catalog_entries().iter().find(|entry| format!("hf:{}", entry.repo) == item.id).unwrap();
        let budget = entry.weight_bytes as f64 / 1_073_741_824.0 * 1.5 + 2.0;
        assert_eq!(item.estimated_resident_gb.to_bits(), budget.to_bits());
        assert_eq!(item.role, entry.role);
        assert_eq!(item.upstream, entry.bases[0].repo);
        assert_eq!(item.quantization, format!("{}bit", entry.quantization.bits));
        assert_eq!(item.license, "Apache-2.0");
        assert_eq!(item.name, entry.repo.rsplit('/').next().unwrap().replace('-', " "));
    }
    assert!(catalog_items(Some("text"), 16.0, &HashMap::new()).iter().all(|item| item.role == "text"));
    assert!(catalog_items(Some("embedding"), 16.0, &HashMap::new()).is_empty());
}

#[test]
fn models_root_follows_model_cache_paths() {
    let scratch = Scratch::new("root");
    assert_eq!(models_root(), Some(scratch.dir.join("hf/models")));
    unsafe { std::env::set_var("FILEID_HF_CACHE_ROOT", "relative/path") };
    assert_eq!(models_root(), Some(scratch.dir.join("home/Documents/huggingface/models")));
    unsafe { std::env::set_var("CFFIXED_USER_HOME", scratch.dir.join("cf")) };
    assert_eq!(models_root(), Some(scratch.dir.join("cf/Documents/huggingface/models")));
    unsafe { std::env::set_var("APP_SANDBOX_CONTAINER_ID", "com.fileid.app") };
    assert_eq!(models_root(), Some(scratch.dir.join("cf/Library/Application Support/FileID/Models/huggingface/models")));
    let repo = lookup("qwen3_vl_4b").unwrap();
    assert!(model_dir(repo).unwrap().ends_with("models/lmstudio-community/Qwen3-VL-4B-Instruct-MLX-4bit"));
}

#[test]
fn listing_reads_the_hugging_face_tree() {
    let reply = br#"[
        {"type":"directory","oid":"x","size":0,"path":"sub"},
        {"type":"file","oid":"blob","size":2,"path":"config.json"},
        {"type":"file","oid":"blob","size":135,"path":"model.safetensors","lfs":{"oid":"abc","size":4096,"pointerSize":135}},
        {"type":"file","path":"sub/x.bin","lfs":{"oid":"def","size":9}}
    ]"#;
    let files = parse_listing(reply).unwrap();
    assert_eq!(
        files,
        vec![
            RepoFile { path: "config.json".into(), size: 2, sha256: None },
            RepoFile { path: "model.safetensors".into(), size: 135, sha256: Some("abc".into()) },
            RepoFile { path: "sub/x.bin".into(), size: 9, sha256: Some("def".into()) },
        ]
    );
    assert!(parse_listing(b"{\"error\":\"nope\"}").is_err());
}

#[test]
fn plan_keeps_verified_files_and_finish_writes_the_rust_record() {
    let _scratch = Scratch::new("plan");
    let fixture = fixture();
    let revision = fixture["revision"].as_str().unwrap();
    let repo = test_repo(revision, None);
    let dir = write_fixture_files(repo);
    std::fs::write(dir.join("config.json"), b"{ }x").unwrap();

    let plan = plan(repo, fixture_listing()).unwrap();
    assert_eq!(plan.files.len(), 2, "README.md is not downloaded");
    assert_eq!(plan.fetch.files.len(), 1, "the verified weights are kept; the unpinned config is fetched");
    let config = &plan.fetch.files[0];
    assert_eq!(config.url, format!("https://huggingface.co/test-org/Test-Model-4bit/resolve/{revision}/config.json"));
    assert_eq!((config.sha256.as_deref(), config.approx_bytes), (None, 2));
    assert!(finish(&plan).is_err(), "a wrong-size file is never attested");

    std::fs::write(dir.join("config.json"), b"{}").unwrap();
    finish(&plan).unwrap();
    let written = std::fs::read(verified_record_path(&dir, revision)).unwrap();
    assert_eq!(String::from_utf8(written).unwrap(), fixture["rustManifest"].as_str().unwrap());
    let identity: serde_json::Value = serde_json::from_slice(&std::fs::read(identity_record_path(&dir, revision)).unwrap()).unwrap();
    assert_eq!(identity["version"], 2);
    assert_eq!(identity["manifestSHA256"], fixture["rustManifestSHA256"]);
    assert_eq!(identity["files"].as_array().unwrap().len(), 2);
    assert!(installed_marker_path(&dir).is_file());
    assert!(installed(repo));
}

#[test]
fn swift_record_is_accepted_and_gains_an_identity_record() {
    let _scratch = Scratch::new("swift");
    let fixture = fixture();
    let revision = fixture["revision"].as_str().unwrap();
    let repo = test_repo(revision, None);
    let dir = write_fixture_files(repo);
    std::fs::write(verified_record_path(&dir, revision), fixture["swiftManifest"].as_str().unwrap()).unwrap();

    assert!(installed(repo));
    let identity: serde_json::Value = serde_json::from_slice(&std::fs::read(identity_record_path(&dir, revision)).unwrap()).unwrap();
    assert_eq!(identity["manifestSHA256"], fixture["swiftManifestSHA256"]);
    let paths = vec!["config.json".to_string(), "weights/model.safetensors".to_string()];
    let manifest = std::fs::read(verified_record_path(&dir, revision)).unwrap();
    assert!(identity_matches(&dir, revision, &manifest, &paths));
    assert!(installed(repo));

    std::fs::write(dir.join("weights/model.safetensors"), vec![8u8; 4096]).unwrap();
    assert!(!identity_matches(&dir, revision, &manifest, &paths));
    assert!(!installed(repo), "a same-size change fails the hash");
    assert!(!verified_record_path(&dir, revision).exists(), "a record that fails is removed");
}

#[test]
fn wrong_revision_or_missing_file_is_not_installed() {
    let _scratch = Scratch::new("revision");
    let fixture = fixture();
    let revision = fixture["revision"].as_str().unwrap();
    let repo = test_repo(revision, None);
    let dir = write_fixture_files(repo);
    let other = "f".repeat(40);
    std::fs::write(verified_record_path(&dir, &other), fixture["swiftManifest"].as_str().unwrap()).unwrap();
    assert!(!verified_record_is_valid(&dir, &other));
    std::fs::write(verified_record_path(&dir, revision), fixture["swiftManifest"].as_str().unwrap()).unwrap();
    std::fs::remove_file(dir.join("config.json")).unwrap();
    assert!(!installed(repo));
}

#[test]
fn catalog_models_must_carry_the_audited_weights() {
    let _scratch = Scratch::new("audit");
    let fixture = fixture();
    let revision = fixture["revision"].as_str().unwrap().to_string();
    let weights_sha = fixture_listing()[1].sha256.clone().unwrap();
    let entry: &'static CatalogModel = Box::leak(Box::new(CatalogModel {
        repo: "test-org/Test-Model-4bit".into(),
        revision: revision.clone(),
        license: "apache-2.0".into(),
        architecture: "qwen3".into(),
        role: "text".into(),
        weight_bytes: 4096,
        bases: vec![],
        files: vec![Weight { path: "weights/model.safetensors".into(), sha256: weights_sha, size: 4096 }],
        quantization: Quantization { bits: 4, group_size: 64, mode: "affine".into() },
    }));
    let repo = test_repo(&revision, Some(entry));
    let dir = write_fixture_files(repo);
    let plan_ok = plan(repo, fixture_listing()).unwrap();
    finish(&plan_ok).unwrap();
    assert!(installed(repo));

    let mut listing = fixture_listing();
    listing[1].sha256 = Some("0".repeat(64));
    assert!(plan(repo, listing).unwrap_err().to_string().contains("audited catalog"));

    let altered = fixture["rustManifest"].as_str().unwrap().replace("weights/model.safetensors", "weights/other.safetensors");
    std::fs::write(verified_record_path(&dir, &revision), altered).unwrap();
    assert!(!catalog_record_is_valid(&dir, repo));
}

#[test]
fn unsafe_listing_paths_are_refused() {
    let _scratch = Scratch::new("unsafe");
    let repo = test_repo(&"a".repeat(40), None);
    for path in ["../escape.json", "/etc/passwd", "a/../../b"] {
        let listing = vec![RepoFile { path: path.into(), size: 1, sha256: None }];
        assert!(plan(repo, listing).is_err(), "{path}");
    }
    assert!(plan(repo, vec![RepoFile { path: "README.md".into(), size: 1, sha256: None }]).is_err(), "nothing to download");
    assert_eq!(encode_path("sub dir/a+b.json"), "sub%20dir/a%2Bb.json");
}

#[test]
fn license_marker_is_the_swift_acceptance_record() {
    let scratch = Scratch::new("license");
    let gemma = lookup("gemma_3_4b").unwrap();
    assert!(terms_accepted(lookup("qwen3_vl_4b").unwrap()));
    assert!(!terms_accepted(gemma));
    let folder = scratch.dir.join("home/Library/Application Support/FileID/LicenseAcceptances");
    std::fs::create_dir_all(&folder).unwrap();
    let marker = folder.join("Gemma-2026-07-16.accepted");
    std::fs::write(&marker, "policy=Gemma\nreviewedAt=2026-07-16\nterms=https://ai.google.dev/gemma/terms\n").unwrap();
    assert!(terms_accepted(gemma));
    std::fs::write(&marker, "policy=Gemma\n").unwrap();
    assert!(!terms_accepted(gemma));
    assert_eq!(license_name(gemma), "Gemma Terms of Use");
}

#[test]
fn swift_reviewed_date_matches_the_manifest_policy() {
    let source = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apple/shared/Sources/FileIDShared/ModelLicenseAcceptance.swift"),
    )
    .unwrap();
    assert!(source.contains(&format!("reviewedAt = \"{LICENSE_REVIEWED_AT}\"")));
    assert_eq!(super::super::manifest::manifest().license_policies["Gemma"].reviewed_at, LICENSE_REVIEWED_AT);
}
