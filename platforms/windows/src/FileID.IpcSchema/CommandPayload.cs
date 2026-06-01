// IPCCommand payload — externally-tagged discriminated union.
//
// Wire shape: {"<variantName>": <body>}, where empty-payload variants
// encode their body as `{}` (NOT a bare string — Swift Codable's auto-
// synthesis for enums with mixed associated-value cases always emits a
// keyed object). Round-trip tests in FileID.IpcSchema.Tests assert this.
//
// The custom JsonConverter below dispatches by the outer key; the variant
// classes are simple records.

using System.Text.Json;
using System.Text.Json.Serialization;

namespace FileID.IpcSchema;

[JsonConverter(typeof(CommandPayloadJsonConverter))]
public abstract record CommandPayload;

public sealed record StartScanCommand(
    string RootPath,
    string? RootDisplay,
    // `rescan = false` (default) is incremental: engine skips files where
    // `scanned_at >= modified_at`. `rescan = true` forces every file to
    // be re-tagged.
    bool Rescan = false) : CommandPayload;

public sealed record PauseScanCommand : CommandPayload;
public sealed record ResumeScanCommand : CommandPayload;
public sealed record CancelScanCommand : CommandPayload;
public sealed record RequestStatusCommand : CommandPayload;
public sealed record ShutdownCommand : CommandPayload;
public sealed record RunFaceClusteringCommand : CommandPayload;

/// <summary>Tell the engine to re-probe CUDA Toolkit + cuDNN availability
/// without an engine restart. The engine replies with a `hardwareReprobed`
/// event carrying fresh `HardwareInfo` and a diagnostics string when the
/// pack is still missing.</summary>
public sealed record VerifyCudaPackCommand : CommandPayload;

public sealed record DeepAnalyzeFileCommand(
    [property: JsonPropertyName("fileID")] long FileId,
    string ModelKind) : CommandPayload;

public sealed record DeepAnalyzeFolderCommand(
    string PathPrefix,
    string ModelKind) : CommandPayload;

public sealed record DeepAnalyzeAllCommand(
    string ModelKind,
    bool SkipExisting,
    bool TagsOnly = false) : CommandPayload;

public sealed record DeepAnalyzeCancelCommand : CommandPayload;

public sealed record PrewarmModelCommand(string ModelKind) : CommandPayload;

public sealed record CancelPrewarmCommand : CommandPayload;

public sealed record PlanRestructureCommand(string LibraryRoot) : CommandPayload;

public sealed record ApplyRestructureCommand(
    string LibraryRoot,
    System.Collections.Generic.IReadOnlyList<RestructureMove> Moves,
    bool UseSymlinks = false) : CommandPayload;

public sealed record RestructureMove(
    long FileId,
    string Source,
    string Destination,
    string Category,
    /// <summary>Engine-authoritative per-move tier (Anchor / Mixed / Junk).
    /// Null on plans from older engine builds — callers should fall back to
    /// a local heuristic in that case.</summary>
    string? Tier = null,
    /// <summary>Butler confidence band — "auto" / "review" / "ask"
    /// (RESTRUCTURE.md §6). Empty on older engines.</summary>
    string Confidence = "",
    /// <summary>Plain-language "why filed here", shown in the drill-down.</summary>
    string? Reason = null);

public sealed record ApplyTagsCommand(
    System.Collections.Generic.IReadOnlyList<long> FileIds,
    System.Collections.Generic.IReadOnlyList<string> Tags,
    string Mode = "add") : CommandPayload;

public sealed record RenameFilesCommand(
    System.Collections.Generic.IReadOnlyList<RenameEntry> Renames) : CommandPayload;

public sealed record RenameEntry(long FileId, string NewName);

public sealed record TrashFilesCommand(
    System.Collections.Generic.IReadOnlyList<long> FileIds) : CommandPayload;

public sealed record MergeClustersCommand(
    long SourcePersonId,
    long DestinationPersonId) : CommandPayload;

public sealed record EmbedTextQueryCommand(
    string Query,
    string QueryId) : CommandPayload;

public sealed record RenamePersonCommand(
    long PersonId,
    string? Title = null,
    string? FirstName = null,
    string? MiddleName = null,
    string? LastName = null,
    string? Suffix = null) : CommandPayload;

/// <summary>FEAT-CRIT-1: bulk mark-as-unknown for People multi-select.</summary>
public sealed record MarkPersonsAsUnknownCommand(
    System.Collections.Generic.IReadOnlyList<long> PersonIds) : CommandPayload;

public sealed record FindMergeSuggestionsCommand : CommandPayload;

/// <summary>Record a user "different people" verdict for a suggested pair so
/// findMergeSuggestions stops re-suggesting it. Routed through the engine's
/// single-writer DB connection; keyed on stable anchor face ids.</summary>
public sealed record MarkPersonsDifferentCommand(
    long SourcePersonId,
    long DestinationPersonId,
    long SourceAnchorFaceId,
    long DestinationAnchorFaceId) : CommandPayload;

public sealed record EmbedImageQueryCommand(
    long FileId,
    string QueryId) : CommandPayload;

public sealed record RestoreFromTrashCommand(string BatchId) : CommandPayload;

public sealed record RevertMergeCommand(
    long SourcePersonId,
    long DestinationPersonId,
    System.Collections.Generic.IReadOnlyList<long> FaceIdsToRevert) : CommandPayload;

public sealed record WipeLibraryCommand : CommandPayload;

/// <summary>
/// Reads/writes the externally-tagged shape Swift's Codable produces.
/// One key, value is the body object (or `{}` for empty-payload variants).
/// </summary>
public sealed class CommandPayloadJsonConverter : JsonConverter<CommandPayload>
{
    public override CommandPayload Read(ref Utf8JsonReader reader, Type typeToConvert, JsonSerializerOptions options)
    {
        if (reader.TokenType != JsonTokenType.StartObject)
        {
            throw new JsonException("CommandPayload: expected '{'");
        }
        if (!reader.Read() || reader.TokenType != JsonTokenType.PropertyName)
        {
            throw new JsonException("CommandPayload: expected variant key");
        }
        string variant = reader.GetString() ?? throw new JsonException("CommandPayload: null variant key");

        // Move to the body. Every variant encodes its body as a (possibly
        // empty) JSON object.
        if (!reader.Read())
        {
            throw new JsonException($"CommandPayload({variant}): truncated");
        }

        CommandPayload payload = variant switch
        {
            "startScan" => JsonSerializer.Deserialize<StartScanCommand>(ref reader, options) ?? throw new JsonException("startScan: null body"),
            "deepAnalyzeFile" => JsonSerializer.Deserialize<DeepAnalyzeFileCommand>(ref reader, options) ?? throw new JsonException("deepAnalyzeFile: null body"),
            "deepAnalyzeFolder" => JsonSerializer.Deserialize<DeepAnalyzeFolderCommand>(ref reader, options) ?? throw new JsonException("deepAnalyzeFolder: null body"),
            "deepAnalyzeAll" => JsonSerializer.Deserialize<DeepAnalyzeAllCommand>(ref reader, options) ?? throw new JsonException("deepAnalyzeAll: null body"),
            "prewarmModel" => JsonSerializer.Deserialize<PrewarmModelCommand>(ref reader, options) ?? throw new JsonException("prewarmModel: null body"),
            "planRestructure" => JsonSerializer.Deserialize<PlanRestructureCommand>(ref reader, options) ?? throw new JsonException("planRestructure: null body"),
            "applyRestructure" => JsonSerializer.Deserialize<ApplyRestructureCommand>(ref reader, options) ?? throw new JsonException("applyRestructure: null body"),
            "applyTags" => JsonSerializer.Deserialize<ApplyTagsCommand>(ref reader, options) ?? throw new JsonException("applyTags: null body"),
            "renameFiles" => JsonSerializer.Deserialize<RenameFilesCommand>(ref reader, options) ?? throw new JsonException("renameFiles: null body"),
            "trashFiles" => JsonSerializer.Deserialize<TrashFilesCommand>(ref reader, options) ?? throw new JsonException("trashFiles: null body"),
            "mergeClusters" => JsonSerializer.Deserialize<MergeClustersCommand>(ref reader, options) ?? throw new JsonException("mergeClusters: null body"),
            "embedTextQuery" => JsonSerializer.Deserialize<EmbedTextQueryCommand>(ref reader, options) ?? throw new JsonException("embedTextQuery: null body"),
            "renamePerson" => JsonSerializer.Deserialize<RenamePersonCommand>(ref reader, options) ?? throw new JsonException("renamePerson: null body"),
            "markPersonsAsUnknown" => JsonSerializer.Deserialize<MarkPersonsAsUnknownCommand>(ref reader, options) ?? throw new JsonException("markPersonsAsUnknown: null body"),
            "markPersonsDifferent" => JsonSerializer.Deserialize<MarkPersonsDifferentCommand>(ref reader, options) ?? throw new JsonException("markPersonsDifferent: null body"),
            "findMergeSuggestions" => Empty<FindMergeSuggestionsCommand>(ref reader),
            "embedImageQuery" => JsonSerializer.Deserialize<EmbedImageQueryCommand>(ref reader, options) ?? throw new JsonException("embedImageQuery: null body"),
            "restoreFromTrash" => JsonSerializer.Deserialize<RestoreFromTrashCommand>(ref reader, options) ?? throw new JsonException("restoreFromTrash: null body"),
            "revertMerge" => JsonSerializer.Deserialize<RevertMergeCommand>(ref reader, options) ?? throw new JsonException("revertMerge: null body"),

            "wipeLibrary" => Empty<WipeLibraryCommand>(ref reader),
            "pauseScan" => Empty<PauseScanCommand>(ref reader),
            "resumeScan" => Empty<ResumeScanCommand>(ref reader),
            "cancelScan" => Empty<CancelScanCommand>(ref reader),
            "requestStatus" => Empty<RequestStatusCommand>(ref reader),
            "shutdown" => Empty<ShutdownCommand>(ref reader),
            "runFaceClustering" => Empty<RunFaceClusteringCommand>(ref reader),
            "verifyCudaPack" => Empty<VerifyCudaPackCommand>(ref reader),
            "deepAnalyzeCancel" => Empty<DeepAnalyzeCancelCommand>(ref reader),
            "cancelPrewarm" => Empty<CancelPrewarmCommand>(ref reader),

            _ => throw new JsonException($"CommandPayload: unknown variant '{variant}'"),
        };

        // Close the outer object: the Deserialize call left the reader on
        // the body's EndObject (or value); advance to the outer EndObject.
        if (!reader.Read() || reader.TokenType != JsonTokenType.EndObject)
        {
            throw new JsonException($"CommandPayload({variant}): expected outer '}}'");
        }
        return payload;
    }

    public override void Write(Utf8JsonWriter writer, CommandPayload value, JsonSerializerOptions options)
    {
        writer.WriteStartObject();
        switch (value)
        {
            case StartScanCommand c: WriteVariant(writer, "startScan", c, options); break;
            case PauseScanCommand: WriteEmpty(writer, "pauseScan"); break;
            case ResumeScanCommand: WriteEmpty(writer, "resumeScan"); break;
            case CancelScanCommand: WriteEmpty(writer, "cancelScan"); break;
            case RequestStatusCommand: WriteEmpty(writer, "requestStatus"); break;
            case ShutdownCommand: WriteEmpty(writer, "shutdown"); break;
            case RunFaceClusteringCommand: WriteEmpty(writer, "runFaceClustering"); break;
            case VerifyCudaPackCommand: WriteEmpty(writer, "verifyCudaPack"); break;
            case DeepAnalyzeFileCommand c: WriteVariant(writer, "deepAnalyzeFile", c, options); break;
            case DeepAnalyzeFolderCommand c: WriteVariant(writer, "deepAnalyzeFolder", c, options); break;
            case DeepAnalyzeAllCommand c: WriteVariant(writer, "deepAnalyzeAll", c, options); break;
            case DeepAnalyzeCancelCommand: WriteEmpty(writer, "deepAnalyzeCancel"); break;
            case PrewarmModelCommand c: WriteVariant(writer, "prewarmModel", c, options); break;
            case CancelPrewarmCommand: WriteEmpty(writer, "cancelPrewarm"); break;
            case PlanRestructureCommand c: WriteVariant(writer, "planRestructure", c, options); break;
            case ApplyRestructureCommand c: WriteVariant(writer, "applyRestructure", c, options); break;
            case ApplyTagsCommand c: WriteVariant(writer, "applyTags", c, options); break;
            case RenameFilesCommand c: WriteVariant(writer, "renameFiles", c, options); break;
            case TrashFilesCommand c: WriteVariant(writer, "trashFiles", c, options); break;
            case MergeClustersCommand c: WriteVariant(writer, "mergeClusters", c, options); break;
            case EmbedTextQueryCommand c: WriteVariant(writer, "embedTextQuery", c, options); break;
            case RenamePersonCommand c: WriteVariant(writer, "renamePerson", c, options); break;
            case MarkPersonsAsUnknownCommand c: WriteVariant(writer, "markPersonsAsUnknown", c, options); break;
            case MarkPersonsDifferentCommand c: WriteVariant(writer, "markPersonsDifferent", c, options); break;
            case FindMergeSuggestionsCommand: WriteEmpty(writer, "findMergeSuggestions"); break;
            case EmbedImageQueryCommand c: WriteVariant(writer, "embedImageQuery", c, options); break;
            case RestoreFromTrashCommand c: WriteVariant(writer, "restoreFromTrash", c, options); break;
            case RevertMergeCommand c: WriteVariant(writer, "revertMerge", c, options); break;
            case WipeLibraryCommand: WriteEmpty(writer, "wipeLibrary"); break;
            default:
                throw new JsonException($"CommandPayload: unknown C# type {value.GetType().FullName}");
        }
        writer.WriteEndObject();
    }

    /// <summary>
    /// Reads `{}` and constructs an empty-record. Used for payload-less
    /// variants. The caller is at StartObject.
    /// </summary>
    private static T Empty<T>(ref Utf8JsonReader reader) where T : CommandPayload, new()
    {
        if (reader.TokenType != JsonTokenType.StartObject)
        {
            throw new JsonException($"{typeof(T).Name}: expected '{{}}'");
        }
        if (!reader.Read() || reader.TokenType != JsonTokenType.EndObject)
        {
            throw new JsonException($"{typeof(T).Name}: payload must be '{{}}'");
        }
        return new T();
    }

    private static void WriteVariant<T>(Utf8JsonWriter writer, string key, T value, JsonSerializerOptions options)
    {
        writer.WritePropertyName(key);
        JsonSerializer.Serialize(writer, value, options);
    }

    private static void WriteEmpty(Utf8JsonWriter writer, string key)
    {
        writer.WritePropertyName(key);
        writer.WriteStartObject();
        writer.WriteEndObject();
    }
}
