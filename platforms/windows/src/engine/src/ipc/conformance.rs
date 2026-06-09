//! Schema-conformance suite: serializes an exemplar of every CommandPayload /
//! EventPayload variant and structurally checks the wire JSON against
//! `shared/ipc-schema/ipc.schema.json`. Guards the L1 finding class (key-casing
//! drift like `fileID` vs `fileId`) that a pure Rust↔Rust round-trip can't see.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;

use super::{
    ApplyRestructurePayload, ApplyTagsPayload, BatchSummary, BulkActionItem, BulkActionResult,
    CancelPrewarmPayload, ClipTextEmbedding, CommandPayload, DeepAnalyzeAllPayload,
    DeepAnalyzeComplete, DeepAnalyzeFileDone, DeepAnalyzeFilePayload, DeepAnalyzeFolderPayload,
    DeepAnalyzeProgress, DeepAnalyzeStarting, DeepAnalyzeStartingPhase, DiscoveryCompletePayload,
    EmbedImageQueryPayload, EmbedTextQueryPayload, Empty, EngineError, EngineInfo, EventPayload,
    FaceClusteringResult, FileDoneEvent, FolderClassificationCounts, GenerateVideoThumbnailPayload,
    HardwareInfo, HardwareReprobed, IpcCommand, IpcEvent, JobCategory, LibraryWiped, LogLevel,
    LogLine, MarkPersonsAsUnknownPayload, MarkPersonsDifferentPayload, MergeClustersPayload,
    MergeSuggestion, MergeSuggestions, ModelDownloadProgress, PlanRestructurePayload,
    PrewarmModelPayload, QueueState, QueuedJob, RenameEntry, RenameFilesPayload,
    RenamePersonPayload, RestoreFromTrashPayload, RestructureApplyResult, RestructureCategoryCount,
    RestructureMove, RestructurePlan, RevertMergePayload, ScanComplete, ScanPhase, ScanProgress,
    StartScanPayload, TagMode, ThumbnailGenerated, TrashFilesPayload, Wrap,
};

/// Schema tags with no Windows implementation. Empty today: the schema's
/// `startScan` already carries the cross-platform `rootPath` shape (no
/// macOS-only `rootBookmark` variant survives in `$defs.CommandPayload`),
/// and every other schema tag is implemented here. Add a tag ONLY for a
/// platform divergence documented in the schema's description for it.
const SCHEMA_ONLY_COMMAND_TAGS: &[&str] = &[];
const SCHEMA_ONLY_EVENT_TAGS: &[&str] = &[];

fn load_schema() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../shared/ipc-schema/ipc.schema.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read ipc.schema.json at {}: {e}", path.display()));
    serde_json::from_str(&text).expect("ipc.schema.json is not valid JSON")
}

fn resolve<'a>(root: &'a Value, node: &'a Value) -> &'a Value {
    match node.get("$ref").and_then(Value::as_str) {
        Some(r) => {
            let mut cur = root;
            for seg in r.trim_start_matches("#/").split('/') {
                cur = cur
                    .get(seg)
                    .unwrap_or_else(|| panic!("unresolvable $ref segment '{seg}' in '{r}'"));
            }
            resolve(root, cur)
        }
        None => node,
    }
}

fn schema_tags(root: &Value, union_def: &str) -> BTreeSet<String> {
    root["$defs"][union_def]["oneOf"]
        .as_array()
        .unwrap_or_else(|| panic!("$defs.{union_def}.oneOf missing"))
        .iter()
        .map(|variant| {
            let required = variant["required"]
                .as_array()
                .unwrap_or_else(|| panic!("oneOf variant lacks required: {variant}"));
            assert_eq!(required.len(), 1, "oneOf variant must have exactly one tag: {variant}");
            required[0].as_str().expect("tag must be a string").to_owned()
        })
        .collect()
}

fn variant_schema<'a>(root: &'a Value, union_def: &str, tag: &str) -> &'a Value {
    root["$defs"][union_def]["oneOf"]
        .as_array()
        .expect("oneOf must be an array")
        .iter()
        .find(|variant| variant["required"][0].as_str() == Some(tag))
        .unwrap_or_else(|| panic!("$defs.{union_def} has no oneOf variant tagged '{tag}'"))
}

/// Recursively asserts the serialized value fits the schema node:
/// every serialized key exists in `properties` wherever the schema is closed
/// (`additionalProperties: false` — catches casing drift), every `required`
/// key is present (optional-when-None fields are only checked if required),
/// and string values respect `enum` lists. `anyOf` here is always the
/// nullable `[null, X]` shape; open objects (no `properties`) are skipped.
fn assert_conforms(root: &Value, schema_node: &Value, value: &Value, path: &str) {
    let node = resolve(root, schema_node);

    if let Some(branches) = node.get("anyOf").and_then(Value::as_array) {
        if value.is_null() {
            return;
        }
        let branch = branches
            .iter()
            .map(|b| resolve(root, b))
            .find(|b| b.get("type").and_then(Value::as_str) != Some("null"))
            .unwrap_or_else(|| panic!("{path}: anyOf has no non-null branch"));
        assert_conforms(root, branch, value, path);
        return;
    }

    match value {
        Value::Object(obj) => {
            let props = node.get("properties").and_then(Value::as_object);
            if node.get("additionalProperties").and_then(Value::as_bool) == Some(false) {
                for key in obj.keys() {
                    assert!(
                        props.is_some_and(|p| p.contains_key(key)),
                        "{path}.{key}: key serialized by Rust but absent from schema properties (wire-format drift)"
                    );
                }
            }
            if let Some(required) = node.get("required").and_then(Value::as_array) {
                for key in required {
                    let key = key.as_str().expect("required entries must be strings");
                    assert!(
                        obj.contains_key(key),
                        "{path}: schema-required key '{key}' missing from Rust serialization"
                    );
                }
            }
            if let Some(props) = props {
                for (key, child) in obj {
                    if let Some(child_schema) = props.get(key) {
                        assert_conforms(root, child_schema, child, &format!("{path}.{key}"));
                    }
                }
            }
        }
        Value::Array(items) => {
            if let Some(item_schema) = node.get("items") {
                for (i, item) in items.iter().enumerate() {
                    assert_conforms(root, item_schema, item, &format!("{path}[{i}]"));
                }
            }
        }
        Value::String(s) => {
            if let Some(allowed) = node.get("enum").and_then(Value::as_array) {
                assert!(
                    allowed.iter().any(|a| a.as_str() == Some(s.as_str())),
                    "{path}: serialized string '{s}' not in schema enum {allowed:?}"
                );
            }
        }
        _ => {}
    }
}

fn wire_tag(payload: &Value) -> String {
    let obj = payload.as_object().expect("payload must be an object");
    assert_eq!(obj.len(), 1, "externally-tagged payload must have exactly one key: {payload}");
    obj.keys().next().expect("tag key").clone()
}

fn assert_payload_conforms(root: &Value, union_def: &str, payload: &Value, expected_tag: &str) {
    let tag = wire_tag(payload);
    assert_eq!(tag, expected_tag, "serde rename drifted from the variant's expected wire tag");
    let variant = variant_schema(root, union_def, &tag);
    assert_conforms(root, variant, payload, &format!("{union_def}.{tag}"));
}

fn assert_tag_sets_match(
    root: &Value,
    union_def: &str,
    rust_tags: &BTreeSet<&str>,
    schema_only_allowlist: &[&str],
) {
    let schema_set = schema_tags(root, union_def);
    for tag in &schema_set {
        assert!(
            rust_tags.contains(tag.as_str()) || schema_only_allowlist.contains(&tag.as_str()),
            "schema {union_def} tag '{tag}' has no Rust exemplar — implement the variant + add an exemplar in conformance.rs (or allowlist a schema-documented platform divergence)"
        );
    }
    for tag in rust_tags {
        assert!(
            schema_set.contains(*tag),
            "Rust {union_def} tag '{tag}' is absent from ipc.schema.json — the schema must land first"
        );
    }
}

/// Exhaustive match (no wildcard arm) — adding a CommandPayload variant fails
/// compilation here until its wire tag + an exemplar below are added.
fn command_tag(payload: &CommandPayload) -> &'static str {
    match payload {
        CommandPayload::StartScan(_) => "startScan",
        CommandPayload::PauseScan(_) => "pauseScan",
        CommandPayload::ResumeScan(_) => "resumeScan",
        CommandPayload::CancelScan(_) => "cancelScan",
        CommandPayload::RequestStatus(_) => "requestStatus",
        CommandPayload::Shutdown(_) => "shutdown",
        CommandPayload::RunFaceClustering(_) => "runFaceClustering",
        CommandPayload::DeepAnalyzeFile(_) => "deepAnalyzeFile",
        CommandPayload::DeepAnalyzeFolder(_) => "deepAnalyzeFolder",
        CommandPayload::DeepAnalyzeAll(_) => "deepAnalyzeAll",
        CommandPayload::DeepAnalyzeCancel(_) => "deepAnalyzeCancel",
        CommandPayload::PrewarmModel(_) => "prewarmModel",
        CommandPayload::CancelPrewarm(_) => "cancelPrewarm",
        CommandPayload::PlanRestructure(_) => "planRestructure",
        CommandPayload::ApplyRestructure(_) => "applyRestructure",
        CommandPayload::ApplyTags(_) => "applyTags",
        CommandPayload::RenameFiles(_) => "renameFiles",
        CommandPayload::TrashFiles(_) => "trashFiles",
        CommandPayload::MergeClusters(_) => "mergeClusters",
        CommandPayload::EmbedTextQuery(_) => "embedTextQuery",
        CommandPayload::RenamePerson(_) => "renamePerson",
        CommandPayload::MarkPersonsAsUnknown(_) => "markPersonsAsUnknown",
        CommandPayload::FindMergeSuggestions(_) => "findMergeSuggestions",
        CommandPayload::MarkPersonsDifferent(_) => "markPersonsDifferent",
        CommandPayload::EmbedImageQuery(_) => "embedImageQuery",
        CommandPayload::RestoreFromTrash(_) => "restoreFromTrash",
        CommandPayload::VerifyCudaPack(_) => "verifyCudaPack",
        CommandPayload::RevertMerge(_) => "revertMerge",
        CommandPayload::WipeLibrary(_) => "wipeLibrary",
        CommandPayload::GenerateVideoThumbnail(_) => "generateVideoThumbnail",
    }
}

/// Exhaustive match (no wildcard arm) — adding an EventPayload variant fails
/// compilation here until its wire tag + an exemplar below are added.
fn event_tag(payload: &EventPayload) -> &'static str {
    match payload {
        EventPayload::Ready(_) => "ready",
        EventPayload::Progress(_) => "progress",
        EventPayload::PhaseChanged(_) => "phaseChanged",
        EventPayload::DiscoveryComplete(_) => "discoveryComplete",
        EventPayload::FileDone(_) => "fileDone",
        EventPayload::BatchSummary(_) => "batchSummary",
        EventPayload::ScanComplete(_) => "scanComplete",
        EventPayload::Error(_) => "error",
        EventPayload::Log(_) => "log",
        EventPayload::FaceClusteringComplete(_) => "faceClusteringComplete",
        EventPayload::DeepAnalyzeStarting(_) => "deepAnalyzeStarting",
        EventPayload::DeepAnalyzeProgress(_) => "deepAnalyzeProgress",
        EventPayload::DeepAnalyzeFileDone(_) => "deepAnalyzeFileDone",
        EventPayload::DeepAnalyzeComplete(_) => "deepAnalyzeComplete",
        EventPayload::ModelDownloadProgress(_) => "modelDownloadProgress",
        EventPayload::QueueState(_) => "queueState",
        EventPayload::RestructurePlan(_) => "restructurePlan",
        EventPayload::RestructureApplyResult(_) => "restructureApplyResult",
        EventPayload::BulkActionResult(_) => "bulkActionResult",
        EventPayload::ClipTextEmbedding(_) => "clipTextEmbedding",
        EventPayload::MergeSuggestions(_) => "mergeSuggestions",
        EventPayload::HardwareReprobed(_) => "hardwareReprobed",
        EventPayload::LibraryWiped(_) => "libraryWiped",
        EventPayload::ThumbnailGenerated(_) => "thumbnailGenerated",
    }
}

fn hardware_info() -> HardwareInfo {
    HardwareInfo {
        gpu_vendor: "nvidia".into(),
        adapter_name: Some("NVIDIA GeForce RTX 2060".into()),
        execution_provider: "directml".into(),
        physical_cpu_cores: 8,
        cuda_pack_present: false,
        openvino_pack_present: false,
        qnn_pack_present: false,
        recommendation: "Install the NVIDIA CUDA Pack for faster inference".into(),
        p_cores: 6,
        e_cores: 8,
        logical_cpu_cores: 20,
        worker_cap: 14,
        ram_total_mb: 32768,
        ram_available_mb: 16384,
        memory_tier: "balanced".into(),
        vram_mb: 6144,
        npu_present: true,
        power_source: "ac".into(),
        battery_percent: Some(80),
        active_profile: "auto".into(),
    }
}

fn restructure_move() -> RestructureMove {
    RestructureMove {
        file_id: 1,
        source: r"C:\Users\adam\Pictures\IMG_0001.jpg".into(),
        destination: r"C:\Users\adam\Pictures\Photos\2024\01\IMG_0001.jpg".into(),
        category: "Photos/2024/01".into(),
        tier: Some("Anchor".into()),
        confidence: "auto".into(),
        reason: Some("Photo from 2024".into()),
    }
}

/// Optional fields are deliberately `Some(...)` so every serializable key is
/// exercised against the schema's `properties`.
fn command_exemplars() -> Vec<CommandPayload> {
    vec![
        CommandPayload::StartScan(StartScanPayload {
            root_path: r"C:\Users\adam\Pictures".into(),
            root_display: Some("Pictures".into()),
            rescan: true,
        }),
        CommandPayload::PauseScan(Empty {}),
        CommandPayload::ResumeScan(Empty {}),
        CommandPayload::CancelScan(Empty {}),
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
            tags_only: true,
            propose_renames: true,
        }),
        CommandPayload::DeepAnalyzeCancel(Empty {}),
        CommandPayload::PrewarmModel(PrewarmModelPayload { model_kind: "arcface".into() }),
        CommandPayload::CancelPrewarm(CancelPrewarmPayload {
            model_kind: Some("arcface".into()),
        }),
        CommandPayload::PlanRestructure(PlanRestructurePayload {
            library_root: r"C:\Users\adam\Pictures".into(),
        }),
        CommandPayload::ApplyRestructure(ApplyRestructurePayload {
            library_root: r"C:\Users\adam\Pictures".into(),
            moves: vec![restructure_move()],
            use_symlinks: true,
        }),
        CommandPayload::ApplyTags(ApplyTagsPayload {
            file_ids: vec![1, 2, 3],
            tags: vec!["hawaii".into(), "sunset".into()],
            mode: TagMode::Replace,
        }),
        CommandPayload::RenameFiles(RenameFilesPayload {
            renames: vec![RenameEntry { file_id: 1, new_name: "Renamed.jpg".into() }],
        }),
        CommandPayload::TrashFiles(TrashFilesPayload { file_ids: vec![1, 2, 3] }),
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
            title: Some("Dr.".into()),
            first_name: Some("Ada".into()),
            middle_name: Some("Byron".into()),
            last_name: Some("Lovelace".into()),
            suffix: Some("Jr.".into()),
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
        }),
    ]
}

/// Optional fields are deliberately `Some(...)` so every serializable key is
/// exercised against the schema's `properties`.
fn event_exemplars() -> Vec<EventPayload> {
    vec![
        EventPayload::Ready(Wrap::new(EngineInfo {
            version: "0.1.0".into(),
            pid: 4242,
            worker_cap: 14,
            physical_memory_gb: 32.0,
            hardware: Some(hardware_info()),
        })),
        EventPayload::Progress(Wrap::new(ScanProgress {
            session_id: "s-1".into(),
            phase: ScanPhase::Tagging,
            total: 1000,
            discovered: 1000,
            processed: 500,
            failed: 1,
            files_per_second: 140.0,
            eta_seconds: Some(3.5),
            resident_mb: 512,
            available_mb: 8192,
        })),
        EventPayload::PhaseChanged(Wrap::new(ScanPhase::PostScan)),
        EventPayload::DiscoveryComplete(DiscoveryCompletePayload { total_files: 1000 }),
        EventPayload::FileDone(Wrap::new(FileDoneEvent {
            path: r"C:\Users\adam\Pictures\IMG_0001.jpg".into(),
            kind: "image".into(),
            total_ms: 7.2,
            failed: true,
            error_message: Some("decode failed".into()),
            skipped_stages: vec!["face_detection".into()],
        })),
        EventPayload::BatchSummary(Wrap::new(BatchSummary {
            batch_index: 1,
            files_in_batch: 64,
            processed_total: 128,
            wall_seconds: 0.5,
            files_per_second: 128.0,
            utilization: 0.9,
            vision_p50_ms: 4.0,
            vision_p95_ms: 9.0,
            clip_p50_ms: 2.0,
            clip_p95_ms: 5.0,
            store_insert_p50_ms: 0.3,
            store_insert_p95_ms: 0.9,
            resident_mb: 512,
            available_mb: 8192,
        })),
        EventPayload::ScanComplete(Wrap::new(ScanComplete {
            session_id: "s-1".into(),
            total_files: 1000,
            processed_files: 999,
            failed_files: 1,
            total_seconds: 7.1,
        })),
        EventPayload::Error(Wrap::new(EngineError {
            kind: "vision_failed".into(),
            message: "inference failed".into(),
            path: Some(r"C:\Users\adam\Pictures\bad.jpg".into()),
            model_kind: Some("mobileclip_s2".into()),
        })),
        EventPayload::Log(Wrap::new(LogLine {
            level: LogLevel::Info,
            message: "hello".into(),
        })),
        EventPayload::FaceClusteringComplete(Wrap::new(FaceClusteringResult {
            person_count: 3,
            face_count: 40,
            unmatched_faces: 2,
            duration_seconds: 1.2,
        })),
        EventPayload::DeepAnalyzeStarting(Wrap::new(DeepAnalyzeStarting {
            model_kind: "qwen2_5_vl_7b".into(),
            phase: DeepAnalyzeStartingPhase::LoadingModel,
            message: "Loading model".into(),
        })),
        EventPayload::DeepAnalyzeProgress(Wrap::new(DeepAnalyzeProgress {
            processed: 10,
            total: 100,
            eta_seconds: Some(90.0),
            current_path: Some(r"C:\Users\adam\Pictures\IMG_0002.jpg".into()),
            model_kind: "qwen2_5_vl_7b".into(),
            current_caption: Some("A sunset over".into()),
        })),
        EventPayload::DeepAnalyzeFileDone(Wrap::new(DeepAnalyzeFileDone {
            file_id: 42,
            description: "A sunset over the ocean".into(),
            proposed_name: Some("Sunset Over Ocean.jpg".into()),
            model_kind: "qwen2_5_vl_7b".into(),
        })),
        EventPayload::DeepAnalyzeComplete(Wrap::new(DeepAnalyzeComplete {
            processed: 100,
            failed: 0,
            total_seconds: 60.0,
            model_kind: "qwen2_5_vl_7b".into(),
            cancelled: false,
        })),
        EventPayload::ModelDownloadProgress(Wrap::new(ModelDownloadProgress {
            model_kind: "qwen2_5_vl_7b".into(),
            fraction: 0.5,
            message: "Downloading".into(),
            bytes_done: Some(1024),
            total_bytes: Some(2048),
        })),
        EventPayload::QueueState(Wrap::new(QueueState {
            running: Some(QueuedJob {
                id: "job-1".into(),
                category: JobCategory::Scan,
                title: "Scanning Pictures".into(),
                eta_seconds: Some(12.0),
            }),
            pending: vec![QueuedJob {
                id: "job-2".into(),
                category: JobCategory::DeepAnalyze,
                title: "Deep Analyze".into(),
                eta_seconds: Some(120.0),
            }],
            total_eta_seconds: Some(132.0),
        })),
        EventPayload::RestructurePlan(Wrap::new(RestructurePlan {
            library_root: r"C:\Users\adam\Pictures".into(),
            moves: vec![restructure_move()],
            category_counts: vec![RestructureCategoryCount {
                category: "Photos/2024/01".into(),
                count: 1,
            }],
            folder_classifications: Some(FolderClassificationCounts {
                anchor_folders: 1,
                mixed_folders: 2,
                junk_folders: 3,
            }),
        })),
        EventPayload::RestructureApplyResult(Wrap::new(RestructureApplyResult {
            applied: 10,
            failed: 1,
            privilege_error: Some("Developer Mode required for symlinks".into()),
        })),
        EventPayload::BulkActionResult(Wrap::new(BulkActionResult {
            action: "trashFiles:00000000-0000-0000-0000-000000000000".into(),
            succeeded: 2,
            failed: 1,
            messages: vec![BulkActionItem {
                file_id: Some(1),
                ok: false,
                message: Some("file locked".into()),
            }],
        })),
        EventPayload::ClipTextEmbedding(Wrap::new(ClipTextEmbedding {
            query_id: "q-1".into(),
            query: "sunset at the beach".into(),
            embedding: vec![0.0; 512],
        })),
        EventPayload::MergeSuggestions(Wrap::new(MergeSuggestions {
            pairs: vec![MergeSuggestion {
                source_person_id: 1,
                destination_person_id: 2,
                similarity: 0.62,
                source_anchor_face_id: 10,
                destination_anchor_face_id: 20,
                source_member_count: 5,
                destination_member_count: 7,
            }],
        })),
        EventPayload::HardwareReprobed(Wrap::new(HardwareReprobed {
            hardware: hardware_info(),
            diagnostics: Some("cuDNN not found in PATH".into()),
        })),
        EventPayload::LibraryWiped(Wrap::new(LibraryWiped {
            ok: false,
            message: Some("tags table truncate failed".into()),
        })),
        EventPayload::ThumbnailGenerated(Wrap::new(ThumbnailGenerated {
            path: r"C:\Users\adam\Videos\clip.mp4".into(),
            modified_at: Some(1_700_000_000.0),
            bytes: "/9j/4AAQSkZJRg==".into(),
        })),
    ]
}

#[test]
fn every_command_exemplar_matches_schema_shape() {
    let root = load_schema();
    for payload in command_exemplars() {
        let expected = command_tag(&payload);
        let cmd = IpcCommand { id: "conf-1".into(), payload };
        let v = serde_json::to_value(&cmd)
            .unwrap_or_else(|e| panic!("encode failed for {expected}: {e}"));
        assert_conforms(&root, &root["$defs"]["IPCCommand"], &v, "IPCCommand");
        assert_payload_conforms(&root, "CommandPayload", &v["payload"], expected);
    }
}

#[test]
fn every_event_exemplar_matches_schema_shape() {
    let root = load_schema();
    for payload in event_exemplars() {
        let expected = event_tag(&payload);
        let evt = IpcEvent::now(payload);
        let v = serde_json::to_value(&evt)
            .unwrap_or_else(|e| panic!("encode failed for {expected}: {e}"));
        assert_conforms(&root, &root["$defs"]["IPCEvent"], &v, "IPCEvent");
        assert_payload_conforms(&root, "EventPayload", &v["payload"], expected);
    }
}

/// Negative self-test: the checker must reject the exact L1 drift class
/// (`fileId` instead of `fileID`), or this suite guards nothing.
#[test]
#[should_panic(expected = "wire-format drift")]
fn checker_rejects_wrong_cased_key() {
    let root = load_schema();
    let bad = serde_json::json!({ "deepAnalyzeFile": { "fileId": 42, "modelKind": "m" } });
    assert_payload_conforms(&root, "CommandPayload", &bad, "deepAnalyzeFile");
}

#[test]
#[should_panic(expected = "schema-required key")]
fn checker_rejects_missing_required_key() {
    let root = load_schema();
    let bad = serde_json::json!({ "deepAnalyzeFile": { "fileID": 42 } });
    assert_payload_conforms(&root, "CommandPayload", &bad, "deepAnalyzeFile");
}

#[test]
fn command_tag_set_matches_schema() {
    let root = load_schema();
    let exemplars = command_exemplars();
    let rust_tags: BTreeSet<&str> = exemplars.iter().map(command_tag).collect();
    assert_eq!(rust_tags.len(), exemplars.len(), "duplicate command exemplar tags");
    assert_tag_sets_match(&root, "CommandPayload", &rust_tags, SCHEMA_ONLY_COMMAND_TAGS);
}

#[test]
fn event_tag_set_matches_schema() {
    let root = load_schema();
    let exemplars = event_exemplars();
    let rust_tags: BTreeSet<&str> = exemplars.iter().map(event_tag).collect();
    assert_eq!(rust_tags.len(), exemplars.len(), "duplicate event exemplar tags");
    assert_tag_sets_match(&root, "EventPayload", &rust_tags, SCHEMA_ONLY_EVENT_TAGS);
}
