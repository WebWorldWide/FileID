using FileID.IpcSchema;

namespace FileID.ViewModels;

/// <summary>Search &amp; Moments timeline geometry (macOS MomentTimeline, GTK
/// moments_timeline.rs): the Chapters lane, the gold playhead and the ruler.
/// Examined and Speech lanes are not drawn here because the catalog IPC
/// reply for a file carries chapters only.</summary>
internal static class MomentTimelinePolicy
{
    public enum ChapterState { Accepted, Draft, Stale }

    public enum TickAnchor { Leading, Center, Trailing }

    public const double MinChapterWidth = 3;
    public const double KeyboardStep = 5;
    private const int MaxTicks = 8;
    private static readonly double[] Steps = { 1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600, 7200 };

    public static ChapterState State(CatalogChapter chapter) =>
        chapter.Stale ? ChapterState.Stale
        : chapter.UserEdited || chapter.ModelVersion == "user" ? ChapterState.Accepted
        : ChapterState.Draft;

    /// <summary>The player's length when known, else the end of the last chapter (at least 1 s).</summary>
    public static double Duration(double? natural, IReadOnlyList<CatalogChapter> chapters)
    {
        if (natural is { } known && double.IsFinite(known) && known > 0) return known;
        var end = chapters.Count == 0 ? 0 : chapters.Max(chapter => Math.Max(chapter.EndSeconds, chapter.StartSeconds));
        return double.IsFinite(end) ? Math.Max(end, 1) : 1;
    }

    public static double X(double seconds, double duration, double width)
    {
        if (duration <= 0 || !double.IsFinite(seconds) || width <= 0) return 0;
        return Math.Clamp(seconds, 0, duration) / duration * width;
    }

    public static (double X, double Width) ChapterSpan(CatalogChapter chapter, double duration, double width)
    {
        var x0 = X(chapter.StartSeconds, duration, width);
        var x1 = Math.Max(x0 + MinChapterWidth, X(Math.Max(chapter.EndSeconds, chapter.StartSeconds), duration, width) - 1);
        return (x0, x1 - x0);
    }

    public static double SeekSeconds(double x, double width, double duration) =>
        duration <= 0 ? 0 : Math.Clamp(x / Math.Max(width, 1), 0, 1) * duration;

    /// <summary>First step from the macOS list that gives at most eight ticks.</summary>
    public static double RulerStep(double duration)
    {
        foreach (var step in Steps)
        {
            if (duration / step <= MaxTicks) return step;
        }
        return Math.Max(Steps[^1], duration / MaxTicks);
    }

    public static IReadOnlyList<(double Seconds, TickAnchor Anchor)> Ticks(double duration)
    {
        if (!double.IsFinite(duration) || duration <= 0) return new[] { (0d, TickAnchor.Leading) };
        var step = RulerStep(duration);
        var ticks = new List<(double, TickAnchor)>();
        for (var i = 0; i * step <= duration + 0.001; i++)
        {
            var t = i * step;
            var anchor = i == 0 ? TickAnchor.Leading : t + step > duration + 0.001 ? TickAnchor.Trailing : TickAnchor.Center;
            ticks.Add((t, anchor));
        }
        return ticks;
    }

    public static bool IsCurrent(CatalogChapter chapter, double playhead, bool playing) =>
        playing && playhead >= chapter.StartSeconds && playhead < Math.Max(chapter.EndSeconds, chapter.StartSeconds + 0.5);

    public static string LaneValue(int chapters) =>
        chapters == 0 ? "None" : $"{chapters} chapter{(chapters == 1 ? "" : "s")}";

    public static string LaneSpoken(int chapters, double playhead) =>
        $"{LaneValue(chapters)}, playhead at {HitPresentation.Clock(playhead)}";

    public static string PlayerMeta(double position, double duration) =>
        $"{HitPresentation.Clock(position)} / {HitPresentation.Clock(duration)}";

    /// <summary>The lavender note over the chapter list while drafts are unreviewed.</summary>
    public static string? DraftNote(IReadOnlyList<CatalogChapter> chapters)
    {
        var drafts = chapters.Where(chapter => State(chapter) == ChapterState.Draft).ToArray();
        if (drafts.Any(chapter => chapter.ModelVersion.StartsWith("timeline-moment-sequence-v1/", StringComparison.Ordinal)))
        {
            return "Suggested chapters are drafts. Check what happens and when before you accept them.";
        }
        if (drafts.Any(chapter => chapter.ModelVersion.StartsWith("timeline-chapter-suggestion-v1/", StringComparison.Ordinal)))
        {
            return "Suggested chapters come from a few frames, so short moments can be missed. Check them before relying on them.";
        }
        return null;
    }
}
