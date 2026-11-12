//! IPC protocol types for `shared/ipc-schema/ipc.schema.json`.
//!
//! The message types live in `generated.rs`, written by
//! `shared/scripts/gen_ipc_dtos.py` from the schema plus `codegen.json`. This
//! module keeps the hand-written helpers: `Empty`, `Wrap`, `HardwareInfo`
//! (an open object in the schema), constructors and command validation.

mod generated;
pub use generated::*;
pub mod catalog;
use serde::{Deserialize, Serialize};

pub mod sink;
pub(crate) mod bounded_read;
#[cfg(test)]
pub(crate) mod conformance;

impl IpcEvent {
    pub fn now(payload: EventPayload) -> Self {
        Self { t: chrono::Utc::now(), payload }
    }
}

impl CommandPayload {
    /// `BulkActionResult.action` for the commands that answer with one.
    pub fn action_name(&self) -> Option<&'static str> {
        Some(match self {
            Self::ApplyTags(_) => "applyTags",
            Self::RenameFiles(_) => "renameFiles",
            Self::TrashFiles(_) => "trashFiles",
            Self::MergeClusters(_) => "mergeClusters",
            Self::RenamePerson(_) => "renamePerson",
            Self::ReassignFace(_) => "reassignFace",
            Self::MarkPersonsAsUnknown(_) => "markPersonsAsUnknown",
            Self::MarkPersonsDifferent(_) => "markPersonsDifferent",
            Self::RestoreFromTrash(_) => "restoreFromTrash",
            Self::RevertMerge(_) => "revertMerge",
            Self::PurgeExcluded(_) => "purgeExcluded",
            _ => return None,
        })
    }

    /// Ids a bulk action reports per item; empty means one summary item.
    pub fn target_ids(&self) -> Vec<i64> {
        match self {
            Self::ApplyTags(payload) => payload.file_ids.clone(),
            Self::TrashFiles(payload) => payload.file_ids.clone(),
            Self::RenameFiles(payload) => payload.renames.iter().map(|rename| rename.file_id).collect(),
            Self::MarkPersonsAsUnknown(payload) => payload.person_ids.clone(),
            Self::RenamePerson(payload) => vec![payload.person_id],
            Self::ReassignFace(payload) => vec![payload.face_id],
            _ => Vec::new(),
        }
    }
}

const MAX_BULK_ITEMS: usize = 100_000;
const MAX_RESTRUCTURE_MOVES: usize = 250_000;
const MAX_EXCLUDED_PATHS: usize = 10_000;
const MAX_TAGS_PER_COMMAND: usize = 1_024;
const MAX_APPLY_TAG_OPERATIONS: usize = 100_000;
const MAX_EXACT_TRASH_BYTES: u64 = 64 * 1024 * 1024 * 1024;
/// A Windows long path is 32,767 UTF-16 units, up to three UTF-8 bytes each.
const MAX_PATH_BYTES: usize = 128 * 1024;
/// Tags, person names and file names (255 UTF-16 units at most on disk).
const MAX_NAME_BYTES: usize = 4 * 1024;
const MAX_MODEL_KIND_BYTES: usize = 128;
const MAX_TOKEN_BYTES: usize = 1024;
const MAX_QUERY_BYTES: usize = 64 * 1024;

pub(crate) fn normalize_and_validate_command(payload: &mut CommandPayload) -> Result<(), String> {
    fn check_len(field: &str, len: usize, max: usize) -> Result<(), String> {
        if len > max {
            Err(format!("{field} contains {len} items; maximum is {max}"))
        } else {
            Ok(())
        }
    }

    /// No real path, name or identifier is this long or holds a NUL.
    fn check_text(field: &str, value: &str, max: usize) -> Result<(), String> {
        if value.len() > max {
            Err(format!("{field} is {} bytes; maximum is {max}", value.len()))
        } else if value.contains('\0') {
            Err(format!("{field} contains a NUL character"))
        } else {
            Ok(())
        }
    }

    fn check_all<'a>(field: &str, values: impl IntoIterator<Item = &'a String>, max: usize) -> Result<(), String> {
        values.into_iter().try_for_each(|value| check_text(field, value, max))
    }

    fn dedupe_ids(ids: &mut Vec<i64>) {
        let mut seen = std::collections::HashSet::with_capacity(ids.len());
        ids.retain(|id| seen.insert(*id));
    }

    fn dedupe_strings(values: &mut Vec<String>) {
        let mut seen = std::collections::HashSet::with_capacity(values.len());
        values.retain(|value| seen.insert(value.clone()));
    }

    match payload {
        CommandPayload::StartScan(payload) => {
            check_text("startScan.rootPath", &payload.root_path, MAX_PATH_BYTES)?;
            check_all("startScan.rootDisplay", &payload.root_display, MAX_PATH_BYTES)?;
            if let Some(paths) = &mut payload.excluded_paths {
                check_len("startScan.excludedPaths", paths.len(), MAX_EXCLUDED_PATHS)?;
                check_all("startScan.excludedPaths", paths.iter(), MAX_PATH_BYTES)?;
                dedupe_strings(paths);
            }
        }
        CommandPayload::PurgeExcluded(payload) => {
            check_len("purgeExcluded.excludedPaths", payload.excluded_paths.len(), MAX_EXCLUDED_PATHS)?;
            check_all("purgeExcluded.excludedPaths", &payload.excluded_paths, MAX_PATH_BYTES)?;
            dedupe_strings(&mut payload.excluded_paths);
        }
        CommandPayload::DeepAnalyzeFile(payload) => {
            check_text("deepAnalyzeFile.modelKind", &payload.model_kind, MAX_MODEL_KIND_BYTES)?;
        }
        CommandPayload::DeepAnalyzeFolder(payload) => {
            check_text("deepAnalyzeFolder.modelKind", &payload.model_kind, MAX_MODEL_KIND_BYTES)?;
            check_text("deepAnalyzeFolder.pathPrefix", &payload.path_prefix, MAX_PATH_BYTES)?;
        }
        CommandPayload::PrewarmModel(payload) => {
            check_text("prewarmModel.modelKind", &payload.model_kind, MAX_MODEL_KIND_BYTES)?;
        }
        CommandPayload::CancelPrewarm(payload) => {
            check_all("cancelPrewarm.modelKind", &payload.model_kind, MAX_MODEL_KIND_BYTES)?;
        }
        CommandPayload::PlanRestructure(payload) => {
            check_text("planRestructure.libraryRoot", &payload.library_root, MAX_PATH_BYTES)?;
        }
        CommandPayload::UndoRestructure(payload) => {
            check_text("undoRestructure.libraryRoot", &payload.library_root, MAX_PATH_BYTES)?;
            check_all("undoRestructure.shortcutUndoToken", &payload.shortcut_undo_token, MAX_TOKEN_BYTES)?;
        }
        CommandPayload::RenamePerson(payload) => {
            for value in [&payload.title, &payload.first_name, &payload.middle_name, &payload.last_name, &payload.suffix] {
                check_all("renamePerson name field", value, MAX_NAME_BYTES)?;
            }
        }
        CommandPayload::EmbedTextQuery(payload) => {
            check_text("embedTextQuery.query", &payload.query, MAX_QUERY_BYTES)?;
            check_text("embedTextQuery.queryID", &payload.query_id, MAX_TOKEN_BYTES)?;
        }
        CommandPayload::EmbedImageQuery(payload) => {
            check_text("embedImageQuery.queryID", &payload.query_id, MAX_TOKEN_BYTES)?;
        }
        CommandPayload::GenerateVideoThumbnail(payload) => {
            check_text("generateVideoThumbnail.path", &payload.path, MAX_PATH_BYTES)?;
        }
        CommandPayload::RestoreFromTrash(payload) => {
            check_text("restoreFromTrash.batchID", &payload.batch_id, MAX_TOKEN_BYTES)?;
        }
        CommandPayload::DeepAnalyzeAll(payload) => {
            check_text("deepAnalyzeAll.modelKind", &payload.model_kind, MAX_MODEL_KIND_BYTES)?;
            // The schema caps excludedFolders at 256; enforce it here too so the
            // bound doesn't depend solely on each app's own sanitizer (a
            // hand-edited settings.json or a third-party client bypasses those).
            if let Some(folders) = &mut payload.excluded_folders {
                check_len(
                    "deepAnalyzeAll.excludedFolders",
                    folders.len(),
                    MAX_EXCLUDED_PATHS,
                )?;
                check_all("deepAnalyzeAll.excludedFolders", folders.iter(), MAX_PATH_BYTES)?;
                dedupe_strings(folders);
            }
        }
        CommandPayload::ApplyRestructure(payload) => {
            check_len("applyRestructure.moves", payload.moves.len(), MAX_RESTRUCTURE_MOVES)?;
            check_text("applyRestructure.libraryRoot", &payload.library_root, MAX_PATH_BYTES)?;
            check_all("applyRestructure.planID", &payload.plan_id, MAX_TOKEN_BYTES)?;
            for entry in &payload.moves {
                check_text("applyRestructure move source", &entry.source, MAX_PATH_BYTES)?;
                check_text("applyRestructure move destination", &entry.destination, MAX_PATH_BYTES)?;
                check_text("applyRestructure move category", &entry.category, MAX_PATH_BYTES)?;
                check_text("applyRestructure move confidence", &entry.confidence, MAX_TOKEN_BYTES)?;
                check_all("applyRestructure move reason", &entry.reason, MAX_PATH_BYTES)?;
                check_all("applyRestructure move tier", &entry.tier, MAX_TOKEN_BYTES)?;
                check_all("applyRestructure move contentHash", &entry.content_hash, MAX_TOKEN_BYTES)?;
            }
            let mut seen = std::collections::HashSet::with_capacity(payload.moves.len());
            payload.moves.retain(|entry| seen.insert(entry.file_id));
        }
        CommandPayload::ApplyTags(payload) => {
            check_len("applyTags.fileIDs", payload.file_ids.len(), MAX_BULK_ITEMS)?;
            check_len("applyTags.tags", payload.tags.len(), MAX_TAGS_PER_COMMAND)?;
            check_all("applyTags.tags", &payload.tags, MAX_NAME_BYTES)?;
            dedupe_ids(&mut payload.file_ids);
            dedupe_strings(&mut payload.tags);
            let operations = payload.file_ids.len().saturating_mul(payload.tags.len());
            if operations > MAX_APPLY_TAG_OPERATIONS {
                return Err(format!(
                    "applyTags expands to {operations} file/tag operations; maximum is {MAX_APPLY_TAG_OPERATIONS}"
                ));
            }
        }
        CommandPayload::RenameFiles(payload) => {
            check_len("renameFiles.renames", payload.renames.len(), MAX_BULK_ITEMS)?;
            check_all("renameFiles.newName", payload.renames.iter().map(|entry| &entry.new_name), MAX_NAME_BYTES)?;
            let mut seen = std::collections::HashSet::with_capacity(payload.renames.len());
            payload.renames.retain(|entry| seen.insert(entry.file_id));
        }
        CommandPayload::TrashFiles(payload) => {
            check_len("trashFiles.fileIDs", payload.file_ids.len(), MAX_BULK_ITEMS)?;
            if let Some(identities) = &payload.exact_identities {
                check_len("trashFiles.exactIdentities", identities.len(), MAX_BULK_ITEMS)?;
                let requested: std::collections::HashSet<i64> =
                    payload.file_ids.iter().copied().collect();
                let selected_paths: std::collections::HashSet<&str> =
                    identities.iter().map(|identity| identity.path.as_str()).collect();
                let mut seen = std::collections::HashSet::with_capacity(identities.len());
                let mut exact_bytes = 0u64;
                for identity in identities {
                    if !requested.contains(&identity.file_id) {
                        return Err(format!(
                            "trashFiles exact identity #{} is not in fileIDs",
                            identity.file_id
                        ));
                    }
                    if !seen.insert(identity.file_id) {
                        return Err(format!(
                            "trashFiles contains duplicate exact identity #{}",
                            identity.file_id
                        ));
                    }
                    check_text("trashFiles exact identity path", &identity.path, MAX_PATH_BYTES)?;
                    check_text("trashFiles exact identity keeperPath", &identity.keeper_path, MAX_PATH_BYTES)?;
                    if identity.path.is_empty()
                        || identity.size_bytes < 0
                        || identity.sha256_hex.len() != 64
                        || !identity.sha256_hex.bytes().all(|byte| byte.is_ascii_hexdigit())
                        || identity.keeper_path.is_empty()
                        || selected_paths.contains(identity.keeper_path.as_str())
                        || identity.keeper_size_bytes < 0
                        || identity.size_bytes != identity.keeper_size_bytes
                        || !identity
                            .sha256_hex
                            .eq_ignore_ascii_case(&identity.keeper_sha256_hex)
                        || identity.keeper_sha256_hex.len() != 64
                        || !identity
                            .keeper_sha256_hex
                            .bytes()
                            .all(|byte| byte.is_ascii_hexdigit())
                    {
                        return Err(format!(
                            "trashFiles exact identity #{} is invalid",
                            identity.file_id
                        ));
                    }
                    exact_bytes = exact_bytes
                        .checked_add(identity.size_bytes as u64)
                        .and_then(|total| total.checked_add(identity.keeper_size_bytes as u64))
                        .ok_or_else(|| "trashFiles exact byte total overflowed".to_string())?;
                }
                if seen != requested {
                    return Err(
                        "trashFiles exactIdentities must cover every requested fileID".into(),
                    );
                }
                if exact_bytes > MAX_EXACT_TRASH_BYTES {
                    return Err(format!(
                        "trashFiles exact verification needs {exact_bytes} bytes; maximum is {MAX_EXACT_TRASH_BYTES}"
                    ));
                }
            }
            dedupe_ids(&mut payload.file_ids);
        }
        CommandPayload::MarkPersonsAsUnknown(payload) => {
            check_len("markPersonsAsUnknown.personIDs", payload.person_ids.len(), MAX_BULK_ITEMS)?;
            dedupe_ids(&mut payload.person_ids);
        }
        CommandPayload::RevertMerge(payload) => {
            check_len("revertMerge.faceIDsToRevert", payload.face_ids_to_revert.len(), MAX_BULK_ITEMS)?;
            dedupe_ids(&mut payload.face_ids_to_revert);
        }
        _ => {}
    }
    Ok(())
}

/// Empty object — `{}`. Serde encodes a unit struct as `null`, which is wrong;
/// an empty struct with no fields encodes as `{}` like Swift produces.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Empty {}

impl HealthCheckPayload {
    pub fn is_valid(&self) -> bool {
        (1..=128).contains(&self.request_id.len())
            && self.request_id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    }
}

#[cfg(test)]
mod health_tests {
    use super::HealthCheckPayload;

    #[test]
    fn correlation_nonce_is_bounded_ascii() {
        for request_id in ["probe-1".into(), "GEN_4-probe_2".into(), "a".repeat(128)] {
            assert!(HealthCheckPayload { request_id }.is_valid());
        }
        for request_id in [String::new(), " ".into(), "../file".into(), "line\nfeed".into(), "é".into(), "a\0b".into(), "a".repeat(129)] {
            assert!(!HealthCheckPayload { request_id }.is_valid());
        }
    }
}

impl ListRootsPayload {
    pub fn is_valid(&self) -> bool {
        HealthCheckPayload { request_id: self.request_id.clone() }.is_valid()
    }
}

impl ListModelsPayload {
    pub fn is_valid(&self) -> bool {
        HealthCheckPayload { request_id: self.request_id.clone() }.is_valid()
            && self.role.as_deref().is_none_or(|role| ["vision", "text", "embedding", "reranker", "speech"].contains(&role))
    }
}

impl BenchmarkModelPayload {
    pub fn is_valid(&self) -> bool {
        HealthCheckPayload { request_id: self.request_id.clone() }.is_valid() && (1..=128).contains(&self.model_kind.len())
    }
}

fn default_true() -> bool {
    true
}

/// Wraps a single positional value in `{"_0": ...}` to match Swift Codable
/// auto-synthesis for cases like `case ready(EngineInfo)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wrap<T> {
    #[serde(rename = "_0")]
    pub inner: T,
}

impl<T> Wrap<T> {
    pub fn new(inner: T) -> Self {
        Self { inner }
    }
}

/// `EngineInfo.hardware` / `hardwareReprobed.hardware`. The schema leaves this
/// object open, so it stays hand-written.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HardwareInfo {
    /// "nvidia" / "amd" / "intel" / "qualcomm" / "other" / "none".
    pub gpu_vendor: String,
    /// Friendly adapter name as reported by DXGI ("NVIDIA GeForce RTX 4070",
    /// "AMD Radeon RX 7900 XT", "Intel(R) Arc(TM) A380 Graphics", etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_name: Option<String>,
    /// EP the engine picked: "cuda" / "tensorrt" / "directml" / "openvino"
    /// / "qnn" / "cpu". This is what ML inference will use unless the
    /// user overrides via Settings.
    pub execution_provider: String,
    /// Number of physical CPU cores.
    pub physical_cpu_cores: u32,
    /// Whether each Performance Pack is detected on this machine.
    pub cuda_pack_present: bool,
    pub openvino_pack_present: bool,
    pub qnn_pack_present: bool,
    /// "Install the NVIDIA CUDA Pack for ~30% faster inference" — the
    /// engine writes a contextual recommendation here based on detected
    /// vendor + already-installed packs. Empty string when the user is
    /// already on the optimal path.
    #[serde(default)]
    pub recommendation: String,
    // ─── Adaptive-utilization diagnostics. All optional
    //     so an older C# build talking to a newer engine still deserializes
    //     cleanly. ───
    /// CPU performance-core count (Intel hybrid 12th-gen+, future AMD
    /// dense-core parts). 0 on non-hybrid CPUs (use physical_cpu_cores).
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub p_cores: u32,
    /// CPU efficiency-core count. 0 on non-hybrid CPUs.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub e_cores: u32,
    /// Logical processor count (cores × SMT threads).
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub logical_cpu_cores: u32,
    /// Worker thread cap currently in effect (= cpu_topology().worker_cap()).
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub worker_cap: u32,
    /// Total physical RAM in MiB.
    #[serde(rename = "ramTotalMB", default, skip_serializing_if = "is_zero_u64")]
    pub ram_total_mb: u64,
    /// Currently-available RAM in MiB (GlobalMemoryStatusEx ullAvailPhys).
    #[serde(rename = "ramAvailableMB", default, skip_serializing_if = "is_zero_u64")]
    pub ram_available_mb: u64,
    /// Active memory tier: "low" / "balanced" / "high". Drives batch size,
    /// channel caps, ML pool size.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub memory_tier: String,
    /// Dedicated GPU VRAM in MiB (DXGI DedicatedVideoMemory). 0 when no
    /// physical adapter was found.
    #[serde(rename = "vramMB", default, skip_serializing_if = "is_zero_u64")]
    pub vram_mb: u64,
    /// NPU presence (Intel AI Boost, AMD XDNA, Qualcomm Hexagon).
    /// Detection is first-pass — Qualcomm via the existing QNN probe;
    /// Intel/AMD report `false` for now (NEXT.md entry tracks).
    #[serde(default, skip_serializing_if = "is_false")]
    pub npu_present: bool,
    /// Power source: "ac" / "battery" / "unknown".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub power_source: String,
    /// Battery percent (0–100) when on battery. None on desktops without
    /// a battery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub battery_percent: Option<u8>,
    /// Currently-active performance profile: "eco" / "auto" / "performance".
    /// Phase-1 ships "auto" only; Eco / Performance are grayed in the UI.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub active_profile: String,
}

fn is_zero_u32(v: &u32) -> bool { *v == 0 }
fn is_zero_u64(v: &u64) -> bool { *v == 0 }
fn is_false(v: &bool) -> bool { !*v }

impl EngineError {
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self { kind: kind.into(), message: message.into(), path: None, model_kind: None }
    }

    pub fn event(kind: impl Into<String>, message: impl Into<String>) -> IpcEvent {
        Self::new(kind, message).into_event()
    }

    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn with_model(mut self, model_kind: impl Into<String>) -> Self {
        self.model_kind = Some(model_kind.into());
        self
    }

    pub fn into_event(self) -> IpcEvent {
        IpcEvent::now(EventPayload::Error(Wrap::new(self)))
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// The engine MUST emit ready in the exact shape Swift Codable expects.
    /// Spot-check: `{"t":"...","payload":{"ready":{"_0":{...}}}}` with sorted keys.
    #[test]
    fn ready_event_wire_shape() {
        let evt = IpcEvent {
            t: chrono::DateTime::parse_from_rfc3339("2026-05-02T12:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            payload: EventPayload::Ready(Wrap::new(EngineInfo {
                version: "0.1.0".into(),
                pid: 12345,
                worker_cap: 14,
                physical_memory_gb: 16.0,
                engine: None,
                hardware: None,
            })),
        };
        let j = serde_json::to_value(&evt).unwrap();
        let inner = j.get("payload").unwrap()
                      .get("ready").unwrap()
                      .get("_0").unwrap();
        assert_eq!(inner.get("version").unwrap(), "0.1.0");
        assert_eq!(inner.get("pid").unwrap(), 12345);
        assert_eq!(inner.get("workerCap").unwrap(), 14);
        assert_eq!(inner.get("physicalMemoryGB").unwrap(), 16.0);
    }

    /// startScan sent by the app must round-trip cleanly with the shared
    /// path and optional bookmark fields.
    #[test]
    fn start_scan_command_roundtrip() {
        let cmd = IpcCommand {
            id: "test-1".into(),
            payload: CommandPayload::StartScan(StartScanPayload {
                root_path: r"C:\Users\adam\Pictures".into(),
                root_bookmark: None,
                root_display: Some("Pictures".into()),
                rescan: false,
                excluded_paths: None,
            }),
        };
        let j = serde_json::to_string(&cmd).unwrap();
        assert!(!j.contains("excludedPaths"), "None must omit the key on the wire");
        let parsed: IpcCommand = serde_json::from_str(&j).unwrap();
        match parsed.payload {
            CommandPayload::StartScan(p) => {
                assert_eq!(p.root_path, r"C:\Users\adam\Pictures");
                assert_eq!(p.root_display.as_deref(), Some("Pictures"));
                assert!(p.excluded_paths.is_none());
            }
            other => panic!("expected StartScan variant, got {other:?}"),
        }
    }

    /// `excludedPaths` is additive-optional: legacy JSON without the key must
    /// decode to `None`, and a populated list must round-trip verbatim.
    #[test]
    fn start_scan_excluded_paths_optional_roundtrip() {
        let legacy = r#"{"id":"c-1","payload":{"startScan":{"rootPath":"C:\\pics"}}}"#;
        match serde_json::from_str::<IpcCommand>(legacy).unwrap().payload {
            CommandPayload::StartScan(p) => assert!(p.excluded_paths.is_none()),
            other => panic!("expected StartScan, got {other:?}"),
        }
        let cmd = IpcCommand {
            id: "c-2".into(),
            payload: CommandPayload::StartScan(StartScanPayload {
                root_path: r"C:\pics".into(),
                root_bookmark: None,
                root_display: None,
                rescan: true,
                excluded_paths: Some(vec![r"C:\pics\raw".into(), r"C:\pics\tmp\".into()]),
            }),
        };
        let j = serde_json::to_string(&cmd).unwrap();
        match serde_json::from_str::<IpcCommand>(&j).unwrap().payload {
            CommandPayload::StartScan(p) => assert_eq!(
                p.excluded_paths.as_deref(),
                Some(&[r"C:\pics\raw".to_string(), r"C:\pics\tmp\".to_string()][..])
            ),
            other => panic!("expected StartScan, got {other:?}"),
        }
    }

    /// Empty-payload commands must serialize as `{"shutdown":{}}`, NOT
    /// `"shutdown"` (which would be serde's default for a unit variant).
    #[test]
    fn shutdown_command_wire_shape() {
        let cmd = IpcCommand {
            id: "test-2".into(),
            payload: CommandPayload::Shutdown(Empty {}),
        };
        let v = serde_json::to_value(&cmd).unwrap();
        let payload = v.get("payload").unwrap();
        let shutdown = payload.get("shutdown").unwrap();
        assert!(shutdown.is_object());
        assert_eq!(shutdown.as_object().unwrap().len(), 0);
    }

    /// A schema-conformant `deepAnalyzeAll` that OMITS `tagsOnly`
    /// (and `proposeRenames`) must decode, defaulting `tags_only=false` and
    /// `propose_renames=true`. This is the contract the Swift mirror must match.
    #[test]
    fn deep_analyze_all_decodes_with_optional_fields_omitted() {
        let json = r#"{"id":"c-1","payload":{"deepAnalyzeAll":{"modelKind":"qwen2_5_vl_7b","skipExisting":true}}}"#;
        let cmd: IpcCommand = serde_json::from_str(json).expect("decode without tagsOnly/proposeRenames");
        match cmd.payload {
            CommandPayload::DeepAnalyzeAll(p) => {
                assert_eq!(p.model_kind, "qwen2_5_vl_7b");
                assert!(p.skip_existing);
                assert!(!p.tags_only, "tagsOnly must default to false when omitted");
                assert!(p.propose_renames, "proposeRenames must default to true when omitted");
            }
            other => panic!("expected DeepAnalyzeAll, got {other:?}"),
        }
        // proposeRenames explicitly false must survive (caption + tags, no rename).
        let json2 = r#"{"id":"c-2","payload":{"deepAnalyzeAll":{"modelKind":"m","skipExisting":false,"tagsOnly":false,"proposeRenames":false}}}"#;
        let cmd2: IpcCommand = serde_json::from_str(json2).unwrap();
        match cmd2.payload {
            CommandPayload::DeepAnalyzeAll(p) => assert!(!p.propose_renames),
            other => panic!("expected DeepAnalyzeAll, got {other:?}"),
        }
    }

    #[test]
    fn deep_analyze_all_scopes_roundtrip_without_casing_drift() {
        let json = r#"{"id":"scope","payload":{"deepAnalyzeAll":{"modelKind":"qwen2_5_vl_7b","skipExisting":true,"fileIDs":[3,4],"excludedFolders":["/generated/private"]}}}"#;
        let command: IpcCommand = serde_json::from_str(json).unwrap();
        let encoded = serde_json::to_value(&command).unwrap();
        let scope = &encoded["payload"]["deepAnalyzeAll"];
        assert_eq!(scope["fileIDs"], serde_json::json!([3, 4]));
        assert!(scope.get("fileIds").is_none());
        assert_eq!(scope["excludedFolders"], serde_json::json!(["/generated/private"]));
    }

    #[test]
    fn deep_analyze_all_misspelled_selection_cannot_become_whole_library() {
        let json = r#"{"id":"scope","payload":{"deepAnalyzeAll":{"modelKind":"qwen2_5_vl_7b","skipExisting":true,"fileIds":[]}}}"#;
        assert!(serde_json::from_str::<IpcCommand>(json).is_err());
    }

    #[test]
    fn deep_analyze_all_empty_selection_is_distinct_from_null_or_omitted() {
        for (fields, selected) in [
            (serde_json::json!({}), false),
            (serde_json::json!({"fileIDs": null, "excludedFolders": null}), false),
            (serde_json::json!({"fileIDs": [], "excludedFolders": []}), true),
        ] {
            let mut scope = serde_json::json!({"modelKind": "qwen2_5_vl_7b", "skipExisting": true});
            scope.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
            let command: IpcCommand = serde_json::from_value(serde_json::json!({"id": "scope", "payload": {"deepAnalyzeAll": scope}})).unwrap();
            let CommandPayload::DeepAnalyzeAll(payload) = command.payload else { panic!("wrong command") };
            assert_eq!(payload.file_ids.is_some(), selected);
            assert_eq!(payload.excluded_folders.is_some(), selected);
            assert!(payload.file_ids.as_ref().is_none_or(Vec::is_empty));
        }
    }

    /// `cancelPrewarm` must decode with `modelKind` OMITTED (the
    /// cancel-all form → `None`) and with it present (targeted per-model cancel).
    /// The empty-object form `{"cancelPrewarm":{}}` is the canonical cancel-all.
    #[test]
    fn cancel_prewarm_decodes_with_optional_model_kind() {
        let all = r#"{"id":"c-1","payload":{"cancelPrewarm":{}}}"#;
        match serde_json::from_str::<IpcCommand>(all).unwrap().payload {
            CommandPayload::CancelPrewarm(p) => {
                assert!(p.model_kind.is_none(), "absent modelKind = cancel-all (None)");
            }
            other => panic!("expected CancelPrewarm, got {other:?}"),
        }
        let one = r#"{"id":"c-2","payload":{"cancelPrewarm":{"modelKind":"qwen2_5_vl_7b"}}}"#;
        match serde_json::from_str::<IpcCommand>(one).unwrap().payload {
            CommandPayload::CancelPrewarm(p) => {
                assert_eq!(p.model_kind.as_deref(), Some("qwen2_5_vl_7b"));
            }
            other => panic!("expected CancelPrewarm, got {other:?}"),
        }
    }

    #[test]
    fn scan_phase_enum_lowercased() {
        let j = serde_json::to_string(&ScanPhase::Discovering).unwrap();
        assert_eq!(j, "\"discovering\"");
        let j2 = serde_json::to_string(&ScanPhase::PostScan).unwrap();
        assert_eq!(j2, "\"postScan\"");
    }

    #[test]
    fn destructive_command_ids_are_deduplicated_before_dispatch() {
        let mut payload = CommandPayload::TrashFiles(TrashFilesPayload {
            file_ids: vec![7, 7, 9, 7, 9],
            exact_identities: None,
        });
        normalize_and_validate_command(&mut payload).unwrap();
        let CommandPayload::TrashFiles(payload) = payload else {
            unreachable!();
        };
        assert_eq!(payload.file_ids, vec![7, 9]);
    }

    #[test]
    fn exact_trash_identity_round_trips_and_must_belong_to_request() {
        let exact = |file_id, path: &str, size_bytes| ExactTrashIdentity {
            file_id,
            path: path.into(),
            size_bytes,
            sha256_hex: "ab".repeat(32),
            keeper_path: "/library/keeper.bin".into(),
            keeper_size_bytes: size_bytes,
            keeper_sha256_hex: "ab".repeat(32),
        };
        let mut payload = CommandPayload::TrashFiles(TrashFilesPayload {
            file_ids: vec![7],
            exact_identities: Some(vec![exact(7, "/library/a.bin", 4)]),
        });
        normalize_and_validate_command(&mut payload).unwrap();
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(
            json["trashFiles"]["exactIdentities"][0]["fileID"],
            serde_json::json!(7)
        );
        assert_eq!(
            json["trashFiles"]["exactIdentities"][0]["keeperSha256Hex"],
            serde_json::json!("ab".repeat(32))
        );

        let mut invalid = CommandPayload::TrashFiles(TrashFilesPayload {
            file_ids: vec![9],
            exact_identities: Some(vec![exact(7, "/library/a.bin", 4)]),
        });
        assert!(normalize_and_validate_command(&mut invalid)
            .unwrap_err()
            .contains("not in fileIDs"));

        let mut partial = CommandPayload::TrashFiles(TrashFilesPayload {
            file_ids: vec![7, 9],
            exact_identities: Some(vec![exact(7, "/library/a.bin", 4)]),
        });
        assert!(normalize_and_validate_command(&mut partial)
            .unwrap_err()
            .contains("cover every"));

        let mut unequal = exact(7, "/library/a.bin", 4);
        unequal.keeper_sha256_hex = "cd".repeat(32);
        let mut unequal_payload = CommandPayload::TrashFiles(TrashFilesPayload {
            file_ids: vec![7],
            exact_identities: Some(vec![unequal]),
        });
        assert!(normalize_and_validate_command(&mut unequal_payload)
            .unwrap_err()
            .contains("invalid"));

        let mut wrong_size = exact(7, "/library/a.bin", 4);
        wrong_size.keeper_size_bytes = 5;
        let mut wrong_size_payload = CommandPayload::TrashFiles(TrashFilesPayload {
            file_ids: vec![7],
            exact_identities: Some(vec![wrong_size]),
        });
        assert!(normalize_and_validate_command(&mut wrong_size_payload)
            .unwrap_err()
            .contains("invalid"));

        let mut over_budget = CommandPayload::TrashFiles(TrashFilesPayload {
            file_ids: vec![7],
            exact_identities: Some(vec![exact(
                7,
                "/library/huge.bin",
                (MAX_EXACT_TRASH_BYTES + 1) as i64,
            )]),
        });
        assert!(normalize_and_validate_command(&mut over_budget)
            .unwrap_err()
            .contains("maximum"));
    }

    #[test]
    fn destructive_command_rejects_max_plus_one_items() {
        let mut payload = CommandPayload::TrashFiles(TrashFilesPayload {
            file_ids: vec![1; MAX_BULK_ITEMS + 1],
            exact_identities: None,
        });
        let error = normalize_and_validate_command(&mut payload).unwrap_err();
        assert!(error.contains("maximum"));
    }

    #[test]
    fn apply_tags_rejects_excessive_cartesian_work() {
        let mut payload = CommandPayload::ApplyTags(ApplyTagsPayload {
            file_ids: (0..1_001).collect(),
            tags: (0..100).map(|index| format!("tag-{index}")).collect(),
            mode: TagMode::Add,
        });
        let error = normalize_and_validate_command(&mut payload).unwrap_err();
        assert!(error.contains("file/tag operations"));
    }

    #[test]
    fn restructure_moves_dedupe_by_file_id() {
        let move_for = |file_id| RestructureMove {
            file_id,
            source: format!("/source/{file_id}"),
            destination: format!("/destination/{file_id}"),
            category: "Documents".into(),
            tier: None,
            confidence: "auto".into(),
            reason: None,
            ..Default::default()
        };
        let mut payload = CommandPayload::ApplyRestructure(ApplyRestructurePayload {
            library_root: "/".into(),
            plan_id: None,
            moves: vec![move_for(1), move_for(1), move_for(2)],
            use_symlinks: false,
        });
        normalize_and_validate_command(&mut payload).unwrap();
        let CommandPayload::ApplyRestructure(payload) = payload else {
            unreachable!();
        };
        assert_eq!(payload.moves.iter().map(|entry| entry.file_id).collect::<Vec<_>>(), vec![1, 2]);
    }

    /// Every CommandPayload variant must round-trip through serde without
    /// losing its discriminant. Catches:
    ///   - `#[serde(rename = "…")]` drift between Rust + Swift schema
    ///   - Empty-struct vs unit-variant mistakes (Swift expects `{}`, not `null`)
    ///   - Field renames inside a payload that break decode
    ///   - Missing `#[serde(default)]` on an optional that becomes required
    ///
    /// When you add a CommandPayload variant you MUST add a case below or
    /// the test loses coverage silently.
    #[test]
    fn every_command_variant_round_trips() {
        let cases: Vec<CommandPayload> = vec![
            CommandPayload::GrantFolderAccess(GrantFolderAccessPayload { root_path: r"C:\Photos".into(), root_bookmark: "AQID".into() }),
            CommandPayload::StartScan(StartScanPayload {
                root_path: r"C:\Users\adam\Pictures".into(),
                root_bookmark: None,
                root_display: Some("Pictures".into()),
                rescan: false,
                excluded_paths: Some(vec![r"C:\Users\adam\Pictures\node_backups".into()]),
            }),
            CommandPayload::PauseScan(Empty {}),
            CommandPayload::ResumeScan(Empty {}),
            CommandPayload::CancelScan(Empty {}),
            CommandPayload::CancelRestructure(Empty {}),
            CommandPayload::HealthCheck(HealthCheckPayload {
                request_id: "health-check-1".into(),
            }),
            CommandPayload::RequestStatus(Empty {}),
            CommandPayload::Shutdown(Empty {}),
            CommandPayload::RunFaceClustering(Empty {}),
            CommandPayload::DeepAnalyzeFile(DeepAnalyzeFilePayload {
                file_id: 42,
                model_kind: "qwen2_5_vl_7b".into(),
            }),
            CommandPayload::DeepAnalyzeFolder(DeepAnalyzeFolderPayload {
                path_prefix: r"C:\Users\adam\Pictures\2024".into(),
                model_kind: "qwen2_5_vl_7b".into(),
            }),
            CommandPayload::DeepAnalyzeAll(DeepAnalyzeAllPayload {
                model_kind: "qwen2_5_vl_7b".into(),
                skip_existing: true,
                file_ids: Some(vec![42, 99]),
                tags_only: true,
                propose_renames: true,
                excluded_folders: Some(vec![r"C:\Users\adam\Private".into()]),
            }),
            CommandPayload::DeepAnalyzeCancel(Empty {}),
            CommandPayload::PrewarmModel(PrewarmModelPayload {
                model_kind: "arcface".into(),
            }),
            CommandPayload::CancelPrewarm(CancelPrewarmPayload { model_kind: None }),
            CommandPayload::PlanRestructure(PlanRestructurePayload {
                library_root: r"C:\Users\adam\Pictures".into(),
                supports_paged_plans: false,
            }),
            CommandPayload::UndoRestructure(UndoRestructurePayload {
                library_root: r"C:\Users\adam\Pictures".into(),
                shortcut_undo_token: None,
            }),
            CommandPayload::ApplyRestructure(ApplyRestructurePayload {
                library_root: r"C:\Users\adam\Pictures".into(),
                plan_id: None,
                moves: vec![RestructureMove {
                    file_id: 1,
                    source: r"C:\Users\adam\Pictures\IMG_0001.jpg".into(),
                    destination: r"C:\Users\adam\Pictures\Photos\2024\01\IMG_0001.jpg".into(),
                    category: "Photos/2024/01".into(),
                    tier: Some("Anchor".into()),
                    confidence: "auto".into(),
                    reason: Some("Photo from 2024".into()),
                    content_hash: None,
                    size_bytes: None,
                    file_ref: None,
                    modified_at: None,
                }],
                use_symlinks: false,
            }),
            CommandPayload::ApplyTags(ApplyTagsPayload {
                file_ids: vec![1, 2, 3],
                tags: vec!["hawaii".into(), "sunset".into()],
                mode: TagMode::Add,
            }),
            CommandPayload::RenameFiles(RenameFilesPayload {
                renames: vec![RenameEntry {
                    file_id: 1,
                    new_name: "Renamed.jpg".into(),
                }],
            }),
            CommandPayload::TrashFiles(TrashFilesPayload {
                file_ids: vec![1, 2, 3],
                exact_identities: None,
            }),
            CommandPayload::MergeClusters(MergeClustersPayload {
                source_person_id: 1,
                destination_person_id: 2,
            }),
            CommandPayload::EmbedTextQuery(EmbedTextQueryPayload {
                query: "sunset at the beach".into(),
                query_id: "q-1".into(),
            }),
            CommandPayload::RenamePerson(RenamePersonPayload {
                person_id: 1,
                title: None,
                first_name: Some("Mom".into()),
                middle_name: None,
                last_name: None,
                suffix: None,
            }),
            CommandPayload::MarkPersonsAsUnknown(MarkPersonsAsUnknownPayload {
                person_ids: vec![1, 2],
            }),
            CommandPayload::FindMergeSuggestions(Empty {}),
            CommandPayload::MarkPersonsDifferent(MarkPersonsDifferentPayload {
                source_person_id: 1,
                destination_person_id: 2,
                source_anchor_face_id: 10,
                destination_anchor_face_id: 20,
            }),
            CommandPayload::EmbedImageQuery(EmbedImageQueryPayload {
                file_id: 1,
                query_id: "q-2".into(),
            }),
            CommandPayload::RestoreFromTrash(RestoreFromTrashPayload {
                batch_id: "00000000-0000-0000-0000-000000000000".into(),
            }),
            CommandPayload::VerifyCudaPack(Empty {}),
            CommandPayload::RevertMerge(RevertMergePayload {
                source_person_id: 1,
                destination_person_id: 2,
                face_ids_to_revert: vec![10, 11, 12],
            }),
            CommandPayload::WipeLibrary(Empty {}),
            CommandPayload::GenerateVideoThumbnail(GenerateVideoThumbnailPayload {
                path: r"C:\Users\adam\Videos\clip.mp4".into(),
                modified_at: Some(1_700_000_000.0),
                max_pixel_size: Some(512),
            }),
            CommandPayload::PurgeExcluded(PurgeExcludedPayload {
                excluded_paths: vec![r"C:\Users\adam\Pictures\node_backups".into()],
            }),
        ];

        for payload in &cases {
            let cmd = IpcCommand {
                id: format!("test-{:?}", std::mem::discriminant(payload)),
                payload: payload.clone(),
            };
            let json = serde_json::to_string(&cmd)
                .unwrap_or_else(|e| panic!("encode failed for {payload:?}: {e}"));
            let decoded: IpcCommand = serde_json::from_str(&json)
                .unwrap_or_else(|e| panic!("decode failed for json {json}: {e}"));
            assert_eq!(
                std::mem::discriminant(payload),
                std::mem::discriminant(&decoded.payload),
                "variant changed during round-trip:\n  original = {payload:?}\n  json     = {json}\n  parsed   = {:?}",
                decoded.payload,
            );
        }
    }

    /// The `thumbnailGenerated` event must serialize in the Wrap<T> `_0`-nested
    /// camelCase shape, carry `modifiedAt` through, and round-trip without
    /// losing its base64 string payload.
    #[test]
    fn thumbnail_generated_event_round_trips() {
        let evt = IpcEvent::now(EventPayload::ThumbnailGenerated(Wrap::new(
            ThumbnailGenerated {
                path: r"C:\Users\adam\Videos\clip.mp4".into(),
                modified_at: Some(1_700_000_000.0),
                bytes: "/9j/4AAQSkZJRg==".into(),
            },
        )));
        let v = serde_json::to_value(&evt).unwrap();
        let inner = v
            .get("payload")
            .unwrap()
            .get("thumbnailGenerated")
            .unwrap()
            .get("_0")
            .unwrap();
        assert_eq!(inner.get("path").unwrap(), r"C:\Users\adam\Videos\clip.mp4");
        assert_eq!(inner.get("modifiedAt").unwrap(), 1_700_000_000.0);
        assert!(inner.get("bytes").unwrap().is_string());

        let json = serde_json::to_string(&evt).unwrap();
        let decoded: IpcEvent = serde_json::from_str(&json).unwrap();
        match decoded.payload {
            EventPayload::ThumbnailGenerated(w) => {
                assert_eq!(w.inner.path, r"C:\Users\adam\Videos\clip.mp4");
                assert_eq!(w.inner.modified_at, Some(1_700_000_000.0));
                assert_eq!(w.inner.bytes, "/9j/4AAQSkZJRg==");
            }
            other => panic!("expected ThumbnailGenerated, got {other:?}"),
        }
    }

    // Arbitrary StartScan root_paths must round-trip through serde_json
    // without character corruption — guards against encoder changes that
    // drop non-ASCII bytes or fail to escape backslashes / quotes.
    proptest::proptest! {
        #[test]
        fn start_scan_root_path_round_trips(path in "[\\PC]{1,200}") {
            let cmd = IpcCommand {
                id: "p-1".into(),
                payload: CommandPayload::StartScan(StartScanPayload {
                    root_path: path.clone(),
                    root_bookmark: None,
                    root_display: None,
                    rescan: false,
                    excluded_paths: None,
                }),
            };
            let json = serde_json::to_string(&cmd).expect("encode");
            let decoded: IpcCommand = serde_json::from_str(&json).expect("decode");
            match decoded.payload {
                CommandPayload::StartScan(p) => {
                    proptest::prop_assert_eq!(p.root_path, path);
                    proptest::prop_assert_eq!(p.root_display, None);
                    proptest::prop_assert!(!p.rescan);
                }
                other => proptest::prop_assert!(false, "expected StartScan, got {:?}", other),
            }
        }
    }
}
