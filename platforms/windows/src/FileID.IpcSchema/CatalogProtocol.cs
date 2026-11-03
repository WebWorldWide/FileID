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

public sealed record CatalogEvent(
    string Id,
    string Title,
    string Goal,
    IReadOnlyList<long> FileIDs,
    bool UserEdited);
public sealed record CatalogTakeFeedback(
    string EventID,
    long FileID,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] double? OutcomeScore,
    bool Preferred);
public sealed record CatalogTake(
    string EventID,
    long FileID,
    string Path,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] double? OutcomeScore,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] double? QualityScore,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] double? Confidence,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? Explanation,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? SourceRevision,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? ModelVersion,
    bool Preferred,
    bool Stale);
public sealed record CatalogTakeRecommendation(
    string EventID,
    string Status,
    IReadOnlyList<long> FileIDs,
    string Reason);

public sealed record CatalogTakeGroupMember(long FileID, string Path);

public sealed record CatalogTakeGroupSuggestion(
    IReadOnlyList<CatalogTakeGroupMember> Members,
    double Similarity,
    string Reason);
public sealed record CatalogTakeBox(double X, double Y, double Width, double Height);

public sealed record CatalogTakeShot(
    long PrimaryFileID,
    IReadOnlyList<long> FileIDs,
    string Path,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] double? CapturedAt = null);

public sealed record CatalogTakeAutoGroup(
    string GroupKey,
    string Kind,
    string Status,
    double CapturedAt,
    double Similarity,
    IReadOnlyList<CatalogTakeShot> Shots);

public sealed record CatalogTakeQuality(
    long PrimaryFileID,
    IReadOnlyList<long> FileIDs,
    string Path,
    double Score,
    int Rank,
    bool Recommended,
    IReadOnlyList<string> Reasons,
    IReadOnlyList<CatalogTakeBox> Faces,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] double? Sharpness = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] double? Exposure = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] bool? MotionBlur = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] int? FaceCount = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] bool? EyesClosed = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] bool? FaceCutOff = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] double? Stability = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] double? Action = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] double? PeakSeconds = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] CatalogTakeBox? Subject = null);

public sealed record CatalogTakeGoalRanking(
    string Goal,
    string Status,
    IReadOnlyList<long> Order,
    IReadOnlyList<long> MetFileIDs,
    string Summary,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? Model = null);

public sealed record CatalogTakeDecision(string GroupKey, string Decision, IReadOnlyList<long> FileIDs);

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
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? TimelineMode = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] CatalogEvent? Event = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? EventID = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] CatalogTakeFeedback? TakeFeedback = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] CatalogTakeDecision? TakeDecision = null);

public sealed record CatalogResponse(
    string RequestID,
    string Status,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? Message,
    IReadOnlyList<CatalogHit> Hits,
    IReadOnlyList<CatalogChapter> Chapters,
    IReadOnlyList<CatalogJob> Jobs,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] IReadOnlyList<CatalogEvent>? Events = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] IReadOnlyList<CatalogTake>? Takes = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] CatalogTakeRecommendation? Recommendation = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] IReadOnlyList<CatalogTakeGroupSuggestion>? SuggestedTakeGroups = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] IReadOnlyList<CatalogTakeAutoGroup>? TakeGroups = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] IReadOnlyList<CatalogTakeQuality>? TakeQualities = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] CatalogTakeGoalRanking? TakeGoalRanking = null);

