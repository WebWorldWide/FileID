//! IPC protocol types — Rust mirror of `shared/ipc-schema/ipc.schema.json`.
//!
//! The wire format is the externally-tagged shape Swift's auto-synthesized
//! Codable produces: `{"caseName": <payload>}` for variants with a payload,
//! `{"caseName": {}}` for variants without. Cases that have a single unnamed
//! associated value (Swift `case ready(EngineInfo)`) wrap the payload in
//! `{"_0": <value>}`. Serde's `tag` attribute can't model that exactly, so
//! the union enums use `#[serde(rename_all_fields = "camelCase")]` plus
//! manual variant shapes that mirror the schema byte-for-byte.
//!
//! Edit this file in lockstep with `ipc.schema.json`. The two MUST agree.

use serde::{Deserialize, Serialize};

pub mod sink;

// ─── Envelopes ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcCommand {
    pub id: String,
    pub payload: CommandPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcEvent {
    /// ISO8601 timestamp. We let chrono handle the encoding via its `serde` feature.
    pub t: chrono::DateTime<chrono::Utc>,
    pub payload: EventPayload,
}

impl IpcEvent {
    pub fn now(payload: EventPayload) -> Self {
        Self { t: chrono::Utc::now(), payload }
    }
}

// ─── Command payload ────────────────────────────────────────────────────────
//
// Externally-tagged enum. Each variant is a struct (object payload) or unit
// (encoded as `{}`). Empty payloads use `serde(rename = "...")` + a unit
// struct wrapper because serde's pure unit variant with externally-tagged
// representation encodes as a bare string `"caseName"` — that's NOT what
// Swift's auto-synthesis emits. Swift emits `{"caseName": {}}`. So every
// "payload-less" variant carries an empty struct here.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CommandPayload {
    #[serde(rename = "startScan")]
    StartScan(StartScanPayload),

    #[serde(rename = "pauseScan")]
    PauseScan(Empty),
    #[serde(rename = "resumeScan")]
    ResumeScan(Empty),
    #[serde(rename = "cancelScan")]
    CancelScan(Empty),
    #[serde(rename = "requestStatus")]
    RequestStatus(Empty),
    #[serde(rename = "shutdown")]
    Shutdown(Empty),
    #[serde(rename = "runFaceClustering")]
    RunFaceClustering(Empty),

    #[serde(rename = "deepAnalyzeFile")]
    DeepAnalyzeFile(DeepAnalyzeFilePayload),
    #[serde(rename = "deepAnalyzeFolder")]
    DeepAnalyzeFolder(DeepAnalyzeFolderPayload),
    #[serde(rename = "deepAnalyzeAll")]
    DeepAnalyzeAll(DeepAnalyzeAllPayload),
    #[serde(rename = "deepAnalyzeCancel")]
    DeepAnalyzeCancel(Empty),

    #[serde(rename = "prewarmModel")]
    PrewarmModel(PrewarmModelPayload),
    #[serde(rename = "cancelPrewarm")]
    CancelPrewarm(Empty),
}

/// Empty object — `{}`. Serde encodes a unit struct as `null`, which is wrong;
/// an empty struct with no fields encodes as `{}` like Swift produces.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Empty {}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartScanPayload {
    /// Absolute filesystem path to the folder root to scan.
    pub root_path: String,
    /// Optional human-readable label; if absent, callers default to root_path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_display: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeepAnalyzeFilePayload {
    #[serde(rename = "fileID")]
    pub file_id: i64,
    pub model_kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeepAnalyzeFolderPayload {
    pub path_prefix: String,
    pub model_kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeepAnalyzeAllPayload {
    pub model_kind: String,
    pub skip_existing: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrewarmModelPayload {
    pub model_kind: String,
}

// ─── Event payload ──────────────────────────────────────────────────────────
//
// Variants whose Swift case has a single unnamed associated value encode as
// `{"_0": <payload>}` after the outer `{"variantName": ...}` wrapper. We
// model that with a `Wrap<T>` newtype so each variant carries a cleanly
// typed inner struct.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EventPayload {
    #[serde(rename = "ready")]
    Ready(Wrap<EngineInfo>),

    #[serde(rename = "progress")]
    Progress(Wrap<ScanProgress>),

    #[serde(rename = "phaseChanged")]
    PhaseChanged(Wrap<ScanPhase>),

    #[serde(rename = "discoveryComplete")]
    DiscoveryComplete(DiscoveryCompletePayload),

    #[serde(rename = "fileDone")]
    FileDone(Wrap<FileDoneEvent>),

    #[serde(rename = "batchSummary")]
    BatchSummary(Wrap<BatchSummary>),

    #[serde(rename = "scanComplete")]
    ScanComplete(Wrap<ScanComplete>),

    #[serde(rename = "error")]
    Error(Wrap<EngineError>),

    #[serde(rename = "log")]
    Log(Wrap<LogLine>),

    #[serde(rename = "faceClusteringComplete")]
    FaceClusteringComplete(Wrap<FaceClusteringResult>),

    #[serde(rename = "deepAnalyzeStarting")]
    DeepAnalyzeStarting(Wrap<DeepAnalyzeStarting>),

    #[serde(rename = "deepAnalyzeProgress")]
    DeepAnalyzeProgress(Wrap<DeepAnalyzeProgress>),

    #[serde(rename = "deepAnalyzeFileDone")]
    DeepAnalyzeFileDone(Wrap<DeepAnalyzeFileDone>),

    #[serde(rename = "deepAnalyzeComplete")]
    DeepAnalyzeComplete(Wrap<DeepAnalyzeComplete>),

    #[serde(rename = "modelDownloadProgress")]
    ModelDownloadProgress(Wrap<ModelDownloadProgress>),

    #[serde(rename = "queueState")]
    QueueState(Wrap<QueueState>),
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineInfo {
    pub version: String,
    pub pid: i32,
    pub worker_cap: u32,
    #[serde(rename = "physicalMemoryGB")]
    pub physical_memory_gb: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ScanPhase {
    Idle,
    Discovering,
    Tagging,
    PostScan,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanProgress {
    #[serde(rename = "sessionID")]
    pub session_id: String,
    pub phase: ScanPhase,
    pub total: u64,
    pub discovered: u64,
    pub processed: u64,
    pub failed: u64,
    pub files_per_second: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta_seconds: Option<f64>,
    #[serde(rename = "residentMB")]
    pub resident_mb: u64,
    #[serde(rename = "availableMB")]
    pub available_mb: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryCompletePayload {
    #[serde(rename = "totalFiles")]
    pub total_files: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDoneEvent {
    pub path: String,
    pub kind: String,
    pub total_ms: f64,
    pub failed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchSummary {
    pub batch_index: u32,
    pub files_in_batch: u32,
    pub processed_total: u64,
    pub wall_seconds: f64,
    pub files_per_second: f64,
    pub utilization: f64,
    #[serde(rename = "visionP50Ms")]
    pub vision_p50_ms: f64,
    #[serde(rename = "visionP95Ms")]
    pub vision_p95_ms: f64,
    #[serde(rename = "clipP50Ms")]
    pub clip_p50_ms: f64,
    #[serde(rename = "clipP95Ms")]
    pub clip_p95_ms: f64,
    #[serde(rename = "storeInsertP50Ms")]
    pub store_insert_p50_ms: f64,
    #[serde(rename = "storeInsertP95Ms")]
    pub store_insert_p95_ms: f64,
    #[serde(rename = "residentMB")]
    pub resident_mb: u64,
    #[serde(rename = "availableMB")]
    pub available_mb: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanComplete {
    #[serde(rename = "sessionID")]
    pub session_id: String,
    pub total_files: u64,
    pub processed_files: u64,
    pub failed_files: u64,
    pub total_seconds: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineError {
    /// Stable kind code: `discovery_failed`, `vision_failed`, `db_failed`,
    /// `model_load_failed`, `ipc_unknown_command`, `unknown`, ...
    pub kind: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogLine {
    pub level: LogLevel,
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel { Debug, Info, Warn, Error }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FaceClusteringResult {
    pub person_count: u32,
    pub face_count: u64,
    pub unmatched_faces: u64,
    pub duration_seconds: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeepAnalyzeStarting {
    pub model_kind: String,
    pub phase: DeepAnalyzeStartingPhase,
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DeepAnalyzeStartingPhase {
    Queued,
    LoadingModel,
    ResolvingTargets,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeepAnalyzeProgress {
    pub processed: u64,
    pub total: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta_seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_path: Option<String>,
    pub model_kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeepAnalyzeFileDone {
    #[serde(rename = "fileID")]
    pub file_id: i64,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_name: Option<String>,
    pub model_kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeepAnalyzeComplete {
    pub processed: u64,
    pub failed: u64,
    pub total_seconds: f64,
    pub model_kind: String,
    pub cancelled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelDownloadProgress {
    pub model_kind: String,
    pub fraction: f64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_done: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub running: Option<QueuedJob>,
    pub pending: Vec<QueuedJob>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_eta_seconds: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedJob {
    pub id: String,
    pub category: JobCategory,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta_seconds: Option<f64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum JobCategory { Scan, FaceCluster, DeepAnalyze }

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

    /// startScan sent by the app must round-trip cleanly with the new
    /// `rootPath` field (not the legacy `rootBookmark`).
    #[test]
    fn start_scan_command_roundtrip() {
        let cmd = IpcCommand {
            id: "test-1".into(),
            payload: CommandPayload::StartScan(StartScanPayload {
                root_path: r"C:\Users\adam\Pictures".into(),
                root_display: Some("Pictures".into()),
            }),
        };
        let j = serde_json::to_string(&cmd).unwrap();
        let parsed: IpcCommand = serde_json::from_str(&j).unwrap();
        match parsed.payload {
            CommandPayload::StartScan(p) => {
                assert_eq!(p.root_path, r"C:\Users\adam\Pictures");
                assert_eq!(p.root_display.as_deref(), Some("Pictures"));
            }
            _ => panic!("unexpected variant"),
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

    #[test]
    fn scan_phase_enum_lowercased() {
        let j = serde_json::to_string(&ScanPhase::Discovering).unwrap();
        assert_eq!(j, "\"discovering\"");
        let j2 = serde_json::to_string(&ScanPhase::PostScan).unwrap();
        assert_eq!(j2, "\"postScan\"");
    }
}
