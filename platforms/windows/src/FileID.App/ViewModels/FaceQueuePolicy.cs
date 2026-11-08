using FileID.IpcSchema;

namespace FileID.ViewModels;

/// <summary>Text, counts and visibility for the People review queues (faces, merges, splits),
/// Accept all suggested and the face refresh job. Lockstep with macOS FaceQueues and GTK face_queues.</summary>
internal static class FaceQueuePolicy
{
    public const string Faces = "faces";
    public const string Merges = "merges";
    public const string Splits = "splits";
    public const string Refresh = "refresh";
    public const string AcceptAll = "acceptAll";
    public const string Merge = "merge";
    public const string NotSamePerson = "notSamePerson";
    public const string Dismiss = "dismiss";
    public const string Split = "split";
    public const string JobKind = "faceRefresh";
    public const string JobId = "face-refresh";
    public const string Unnamed = "Unnamed";
    public const string LowQualityText = "Small or blurry face";
    public const string UpdateFacesText = "Update faces";
    public const string PauseLabel = "Pause updating faces";
    public const string ResumeLabel = "Resume updating faces";
    public const string CancelLabel = "Stop updating faces";
    public const string Dismissed = "Dismissed the suggestion";
    public const int StripSize = 6;

    public static string Name(string? name) => string.IsNullOrWhiteSpace(name) ? Unnamed : name.Trim();

    public static string Plural(long count, string one, string many) => count == 1 ? $"1 {one}" : $"{count} {many}";

    public static string FaceCount(long count) => Plural(Math.Max(0, count), "face", "faces");

    public static bool IsLowQuality(FaceReviewItem item) => item.LowQuality == true;

    public static string LowQualityNote(FaceReviewItem item) => IsLowQuality(item) ? LowQualityText : "";

    public static int SuggestedCount(IEnumerable<FaceReviewItem> items) => items.Count(item => item.Reason == "suggested");

    public static bool ShowAcceptAll(long? personId, int suggested) => personId is not null && suggested > 0;

    public static string AcceptAllText(int suggested) => $"Accept all suggested ({suggested})";

    public static string AcceptAllLabel(int suggested, string? personName) =>
        $"Accept all {Plural(suggested, "suggested face", "suggested faces")} for {FaceReviewPolicy.Name(personName)}";

    public static string AcceptAllMessage(FaceReviewPage page)
    {
        var accepted = page.AcceptedFaceIds?.Count ?? 0;
        var skipped = page.SkippedFaceIds?.Count ?? 0;
        var text = $"Accepted {FaceCount(accepted)}";
        return skipped > 0 ? $"{text} · {skipped} skipped, already in their photo" : text;
    }

    public static string QueueTitle(string queue) => queue switch
    {
        Merges => "Merges",
        Splits => "Splits",
        _ => "Faces",
    };

    public static string QueueText(string queue, long total) => $"{QueueTitle(queue)} ({Math.Max(0, total)})";

    public static string QueueLabel(string queue, long total)
    {
        var count = Math.Max(0, total);
        return queue switch
        {
            Merges => $"Merges: {Plural(count, "pair of people", "pairs of people")} who may be the same person",
            Splits => $"Splits: {Plural(count, "person", "people")} who may be two people",
            _ => $"Faces: {Plural(count, "face", "faces")} to review",
        };
    }

    public static string EmptyText(string queue) => queue switch
    {
        Merges => "No people look like the same person.",
        Splits => "No one looks like two different people.",
        _ => FaceReviewPolicy.EmptyText,
    };

    public static string QueueHelp(string queue) => queue switch
    {
        Merges => "People who look like the same person. Merging keeps the first name and moves every face over.",
        Splits => "People whose faces look like two different people. Splitting moves one group to a new person.",
        _ => "New faces that look like someone you already have. Nothing is added to a person until you accept it.",
    };

    public static string? ListQueue(string queue) => queue == Faces ? null : queue;

    public static string Normalize(string? queue) => queue is Merges or Splits ? queue : Faces;

    public static string MergeRowLabel(FaceMergeSuggestion merge) =>
        $"{Name(merge.OtherName)} ({FaceCount(merge.OtherFaceCount)}) may be the same person as {Name(merge.PersonName)} ({FaceCount(merge.FaceCount)}), {FaceReviewPolicy.Percent(merge.Score)}% match.";

    public static string MergeLabel(FaceMergeSuggestion merge) => $"Merge {Name(merge.OtherName)} into {Name(merge.PersonName)}";

    public static string NotSameLabel(FaceMergeSuggestion merge) =>
        $"{Name(merge.OtherName)} and {Name(merge.PersonName)} are not the same person";

    public static string DismissMergeLabel(FaceMergeSuggestion merge) =>
        $"Dismiss merging {Name(merge.OtherName)} into {Name(merge.PersonName)}";

    public static string MergeMessage(string resolution, FaceMergeSuggestion merge) => resolution switch
    {
        Merge => $"Merged {Name(merge.OtherName)} into {Name(merge.PersonName)}",
        NotSamePerson => $"Kept {Name(merge.OtherName)} and {Name(merge.PersonName)} apart",
        _ => Dismissed,
    };

    public static string KeepCaption(FaceSplitSuggestion split) => $"Stays ({Math.Max(0, split.KeepCount)})";

    public static string MoveCaption(FaceSplitSuggestion split) => $"Moves to a new person ({Math.Max(0, split.SplitCount)})";

    public static string SplitRowLabel(FaceSplitSuggestion split) =>
        $"{Name(split.PersonName)} may be two people: {FaceCount(split.KeepCount)} stay, {FaceCount(split.SplitCount)} move to a new person.";

    public static string SplitLabel(FaceSplitSuggestion split) => $"Split {Name(split.PersonName)} into two people";

    public static string DismissSplitLabel(FaceSplitSuggestion split) => $"Dismiss splitting {Name(split.PersonName)}";

    public static string SplitMessage(string resolution, FaceSplitSuggestion split) =>
        resolution == Split ? $"Split {Name(split.PersonName)} into two people" : Dismissed;

    public static IReadOnlyList<long> Strip(IReadOnlyList<long>? faceIds) =>
        (faceIds ?? Array.Empty<long>()).Where(id => id > 0).Distinct().Take(StripSize).ToArray();

    public static string StripFaceLabel(string? personName, int index) => $"Face {index + 1} of {Name(personName)}";

    public static CatalogRequest EnqueueRequest(string requestId) =>
        new(requestId, "enqueueFaceRefresh", null, null, null, null, null, null);

    public static CatalogJob? Job(IEnumerable<CatalogJob>? jobs) => jobs?.FirstOrDefault(job => job.Kind == JobKind);

    public static bool IsJobUpdate(CatalogResponse? response) => Job(response?.Jobs) is not null;

    /// <summary>The job to show and control: a pushed job updated with the newer status read after it,
    /// or one built from the status when no push has arrived yet.</summary>
    public static CatalogJob? MergeJob(CatalogJob? job, FaceRefreshStatus? status)
    {
        if (status?.State is not { } state) return job;
        if (job is null)
            return new CatalogJob(status.JobId ?? JobId, JobKind, Array.Empty<long>(), state, status.Progress ?? 0, null, 0, 0);
        return job with { State = state, Progress = status.Progress ?? job.Progress, Error = state == job.State ? job.Error : null };
    }

    public static bool IsActive(string? state) => state is "queued" or "running" or "paused";

    public static int Percent(double? progress) =>
        progress is { } p && double.IsFinite(p) ? (int)Math.Clamp(Math.Floor(p * 100), 0, 100) : 0;

    public static FaceRefreshView RefreshView(FaceRefreshStatus? status, CatalogJob? job)
    {
        var state = job?.State ?? status?.State;
        var progress = job?.Progress ?? status?.Progress;
        var outdated = Math.Max(0, status?.Outdated ?? 0);
        var active = IsActive(state);
        if (!active && outdated == 0) return FaceRefreshView.Hidden;
        var note = state is "paused" or "failed" && !string.IsNullOrWhiteSpace(job?.Error) ? job!.Error!.Trim() : "";
        if (!active)
        {
            return new FaceRefreshView(true, $"Faces need updating ({outdated})", true, false, false, false, note,
                $"{Plural(outdated, "face needs", "faces need")} updating for the current face model");
        }
        var text = $"Updating faces: {Percent(progress)}%" + Tally(status);
        if (state == "paused") text += " · paused";
        return new FaceRefreshView(true, text, false, CatalogJobActions.CanPause(state), state == "paused",
            CatalogJobActions.CanCancel(state), note, text);
    }

    private static string Tally(FaceRefreshStatus? status)
    {
        if (status is null) return "";
        var parts = new List<string>(3);
        if (status.Missing > 0) parts.Add($"{status.Missing} missing");
        if (status.Offline > 0) parts.Add($"{status.Offline} offline");
        if (status.Failed > 0) parts.Add($"{status.Failed} failed");
        return parts.Count == 0 ? "" : " · " + string.Join(" · ", parts);
    }

    public static string UpdateLabel(long outdated) => $"Update faces, {Plural(Math.Max(0, outdated), "face needs", "faces need")} updating";
}

internal sealed record FaceRefreshView(
    bool Visible,
    string Text,
    bool CanUpdate,
    bool CanPause,
    bool CanResume,
    bool CanCancel,
    string Note,
    string Label)
{
    public static readonly FaceRefreshView Hidden = new(false, "", false, false, false, false, "", "");
}

/// <summary>One page-at-a-time review list keyed by a suggestion identity, deduped across pages.</summary>
internal sealed class KeyedReviewQueue<TItem, TKey> where TKey : notnull
{
    private readonly List<TItem> _items = new();
    private readonly Func<TItem, TKey> _key;

    public KeyedReviewQueue(Func<TItem, TKey> key) => _key = key;

    public IReadOnlyList<TItem> Items => _items;
    public long Total { get; private set; }
    public long NextOffset => _items.Count;
    public bool HasMore => _items.Count < Total;
    public bool IsEmpty => Total == 0 && _items.Count == 0;

    public IReadOnlyList<TItem> Reset(IReadOnlyList<TItem>? items, long total)
    {
        _items.Clear();
        return Append(items, total);
    }

    public IReadOnlyList<TItem> Append(IReadOnlyList<TItem>? items, long total)
    {
        Total = Math.Max(0, total);
        var known = _items.Select(_key).ToHashSet();
        var added = (items ?? Array.Empty<TItem>()).Where(item => known.Add(_key(item))).ToList();
        _items.AddRange(added);
        return added;
    }

    public bool Remove(TKey key)
    {
        var index = _items.FindIndex(item => EqualityComparer<TKey>.Default.Equals(_key(item), key));
        if (index < 0) return false;
        _items.RemoveAt(index);
        Total = Math.Max(0, Total - 1);
        return true;
    }
}

/// <summary>Per-queue totals for the switcher. Faces is the existing suggestions queue.</summary>
internal sealed class FaceQueueTotals
{
    private readonly Dictionary<string, long> _totals = new() { [FaceQueuePolicy.Faces] = 0, [FaceQueuePolicy.Merges] = 0, [FaceQueuePolicy.Splits] = 0 };

    public long this[string queue] => _totals.TryGetValue(FaceQueuePolicy.Normalize(queue), out var total) ? total : 0;

    public long Sum => _totals.Values.Sum();

    public void Set(string? queue, long total) => _totals[FaceQueuePolicy.Normalize(queue)] = Math.Max(0, total);
}
