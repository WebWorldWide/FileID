using FileID.IpcSchema;
using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class OfflineRootsTests
{
    private static readonly string?[] OfflineNotifications = { nameof(FileTile.IsOffline), nameof(FileTile.AccessibleName) };

    private static LibraryRoot Root(string path, bool online) => new(1, path, path, "external", true, online, 0, 0);

    [Fact]
    public void FilesUnderAnOfflineRootReadOffline()
    {
        var roots = OfflineRoots.From(new[] { Root(@"E:\Camera", online: false), Root(@"C:\Users\you\Pictures", online: true) });
        Assert.True(roots.IsOffline(@"E:\Camera\2024\IMG_0001.JPG"));
        Assert.True(roots.IsOffline(@"e:\camera\clip.mov"));
        Assert.True(roots.IsOffline(@"E:\Camera"));
        Assert.False(roots.IsOffline(@"E:\Camera Roll\a.jpg"));
        Assert.False(roots.IsOffline(@"C:\Users\you\Pictures\a.jpg"));
        Assert.False(roots.IsOffline(@"D:\elsewhere\a.jpg"));
        Assert.False(roots.IsOffline(null));
    }

    [Fact]
    public void TheMostSpecificRootDecides()
    {
        var roots = OfflineRoots.From(new[] { Root(@"F:\", online: false), Root(@"F:\Mirror\Photos\", online: true) });
        Assert.True(roots.IsOffline(@"F:\Other\a.jpg"));
        Assert.False(roots.IsOffline(@"F:\Mirror\Photos\a.jpg"));
        var nested = OfflineRoots.From(new[] { Root("/Volumes/Drive", online: true), Root("/Volumes/Drive/Archive", online: false) });
        Assert.True(nested.IsOffline("/Volumes/Drive/Archive/x.heic"));
        Assert.False(nested.IsOffline("/Volumes/Drive/x.heic"));
    }

    [Fact]
    public void AllOnlineMeansNothingIsOfflineAndEqualityIsByContent()
    {
        var online = OfflineRoots.From(new[] { Root(@"E:\Camera", online: true) });
        Assert.True(online.IsEmpty);
        Assert.Same(OfflineRoots.None, online);
        Assert.False(online.IsOffline(@"E:\Camera\a.jpg"));
        var a = OfflineRoots.From(new[] { Root(@"E:\Camera", false), Root(@"C:\Pics", true) });
        var b = OfflineRoots.From(new[] { Root(@"C:\Pics\", true), Root(@"e:/camera", false) });
        Assert.Equal(a, b);
        Assert.NotEqual(a, OfflineRoots.None);
    }

    [Fact]
    public void TileAccessibleNameAndFlagFollowOfflineState()
    {
        var tile = new FileTile { Id = 1, Path = @"E:\Camera\a.jpg", FileName = "a.jpg", Kind = "image", SizeBytes = 1 };
        var raised = new List<string?>();
        tile.PropertyChanged += (_, e) => raised.Add(e.PropertyName);
        tile.IsOffline = true;
        tile.IsOffline = true;
        Assert.Equal(OfflineNotifications, raised);
        Assert.Contains("offline", tile.AccessibleName, StringComparison.Ordinal);
    }
}
