using FileID.IpcSchema;
using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class MomentsPresentationTests
{
    private static CatalogHit Hit(string kind, double? start = null, long? page = null, string text = "") =>
        new(7, "C:\\Videos\\Birthday\\party.mov", kind, text, null, start, page);

    private static CatalogChapter Chapter(double start, double end, bool userEdited = false, bool stale = false, string model = "timeline-moment-sequence-v1/abc") =>
        new("c", 7, start, end, "Gift", "", "", userEdited ? "user" : model, 0.6, userEdited, stale);

    [Fact]
    public void ResultRowsShowTimePageAndKindLikeTheMac()
    {
        Assert.Equal("1:05 · Chapter", HitPresentation.ResultMeta(Hit("chapter", 65.4)));
        Assert.Equal("Page 3 · Text in document", HitPresentation.ResultMeta(Hit("documentText", page: 3)));
        Assert.Equal("1:00:00 · Video frame, not checked", HitPresentation.ResultMeta(Hit("sampledFrame", 3600)));
        Assert.Equal("PDF", HitPresentation.ResultMeta(Hit("pdf")));
        Assert.Equal("Image", HitPresentation.ResultMeta(Hit("image")));
        Assert.Equal("party.mov, 0:12 · Speech or text", HitPresentation.ResultSpoken(Hit("passage", 12)));
        Assert.Equal("party.mov", HitPresentation.FileName("C:\\Videos\\Birthday\\party.mov"));
        Assert.Equal("C:\\Videos\\Birthday", HitPresentation.Folder("C:\\Videos\\Birthday\\party.mov"));
        Assert.Equal("party.mov", HitPresentation.FileName("/home/a/party.mov"));
        Assert.Equal("0:00", HitPresentation.Clock(double.NaN));
        Assert.Equal("Type and press Enter", HitPresentation.ResultsMeta(0, searched: false));
        Assert.Equal("No results", HitPresentation.ResultsMeta(0, searched: true));
        Assert.Equal("1 result", HitPresentation.ResultsMeta(1, searched: true));
    }

    [Fact]
    public void SourcesOnlyCallOutTimePageAndUncheckedFrames()
    {
        Assert.Equal("0:42", HitPresentation.SourceMeta(Hit("chapter", 42)));
        Assert.Equal("", HitPresentation.SourceMeta(Hit("ocrText")));
        Assert.Equal("0:03 · Video frame, not checked", HitPresentation.SourceMeta(Hit("sampledFrame", 3)));
        Assert.Equal("Source 2: party.mov, Page 4", HitPresentation.SourceSpoken(2, Hit("documentText", page: 4)));
        Assert.Equal("Source 1: party.mov", HitPresentation.SourceSpoken(1, Hit("ocrText")));
    }

    [Fact]
    public void RulerStepsGiveAtMostEightTicksWithEdgeAnchors()
    {
        Assert.Equal(1, MomentTimelinePolicy.RulerStep(8));
        Assert.Equal(2, MomentTimelinePolicy.RulerStep(9));
        Assert.Equal(15, MomentTimelinePolicy.RulerStep(100));
        Assert.Equal(7200, MomentTimelinePolicy.RulerStep(50_000));
        Assert.True(MomentTimelinePolicy.Ticks(1_000_000).Count <= 9);
        var ticks = MomentTimelinePolicy.Ticks(30);
        Assert.Equal(7, ticks.Count);
        Assert.Equal(MomentTimelinePolicy.TickAnchor.Leading, ticks[0].Anchor);
        Assert.Equal(MomentTimelinePolicy.TickAnchor.Center, ticks[3].Anchor);
        Assert.Equal((30d, MomentTimelinePolicy.TickAnchor.Trailing), ticks[^1]);
        Assert.Single(MomentTimelinePolicy.Ticks(0));
    }

    [Fact]
    public void ChaptersMapOntoTheLaneAndClicksSeek()
    {
        var chapters = new[] { Chapter(10, 20), Chapter(40, 40) };
        Assert.Equal(40, MomentTimelinePolicy.Duration(null, chapters));
        Assert.Equal(95.5, MomentTimelinePolicy.Duration(95.5, chapters));
        Assert.Equal(1, MomentTimelinePolicy.Duration(0, Array.Empty<CatalogChapter>()));
        Assert.Equal((25d, 24d), MomentTimelinePolicy.ChapterSpan(chapters[0], 40, 100));
        Assert.Equal(MomentTimelinePolicy.MinChapterWidth, MomentTimelinePolicy.ChapterSpan(chapters[1], 40, 100).Width);
        Assert.Equal(100, MomentTimelinePolicy.X(999, 40, 100));
        Assert.Equal(0, MomentTimelinePolicy.X(5, 0, 100));
        Assert.Equal(20, MomentTimelinePolicy.SeekSeconds(50, 100, 40));
        Assert.Equal(40, MomentTimelinePolicy.SeekSeconds(150, 100, 40));
        Assert.True(MomentTimelinePolicy.IsCurrent(chapters[0], 15, playing: true));
        Assert.False(MomentTimelinePolicy.IsCurrent(chapters[0], 15, playing: false));
        Assert.True(MomentTimelinePolicy.IsCurrent(chapters[1], 40.2, playing: true));
        Assert.Equal("2 chapters, playhead at 0:15", MomentTimelinePolicy.LaneSpoken(MomentTimelinePolicy.LaneValue(2), 15));
        Assert.Equal("None", MomentTimelinePolicy.LaneValue(0));
        Assert.Equal("0:15 / 1:40", MomentTimelinePolicy.PlayerMeta(15, 100));
    }

    [Fact]
    public void ChapterStatesAndDraftNotesFollowWhoWroteThem()
    {
        Assert.Equal(MomentTimelinePolicy.ChapterState.Draft, MomentTimelinePolicy.State(Chapter(0, 1)));
        Assert.Equal(MomentTimelinePolicy.ChapterState.Accepted, MomentTimelinePolicy.State(Chapter(0, 1, userEdited: true)));
        Assert.Equal(MomentTimelinePolicy.ChapterState.Stale, MomentTimelinePolicy.State(Chapter(0, 1, userEdited: true, stale: true)));
        Assert.StartsWith("Suggested chapters are drafts", MomentTimelinePolicy.DraftNote(new[] { Chapter(0, 1) }), StringComparison.Ordinal);
        Assert.StartsWith("Suggested chapters come from a few frames", MomentTimelinePolicy.DraftNote(new[] { Chapter(0, 1, model: "timeline-chapter-suggestion-v1/x") }), StringComparison.Ordinal);
        Assert.Null(MomentTimelinePolicy.DraftNote(new[] { Chapter(0, 1, userEdited: true), Chapter(0, 1, stale: true) }));
    }

    [Fact]
    public void LaneSpansAreClampedSortedAndMergedWithinTheGap()
    {
        var spans = new (double Start, double End)[] { (30, 35), (-4, 2), (10, 12), (11, 15), (double.NaN, 3), (16, double.NaN), (90, 120), (50, 40) };
        var merged = MomentTimelinePolicy.MergeSpans(spans, 100);
        Assert.Equal("0-2 10-15 16-16 30-35 50-50 90-100", Describe(merged));
        Assert.Equal("0-2 10-16 30-35 50-50 90-100", Describe(MomentTimelinePolicy.MergeSpans(spans, 100, gap: 1)));
        Assert.Equal("0-35 50-50 90-100", Describe(MomentTimelinePolicy.MergeSpans(spans, 100, gap: 14)));
        Assert.Empty(MomentTimelinePolicy.MergeSpans(Array.Empty<(double, double)>(), 100));
        Assert.Equal(0.5, MomentTimelinePolicy.PixelSeconds(100, 200));
        Assert.Equal(0, MomentTimelinePolicy.PixelSeconds(100, 0));
    }

    [Fact]
    public void ExaminedPointsBecomeTicksAndSpeechKeepsAMinimumWidth()
    {
        Assert.Equal((9.5, 1d, true), MomentTimelinePolicy.ExaminedMark((10, 10.5), 100, 100));
        Assert.Equal((0d, 1d, true), MomentTimelinePolicy.ExaminedMark((0, 0), 100, 100));
        Assert.Equal((10d, 5d, false), MomentTimelinePolicy.ExaminedMark((10, 15), 100, 100));
        Assert.Equal((20d, 2d), MomentTimelinePolicy.SpeechMark((20, 20), 100, 100));
        Assert.Equal((20d, 10d), MomentTimelinePolicy.SpeechMark((20, 30), 100, 100));
        Assert.Equal("Not analyzed yet", MomentTimelinePolicy.ExaminedValue(0));
        Assert.Equal("1 sampled frame", MomentTimelinePolicy.ExaminedValue(1));
        Assert.Equal("3 passages", MomentTimelinePolicy.SpeechValue(3));
        Assert.Equal("None", MomentTimelinePolicy.SpeechValue(0));
    }

    [Fact]
    public void DurationFallsBackToTheLatestEndOfAnyLane()
    {
        var examined = new (double Start, double End)[] { (0, 12), (55, 60) };
        var speech = new (double Start, double End)[] { (70, 72), (double.NaN, double.NaN) };
        Assert.Equal(72, MomentTimelinePolicy.Duration(null, new[] { Chapter(10, 20) }, examined, speech));
        Assert.Equal(20, MomentTimelinePolicy.Duration(null, new[] { Chapter(10, 20) }));
        Assert.Equal(30, MomentTimelinePolicy.Duration(30, new[] { Chapter(10, 20) }, examined, speech));
    }

    private static string Describe(IReadOnlyList<(double Start, double End)> spans) =>
        string.Join(" ", spans.Select(span => FormattableString.Invariant($"{span.Start:0.##}-{span.End:0.##}")));
}
