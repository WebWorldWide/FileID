using System.Collections.Generic;
using System.Text.Json.Serialization;

namespace FileID.IpcSchema;

public sealed record ChatMessage(string Id, string Role, string Text, double CreatedAt);
public sealed record ChatRequest(
    string RequestID,
    string ConversationID,
    string Action,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? Text = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] bool? UseModel = null);
public sealed record ChatResponse(
    string RequestID,
    string ConversationID,
    string Status,
    string Message,
    IReadOnlyList<ChatMessage> Messages,
    IReadOnlyList<CatalogHit> Hits);
