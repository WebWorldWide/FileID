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
pub(crate) mod bounded_read;

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

    #[serde(rename = "planRestructure")]
    PlanRestructure(PlanRestructurePayload),
    #[serde(rename = "applyRestructure")]
    ApplyRestructure(ApplyRestructurePayload),

    /// Bulk-tag a set of files. Tags persist via shell::tags sidecar +
    /// the DB `tags` table.
    #[serde(rename = "applyTags")]
    ApplyTags(ApplyTagsPayload),

    /// Bulk-rename a set of files. Each entry is (file_id, new_name).
    /// Engine moves on disk + updates DB row in same tx; emits
    /// renameResult per file.
    #[serde(rename = "renameFiles")]
    RenameFiles(RenameFilesPayload),

    /// Trash a set of files via shell::trash IFileOperation. 8-parallel
    /// COM-apartment pool. Emits trashResult per file.
    #[serde(rename = "trashFiles")]
    TrashFiles(TrashFilesPayload),

    /// Merge two person clusters. All face_prints with person_id = src
    /// are reassigned to dst; src person row is deleted.
    #[serde(rename = "mergeClusters")]
    MergeClusters(MergeClustersPayload),

    /// Run CLIP text encoder on a free-text query. Engine emits a
    /// `clipTextEmbedding` event with the 512-d float32 vector so the
    /// app can dot-product it against `clip_embeddings` in-process.
    #[serde(rename = "embedTextQuery")]
    EmbedTextQuery(EmbedTextQueryPayload),

    /// Save the structured-name fields for a person cluster. Routed
    /// through the engine's single-writer DB connection so concurrent
    /// edits don't contend SQLite locks.
    #[serde(rename = "renamePerson")]
    RenamePerson(RenamePersonPayload),

    /// FEAT-CRIT-1: bulk mark-as-unknown for multi-select People mode.
    /// Sets `persons.is_unknown = 1` and clears name fields for every id.
    #[serde(rename = "markPersonsAsUnknown")]
    MarkPersonsAsUnknown(MarkPersonsAsUnknownPayload),

    /// Find merge-candidate cluster pairs by ArcFace cosine similarity in
    /// the uncertain band 0.45–0.70. Engine emits `mergeSuggestions`.
    #[serde(rename = "findMergeSuggestions")]
    FindMergeSuggestions(Empty),

    /// Pull a file's stored CLIP image embedding from the DB and emit
    /// it via `clipTextEmbedding` (reusing the same channel — the app's
    /// SemanticSearchAsync doesn't care whether the seed is from text or
    /// from an image). Used by "Find similar" right-click action.
    #[serde(rename = "embedImageQuery")]
    EmbedImageQuery(EmbedImageQueryPayload),

    /// Undo a previous trashFiles call. Looks up the trashed paths in
    /// the trash_log sidecar JSON, calls IFileOperation.MoveItems to
    /// restore them from the Recycle Bin, re-inserts DB rows.
    #[serde(rename = "restoreFromTrash")]
    RestoreFromTrash(RestoreFromTrashPayload),

    /// Re-probe CUDA Toolkit + cuDNN availability without restarting the
    /// engine. After the user manually installs cuDNN from NVIDIA's site,
    /// the Settings → Performance "Verify install" button sends this; the
    /// engine replies with a `hardwareReprobed` event carrying fresh
    /// `HardwareInfo` + an optional `diagnostics` string explaining why
    /// a negative probe came back negative.
    #[serde(rename = "verifyCudaPack")]
    VerifyCudaPack(Empty),

    /// Undo a mergeClusters call. App passes the original (face_id,
    /// previous_person_id) pairs it captured at merge time; engine
    /// re-creates the source person row + reassigns the faces.
    #[serde(rename = "revertMerge")]
    RevertMerge(RevertMergePayload),
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
    /// When true, force every file to be reprocessed even if
    /// `scanned_at >= modified_unix` in the DB. Default false = incremental
    /// rescan (skip already-current files).
    #[serde(default)]
    pub rescan: bool,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanRestructurePayload {
    /// Absolute path of the user's library root. Every proposed
    /// destination is canonicalized + verified to fall inside this root
    /// (path-traversal guard before apply).
    pub library_root: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyRestructurePayload {
    pub library_root: String,
    pub moves: Vec<RestructureMove>,
    /// `false` (default): real `MoveFileExW` move on disk + DB update.
    /// `true`: create a `CreateSymbolicLinkW` next to the original so the
    /// user can preview the layout without touching their files.
    #[serde(default)]
    pub use_symlinks: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestructureMove {
    pub file_id: i64,
    pub source: String,
    pub destination: String,
    pub category: String,
    /// Per-move tier — "Anchor" / "Mixed" / "Junk", derived from the
    /// source folder's `classify_folders` classification. None for older
    /// engines; the app falls back to its local heuristic when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyTagsPayload {
    pub file_ids: Vec<i64>,
    pub tags: Vec<String>,
    /// "add" (default) appends; "replace" overwrites.
    #[serde(default)]
    pub mode: TagMode,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TagMode {
    #[default]
    Add,
    Replace,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameFilesPayload {
    pub renames: Vec<RenameEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameEntry {
    pub file_id: i64,
    /// New filename only (no directory components). Engine resolves the
    /// destination as `dirname(current) + new_name`.
    pub new_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashFilesPayload {
    pub file_ids: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeClustersPayload {
    pub source_person_id: i64,
    pub destination_person_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbedTextQueryPayload {
    pub query: String,
    /// Echoed back on the response event so the caller can correlate
    /// (multiple in-flight queries won't get crossed).
    pub query_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbedImageQueryPayload {
    pub file_id: i64,
    pub query_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreFromTrashPayload {
    /// Identifier from the trash_log JSON (UUID emitted by trashFiles).
    pub batch_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RevertMergePayload {
    pub source_person_id: i64,
    pub destination_person_id: i64,
    pub face_ids_to_revert: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenamePersonPayload {
    pub person_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub middle_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
}

/// FEAT-CRIT-1 payload for bulk mark-as-unknown.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkPersonsAsUnknownPayload {
    pub person_ids: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeSuggestion {
    pub source_person_id: i64,
    pub destination_person_id: i64,
    pub similarity: f32,
    pub source_anchor_face_id: i64,
    pub destination_anchor_face_id: i64,
    pub source_member_count: i64,
    pub destination_member_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeSuggestions {
    pub pairs: Vec<MergeSuggestion>,
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

    #[serde(rename = "restructurePlan")]
    RestructurePlan(Wrap<RestructurePlan>),

    #[serde(rename = "restructureApplyResult")]
    RestructureApplyResult(Wrap<RestructureApplyResult>),

    #[serde(rename = "bulkActionResult")]
    BulkActionResult(Wrap<BulkActionResult>),

    #[serde(rename = "clipTextEmbedding")]
    ClipTextEmbedding(Wrap<ClipTextEmbedding>),

    #[serde(rename = "mergeSuggestions")]
    MergeSuggestions(Wrap<MergeSuggestions>),

    /// Reply to a `verifyCudaPack` command. Carries fresh `HardwareInfo`
    /// (so the Settings card can flip to ✓ if the user just installed
    /// cuDNN) + an optional `diagnostics` string with human-readable
    /// details about why a negative probe came back negative.
    #[serde(rename = "hardwareReprobed")]
    HardwareReprobed(Wrap<HardwareReprobed>),
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
    /// CPU + GPU detection result the engine made on startup. The app's
    /// Settings tab surfaces this so the user knows which acceleration
    /// path is in use, and which Performance Pack would unlock more.
    /// Optional so older clients of this schema don't break.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hardware: Option<HardwareInfo>,
}

/// Reply payload for the `verifyCudaPack` command. Mirrors the EngineInfo's
/// `hardware` field shape so the C# side can reuse `HardwareInfo`.
/// `diagnostics` is a non-PII human-readable explanation when
/// `hardware.cuda_pack_present == false`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HardwareReprobed {
    pub hardware: HardwareInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<String>,
}

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
    // ─── V15.9 adaptive-utilization diagnostics (Issue 3). All optional
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
    /// Stages skipped because the model wasn't loaded (e.g. "face_detection").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped_stages: Vec<String>,
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
#[serde(rename_all = "camelCase")]
pub struct EngineError {
    /// Stable kind code: `discovery_failed`, `vision_failed`, `db_failed`,
    /// `model_load_failed`, `model_download_failed`, `pack_not_available`,
    /// `ipc_unknown_command`, `unknown`, ...
    pub kind: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// For errors that pertain to a specific model install/download, the
    /// model id (e.g. `mobileclip_s2`, `cuda_pack_x64`). Lets the app route
    /// the error to the right install slot without fragile path-string
    /// matching — pack paths and model paths can collide on substrings
    /// (e.g. `cuda` appears in both the CUDA pack path and any error
    /// message that mentions cuda.zip).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_kind: Option<String>,
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
    /// Partial caption text accumulated from per-token streaming. The
    /// engine throttles emissions to every 250 ms so a 50-tok/s VLM
    /// doesn't spam the wire. Empty for non-token progress events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_caption: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestructurePlan {
    pub library_root: String,
    pub moves: Vec<RestructureMove>,
    pub category_counts: Vec<RestructureCategoryCount>,
    /// Engine-authoritative folder classification — Anchor / Mixed / Junk
    /// counts per RestructurePlan. None on older plans.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_classifications: Option<FolderClassificationCounts>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderClassificationCounts {
    pub anchor_folders: u32,
    pub mixed_folders: u32,
    pub junk_folders: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestructureCategoryCount {
    pub category: String,
    pub count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestructureApplyResult {
    pub applied: u32,
    pub failed: u32,
    /// Empty unless the user opted into symlink mode AND the call lacked
    /// SeCreateSymbolicLinkPrivilege (Developer Mode off, non-admin shell).
    /// Surfaces a clear "enable Developer Mode or run as admin" message
    /// to the user via a one-shot dialog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub privilege_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkActionResult {
    /// "applyTags" | "renameFiles" | "trashFiles" | "mergeClusters".
    pub action: String,
    pub succeeded: u32,
    pub failed: u32,
    pub messages: Vec<BulkActionItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkActionItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_id: Option<i64>,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipTextEmbedding {
    pub query_id: String,
    pub query: String,
    /// 512-d L2-normalized float32 embedding from the CLIP text encoder.
    /// App dot-products this against `clip_embeddings` to rank Library
    /// rows by semantic similarity.
    pub embedding: Vec<f32>,
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

    /// startScan sent by the app must round-trip cleanly with the new
    /// `rootPath` field (not the legacy `rootBookmark`).
    #[test]
    fn start_scan_command_roundtrip() {
        let cmd = IpcCommand {
            id: "test-1".into(),
            payload: CommandPayload::StartScan(StartScanPayload {
                root_path: r"C:\Users\adam\Pictures".into(),
                root_display: Some("Pictures".into()),
                rescan: false,
            }),
        };
        let j = serde_json::to_string(&cmd).unwrap();
        let parsed: IpcCommand = serde_json::from_str(&j).unwrap();
        match parsed.payload {
            CommandPayload::StartScan(p) => {
                assert_eq!(p.root_path, r"C:\Users\adam\Pictures");
                assert_eq!(p.root_display.as_deref(), Some("Pictures"));
            }
            other => panic!("expected StartScan variant, got {other:?}"),
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
            CommandPayload::StartScan(StartScanPayload {
                root_path: r"C:\Users\adam\Pictures".into(),
                root_display: Some("Pictures".into()),
                rescan: false,
            }),
            CommandPayload::PauseScan(Empty {}),
            CommandPayload::ResumeScan(Empty {}),
            CommandPayload::CancelScan(Empty {}),
            CommandPayload::RequestStatus(Empty {}),
            CommandPayload::Shutdown(Empty {}),
            CommandPayload::RunFaceClustering(Empty {}),
            CommandPayload::DeepAnalyzeFile(DeepAnalyzeFilePayload {
                file_id: 42,
                model_kind: "qwen2_5_vl_3b".into(),
            }),
            CommandPayload::DeepAnalyzeFolder(DeepAnalyzeFolderPayload {
                path_prefix: r"C:\Users\adam\Pictures\2024".into(),
                model_kind: "qwen2_5_vl_3b".into(),
            }),
            CommandPayload::DeepAnalyzeAll(DeepAnalyzeAllPayload {
                model_kind: "qwen2_5_vl_3b".into(),
                skip_existing: true,
            }),
            CommandPayload::DeepAnalyzeCancel(Empty {}),
            CommandPayload::PrewarmModel(PrewarmModelPayload {
                model_kind: "arcface".into(),
            }),
            CommandPayload::CancelPrewarm(Empty {}),
            CommandPayload::PlanRestructure(PlanRestructurePayload {
                library_root: r"C:\Users\adam\Pictures".into(),
            }),
            CommandPayload::ApplyRestructure(ApplyRestructurePayload {
                library_root: r"C:\Users\adam\Pictures".into(),
                moves: vec![RestructureMove {
                    file_id: 1,
                    source: r"C:\Users\adam\Pictures\IMG_0001.jpg".into(),
                    destination: r"C:\Users\adam\Pictures\Photos\2024\01\IMG_0001.jpg".into(),
                    category: "Photos/2024/01".into(),
                    tier: Some("Anchor".into()),
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
                    root_display: None,
                    rescan: false,
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
