// IPCEvent payload — externally-tagged discriminated union.
//
// Wire shape: like CommandPayload, but variants whose Swift case has a
// single unnamed associated value (e.g. `case ready(EngineInfo)`) wrap the
// payload in `{"_0": <value>}`. The `discoveryComplete` variant is the one
// exception — Swift treats its `(totalFiles: Int)` named-parameter case as
// a struct payload, so it's NOT `_0`-wrapped.
//
// The converter handles both shapes per-variant.

using System.Text.Json;
using System.Text.Json.Serialization;

namespace FileID.IpcSchema;

[JsonConverter(typeof(EventPayloadJsonConverter))]
public abstract record EventPayload;

public sealed record ReadyEvent(EngineInfo Info) : EventPayload;
public sealed record ProgressEvent(ScanProgress Progress) : EventPayload;
public sealed record PhaseChangedEvent(ScanPhase Phase) : EventPayload;

/// <summary>
/// Named-parameter case (no `_0` wrapper). Schema special.
/// </summary>
public sealed record DiscoveryCompleteEvent(ulong TotalFiles) : EventPayload;

public sealed record FileDoneEventWrapper(FileDoneEvent FileDone) : EventPayload;
public sealed record BatchSummaryEvent(BatchSummary Summary) : EventPayload;
public sealed record ScanCompleteEvent(ScanComplete Result) : EventPayload;
public sealed record ErrorEvent(EngineError Error) : EventPayload;
public sealed record LogEvent(LogLine Line) : EventPayload;
public sealed record FaceClusteringCompleteEvent(FaceClusteringResult Result) : EventPayload;
public sealed record DeepAnalyzeStartingEvent(DeepAnalyzeStarting Starting) : EventPayload;
public sealed record DeepAnalyzeProgressEvent(DeepAnalyzeProgress Progress) : EventPayload;
public sealed record DeepAnalyzeFileDoneEvent(DeepAnalyzeFileDone FileDone) : EventPayload;
public sealed record DeepAnalyzeCompleteEvent(DeepAnalyzeComplete Result) : EventPayload;
public sealed record ModelDownloadProgressEvent(ModelDownloadProgress Progress) : EventPayload;
public sealed record QueueStateEvent(QueueState State) : EventPayload;
public sealed record RestructurePlanEvent(RestructurePlan Plan) : EventPayload;
public sealed record RestructureApplyResultEvent(RestructureApplyResult Result) : EventPayload;
public sealed record BulkActionResultEvent(BulkActionResult Result) : EventPayload;
public sealed record ClipTextEmbeddingEvent(ClipTextEmbedding Embedding) : EventPayload;
public sealed record MergeSuggestionsEvent(MergeSuggestions Suggestions) : EventPayload;

/// <summary>Engine reply to a <c>verifyCudaPack</c> command. Carries
/// fresh hardware probe + optional diagnostics text for the Settings →
/// Performance "Verify install" affordance.</summary>
public sealed record HardwareReprobedEvent(HardwareReprobed Result) : EventPayload;
public sealed record LibraryWipedEvent(LibraryWiped Result) : EventPayload;

public sealed class EventPayloadJsonConverter : JsonConverter<EventPayload>
{
    public override EventPayload Read(ref Utf8JsonReader reader, Type typeToConvert, JsonSerializerOptions options)
    {
        if (reader.TokenType != JsonTokenType.StartObject)
        {
            throw new JsonException("EventPayload: expected '{'");
        }
        if (!reader.Read() || reader.TokenType != JsonTokenType.PropertyName)
        {
            throw new JsonException("EventPayload: expected variant key");
        }
        string variant = reader.GetString() ?? throw new JsonException("EventPayload: null variant key");
        if (!reader.Read())
        {
            throw new JsonException($"EventPayload({variant}): truncated");
        }

        EventPayload payload = variant switch
        {
            "ready" => new ReadyEvent(ReadWrapped<EngineInfo>(ref reader, options)),
            "progress" => new ProgressEvent(ReadWrapped<ScanProgress>(ref reader, options)),
            "phaseChanged" => new PhaseChangedEvent(ReadWrapped<ScanPhase>(ref reader, options)),
            "discoveryComplete" => new DiscoveryCompleteEvent(ReadDiscoveryComplete(ref reader)),
            "fileDone" => new FileDoneEventWrapper(ReadWrapped<FileDoneEvent>(ref reader, options)),
            "batchSummary" => new BatchSummaryEvent(ReadWrapped<BatchSummary>(ref reader, options)),
            "scanComplete" => new ScanCompleteEvent(ReadWrapped<ScanComplete>(ref reader, options)),
            "error" => new ErrorEvent(ReadWrapped<EngineError>(ref reader, options)),
            "log" => new LogEvent(ReadWrapped<LogLine>(ref reader, options)),
            "faceClusteringComplete" => new FaceClusteringCompleteEvent(ReadWrapped<FaceClusteringResult>(ref reader, options)),
            "deepAnalyzeStarting" => new DeepAnalyzeStartingEvent(ReadWrapped<DeepAnalyzeStarting>(ref reader, options)),
            "deepAnalyzeProgress" => new DeepAnalyzeProgressEvent(ReadWrapped<DeepAnalyzeProgress>(ref reader, options)),
            "deepAnalyzeFileDone" => new DeepAnalyzeFileDoneEvent(ReadWrapped<DeepAnalyzeFileDone>(ref reader, options)),
            "deepAnalyzeComplete" => new DeepAnalyzeCompleteEvent(ReadWrapped<DeepAnalyzeComplete>(ref reader, options)),
            "modelDownloadProgress" => new ModelDownloadProgressEvent(ReadWrapped<ModelDownloadProgress>(ref reader, options)),
            "queueState" => new QueueStateEvent(ReadWrapped<QueueState>(ref reader, options)),
            "restructurePlan" => new RestructurePlanEvent(ReadWrapped<RestructurePlan>(ref reader, options)),
            "restructureApplyResult" => new RestructureApplyResultEvent(ReadWrapped<RestructureApplyResult>(ref reader, options)),
            "bulkActionResult" => new BulkActionResultEvent(ReadWrapped<BulkActionResult>(ref reader, options)),
            "clipTextEmbedding" => new ClipTextEmbeddingEvent(ReadWrapped<ClipTextEmbedding>(ref reader, options)),
            "mergeSuggestions" => new MergeSuggestionsEvent(ReadWrapped<MergeSuggestions>(ref reader, options)),
            "hardwareReprobed" => new HardwareReprobedEvent(ReadWrapped<HardwareReprobed>(ref reader, options)),
            "libraryWiped" => new LibraryWipedEvent(ReadWrapped<LibraryWiped>(ref reader, options)),
            _ => throw new JsonException($"EventPayload: unknown variant '{variant}'"),
        };

        if (!reader.Read() || reader.TokenType != JsonTokenType.EndObject)
        {
            throw new JsonException($"EventPayload({variant}): expected outer '}}'");
        }
        return payload;
    }

    public override void Write(Utf8JsonWriter writer, EventPayload value, JsonSerializerOptions options)
    {
        writer.WriteStartObject();
        switch (value)
        {
            case ReadyEvent v: WriteWrapped(writer, "ready", v.Info, options); break;
            case ProgressEvent v: WriteWrapped(writer, "progress", v.Progress, options); break;
            case PhaseChangedEvent v: WriteWrapped(writer, "phaseChanged", v.Phase, options); break;
            case DiscoveryCompleteEvent v: WriteDiscoveryComplete(writer, v); break;
            case FileDoneEventWrapper v: WriteWrapped(writer, "fileDone", v.FileDone, options); break;
            case BatchSummaryEvent v: WriteWrapped(writer, "batchSummary", v.Summary, options); break;
            case ScanCompleteEvent v: WriteWrapped(writer, "scanComplete", v.Result, options); break;
            case ErrorEvent v: WriteWrapped(writer, "error", v.Error, options); break;
            case LogEvent v: WriteWrapped(writer, "log", v.Line, options); break;
            case FaceClusteringCompleteEvent v: WriteWrapped(writer, "faceClusteringComplete", v.Result, options); break;
            case DeepAnalyzeStartingEvent v: WriteWrapped(writer, "deepAnalyzeStarting", v.Starting, options); break;
            case DeepAnalyzeProgressEvent v: WriteWrapped(writer, "deepAnalyzeProgress", v.Progress, options); break;
            case DeepAnalyzeFileDoneEvent v: WriteWrapped(writer, "deepAnalyzeFileDone", v.FileDone, options); break;
            case DeepAnalyzeCompleteEvent v: WriteWrapped(writer, "deepAnalyzeComplete", v.Result, options); break;
            case ModelDownloadProgressEvent v: WriteWrapped(writer, "modelDownloadProgress", v.Progress, options); break;
            case QueueStateEvent v: WriteWrapped(writer, "queueState", v.State, options); break;
            case RestructurePlanEvent v: WriteWrapped(writer, "restructurePlan", v.Plan, options); break;
            case RestructureApplyResultEvent v: WriteWrapped(writer, "restructureApplyResult", v.Result, options); break;
            case BulkActionResultEvent v: WriteWrapped(writer, "bulkActionResult", v.Result, options); break;
            case ClipTextEmbeddingEvent v: WriteWrapped(writer, "clipTextEmbedding", v.Embedding, options); break;
            case MergeSuggestionsEvent v: WriteWrapped(writer, "mergeSuggestions", v.Suggestions, options); break;
            case HardwareReprobedEvent v: WriteWrapped(writer, "hardwareReprobed", v.Result, options); break;
            case LibraryWipedEvent v: WriteWrapped(writer, "libraryWiped", v.Result, options); break;
            default:
                throw new JsonException($"EventPayload: unknown C# type {value.GetType().FullName}");
        }
        writer.WriteEndObject();
    }

    /// <summary>
    /// Reads <c>{"_0": &lt;value&gt;}</c> and returns the value. Caller is at StartObject.
    /// Leaves the reader on the EndObject of the wrapper.
    /// </summary>
    private static T ReadWrapped<T>(ref Utf8JsonReader reader, JsonSerializerOptions options)
    {
        if (reader.TokenType != JsonTokenType.StartObject)
        {
            throw new JsonException($"Wrap<{typeof(T).Name}>: expected '{{'");
        }
        if (!reader.Read() || reader.TokenType != JsonTokenType.PropertyName)
        {
            throw new JsonException($"Wrap<{typeof(T).Name}>: expected '_0' key");
        }
        string key = reader.GetString() ?? throw new JsonException("Wrap: null key");
        if (key != "_0")
        {
            throw new JsonException($"Wrap<{typeof(T).Name}>: expected '_0', got '{key}'");
        }
        if (!reader.Read())
        {
            throw new JsonException($"Wrap<{typeof(T).Name}>: truncated before value");
        }
        T value = JsonSerializer.Deserialize<T>(ref reader, options)
            ?? throw new JsonException($"Wrap<{typeof(T).Name}>: null value");
        if (!reader.Read() || reader.TokenType != JsonTokenType.EndObject)
        {
            throw new JsonException($"Wrap<{typeof(T).Name}>: expected wrapper '}}'");
        }
        return value;
    }

    private static void WriteWrapped<T>(Utf8JsonWriter writer, string key, T value, JsonSerializerOptions options)
    {
        writer.WritePropertyName(key);
        writer.WriteStartObject();
        writer.WritePropertyName("_0");
        JsonSerializer.Serialize(writer, value, options);
        writer.WriteEndObject();
    }

    /// <summary>
    /// `discoveryComplete` is the only event whose Swift case has named
    /// parameters (`case discoveryComplete(totalFiles: Int)`). Wire form
    /// is `{"discoveryComplete": {"totalFiles": N}}` — no `_0` wrapper.
    /// </summary>
    private static ulong ReadDiscoveryComplete(ref Utf8JsonReader reader)
    {
        if (reader.TokenType != JsonTokenType.StartObject)
        {
            throw new JsonException("discoveryComplete: expected '{'");
        }
        ulong total = 0;
        bool seen = false;
        while (reader.Read() && reader.TokenType == JsonTokenType.PropertyName)
        {
            string key = reader.GetString() ?? throw new JsonException("discoveryComplete: null key");
            if (!reader.Read())
            {
                throw new JsonException("discoveryComplete: truncated");
            }
            if (key == "totalFiles")
            {
                total = reader.GetUInt64();
                seen = true;
            }
            else
            {
                reader.Skip();
            }
        }
        if (!seen)
        {
            throw new JsonException("discoveryComplete: missing 'totalFiles'");
        }
        if (reader.TokenType != JsonTokenType.EndObject)
        {
            throw new JsonException("discoveryComplete: expected '}'");
        }
        return total;
    }

    private static void WriteDiscoveryComplete(Utf8JsonWriter writer, DiscoveryCompleteEvent ev)
    {
        writer.WritePropertyName("discoveryComplete");
        writer.WriteStartObject();
        writer.WriteNumber("totalFiles", ev.TotalFiles);
        writer.WriteEndObject();
    }
}
