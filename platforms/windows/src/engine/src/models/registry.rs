// Maps a `model_kind` string (sent by the welcome sheet / Settings) to a resolved `Model`: URLs, destinations, SHA256s
// and the sentinel file the engine drops once every artifact has landed.
// Adding a curated model: add its artifacts to shared/models/manifest.json and a `bundle` arm naming their install
// paths.

use std::path::{Path, PathBuf};

use crate::paths;

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub url: String,
    pub dest: PathBuf,
    pub sha256: Option<String>,
    pub approx_bytes: u64,
}

pub type ModelFile = FileEntry;

/// A sidecar the model works without (RAM++ falls back to one global threshold): its absence or a failed download
/// never makes an install incomplete.
pub fn is_optional_artifact(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()) == Some("ram_plus_thresholds.txt")
}

#[derive(Debug, Clone)]
pub struct Model {
    pub id: &'static str,
    pub display_name: &'static str,
    pub files: Vec<FileEntry>,
}

#[derive(Debug)]
pub enum LookupResult {
    Found(Model),
    Unknown,
}

pub(crate) fn vlm_dir_name(model_kind: &str) -> Option<&'static str> {
    match model_kind {
        "qwen3_vl_4b" | "qwen3-vl-4b" => Some("qwen3-vl-4b"),
        "qwen3_vl_8b" | "qwen3-vl-8b" => Some("qwen3-vl-8b"),
        "qwen2_5_vl_7b" | "qwen2.5-vl-7b" => Some("qwen2.5-vl-7b"),
        "gemma_3_4b" | "gemma-3-4b" => Some("gemma-3-4b"),
        "mistral_small_3_2" | "mistral-small-3.2" => Some("mistral-small-3.2"),
        _ => super::catalog::directory(model_kind),
    }
}

struct Bundle {
    id: &'static str,
    display_name: &'static str,
    files: &'static [(&'static str, &'static str)],
}

/// Each manifest artifact id paired with its destination under the models root; URLs, SHA256 pins, sizes and licenses
/// come only from `shared/models/manifest.json`.
fn bundle(model_kind: &str) -> Option<Bundle> {
    if paths::APP_HOSTED && let Some(bundle) = mac_app_bundle(model_kind) {
        return Some(bundle);
    }
    let (id, display_name, files): (_, _, &'static [_]) = match model_kind {
        // The id "arcface" and its aliases keep the C# install slot, sentinel and pre-scan gate unchanged.
        "arcface" | "arcface_default" | "arcface_iresnet50" | "arcface_mobileface" | "arcface_scrfd" | "yunet_sface" => (
            "arcface",
            "Face detection + recognition",
            &[
                ("yunet_detector", "yunet/face_detection_yunet_2023mar.onnx"),
                ("sface_embedder", "sface/face_recognition_sface_2021dec.onnx"),
            ],
        ),
        // The `mobileclip_s2` id and paths predate the switch from MobileCLIP and stay to avoid churning the sentinel.
        "mobileclip_s2" | "mobileclip" => (
            "mobileclip_s2",
            "CLIP ViT-B/32 image encoder",
            &[("clip_vitb32_image", "mobileclip/mobileclip_s2_image.onnx")],
        ),
        "clip_text" => (
            "clip_text",
            "CLIP text encoder",
            &[
                ("clip_vitb32_text", "clip_text/clip_text.onnx"),
                ("clip_bpe_vocab", "clip_text/vocab.json"),
                ("clip_bpe_merges", "clip_text/merges.txt"),
            ],
        ),
        "ram_plus" | "ram-plus" => (
            "ram_plus",
            "RAM++ image tagger",
            &[
                ("ram_plus_model", "ram_plus/ram_plus.onnx"),
                ("ram_plus_tags", "ram_plus/ram_plus_tags.txt"),
                ("ram_plus_thresholds", "ram_plus/ram_plus_thresholds.txt"),
            ],
        ),
        "mistral-small-3.2" => (
            "mistral_small_3_2",
            "Mistral-Small 3.2",
            &[
                ("mistral_small_gguf", "vlm/mistral-small-3.2/model.gguf"),
                ("mistral_small_mmproj", "vlm/mistral-small-3.2/mmproj.gguf"),
            ],
        ),
        "qwen3-vl-4b" => (
            "qwen3_vl_4b",
            "Qwen3-VL 4B",
            &[("qwen3vl4b_gguf", "vlm/qwen3-vl-4b/model.gguf"), ("qwen3vl4b_mmproj", "vlm/qwen3-vl-4b/mmproj.gguf")],
        ),
        "qwen3-vl-8b" => (
            "qwen3_vl_8b",
            "Qwen3-VL 8B",
            &[("qwen3vl8b_gguf", "vlm/qwen3-vl-8b/model.gguf"), ("qwen3vl8b_mmproj", "vlm/qwen3-vl-8b/mmproj.gguf")],
        ),
        "qwen2.5-vl-7b" => (
            "qwen2_5_vl_7b",
            "Qwen2.5-VL 7B",
            &[("qwen25vl_gguf", "vlm/qwen2.5-vl-7b/model.gguf"), ("qwen25vl_mmproj", "vlm/qwen2.5-vl-7b/mmproj.gguf")],
        ),
        "gemma-3-4b" => (
            "gemma_3_4b",
            "Gemma 3 4B",
            &[("gemma3_gguf", "vlm/gemma-3-4b/model.gguf"), ("gemma3_mmproj", "vlm/gemma-3-4b/mmproj.gguf")],
        ),
        "face_mesh" => ("face_mesh", "Face landmarks for eye openness", &[("face_mesh_landmarks", "face_mesh/face_landmarks.onnx")]),
        "bge_text" | "bge_small_en_v1_5" | "bge_small" => (
            "bge_text",
            "BGE-small text embeddings",
            &[("bge_model", "bge_text/bge_small.onnx"), ("bge_vocab", "bge_text/vocab.txt")],
        ),
        _ => return None,
    };
    Some(Bundle { id, display_name, files })
}

/// The Mac-hosted engine installs where the app's own installers (`ModelInstaller`, `CLIPModelInstaller`) put things:
/// faces need only SFace (Vision detects them), and SFace and CLIP use the app's file names.
fn mac_app_bundle(model_kind: &str) -> Option<Bundle> {
    let (id, display_name, files): (_, _, &'static [_]) = match model_kind {
        "arcface" | "arcface_default" | "arcface_iresnet50" | "arcface_mobileface" | "arcface_scrfd" | "yunet_sface" | "sface" => {
            ("arcface", "Face recognition", &[("sface_embedder", "face_recognition_sface_2021dec.onnx")])
        }
        "mobileclip_s2" | "mobileclip" => (
            "mobileclip_s2",
            "CLIP ViT-B/32 image encoder",
            &[("clip_vitb32_image", "mobileclip_image/clip_vitb32_image.onnx")],
        ),
        _ => return None,
    };
    Some(Bundle { id, display_name, files })
}

/// A curated manifest bundle, else a runnable catalog model. A manifest that fails validation resolves nothing.
pub fn lookup_full(model_kind: &str) -> LookupResult {
    let models_root = match paths::models_dir() {
        Ok(p) => p,
        Err(_) => return LookupResult::Unknown,
    };
    let model = match bundle(vlm_dir_name(model_kind).unwrap_or(model_kind)) {
        Some(bundle) => manifest_model(&models_root, &bundle),
        None if paths::APP_HOSTED => None,
        None => catalog_model(&models_root, model_kind),
    };
    model.map_or(LookupResult::Unknown, LookupResult::Found)
}

fn manifest_model(models_root: &Path, bundle: &Bundle) -> Option<Model> {
    let files = bundle
        .files
        .iter()
        .map(|&(artifact_id, dest)| {
            let platform = if paths::APP_HOSTED { "macos" } else { "windows" };
            let artifact = super::manifest::artifact(artifact_id).filter(|artifact| artifact.platforms.iter().any(|p| p == platform))?;
            Some(FileEntry {
                url: artifact.url(),
                dest: dest.split('/').fold(models_root.to_path_buf(), |path, part| path.join(part)),
                sha256: Some(artifact.sha256.clone()),
                approx_bytes: artifact.approx_bytes,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Model { id: bundle.id, display_name: bundle.display_name, files })
}

/// Laid out like the curated VLMs so discovery, provenance and the resident server treat it identically.
fn catalog_model(models_root: &Path, model_kind: &str) -> Option<Model> {
    let entry = super::catalog::runnable(model_kind)?;
    let dir = models_root.join("vlm").join(super::catalog::directory(model_kind)?);
    let files = entry
        .files
        .iter()
        .filter_map(|file| {
            let name = match file.role.as_str() {
                "model" => "model.gguf",
                "projector" => "mmproj.gguf",
                _ => return None,
            };
            Some(FileEntry { url: file.url.clone(), dest: dir.join(name), sha256: file.sha256.clone(), approx_bytes: file.bytes.unwrap_or(0) })
        })
        .collect();
    Some(Model { id: entry.id.as_str(), display_name: entry.name.as_str(), files })
}

/// Token for the EXACT pinned artifacts, from every file's URL + sha256 (so a bumped `resolve/<rev>/` URL on an
/// unpinned entry still invalidates). Deterministic FNV-1a, stable across launches and machines.
fn revision_token(model: &Model) -> String {
    fn feed(h: u64, bytes: &[u8]) -> u64 {
        let mut h = h;
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }
    let mut h: u64 = 0xcbf29ce484222325;
    for f in &model.files {
        h = feed(h, f.url.as_bytes());
        h = feed(h, b"\0");
        h = feed(h, f.sha256.as_deref().unwrap_or("").as_bytes());
        h = feed(h, b"\0");
    }
    format!("{h:016x}")
}

/// Keyed by [`revision_token`], so a pin bump yields a new name and forces a re-fetch; a legacy bare `{id}.installed`
/// is renamed by [`migrate_legacy_sentinel`] on first read.
pub fn sentinel_path(model: &Model) -> Option<PathBuf> {
    let dir = paths::models_dir().ok()?.join(".sentinels");
    let keyed = dir.join(format!("{}-{}.installed", model.id, revision_token(model)));
    migrate_legacy_sentinel(&dir, model, &keyed);
    Some(keyed)
}

pub fn installation_complete(model: &Model) -> bool {
    let Some(sentinel) = sentinel_path(model) else {
        return false;
    };
    installation_complete_with_sentinel(model, &sentinel) || verified_without_sentinel(model)
}

/// macOS: the Swift app installs models without this engine's sentinel, some under other names
/// (`paths::SWIFT_MODEL_LAYOUT`); every file matching its pinned SHA-256 proves the install. Hashes are cached per
/// (size, mtime).
#[cfg(target_os = "macos")]
fn verified_without_sentinel(model: &Model) -> bool {
    !model.files.is_empty()
        && model.files.iter().filter(|file| !is_optional_artifact(&file.dest)).all(|file| {
            !is_zip_entry(file)
                && file
                    .sha256
                    .as_deref()
                    .is_some_and(|expected| cached_sha256_matches(&paths::installed_model_file(&file.dest), expected))
        })
}

#[cfg(not(target_os = "macos"))]
fn verified_without_sentinel(_model: &Model) -> bool {
    false
}

#[cfg(target_os = "macos")]
fn cached_sha256_matches(path: &Path, expected: &str) -> bool {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    type Identity = (PathBuf, u64, Option<std::time::SystemTime>);
    static VERIFIED: OnceLock<Mutex<HashMap<Identity, String>>> = OnceLock::new();

    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    let identity = (path.to_path_buf(), metadata.len(), metadata.modified().ok());
    let cache = VERIFIED.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(actual) = cache.lock().ok().and_then(|cache| cache.get(&identity).cloned()) {
        return actual.eq_ignore_ascii_case(expected);
    }
    let Some(actual) = file_sha256(path) else {
        return false;
    };
    let matches = actual.eq_ignore_ascii_case(expected);
    if let Ok(mut cache) = cache.lock() {
        cache.insert(identity, actual);
    }
    matches
}

pub(crate) fn installation_complete_with_sentinel(model: &Model, sentinel: &Path) -> bool {
    if !sentinel.is_file() || !model.files.iter().filter(|file| !is_optional_artifact(&file.dest)).all(installed_artifact_is_plausible) {
        return false;
    }
    if model.files.iter().any(is_zip_entry) {
        let Some(expected) = installation_attestation(model) else {
            return false;
        };
        return std::fs::read_to_string(sentinel).is_ok_and(|actual| actual == expected);
    }
    true
}

pub fn installation_attestation(model: &Model) -> Option<String> {
    use std::fmt::Write as _;

    let mut attestation = format!("v2:{}:{}\n", model.id, revision_token(model));
    for file in model.files.iter().filter(|file| is_zip_entry(file)) {
        let root = file.dest.parent()?;
        let archive = file.dest.file_name()?.to_string_lossy();
        for path in required_runtime_artifacts(file)? {
            let relative = path.strip_prefix(root).ok()?.to_string_lossy().replace('\\', "/");
            let hash = file_sha256(&path)?;
            writeln!(attestation, "{archive}\t{relative}\t{hash}").ok()?;
        }
    }
    Some(attestation)
}

fn is_zip_entry(file: &FileEntry) -> bool {
    file.dest.extension().and_then(|ext| ext.to_str()) == Some("zip")
}

fn runtime_required_names(name: &str) -> Option<&'static [&'static [&'static str]]> {
    match name {
        "llama-runtime.zip" => Some(&[
            &["llama-server.exe", "llama-server"],
            &["llama-mtmd-cli.exe", "llama-mtmd-cli"],
            &["mtmd.dll", "libmtmd.so", "libmtmd.dylib"],
        ]),
        "cudart.zip" => Some(&[&["cudart64_12.dll"], &["cublas64_12.dll"]]),
        _ => None,
    }
}

fn required_runtime_artifacts(file: &FileEntry) -> Option<Vec<PathBuf>> {
    let root = file.dest.parent()?;
    let name = file.dest.file_name()?.to_str()?;
    let required = runtime_required_names(name)?;
    let mut entries = walkdir::WalkDir::new(root)
        .follow_links(false)
        .max_depth(8)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| entry.metadata().is_ok_and(|metadata| metadata.len() > 0))
        .map(|entry| entry.into_path())
        .collect::<Vec<_>>();
    entries.sort();
    required
        .iter()
        .map(|alternatives| {
            entries.iter().find(|path| {
                path.file_name().is_some_and(|name| {
                    alternatives
                        .iter()
                        .any(|candidate| name.to_string_lossy().eq_ignore_ascii_case(candidate))
                })
            }).cloned()
        })
        .collect()
}

fn installed_artifact_is_plausible(file: &FileEntry) -> bool {
    if file.dest.extension().and_then(|ext| ext.to_str()) == Some("zip") {
        return required_runtime_artifacts(file).is_some();
    }
    let plausible_size = std::fs::metadata(&file.dest).is_ok_and(|metadata| {
        let floor = (file.approx_bytes / 4).max(1);
        metadata.is_file() && metadata.len() >= floor
    });
    plausible_size
        && file
            .sha256
            .as_deref()
            .is_some_and(|expected| file_sha256_matches(&file.dest, expected))
}

fn migrate_legacy_sentinel(dir: &Path, model: &Model, keyed: &Path) {
    if keyed.exists() {
        return;
    }
    let legacy = dir.join(format!("{}.installed", model.id));
    if !legacy.exists() {
        return;
    }
    // A bare legacy marker carries no revision token, so re-check the artifacts against the current pins (approx_bytes
    // is only an estimate) before vouching.
    let pins_valid = model.files.iter().all(|f| match f.sha256.as_deref() {
        Some(expected) => file_sha256_matches(&f.dest, expected),
        None => f.dest.exists(),
    });
    if pins_valid {
        let _ = std::fs::rename(&legacy, keyed);
    } else {
        // Stale or partial pre-bump install: drop the marker so the gate falls through to a clean re-fetch (and the C#
        // SentinelExists check stops reporting it installed).
        let _ = std::fs::remove_file(&legacy);
    }
}

/// Streams the file so a multi-GB weight isn't read into RAM.
fn file_sha256_matches(path: &Path, expected: &str) -> bool {
    file_sha256(path).is_some_and(|actual| actual.eq_ignore_ascii_case(expected))
}

fn file_sha256(path: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        match std::io::Read::read(&mut file, &mut buffer) {
            Ok(0) => break,
            Ok(count) => hasher.update(&buffer[..count]),
            Err(_) => return None,
        }
    }
    Some(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest as _;

    /// Every artifact URL must be on huggingface.co, the engine's only egress; the llama runtimes are intentionally not
    /// listed (GitHub, CI-allowlisted).
    #[test]
    fn all_model_urls_are_huggingface() {
        let kinds = [
            "ram_plus", "mobileclip_s2", "clip_text", "bge_text", "arcface", "face_mesh",
            "qwen2_5_vl_7b", "qwen3_vl_4b", "qwen3_vl_8b",
            "gemma_3_4b", "mistral_small_3_2",
        ];
        for kind in kinds {
            if let LookupResult::Found(m) = lookup_full(kind) {
                for f in &m.files {
                    assert!(
                        f.url.starts_with("https://huggingface.co/"),
                        "{kind} URL not on huggingface.co: {}",
                        f.url
                    );
                }
            }
        }
    }

    #[test]
    fn face_aliases_resolve_to_one_model() {
        for alias in ["arcface", "arcface_default", "yunet_sface"] {
            match lookup_full(alias) {
                LookupResult::Found(m) => assert_eq!(m.id, "arcface", "alias {alias}"),
                LookupResult::Unknown => panic!("face alias {alias} did not resolve"),
            }
        }
    }

    /// Both spellings of every VLM kind resolve to the same bundle under the dotted dir the installer writes.
    #[test]
    #[cfg_attr(all(target_os = "macos", feature = "apple-adapters"), ignore = "the Mac app registers MLX models, not GGUF bundles")]
    fn vlm_kind_aliases_and_dirs_agree() {
        for (snake, dotted) in [
            ("mistral_small_3_2", "mistral-small-3.2"),
            ("qwen2_5_vl_7b", "qwen2.5-vl-7b"),
            ("qwen3_vl_4b", "qwen3-vl-4b"),
            ("qwen3_vl_8b", "qwen3-vl-8b"),
            ("gemma_3_4b", "gemma-3-4b"),
        ] {
            let LookupResult::Found(a) = lookup_full(snake) else {
                panic!("{snake} did not resolve")
            };
            let LookupResult::Found(b) = lookup_full(dotted) else {
                panic!("{dotted} did not resolve")
            };
            assert_eq!(a.id, b.id, "alias pair {snake}/{dotted} must be one model");
            for f in &a.files {
                let dir = f
                    .dest
                    .parent()
                    .and_then(|p| p.file_name())
                    .expect("vlm dest has a parent dir")
                    .to_string_lossy()
                    .into_owned();
                assert_eq!(dir, dotted, "{snake} dest not under vlm/{dotted}: {}", f.dest.display());
            }
        }
    }

    #[test]
    fn unknown_kind_is_unknown() {
        assert!(matches!(
            lookup_full("definitely_not_a_model_kind"),
            LookupResult::Unknown
        ));
    }

    #[test]
    fn installed_artifact_rejects_missing_zero_length_and_truncated_files() {
        let path = std::env::temp_dir().join(format!(
            "fileid-artifact-status-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let file = FileEntry {
            url: "https://huggingface.co/test".into(),
            dest: path.clone(),
            sha256: Some(hex::encode(sha2::Sha256::digest([0u8; 100]))),
            approx_bytes: 400,
        };
        assert!(!installed_artifact_is_plausible(&file));
        std::fs::write(&path, []).unwrap();
        assert!(!installed_artifact_is_plausible(&file));
        std::fs::write(&path, vec![0u8; 99]).unwrap();
        assert!(!installed_artifact_is_plausible(&file));
        std::fs::write(&path, vec![0u8; 100]).unwrap();
        assert!(installed_artifact_is_plausible(&file));
        std::fs::write(&path, vec![1u8; 100]).unwrap();
        assert!(!installed_artifact_is_plausible(&file));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_missing_optional_sidecar_does_not_make_an_install_incomplete() {
        let root = std::env::temp_dir().join(format!("fileid-optional-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let entry = |name: &str, bytes: &[u8]| FileEntry {
            url: format!("https://huggingface.co/test/{name}"),
            dest: root.join(name),
            sha256: Some(hex::encode(sha2::Sha256::digest(bytes))),
            approx_bytes: bytes.len() as u64,
        };
        let model = Model {
            id: "test_optional",
            display_name: "Test optional",
            files: vec![entry("model.onnx", b"weights"), entry("ram_plus_thresholds.txt", b"0.5")],
        };
        std::fs::write(root.join("model.onnx"), b"weights").unwrap();
        let sentinel = root.join("done.installed");
        std::fs::write(&sentinel, b"").unwrap();
        assert!(installation_complete_with_sentinel(&model, &sentinel));
        std::fs::remove_file(root.join("model.onnx")).unwrap();
        assert!(!installation_complete_with_sentinel(&model, &sentinel), "a required file is still required");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_attestation_detects_same_size_corruption_and_missing_leaf() {
        let root = std::env::temp_dir().join(format!(
            "fileid-runtime-attestation-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let server = root.join("llama-server");
        let mtmd_cli = root.join("llama-mtmd-cli");
        let mtmd = root.join("libmtmd.dylib");
        std::fs::write(&server, b"server-a").unwrap();
        std::fs::write(&mtmd_cli, b"cli").unwrap();
        std::fs::write(&mtmd, b"library").unwrap();
        let model = Model {
            id: "test_runtime",
            display_name: "Test runtime",
            files: vec![FileEntry {
                url: "https://huggingface.co/test/llama-runtime.zip".into(),
                dest: root.join("llama-runtime.zip"),
                sha256: Some("00".repeat(32)),
                approx_bytes: 1,
            }],
        };

        let before = installation_attestation(&model).unwrap();
        std::fs::write(&server, b"server-b").unwrap();
        let after = installation_attestation(&model).unwrap();
        assert_ne!(before, after);
        std::fs::remove_file(mtmd).unwrap();
        assert!(installation_attestation(&model).is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn sentinel_path_lives_under_sentinels_dir() {
        if let LookupResult::Found(m) = lookup_full("ram_plus")
            && let Some(p) = sentinel_path(&m) {
                let s = p.to_string_lossy();
                assert!(s.contains(".sentinels"), "sentinel not under .sentinels: {s}");
                let name = p.file_name().unwrap().to_string_lossy();
                assert!(name.starts_with("ram_plus-"), "sentinel not id-prefixed: {s}");
                assert!(name.ends_with(".installed"), "unexpected sentinel: {s}");
            }
    }

    #[test]
    fn pin_bump_invalidates_sentinel() {
        let LookupResult::Found(model) = lookup_full("ram_plus") else {
            return;
        };
        let before = sentinel_path(&model).expect("sentinel path");

        let mut bumped = model.clone();
        assert!(!bumped.files.is_empty(), "ram_plus must have at least one file");
        bumped.files[0].sha256 =
            Some("deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef".into());
        let after = sentinel_path(&bumped).expect("sentinel path");

        assert_ne!(
            before, after,
            "sentinel filename must change when a pinned hash changes"
        );
    }

    #[test]
    fn sentinel_is_stable_for_unchanged_pins() {
        if let LookupResult::Found(m) = lookup_full("mobileclip_s2") {
            assert_eq!(sentinel_path(&m), sentinel_path(&m.clone()));
        }
    }

    fn test_model(dir: &Path, file_name: &str, sha256: Option<String>) -> Model {
        Model {
            id: "ram_plus",
            display_name: "RAM++",
            files: vec![FileEntry {
                url: String::new(),
                dest: dir.join(file_name),
                sha256,
                approx_bytes: 0,
            }],
        }
    }

    fn unique_sentinel_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fileid-sentinel-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A pre-revision-key bare `{id}.installed` is renamed to the keyed name, but only when the on-disk artifacts still
    /// satisfy the current pins (here sha256=None, so existence is the gate).
    #[test]
    fn legacy_sentinel_is_migrated_when_pins_valid() {
        let dir = unique_sentinel_dir("migrate");
        std::fs::write(dir.join("ram_plus.onnx"), b"weights").unwrap();
        let legacy = dir.join("ram_plus.installed");
        std::fs::write(&legacy, b"ram_plus").unwrap();
        let keyed = dir.join("ram_plus-deadbeef00000000.installed");

        migrate_legacy_sentinel(&dir, &test_model(&dir, "ram_plus.onnx", None), &keyed);

        assert!(keyed.exists(), "keyed sentinel not created from a valid legacy marker");
        assert!(!legacy.exists(), "legacy marker not consumed by migration");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A legacy marker whose artifacts no longer satisfy the pins (missing file, or sha256 mismatch) must be deleted,
    /// not promoted.
    #[test]
    fn legacy_sentinel_dropped_when_pins_stale() {
        let dir = unique_sentinel_dir("stale-missing");
        let legacy = dir.join("ram_plus.installed");
        std::fs::write(&legacy, b"ram_plus").unwrap();
        let keyed = dir.join("ram_plus-deadbeef00000000.installed");
        migrate_legacy_sentinel(&dir, &test_model(&dir, "ram_plus.onnx", None), &keyed);
        assert!(!keyed.exists(), "must not vouch when the pinned file is missing");
        assert!(!legacy.exists(), "stale legacy marker must be dropped for a re-fetch");
        let _ = std::fs::remove_dir_all(&dir);

        let dir = unique_sentinel_dir("stale-sha");
        std::fs::write(dir.join("ram_plus.onnx"), b"OLD pre-bump bytes").unwrap();
        let legacy = dir.join("ram_plus.installed");
        std::fs::write(&legacy, b"ram_plus").unwrap();
        let keyed = dir.join("ram_plus-cafebabe00000000.installed");
        let wrong = Some("0000000000000000000000000000000000000000000000000000000000000000".into());
        migrate_legacy_sentinel(&dir, &test_model(&dir, "ram_plus.onnx", wrong), &keyed);
        assert!(!keyed.exists(), "must not vouch when sha256 mismatches the current pin");
        assert!(!legacy.exists(), "sha-mismatched legacy marker must be dropped");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Migration must not clobber an existing keyed sentinel, and is a no-op without a legacy marker.
    #[test]
    fn migration_noop_without_legacy_marker() {
        let dir = unique_sentinel_dir("noop");
        let keyed = dir.join("ram_plus-cafebabe00000000.installed");
        let model = test_model(&dir, "ram_plus.onnx", None);

        migrate_legacy_sentinel(&dir, &model, &keyed);
        assert!(!keyed.exists(), "migration must not fabricate a sentinel");

        std::fs::write(&keyed, b"ram_plus").unwrap();
        let legacy = dir.join("ram_plus.installed");
        std::fs::write(&legacy, b"ram_plus").unwrap();
        migrate_legacy_sentinel(&dir, &model, &keyed);
        assert!(keyed.exists() && legacy.exists(), "existing keyed sentinel must not trigger a rename");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    #[test]
    fn mac_app_bundles_follow_the_shared_layout() {
        let _g = crate::paths::TEST_ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let saved = std::env::var_os("FILEID_MODELS_DIR");
        let dir = unique_sentinel_dir("mac-layout");
        unsafe { std::env::set_var("FILEID_MODELS_DIR", &dir) };
        let layout: serde_json::Value =
            serde_json::from_str(include_str!("../../../../../../shared/test-corpus/mac-model-layout.json")).unwrap();
        for (kind, files) in layout["kinds"].as_object().unwrap() {
            let LookupResult::Found(model) = lookup_full(kind) else { panic!("{kind} did not resolve") };
            let dests: Vec<String> =
                model.files.iter().map(|f| f.dest.strip_prefix(&dir).unwrap().to_string_lossy().replace('\\', "/")).collect();
            let expected: Vec<String> = files.as_array().unwrap().iter().map(|f| f.as_str().unwrap().to_string()).collect();
            assert_eq!(dests, expected, "{kind}");
        }
        assert!(matches!(lookup_full("qwen2_5_vl_7b"), LookupResult::Unknown), "no GGUF models on the Mac app");
        match saved {
            Some(value) => unsafe { std::env::set_var("FILEID_MODELS_DIR", value) },
            None => unsafe { std::env::remove_var("FILEID_MODELS_DIR") },
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn swift_layout_install_is_verified_by_sha_without_a_sentinel() {
        let _g = crate::paths::TEST_ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let saved = std::env::var_os("FILEID_MODELS_DIR");
        let dir = unique_sentinel_dir("swift-layout");
        std::fs::create_dir_all(&dir).unwrap();
        unsafe { std::env::set_var("FILEID_MODELS_DIR", &dir) };

        let bytes = b"sface weights";
        std::fs::write(dir.join("face_recognition_sface_2021dec.onnx"), bytes).unwrap();
        let pin = hex::encode(sha2::Sha256::digest(bytes));
        let model = |sha256: String| Model {
            id: "arcface",
            display_name: "Faces",
            files: vec![FileEntry {
                url: "https://huggingface.co/x".into(),
                dest: dir.join("sface").join("face_recognition_sface_2021dec.onnx"),
                sha256: Some(sha256),
                approx_bytes: bytes.len() as u64,
            }],
        };
        assert!(installation_complete(&model(pin.clone())));
        assert!(!installation_complete(&model("0".repeat(64))));
        assert!(!dir.join(".sentinels").read_dir().is_ok_and(|mut entries| entries.next().is_some()));

        match saved {
            Some(value) => unsafe { std::env::set_var("FILEID_MODELS_DIR", value) },
            None => unsafe { std::env::remove_var("FILEID_MODELS_DIR") },
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
