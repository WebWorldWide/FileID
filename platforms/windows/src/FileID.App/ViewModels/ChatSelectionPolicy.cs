using FileID.IpcSchema;

namespace FileID.ViewModels;

/// <summary>Files attached to the next chat question ("12 selected photos").</summary>
internal sealed record ChatScope(IReadOnlyList<long> FileIDs, string Label);

/// <summary>Exact-match questions over selected files (macOS <c>ChatScope</c> and
/// <c>SelectionResultsStrip</c>): the scope label, the narrowed scope after an
/// answer, and what the strip's buttons act on.</summary>
internal static class ChatSelectionPolicy
{
    public const int MaxFiles = 2000;
    private static readonly HashSet<string> PlayableExtensions = new(StringComparer.OrdinalIgnoreCase)
    {
        ".mp4", ".mov", ".m4v", ".avi", ".mkv", ".wmv", ".webm", ".mts", ".m2ts", ".3gp",
    };

    public static string Noun(IReadOnlyCollection<string> kinds, int count)
    {
        var noun = kinds.All(kind => kind == "image") ? "photo" : kinds.All(kind => kind == "video") ? "video" : "file";
        return count == 1 ? noun : noun + "s";
    }

    public static string KindOf(string path) => PlayableExtensions.Contains(Path.GetExtension(path)) ? "video" : "image";

    public static ChatScope? Selected(IEnumerable<long> ids, IReadOnlyCollection<string> kinds)
    {
        var unique = ids.Where(id => id > 0).Distinct().Take(MaxFiles).ToArray();
        return unique.Length == 0 ? null : new ChatScope(unique, $"{unique.Length} selected {Noun(kinds, unique.Length)}");
    }

    public static ChatScope? Narrowed(IReadOnlyList<CatalogHit> hits)
    {
        var yes = hits.Where(hit => hit.Match == "yes").Select(hit => hit.FileID).ToArray();
        return yes.Length == 0 ? null : new ChatScope(yes, $"{yes.Length} {(yes.Length == 1 ? "match" : "matches")} from the last answer");
    }

    public static bool HasResults(IReadOnlyList<CatalogHit> hits) => hits.Any(hit => hit.Match is not null);

    /// <summary>The chosen tiles, or every match when none are chosen (every file when there are no matches).</summary>
    public static IReadOnlyList<long> Target(IReadOnlyList<CatalogHit> hits, IReadOnlySet<long> chosen)
    {
        if (chosen.Count > 0) return hits.Select(hit => hit.FileID).Where(chosen.Contains).ToArray();
        var matches = hits.Where(hit => hit.Match == "yes").Select(hit => hit.FileID).ToArray();
        return matches.Length > 0 ? matches : hits.Select(hit => hit.FileID).ToArray();
    }

    public static string AskLabel(IReadOnlyList<CatalogHit> hits, int chosen)
    {
        var matches = hits.Count(hit => hit.Match == "yes");
        return (chosen, matches) switch
        {
            (0, 0) => $"Ask about these {hits.Count}",
            (0, 1) => "Ask about the 1 match",
            (0, _) => $"Ask about the {matches} matches",
            _ => $"Ask about {chosen} selected",
        };
    }

    /// <summary>What a Best Takes review attaches: the kept tries when only some are kept, otherwise every try.</summary>
    public static (IReadOnlyList<long> Ids, string Label) TakeTargets(IReadOnlyList<long> primaries, IReadOnlySet<long> keep)
    {
        var subset = keep.Count > 0 && keep.Count < primaries.Count;
        var ids = primaries.Where(id => !subset || keep.Contains(id)).ToArray();
        return (ids, subset ? $"Ask about {ids.Length} kept" : $"Ask about all {ids.Length}");
    }

    public static string MatchSpoken(CatalogHit hit) =>
        $"{(hit.Match == "yes" ? "Match" : "Not sure")}: {Path.GetFileName(hit.Path)}. {hit.Text}";
}
