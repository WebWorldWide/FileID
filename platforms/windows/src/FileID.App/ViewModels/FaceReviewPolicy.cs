using FileID.IpcSchema;

namespace FileID.ViewModels;

internal static class FaceReviewPolicy
{
    public const int PageSize = 50;
    public const string Accept = "accept";
    public const string Reject = "reject";
    public const string Ignore = "ignore";
    public const string UnnamedPerson = "an unnamed person";
    public const string IgnoreLabel = "Ignore this face";
    public const string UndoLabel = "Undo the last review";
    public const string EmptyText = "No suggestions to review.";
    public const string NothingChanged = "Nothing changed.";
    private static readonly char[] Separators = { '/', '\\' };

    public static string Name(string? personName) =>
        string.IsNullOrWhiteSpace(personName) ? UnnamedPerson : personName.Trim();

    public static int Percent(double score) =>
        double.IsFinite(score) ? (int)Math.Clamp(Math.Round(score * 100, MidpointRounding.AwayFromZero), 0, 100) : 0;

    public static string Reason(FaceReviewItem item)
    {
        var name = Name(item.PersonName);
        var pct = Percent(item.Score);
        return item.Reason switch
        {
            "suggested" => $"Looks like {name} ({pct}% match).",
            "uncertain" => $"Might be {name}, but the match is weak ({pct}%).",
            "ambiguous" when item.RunnerUpName is not null || item.RunnerUpPersonId is not null =>
                $"Could be {name} or {Name(item.RunnerUpName)}; both look about as close.",
            "ambiguous" => $"Could be {name}, but it's a close call.",
            _ => $"Might be {name} ({pct}%).",
        };
    }

    public static string FileName(string path)
    {
        var trimmed = path.TrimEnd(Separators);
        var cut = trimmed.LastIndexOfAny(Separators);
        var name = cut >= 0 ? trimmed[(cut + 1)..] : trimmed;
        return name.Length > 0 ? name : path;
    }

    public static string FaceLabel(FaceReviewItem item) => $"Face in {FileName(item.Path)}";
    public static string PersonLabel(FaceReviewItem item) => Name(item.PersonName);
    public static string AcceptLabel(FaceReviewItem item) => $"Accept: this is {Name(item.PersonName)}";
    public static string RejectLabel(FaceReviewItem item) => $"Not {Name(item.PersonName)}";
    public static string RowLabel(FaceReviewItem item) => $"{Reason(item)} In {FileName(item.Path)}.";
    public static string Header(long total) => $"Review suggestions, {total} waiting";
    public static string ButtonText(long total) => $"Review suggestions ({total})";

    public static string ChangeLabel(string resolution, string? personName) => resolution switch
    {
        Accept => $"Accepted a face as {Name(personName)}",
        Reject => $"Marked a face as not {Name(personName)}",
        _ => "Ignored a face",
    };

    public static bool IsOk(FaceReviewPage page) => page.Status == "ok";

    public static string? Failure(FaceReviewPage page) =>
        IsOk(page) ? null : string.IsNullOrWhiteSpace(page.Message) ? NothingChanged : page.Message;
}

internal sealed class FaceReviewQueue
{
    private readonly List<FaceReviewItem> _items = new();

    public IReadOnlyList<FaceReviewItem> Items => _items;
    public long Total { get; private set; }
    public long NextOffset => _items.Count;
    public bool HasMore => _items.Count < Total;
    public bool IsEmpty => Total == 0 && _items.Count == 0;

    public IReadOnlyList<FaceReviewItem> Reset(FaceReviewPage page)
    {
        _items.Clear();
        return Append(page);
    }

    public IReadOnlyList<FaceReviewItem> Append(FaceReviewPage page)
    {
        if (!FaceReviewPolicy.IsOk(page)) return Array.Empty<FaceReviewItem>();
        Total = Math.Max(0, page.Total);
        var known = _items.Select(item => item.FaceId).ToHashSet();
        var added = page.Items.Where(item => known.Add(item.FaceId)).ToList();
        _items.AddRange(added);
        return added;
    }

    public bool Remove(long faceId)
    {
        var index = _items.FindIndex(item => item.FaceId == faceId);
        if (index < 0) return false;
        _items.RemoveAt(index);
        Total = Math.Max(0, Total - 1);
        return true;
    }
}
