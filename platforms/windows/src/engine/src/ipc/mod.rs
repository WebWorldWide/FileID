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
#[cfg(test)]
mod conformance;

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
    CancelPrewarm(CancelPrewarmPayload),

    #[serde(rename = "planRestructure")]
    PlanRestructure(PlanRestructurePayload),
    #[serde(rename = "applyRestructure")]
    ApplyRestructure(ApplyRestructurePayload),
    /// Reverse the most recent applyRestructure: move every file the last run
    /// relocated back (the engine replays its on-disk undo journal). Reply is a
    /// RestructureApplyResult (applied = files moved back). (RESTRUCTURE.md §6)
    #[serde(rename = "undoRestructure")]
    UndoRestructure(UndoRestructurePayload),

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

    /// Record a user "different people" verdict for a suggested pair so
    /// findMergeSuggestions stops re-suggesting it. Routed through the
    /// engine's single-writer DB connection (the app must never open its own
    /// writer) and keyed on stable anchor face ids so it survives re-cluster.
    #[serde(rename = "markPersonsDifferent")]
    MarkPersonsDifferent(MarkPersonsDifferentPayload),

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

    /// Wipe all learned library state (tags, faces, captions, embeddings)
    /// in-process on the engine's writer connection, then reply `libraryWiped`.
    /// The app uses this instead of deleting `fileid.sqlite` itself, which
    /// races the OS file-lock the engine still holds just after process exit.
    #[serde(rename = "wipeLibrary")]
    WipeLibrary(Empty),

    /// Generate a 192px JPEG thumbnail for a video on demand. The engine
    /// extracts a 25%-duration keyframe, resizes it (long side = 192,
    /// aspect-preserved), JPEG-encodes + base64-encodes it, and replies with a
    /// `thumbnailGenerated` event. `modifiedAt` is the file's modified-unix
    /// time, carried through so the app can key its thumbnail cache.
    #[serde(rename = "generateVideoThumbnail")]
    GenerateVideoThumbnail(GenerateVideoThumbnailPayload),

    /// Remove cataloged rows under the given excluded folders immediately
    /// (files on disk untouched; cascades tags/faces/captions/embeddings).
    /// Sent when the user adds an exclusion so the Library reflects it without
    /// waiting for a rescan. Replies with a `bulkActionResult`
    /// (action `purgeExcluded`, succeeded = purged row count).
    #[serde(rename = "purgeExcluded")]
    PurgeExcluded(PurgeExcludedPayload),
}

const MAX_BULK_ITEMS: usize = 100_000;
const MAX_RESTRUCTURE_MOVES: usize = 250_000;
const MAX_EXCLUDED_PATHS: usize = 10_000;
const MAX_TAGS_PER_COMMAND: usize = 1_024;
const MAX_APPLY_TAG_OPERATIONS: usize = 100_000;
const MAX_EXACT_TRASH_BYTES: u64 = 64 * 1024 * 1024 * 1024;

pub(crate) fn normalize_and_validate_command(payload: &mut CommandPayload) -> Result<(), String> {
    fn check_len(field: &str, len: usize, max: usize) -> Result<(), String> {
        if len > max {
            Err(format!("{field} contains {len} items; maximum is {max}"))
        } else {
            Ok(())
        }
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
            if let Some(paths) = &mut payload.excluded_paths {
                check_len("startScan.excludedPaths", paths.len(), MAX_EXCLUDED_PATHS)?;
                dedupe_strings(paths);
            }
        }
        CommandPayload::PurgeExcluded(payload) => {
            check_len("purgeExcluded.excludedPaths", payload.excluded_paths.len(), MAX_EXCLUDED_PATHS)?;
            dedupe_strings(&mut payload.excluded_paths);
        }
        CommandPayload::ApplyRestructure(payload) => {
            check_len("applyRestructure.moves", payload.moves.len(), MAX_RESTRUCTURE_MOVES)?;
            let mut seen = std::collections::HashSet::with_capacity(payload.moves.len());
            payload.moves.retain(|entry| seen.insert(entry.file_id));
        }
        CommandPayload::ApplyTags(payload) => {
            check_len("applyTags.fileIDs", payload.file_ids.len(), MAX_BULK_ITEMS)?;
            check_len("applyTags.tags", payload.tags.len(), MAX_TAGS_PER_COMMAND)?;
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
    /// Folder subtrees to prune from the walk. Absent/None = no exclusions.
    /// Entries outside `root_path` (or equal to it) are ignored; rows already
    /// cataloged under an excluded path are purged at scan start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excluded_paths: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PurgeExcludedPayload {
    pub excluded_paths: Vec<String>,
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
    /// Tags-only fast path (background auto-tag): one VLM call/file instead of
    /// three. Defaults to false (manual Deep Analyze = full caption + rename +
    /// tags). `#[serde(default)]` keeps older clients that omit the field valid.
    #[serde(default)]
    pub tags_only: bool,
    /// Propose smart filenames during the full pass. Defaults to true (the
    /// "Propose renames" checkbox is ticked by default); set false for
    /// caption + tags without the rename VLM call. Ignored when tags_only.
    #[serde(default = "default_true")]
    pub propose_renames: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrewarmModelPayload {
    pub model_kind: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelPrewarmPayload {
    /// Which model's download to cancel. `None`/absent cancels ALL in-flight
    /// prewarms (back-compat with the original payload-less `cancelPrewarm`).
    #[serde(default)]
    pub model_kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanRestructurePayload {
    /// Absolute path of the user's library root. Every proposed
    /// destination is canonicalized + verified to fall inside this root
    /// (path-traversal guard before apply).
    pub library_root: String,
    /// Opt in to a bounded preview backed by an engine-owned plan spool. Old
    /// clients omit this and retain the legacy full-plan response.
    #[serde(default, skip_serializing_if = "is_false")]
    pub supports_paged_plans: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyRestructurePayload {
    pub library_root: String,
    /// Apply an engine-owned paged plan in full. `moves` is empty in this mode.
    #[serde(default, rename = "planID", skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    #[serde(default)]
    pub moves: Vec<RestructureMove>,
    /// `false` (default): real `MoveFileExW` move on disk + DB update.
    /// `true`: create a `CreateSymbolicLinkW` next to the original so the
    /// user can preview the layout without touching their files.
    #[serde(default)]
    pub use_symlinks: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoRestructurePayload {
    /// Same library root the apply used — the destinations the undo writes back
    /// to are containment-checked against it. (R2)
    pub library_root: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestructureMove {
    #[serde(rename = "fileID")]
    pub file_id: i64,
    pub source: String,
    pub destination: String,
    pub category: String,
    /// Per-move tier — "Anchor" / "Mixed" / "Junk", derived from the
    /// source folder's `classify_folders` classification. None for older
    /// engines; the app falls back to its local heuristic when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    /// Butler confidence band — "auto" / "review" / "ask" (RESTRUCTURE.md §6).
    /// Drives the app's auto-file / review / ask grouping. Empty on older engines.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub confidence: String,
    /// Plain-language "why filed here" shown in the drill-down. None when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyTagsPayload {
    #[serde(rename = "fileIDs")]
    pub file_ids: Vec<i64>,
    pub tags: Vec<String>,
    /// "add" (default) appends; "replace" overwrites.
    #[serde(default)]
    pub mode: TagMode,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TagMode {
    #[default]
    Add,
    Replace,
    /// Delete each named tag from the selection's `source='user'` rows.
    /// Tags not present on a given file are a no-op for that file (the
    /// per-file row count goes to `succeeded` either way).
    Remove,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameFilesPayload {
    pub renames: Vec<RenameEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameEntry {
    #[serde(rename = "fileID")]
    pub file_id: i64,
    /// New filename only (no directory components). Engine resolves the
    /// destination as `dirname(current) + new_name`.
    pub new_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashFilesPayload {
    #[serde(rename = "fileIDs")]
    pub file_ids: Vec<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact_identities: Option<Vec<ExactTrashIdentity>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExactTrashIdentity {
    #[serde(rename = "fileID")]
    pub file_id: i64,
    pub path: String,
    pub size_bytes: i64,
    pub sha256_hex: String,
    pub keeper_path: String,
    pub keeper_size_bytes: i64,
    pub keeper_sha256_hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeClustersPayload {
    #[serde(rename = "sourcePersonID")]
    pub source_person_id: i64,
    #[serde(rename = "destinationPersonID")]
    pub destination_person_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbedTextQueryPayload {
    pub query: String,
    /// Echoed back on the response event so the caller can correlate
    /// (multiple in-flight queries won't get crossed).
    #[serde(rename = "queryID")]
    pub query_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbedImageQueryPayload {
    #[serde(rename = "fileID")]
    pub file_id: i64,
    #[serde(rename = "queryID")]
    pub query_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateVideoThumbnailPayload {
    pub path: String,
    /// File's modified-unix time (f64 seconds). Echoed back on the reply so
    /// the app can key its `(path, modifiedAt)` thumbnail cache. Optional so
    /// callers that don't know it can omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreFromTrashPayload {
    /// Identifier from the trash_log JSON (UUID emitted by trashFiles).
    #[serde(rename = "batchID")]
    pub batch_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RevertMergePayload {
    #[serde(rename = "sourcePersonID")]
    pub source_person_id: i64,
    #[serde(rename = "destinationPersonID")]
    pub destination_person_id: i64,
    #[serde(rename = "faceIDsToRevert")]
    pub face_ids_to_revert: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenamePersonPayload {
    #[serde(rename = "personID")]
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
    #[serde(rename = "personIDs")]
    pub person_ids: Vec<i64>,
}

/// Payload for `markPersonsDifferent`. Carries both the (volatile) person ids
/// and the (stable) anchor face ids from the suggestion so the engine persists
/// a verdict key that survives re-clustering.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkPersonsDifferentPayload {
    #[serde(rename = "sourcePersonID")]
    pub source_person_id: i64,
    #[serde(rename = "destinationPersonID")]
    pub destination_person_id: i64,
    #[serde(rename = "sourceAnchorFaceID")]
    pub source_anchor_face_id: i64,
    #[serde(rename = "destinationAnchorFaceID")]
    pub destination_anchor_face_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeSuggestion {
    #[serde(rename = "sourcePersonID")]
    pub source_person_id: i64,
    #[serde(rename = "destinationPersonID")]
    pub destination_person_id: i64,
    pub similarity: f32,
    #[serde(rename = "sourceAnchorFaceID")]
    pub source_anchor_face_id: i64,
    #[serde(rename = "destinationAnchorFaceID")]
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

    /// Reply to `wipeLibrary`: the engine truncated all user tables in-process.
    #[serde(rename = "libraryWiped")]
    LibraryWiped(Wrap<LibraryWiped>),

    /// Reply to `generateVideoThumbnail`: a base64-encoded 192px JPEG keyframe
    /// for the requested video. `modifiedAt` is echoed back because the app's
    /// thumbnail cache is keyed on `(path, modifiedAt)`.
    #[serde(rename = "thumbnailGenerated")]
    ThumbnailGenerated(Wrap<ThumbnailGenerated>),
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
    /// `model_load_failed`, `model_download_failed`, `download_tls_pin_failed`,
    /// `pack_not_available`, `runtime_not_installed` (macOS: the `load-dynamic`
    /// ONNX Runtime dylib isn't installed — see `ort_runtime.rs`),
    /// `ipc_unknown_command`, `unknown`, ...
    pub kind: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// For errors that pertain to a specific model install/download, the
    /// model id (e.g. `mobileclip_s2`, `bge_text`, `florence2_base`). Lets
    /// the app route the error to the right install slot without fragile
    /// path-string matching.
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
    #[serde(default, rename = "planID", skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_moves: Option<u64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
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
    /// Bulk command discriminator, including person-verdict and restore actions.
    pub action: String,
    pub succeeded: u32,
    pub failed: u32,
    pub messages: Vec<BulkActionItem>,
}

/// Reply to `wipeLibrary`. `ok` is true when every table was truncated; on
/// failure `message` carries the error so the app can fall back to its own
/// stop→delete→restart wipe path.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryWiped {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkActionItem {
    #[serde(rename = "fileID", default, skip_serializing_if = "Option::is_none")]
    pub file_id: Option<i64>,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipTextEmbedding {
    #[serde(rename = "queryID")]
    pub query_id: String,
    pub query: String,
    /// 512-d L2-normalized float32 embedding from the CLIP text encoder.
    /// App dot-products this against `clip_embeddings` to rank Library
    /// rows by semantic similarity.
    pub embedding: Vec<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThumbnailGenerated {
    pub path: String,
    /// File's modified-unix time (f64 seconds), echoed from the request so the
    /// app can write the bytes under its `(path, modifiedAt)` cache key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<f64>,
    /// Base64-encoded 192px JPEG (aspect-preserved, long side = 192). A base64
    /// string, NOT a number array.
    pub bytes: String,
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

    /// F-C2-001: a schema-conformant `deepAnalyzeAll` that OMITS `tagsOnly`
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

    /// F-C2-002: `cancelPrewarm` must decode with `modelKind` OMITTED (the
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
            CommandPayload::StartScan(StartScanPayload {
                root_path: r"C:\Users\adam\Pictures".into(),
                root_display: Some("Pictures".into()),
                rescan: false,
                excluded_paths: Some(vec![r"C:\Users\adam\Pictures\node_backups".into()]),
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
