using FileID.IpcSchema;

namespace FileID.ViewModels;

/// <summary>Best Takes review rules without any UI: wording, default keep marks and
/// which files a keep set sends to Trash. Mirrors the Linux takes_model.rs and the
/// macOS review page (DESIGN_V3_PORT "Best Takes").</summary>
internal static class TakeReviewPolicy
{
    public static string Noun(CatalogTakeAutoGroup group, int count) =>
        (group.Kind == "photos", count == 1) switch
        {
            (true, true) => "photo",
            (true, false) => "photos",
            (false, true) => "clip",
            _ => "clips",
        };

    public static string GroupTitle(CatalogTakeAutoGroup group) => $"{group.Shots.Count} {Noun(group, group.Shots.Count)}";

    public static string GroupSpoken(CatalogTakeAutoGroup group, string date) =>
        $"{GroupTitle(group)}, {date}{(IsOpen(group) ? "" : ", skipped")}";

    public static bool IsOpen(CatalogTakeAutoGroup group) => group.Status != "skipped";

    public static int OpenCount(IReadOnlyList<CatalogTakeAutoGroup> groups) => groups.Count(IsOpen);

    public static int? FirstOpen(IReadOnlyList<CatalogTakeAutoGroup> groups)
    {
        for (var i = 0; i < groups.Count; i++)
        {
            if (IsOpen(groups[i])) return i;
        }
        return null;
    }

    public static CatalogTakeQuality? Best(IReadOnlyList<CatalogTakeQuality> qualities) =>
        qualities.FirstOrDefault(q => q.Recommended) ?? qualities.OrderBy(q => q.Rank).FirstOrDefault();

    public static HashSet<long> DefaultKeep(IReadOnlyList<CatalogTakeQuality> qualities) =>
        Best(qualities) is { } best ? new HashSet<long> { best.PrimaryFileID } : new HashSet<long>();

    public static void Toggle(HashSet<long> keep, long primaryFileId)
    {
        if (!keep.Remove(primaryFileId)) keep.Add(primaryFileId);
    }

    /// <summary>Every file (RAW and JPEG twins included) of the tries that are not kept.</summary>
    public static IReadOnlyList<long> TrashIds(IReadOnlyList<CatalogTakeQuality> qualities, HashSet<long> keep) =>
        qualities.Where(q => !keep.Contains(q.PrimaryFileID)).SelectMany(q => q.FileIDs).Distinct().OrderBy(id => id).ToArray();

    public static int DroppedTries(IReadOnlyList<CatalogTakeQuality> qualities, HashSet<long> keep) =>
        qualities.Count(q => !keep.Contains(q.PrimaryFileID));

    /// <summary>Primary button label and, when it must be disabled, why.</summary>
    public static (string Label, string? DisabledReason) TrashButton(IReadOnlyList<CatalogTakeQuality> qualities, HashSet<long> keep)
    {
        var dropped = DroppedTries(qualities, keep);
        var kept = qualities.Count - dropped;
        var bestOnly = Best(qualities) is { } best && keep.Count == 1 && keep.Contains(best.PrimaryFileID);
        var label = bestOnly
            ? $"Keep the best, move the other {dropped} to Trash"
            : $"Keep {kept}, move the other {dropped} to Trash";
        var reason = kept == 0
            ? "Mark at least one try to keep."
            : dropped == 0 ? "Every try is marked keep. Use Keep all to close this group." : null;
        return (label, reason);
    }

    public static string ConfirmHeading(CatalogTakeAutoGroup group, IReadOnlyList<CatalogTakeQuality> qualities, HashSet<long> keep)
    {
        var dropped = DroppedTries(qualities, keep);
        return $"Move {dropped} {Noun(group, dropped)} to Trash?";
    }

    public static string ConfirmBody(IReadOnlyList<CatalogTakeQuality> qualities, HashSet<long> keep)
    {
        var kept = string.Join(", ", qualities.Where(q => keep.Contains(q.PrimaryFileID)).Select(q => System.IO.Path.GetFileName(q.Path)));
        var files = TrashIds(qualities, keep).Count;
        return $"Keeps {kept}. The others ({files} file{(files == 1 ? "" : "s")}, including RAW copies) go to the Trash. You can undo right after, or put them back from the Trash.";
    }

    public static CatalogTakeDecision Decision(CatalogTakeAutoGroup group, string kind) =>
        new(group.GroupKey, kind, group.Shots.SelectMany(s => s.FileIDs).ToArray());

    /// <summary>Best against the focused try, or against the runner-up when the best is focused.</summary>
    public static (int Best, int Other)? ComparePair(IReadOnlyList<CatalogTakeQuality> qualities, long focused)
    {
        var best = Best(qualities);
        if (best is null) return null;
        var bestIndex = -1;
        for (var i = 0; i < qualities.Count; i++)
        {
            if (qualities[i].PrimaryFileID == best.PrimaryFileID) bestIndex = i;
        }
        if (bestIndex < 0) return null;
        int other;
        if (qualities[bestIndex].PrimaryFileID == focused)
        {
            other = -1;
            for (var i = 0; i < qualities.Count; i++)
            {
                if (i == bestIndex) continue;
                if (other < 0 || qualities[i].Rank < qualities[other].Rank) other = i;
            }
            if (other < 0) return null;
        }
        else
        {
            other = -1;
            for (var i = 0; i < qualities.Count; i++)
            {
                if (qualities[i].PrimaryFileID == focused) other = i;
            }
            if (other < 0) return null;
        }
        return (bestIndex, other);
    }

    public static string FilmstripHeader(CatalogTakeAutoGroup group, HashSet<long> keep, int total) =>
        $"ALL {total} {Noun(group, total).ToUpperInvariant()} · {keep.Count} TO KEEP";
}
