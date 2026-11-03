using FileID.IpcSchema;
using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class TakeReviewPolicyTests
{
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
        Assert.Equal(new long[] { 2 }, keep.ToArray());
        Assert.Equal(new long[] { 1, 3, 11 }, TakeReviewPolicy.TrashIds(qualities, keep));
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
        Assert.Equal(new long[] { 1, 2 }, decision.FileIDs.ToArray());
        Assert.Equal("ALL 3 PHOTOS · 1 TO KEEP", TakeReviewPolicy.FilmstripHeader(Group("photos", "open", 1, 2, 3), new HashSet<long> { 2 }, 3));
    }
}

public sealed class SidebarTabShortcutTests
{
    [Fact]
    public void BestTakesIsCtrlSixAndTheOtherPagesKeepTheMacNumbers()
    {
        Assert.Equal(6, SidebarTab.Takes.ShortcutNumber);
        Assert.Equal(7, SidebarTab.Cleanup.ShortcutNumber);
        Assert.Equal(9, SidebarTab.Tools.ShortcutNumber);
        Assert.Equal(0, SidebarTab.Models.ShortcutNumber);
        Assert.Equal("Analyze", SidebarTab.Takes.Area);
        Assert.Equal(new[] { "deepanalyze", "chat", "takes", "models" }, SidebarTab.InArea("Analyze").Select(t => t.Id).ToArray());
        Assert.Equal("Analyze / Best Takes", SidebarTab.Takes.Crumb);
        Assert.Same(SidebarTab.All.First(t => t.Id == "takes").Id, SidebarTab.ById("takes").Id);
    }
}
