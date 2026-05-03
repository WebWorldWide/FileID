// Common enum types shared across IPC payloads. Each maps directly onto a
// case enum in shared/ipc-schema/ipc.schema.json.
//
// Naming convention: C# enum names PascalCase; wire form camelCase via
// JsonStringEnumConverter<T>(JsonNamingPolicy.CamelCase) registered in
// IpcCoder.Options. Attribute-based registration with naming policy isn't
// supported, so we register programmatically there.

namespace FileID.IpcSchema;

public enum ScanPhase
{
    Idle,
    Discovering,
    Tagging,
    PostScan,
    Completed,
    Cancelled,
    Failed,
}

public enum JobCategory
{
    Scan,
    FaceCluster,
    DeepAnalyze,
}

public enum LogLevel
{
    Debug,
    Info,
    Warn,
    Error,
}

public enum DeepAnalyzeStartingPhase
{
    Queued,
    LoadingModel,
    ResolvingTargets,
}
