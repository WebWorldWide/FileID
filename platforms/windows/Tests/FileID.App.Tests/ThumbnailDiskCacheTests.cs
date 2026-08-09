using System.Collections.Generic;
using FileID.Services;
using Xunit;

namespace FileID.App.Tests;

public class ThumbnailDiskCacheTests
{
    private static readonly string[] s_oldestThenMiddle = { "oldest", "middle" };
    private static readonly string[] s_mediumThenSmall = { "medium-oldest", "small-middle" };
    private static readonly string[] s_aOldestOnly = { "a-oldest" };

    private static KeyValuePair<string, ThumbnailDiskCache.CacheEntry> Entry(
        string path, long size, long ticks)
        => new(path, new ThumbnailDiskCache.CacheEntry { SizeBytes = size, LastAccessTicks = ticks });

    [Fact]
    public void SelectEvictions_ReturnsEmpty_WhenUnderHeadroom()
    {
        var entries = new[]
        {
            Entry("a", 100, 1),
            Entry("b", 200, 2),
        };

        var picks = ThumbnailDiskCache.SelectEvictions(entries, currentBytes: 300, headroomBytes: 1000);

        Assert.Empty(picks);
    }

    [Fact]
    public void SelectEvictions_EvictsOldestFirst_UntilUnderHeadroom()
    {
        var entries = new[]
        {
            Entry("newest", 100, 3),
            Entry("middle", 100, 2),
            Entry("oldest", 100, 1),
        };

        var picks = ThumbnailDiskCache.SelectEvictions(entries, currentBytes: 300, headroomBytes: 150);

        Assert.Equal(s_oldestThenMiddle, picks);
    }

    [Fact]
    public void SelectEvictions_HandlesUnequalSizes()
    {
        var entries = new[]
        {
            Entry("big-newest", 500, 5),
            Entry("small-middle", 50, 3),
            Entry("medium-oldest", 200, 1),
        };

        var picks = ThumbnailDiskCache.SelectEvictions(entries, currentBytes: 750, headroomBytes: 500);

        Assert.Equal(s_mediumThenSmall, picks);
    }

    [Fact]
    public void SelectEvictions_StopsAtHeadroom_DoesNotOverEvict()
    {
        var entries = new[]
        {
            Entry("a-oldest", 200, 1),
            Entry("b-middle", 200, 2),
            Entry("c-newest", 200, 3),
        };

        var picks = ThumbnailDiskCache.SelectEvictions(entries, currentBytes: 600, headroomBytes: 400);

        Assert.Equal(s_aOldestOnly, picks);
    }
}
