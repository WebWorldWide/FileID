// Plain DTO records used inside command + event payloads. 1:1 mirror of the
// schema's `$defs` section. Records (immutable, init-only, value-equality)
// match Swift's struct semantics and Rust's #[derive(Debug, Clone)] structs.
//
// Property naming policy is CamelCase (configured in IpcCoder); fields whose
// wire names diverge from the C# default carry [JsonPropertyName] overrides
// (mostly the `*ID` and `*MB` / `*GB` style names — Swift Codable doesn't
// auto-camel-case those, so we don't either).

using System.Text.Json.Serialization;

namespace FileID.IpcSchema;

public sealed record EngineInfo(
    string Version,
    int Pid,
    uint WorkerCap,
    [property: JsonPropertyName("physicalMemoryGB")] double PhysicalMemoryGB,
    HardwareInfo? Hardware = null);

public sealed record RestructurePlan(
    string LibraryRoot,
    System.Collections.Generic.IReadOnlyList<RestructureMove> Moves,
    System.Collections.Generic.IReadOnlyList<RestructureCategoryCount> CategoryCounts);

public sealed record RestructureCategoryCount(string Category, uint Count);

public sealed record RestructureApplyResult(
    uint Applied,
    uint Failed,
    string? PrivilegeError = null);

public sealed record BulkActionResult(
    string Action,
    uint Succeeded,
    uint Failed,
    System.Collections.Generic.IReadOnlyList<BulkActionItem> Messages);

public sealed record BulkActionItem(
    long? FileId,
    bool Ok,
    string? Message = null);

public sealed record ClipTextEmbedding(
    string QueryId,
    string Query,
    System.Collections.Generic.IReadOnlyList<float> Embedding);

public sealed record MergeSuggestion(
    long SourcePersonId,
    long DestinationPersonId,
    float Similarity,
    long SourceAnchorFaceId,
    long DestinationAnchorFaceId,
    long SourceMemberCount,
    long DestinationMemberCount);

public sealed record MergeSuggestions(
    System.Collections.Generic.IReadOnlyList<MergeSuggestion> Pairs);

/// <summary>
/// Hardware probe surfaced by the engine on startup. Settings → Performance
/// renders this so the user can see which acceleration path is in use and
/// which Performance Pack would unlock more throughput.
/// </summary>
public sealed record HardwareInfo(
    string GpuVendor,
    string? AdapterName,
    string ExecutionProvider,
    uint PhysicalCpuCores,
    bool CudaPackPresent,
    bool OpenvinoPackPresent,
    bool QnnPackPresent,
    string Recommendation);

public sealed record ScanProgress(
    [property: JsonPropertyName("sessionID")] string SessionId,
    ScanPhase Phase,
    ulong Total,
    ulong Discovered,
    ulong Processed,
    ulong Failed,
    double FilesPerSecond,
    double? EtaSeconds,
    [property: JsonPropertyName("residentMB")] ulong ResidentMb,
    [property: JsonPropertyName("availableMB")] ulong AvailableMb);

public sealed record FileDoneEvent(
    string Path,
    string Kind,
    double TotalMs,
    bool Failed,
    string? ErrorMessage);

public sealed record BatchSummary(
    uint BatchIndex,
    uint FilesInBatch,
    ulong ProcessedTotal,
    double WallSeconds,
    double FilesPerSecond,
    double Utilization,
    [property: JsonPropertyName("visionP50Ms")] double VisionP50Ms,
    [property: JsonPropertyName("visionP95Ms")] double VisionP95Ms,
    [property: JsonPropertyName("clipP50Ms")] double ClipP50Ms,
    [property: JsonPropertyName("clipP95Ms")] double ClipP95Ms,
    [property: JsonPropertyName("storeInsertP50Ms")] double StoreInsertP50Ms,
    [property: JsonPropertyName("storeInsertP95Ms")] double StoreInsertP95Ms,
    [property: JsonPropertyName("residentMB")] ulong ResidentMb,
    [property: JsonPropertyName("availableMB")] ulong AvailableMb);

public sealed record ScanComplete(
    [property: JsonPropertyName("sessionID")] string SessionId,
    ulong TotalFiles,
    ulong ProcessedFiles,
    ulong FailedFiles,
    double TotalSeconds);

public sealed record EngineError(
    string Kind,
    string Message,
    string? Path);

public sealed record LogLine(
    LogLevel Level,
    string Message);

public sealed record FaceClusteringResult(
    uint PersonCount,
    ulong FaceCount,
    ulong UnmatchedFaces,
    double DurationSeconds);

public sealed record DeepAnalyzeStarting(
    string ModelKind,
    DeepAnalyzeStartingPhase Phase,
    string Message);

public sealed record DeepAnalyzeProgress(
    ulong Processed,
    ulong Total,
    double? EtaSeconds,
    string? CurrentPath,
    string ModelKind);

public sealed record DeepAnalyzeFileDone(
    [property: JsonPropertyName("fileID")] long FileId,
    string Description,
    string? ProposedName,
    string ModelKind);

public sealed record DeepAnalyzeComplete(
    ulong Processed,
    ulong Failed,
    double TotalSeconds,
    string ModelKind,
    bool Cancelled);

public sealed record ModelDownloadProgress(
    string ModelKind,
    double Fraction,
    string Message,
    ulong? BytesDone,
    ulong? TotalBytes);

public sealed record QueueState(
    QueuedJob? Running,
    IReadOnlyList<QueuedJob> Pending,
    double? TotalEtaSeconds);

public sealed record QueuedJob(
    string Id,
    JobCategory Category,
    string Title,
    double? EtaSeconds);
