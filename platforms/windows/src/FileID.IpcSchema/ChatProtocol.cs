using System.Collections.Generic;
using System.Text.Json.Serialization;

namespace FileID.IpcSchema;

public sealed record ChatMessage(string Id, string Role, string Text, double CreatedAt);
public sealed record ChatContext(
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? Page = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] IReadOnlyList<long>? FileIDs = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] IReadOnlyList<long>? PersonIDs = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? Folder = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? LibraryRoot = null);
public sealed record ChatRequest(
    string RequestID,
    string ConversationID,
    string Action,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? Text = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] bool? UseModel = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? ModelKind = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] ChatContext? Context = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] IReadOnlyList<float>? QueryVector = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? EmbeddingModel = null);
public sealed record ChatRename(long FileID, string NewName);
/// <summary>A proposed FileID action. Run it only through the existing command it maps to,
/// and only after a confirmation card when RequiresConfirmation is true.</summary>
public sealed record ChatAction(
    string Id,
    string Kind,
    string Title,
    string Detail,
    bool RequiresConfirmation,
    string Source,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? Undo = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] long? FileCount = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] long? ByteCount = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] IReadOnlyList<string>? Examples = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] IReadOnlyList<long>? FileIDs = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] IReadOnlyList<long>? KeeperFileIDs = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? Path = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? Query = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] IReadOnlyList<string>? Kinds = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? Page = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] long? PersonID = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] IReadOnlyList<long>? PersonIDs = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? Name = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? Job = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? Setting = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] bool? Value = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] IReadOnlyList<ChatRename>? Renames = null);
public sealed record ChatResponse(
    string RequestID,
    string ConversationID,
    string Status,
    string Message,
    IReadOnlyList<ChatMessage> Messages,
    IReadOnlyList<CatalogHit> Hits,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] IReadOnlyList<ChatAction>? Actions = null);
