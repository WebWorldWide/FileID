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
        var extra = files > DroppedTries(qualities, keep) ? $" ({files} files, including RAW copies)" : "";
        return $"Keeps {kept}. The others{extra} go to the Trash. You can undo right after, or put them back from the Trash.";
    }

    public static string PrimaryHelp(CatalogTakeAutoGroup group, IReadOnlyList<CatalogTakeQuality> qualities, HashSet<long> keep)
    {
        if (keep.Count == 0) return $"Mark at least one {Noun(group, 1)} to keep (Space)";
        if (DroppedTries(qualities, keep) == 0) return "Everything here is marked keep. Use Keep all.";
        return $"Moves the other {Noun(group, 2)} to the Trash. You can undo right after. (Return)";
    }

    /// <summary>Tries with at least one file the engine reports as moved.</summary>
    public static int MovedShots(IReadOnlyList<CatalogTakeQuality> doomed, IEnumerable<long> movedFileIds)
    {
        var moved = movedFileIds.ToHashSet();
        return doomed.Count(q => q.FileIDs.Any(moved.Contains));
    }

    public static string MovedText(CatalogTakeAutoGroup group, int movedShots, uint failed) =>
        $"Moved {movedShots} {Noun(group, movedShots)} to Trash" + (failed > 0 ? $" · {failed} couldn't be moved" : "");

    public static string PutBackText(uint restored, string? fallback) =>
        restored > 0
            ? $"Put back {restored} file{(restored == 1 ? "" : "s")}"
            : fallback ?? "Nothing was put back. Check the Trash.";

    public static string KeptAllText(CatalogTakeAutoGroup group) =>
        $"Kept all {group.Shots.Count}. This group won't be suggested again.";

    public const string SkippedText = "Skipped. It's at the bottom of the list.";

    /// <summary>After a trash that leaves two or more tries, keep those together so the
    /// group isn't suggested again; null when fewer than two remain.</summary>
    public static CatalogTakeDecision? KeptTogether(CatalogTakeAutoGroup group, IReadOnlyList<CatalogTakeQuality> qualities, HashSet<long> keep, string decision)
    {
        var kept = qualities.Where(q => keep.Contains(q.PrimaryFileID)).ToArray();
        return kept.Length < 2 ? null : new CatalogTakeDecision(group.GroupKey, decision, kept.SelectMany(q => q.FileIDs).ToArray());
    }

    public static int Score(CatalogTakeQuality quality) => (int)Math.Round(quality.Score * 100, MidpointRounding.AwayFromZero);

    public static bool HasRawTwin(CatalogTakeQuality quality) => quality.FileIDs.Count > 1;

    private static readonly HashSet<string> PositiveReasons = new(StringComparer.Ordinal)
    {
        "Sharpest face", "Sharpest", "Eyes open", "Every face visible", "Best exposure",
        "Steadiest", "Most action", "Shows the goal", "Closest to the goal",
    };

    public enum ReasonTone { Neutral, Positive, Goal }

    public static ReasonTone Tone(string reason) =>
        !PositiveReasons.Contains(reason) ? ReasonTone.Neutral
        : reason.Contains("goal", StringComparison.Ordinal) ? ReasonTone.Goal
        : ReasonTone.Positive;

    public static IReadOnlyList<string> Reasons(CatalogTakeQuality quality) => quality.Reasons.Take(4).ToArray();

    public const string NoReasons = "Nothing stands out";

    public static string PaneSpoken(string title, CatalogTakeAutoGroup group, int index, int count, string name) =>
        $"{title}, {Noun(group, 1)} {index} of {count}, {name}";

    public static string TileSpoken(CatalogTakeAutoGroup group, CatalogTakeQuality quality, int index, int count, bool best, bool kept)
    {
        var noun = Noun(group, 1);
        return $"{char.ToUpperInvariant(noun[0])}{noun[1..]} {index} of {count}, {System.IO.Path.GetFileName(quality.Path)}"
            + (best ? ", recommended" : "") + (kept ? ", keep" : "") + $", score {Score(quality)}";
    }

    public static string KeepAnnouncement(CatalogTakeQuality quality, bool kept) =>
        $"{(kept ? "Keeping" : "Not keeping")} {System.IO.Path.GetFileName(quality.Path)}";

    public static string HintLine(CatalogTakeAutoGroup group) =>
        $"← → move · Space keep · Z zoom · Return keeps the marked {Noun(group, 2)} and moves the rest to Trash";

    public static string ZoomLabel(CatalogTakeAutoGroup group, bool zoomed) =>
        zoomed ? $"Show whole {Noun(group, 1)}" : group.Kind == "photos" ? "Zoom to face" : "Zoom in";

    /// <summary>The largest face × 2.6, or the sharp subject × 1.2, kept inside the frame.</summary>
    public static CatalogTakeBox? ZoomBox(CatalogTakeQuality quality)
    {
        var face = quality.Faces.Count == 0 ? null : quality.Faces.MaxBy(f => f.Width * f.Height);
        var source = face ?? quality.Subject;
        if (source is null) return null;
        var factor = face is null ? 1.2 : 2.6;
        var w = Math.Min(1, Math.Max(0.12, source.Width * factor));
        var h = Math.Min(1, Math.Max(0.12, source.Height * factor));
        var x = Math.Min(Math.Max(0, source.X + source.Width / 2 - w / 2), 1 - w);
        var y = Math.Min(Math.Max(0, source.Y + source.Height / 2 - h / 2), 1 - h);
        return new CatalogTakeBox(x, y, w, h);
    }

    /// <summary>Scale and offset that centre <paramref name="box"/> (normalized to the picture)
    /// in a well of <paramref name="wellWidth"/> × <paramref name="wellHeight"/>, for a picture
    /// laid out centred at <paramref name="shownWidth"/> × <paramref name="shownHeight"/>.</summary>
    public static (double Scale, double X, double Y) ZoomTransform(double wellWidth, double wellHeight, double shownWidth, double shownHeight, CatalogTakeBox? box)
    {
        if (box is null || shownWidth <= 0 || shownHeight <= 0 || box.Width <= 0 || box.Height <= 0) return (1, 0, 0);
        var scale = Math.Max(1, Math.Min(wellWidth / (box.Width * shownWidth), wellHeight / (box.Height * shownHeight)));
        var left = (wellWidth - shownWidth) / 2;
        var top = (wellHeight - shownHeight) / 2;
        var centerX = (box.X + box.Width / 2) * shownWidth;
        var centerY = (box.Y + box.Height / 2) * shownHeight;
        return (scale, wellWidth / 2 - left - scale * centerX, wellHeight / 2 - top - scale * centerY);
    }

    public static string DateText(double seconds) =>
        DateTimeOffset.FromUnixTimeSeconds((long)seconds).ToLocalTime().ToString("ddd d MMM, h:mm tt", System.Globalization.CultureInfo.CurrentCulture);

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
