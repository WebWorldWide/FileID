using FileID.IpcSchema;
using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class TakeReviewPolicyTests
{
    private static readonly long[] TrashedWithTwins = { 1, 3, 11 };
    private static readonly long[] OnlyBest = { 2 };
    private static readonly long[] WholeGroup = { 1, 2 };
    private static CatalogTakeQuality Quality(long id, int rank, bool recommended, params long[] files) =>
        new(id, files, $"C:\\shoot\\IMG_{id}.jpg", 1.0 / rank, rank, recommended, Array.Empty<string>(), Array.Empty<CatalogTakeBox>());

    private static CatalogTakeAutoGroup Group(string kind, string status, params long[] ids) =>
        new("takes-1", kind, status, 0, 0.9, ids.Select(id => new CatalogTakeShot(id, new[] { id }, $"C:\\shoot\\IMG_{id}.jpg")).ToArray());

    private static CatalogTakeQuality[] Three() => new[]
    {
        Quality(1, 2, false, 1, 11),
        Quality(2, 1, true, 2, 12),
        Quality(3, 3, false, 3),
    };

    [Fact]
    public void GoalRankingMarksTheTriesAndMovesTheKeep()
    {
        var tries = Three();
        Assert.Equal(new long[] { 2, 1, 3 }, TakeReviewPolicy.GoalOrder(tries));
        Assert.True(TakeReviewPolicy.CanRank(" hit ", tries));
        Assert.False(TakeReviewPolicy.CanRank("  ", tries));
        Assert.False(TakeReviewPolicy.CanRank("hit", tries.Take(1).ToArray()));
        var answer = new CatalogTakeGoalRanking("hit", "ranked", new long[] { 1, 3, 2 }, new long[] { 3 }, "");
        var marked = TakeReviewPolicy.MarkGoal(tries, answer);
        Assert.Equal(new[] { "Closest to the goal" }, marked[0].Reasons);
        Assert.Equal(new[] { "Shows the goal" }, marked[2].Reasons);
        var (keep, focus) = TakeReviewPolicy.AdoptGoal(marked, new HashSet<long> { 2 }, answer)!.Value;
        Assert.Equal(new HashSet<long> { 1 }, keep);
        Assert.Equal(0, focus);
        var (hand, _) = TakeReviewPolicy.AdoptGoal(marked, new HashSet<long> { 2, 3 }, answer)!.Value;
        Assert.Equal(new HashSet<long> { 2, 3 }, hand);
        var unclear = new CatalogTakeGoalRanking("hit", "unclear", new long[] { 2, 1, 3 }, Array.Empty<long>(), "");
        Assert.All(TakeReviewPolicy.MarkGoal(marked, unclear), q => Assert.Empty(q.Reasons));
        Assert.Null(TakeReviewPolicy.AdoptGoal(tries, new HashSet<long>(), unclear));
    }

    [Fact]
    public void TitlesUsePhotosOrClipsAndSkippedGroupsAreSpokenAsSkipped()
    {
        Assert.Equal("3 photos", TakeReviewPolicy.GroupTitle(Group("photos", "open", 1, 2, 3)));
        Assert.Equal("2 clips", TakeReviewPolicy.GroupTitle(Group("videos", "open", 1, 2)));
        Assert.Equal("2 photos, Sat 19 Sep, skipped", TakeReviewPolicy.GroupSpoken(Group("photos", "skipped", 1, 2), "Sat 19 Sep"));
    }

    [Fact]
    public void BestIsKeptByDefaultAndTheRestAreTrashedWithTheirTwins()
    {
        var qualities = Three();
        var keep = TakeReviewPolicy.DefaultKeep(qualities);
        Assert.Equal(OnlyBest, keep.ToArray());
        Assert.Equal(TrashedWithTwins, TakeReviewPolicy.TrashIds(qualities, keep));
        Assert.Equal(2, TakeReviewPolicy.DroppedTries(qualities, keep));
    }

    [Fact]
    public void TogglingChangesTheLabelAndTheDisabledReasons()
    {
        var qualities = Three();
        var keep = TakeReviewPolicy.DefaultKeep(qualities);
        Assert.Equal(("Keep the best, move the other 2 to Trash", (string?)null), TakeReviewPolicy.TrashButton(qualities, keep));
        TakeReviewPolicy.Toggle(keep, 1);
        Assert.Equal("Keep 2, move the other 1 to Trash", TakeReviewPolicy.TrashButton(qualities, keep).Label);
        TakeReviewPolicy.Toggle(keep, 3);
        Assert.Contains("Keep all", TakeReviewPolicy.TrashButton(qualities, keep).DisabledReason);
        keep.Clear();
        Assert.Contains("at least one", TakeReviewPolicy.TrashButton(qualities, keep).DisabledReason);
    }

    [Fact]
    public void ConfirmationNamesTheKeptFileAndCountsEveryFile()
    {
        var qualities = Three();
        var keep = TakeReviewPolicy.DefaultKeep(qualities);
        Assert.Equal("Move 2 photos to Trash?", TakeReviewPolicy.ConfirmHeading(Group("photos", "open", 1, 2, 3), qualities, keep));
        Assert.StartsWith("Keeps IMG_2.jpg. The others (3 files, including RAW copies)", TakeReviewPolicy.ConfirmBody(qualities, keep));
    }

    [Fact]
    public void CompareShowsBestAgainstTheFocusedTryOrTheRunnerUp()
    {
        var qualities = Three();
        Assert.Equal((1, 2), TakeReviewPolicy.ComparePair(qualities, 3));
        Assert.Equal((1, 0), TakeReviewPolicy.ComparePair(qualities, 2));
        Assert.Null(TakeReviewPolicy.ComparePair(qualities, 99));
    }

    [Fact]
    public void GroupsOpenInEngineOrderAndSkippedOnesAreNotOpen()
    {
        var groups = new[] { Group("photos", "skipped", 1, 2), Group("photos", "open", 3, 4), Group("videos", "open", 5, 6) };
        Assert.Equal(2, TakeReviewPolicy.OpenCount(groups));
        Assert.Equal(1, TakeReviewPolicy.FirstOpen(groups));
        Assert.Null(TakeReviewPolicy.FirstOpen(groups.Take(1).ToArray()));
    }

    [Fact]
    public void DecisionsCarryTheWholeGroupAndTheFilmstripHeaderCountsMarks()
    {
        var group = Group("photos", "open", 1, 2);
        var decision = TakeReviewPolicy.Decision(group, "skip");
        Assert.Equal(("takes-1", "skip"), (decision.GroupKey, decision.Decision));
        Assert.Equal(WholeGroup, decision.FileIDs.ToArray());
        Assert.Equal("ALL 3 PHOTOS · 1 TO KEEP", TakeReviewPolicy.FilmstripHeader(Group("photos", "open", 1, 2, 3), new HashSet<long> { 2 }, 3));
    }

    [Fact]
    public void ConfirmationMentionsRawCopiesOnlyWhenThereAreExtraFiles()
    {
        var plain = new[] { Quality(1, 1, true, 1), Quality(2, 2, false, 2) };
        var keep = TakeReviewPolicy.DefaultKeep(plain);
        Assert.Equal("Keeps IMG_1.jpg. The others go to the Trash. You can undo right after, or put them back from the Trash.",
            TakeReviewPolicy.ConfirmBody(plain, keep));
    }

    [Fact]
    public void ToastsCountMovedTriesAndRestoredFiles()
    {
        var group = Group("photos", "open", 1, 2, 3);
        var qualities = Three();
        var doomed = qualities.Where(q => q.PrimaryFileID != 2).ToArray();
        Assert.Equal(1, TakeReviewPolicy.MovedShots(doomed, MovedOnlyTwin));
        Assert.Equal("Moved 1 photo to Trash", TakeReviewPolicy.MovedText(group, 1, 0));
        Assert.Equal("Moved 2 photos to Trash · 1 couldn't be moved", TakeReviewPolicy.MovedText(group, 2, 1));
        Assert.Equal("Put back 3 files", TakeReviewPolicy.PutBackText(3, null));
        Assert.Equal("Put back 1 file", TakeReviewPolicy.PutBackText(1, null));
        Assert.Equal("Nothing was put back. Check the Trash.", TakeReviewPolicy.PutBackText(0, null));
        Assert.Equal("Kept all 3. This group won't be suggested again.", TakeReviewPolicy.KeptAllText(group));
    }

    [Fact]
    public void TwoOrMoreKeptTriesAreKeptTogetherAfterTheTrash()
    {
        var group = Group("photos", "open", 1, 2, 3);
        var qualities = Three();
        Assert.Null(TakeReviewPolicy.KeptTogether(group, qualities, TakeReviewPolicy.DefaultKeep(qualities), "keepAll"));
        var decision = TakeReviewPolicy.KeptTogether(group, qualities, new HashSet<long> { 1, 2 }, "keepAll");
        Assert.NotNull(decision);
        Assert.Equal(KeptPairFiles, decision!.FileIDs.OrderBy(id => id).ToArray());
    }

    [Fact]
    public void ScoresReasonsAndSpokenLabelsFollowTheMac()
    {
        var group = Group("photos", "open", 1, 2, 3);
        var best = Three()[1];
        Assert.Equal(100, TakeReviewPolicy.Score(best));
        Assert.True(TakeReviewPolicy.HasRawTwin(best));
        Assert.Equal(TakeReviewPolicy.ReasonTone.Positive, TakeReviewPolicy.Tone("Eyes open"));
        Assert.Equal(TakeReviewPolicy.ReasonTone.Goal, TakeReviewPolicy.Tone("Shows the goal"));
        Assert.Equal(TakeReviewPolicy.ReasonTone.Neutral, TakeReviewPolicy.Tone("Blurry"));
        Assert.Equal("Photo 2 of 3, IMG_2.jpg, recommended, keep, score 100", TakeReviewPolicy.TileSpoken(group, best, 2, 3, best: true, kept: true));
        Assert.Equal("Best, photo 2 of 3, IMG_2.jpg", TakeReviewPolicy.PaneSpoken("Best", group, 2, 3, "IMG_2.jpg"));
        Assert.Equal("Keeping IMG_2.jpg", TakeReviewPolicy.KeepAnnouncement(best, kept: true));
        Assert.Equal("Zoom to face", TakeReviewPolicy.ZoomLabel(group, zoomed: false));
        Assert.Equal("Show whole clip", TakeReviewPolicy.ZoomLabel(Group("videos", "open", 1, 2), zoomed: true));
        Assert.Contains("Return keeps the marked photos", TakeReviewPolicy.HintLine(group));
    }

    [Fact]
    public void ZoomUsesTheLargestFaceOrTheSubjectAndCentresIt()
    {
        var faces = new[] { new CatalogTakeBox(0.1, 0.1, 0.05, 0.05), new CatalogTakeBox(0.4, 0.4, 0.1, 0.1) };
        var quality = new CatalogTakeQuality(1, OneFile, "C:\\IMG_1.jpg", 0.5, 1, true, Array.Empty<string>(), faces);
        var box = TakeReviewPolicy.ZoomBox(quality)!;
        Assert.Equal(0.32, box.X, 6);
        Assert.Equal(0.26, box.Width, 6);
        var subjectOnly = quality with { Faces = Array.Empty<CatalogTakeBox>(), Subject = new CatalogTakeBox(0.9, 0, 0.2, 0.2) };
        var clamped = TakeReviewPolicy.ZoomBox(subjectOnly)!;
        Assert.Equal(1 - 0.24, clamped.X, 6);
        Assert.Null(TakeReviewPolicy.ZoomBox(quality with { Faces = Array.Empty<CatalogTakeBox>() }));

        var (scale, x, y) = TakeReviewPolicy.ZoomTransform(400, 340, 400, 300, new CatalogTakeBox(0.25, 0.25, 0.5, 0.5));
        Assert.Equal(2, scale, 6);
        Assert.Equal(200, scale * 200 + x, 6);
        Assert.Equal(170, 20 + scale * 150 + y, 6);
        Assert.Equal((1d, 0d, 0d), TakeReviewPolicy.ZoomTransform(400, 340, 400, 300, null));
    }

    private static readonly long[] MovedOnlyTwin = { 11 };
    private static readonly long[] KeptPairFiles = { 1, 2, 11, 12 };
    private static readonly long[] OneFile = { 1 };
}

public sealed class SidebarTabShortcutTests
{
    private static readonly string[] AnalyzeOrder = { "deepanalyze", "chat", "takes" };
    [Fact]
    public void BestTakesIsCtrlSixAndTheOtherPagesKeepTheMacNumbers()
    {
        Assert.Equal(6, SidebarTab.Takes.ShortcutNumber);
        Assert.Equal(7, SidebarTab.Cleanup.ShortcutNumber);
        Assert.Equal(9, SidebarTab.Tools.ShortcutNumber);
        Assert.Equal("Analyze", SidebarTab.Takes.Area);
        Assert.Equal(AnalyzeOrder, SidebarTab.InArea("Analyze").Select(t => t.Id).ToArray());
        Assert.Equal("Analyze / Best Takes", SidebarTab.Takes.Crumb);
        Assert.Equal("takes", SidebarTab.ById("takes").Id);
        Assert.Equal("settings", SidebarTab.ById("models").Id);
        Assert.DoesNotContain(SidebarTab.All, t => t.Id == "models");
    }
}
