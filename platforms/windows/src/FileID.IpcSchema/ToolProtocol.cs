using System.Collections.Generic;
using System.Text.Json.Serialization;

namespace FileID.IpcSchema;

public sealed record ToolRecipe(
    string Kind,
    string Format,
    uint MaxDimension,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] bool? AllowUpscale = null);
public sealed record ToolCapability(string Id, bool Available, IReadOnlyList<string> InputFormats, IReadOnlyList<string> OutputFormats, string Detail);
public sealed record ToolOutput(long FileID, string SourcePath, string OutputPath, string State, string Message);
public sealed record ToolRequest(
    string RequestID,
    string Action,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] IReadOnlyList<long>? FileIDs = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? Destination = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] ToolRecipe? Recipe = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? OperationID = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? DestinationBookmark = null);
public sealed record ToolResponse(
    string RequestID,
    string Status,
    string Message,
    IReadOnlyList<ToolOutput> Outputs,
    IReadOnlyList<ToolCapability> Capabilities,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? OperationID = null);
