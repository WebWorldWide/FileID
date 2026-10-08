using System.Collections.Generic;
using System.Text.Json.Serialization;

namespace FileID.IpcSchema;

public sealed record CatalogChapter(
    string Id,
    long FileID,
    double StartSeconds,
    double EndSeconds,
    string Title,
    string Summary,
    string SourceRevision,
    string ModelVersion,
    double Confidence,
    bool UserEdited,
    bool Stale);

public sealed record CatalogHit(
    long FileID,
    string Path,
    string Kind,
    string Text,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? EvidenceID,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] double? StartSeconds,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] long? Page);

public sealed record CatalogJob(
    string Id,
    string Kind,
    IReadOnlyList<long> FileIDs,
    string State,
    double Progress,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? Error,
    double CreatedAt,
    double UpdatedAt);

public sealed record CatalogRequest(
    string RequestID,
    string Action,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? Query,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] long? FileID,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] CatalogChapter? Chapter,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? ChapterID,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? JobID,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] IReadOnlyList<long>? FileIDs,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? SearchMode = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] IReadOnlyList<float>? QueryVector = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? EmbeddingModel = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] int? Limit = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? ResultScope = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? TimelineMode = null);

public sealed record CatalogResponse(
    string RequestID,
    string Status,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? Message,
    IReadOnlyList<CatalogHit> Hits,
    IReadOnlyList<CatalogChapter> Chapters,
    IReadOnlyList<CatalogJob> Jobs);

