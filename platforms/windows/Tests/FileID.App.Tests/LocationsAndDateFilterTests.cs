using System.Globalization;
using System.Text.Json;
using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class LocationsAndDateFilterTests
{
    private static readonly DateTimeOffset Now = new(2026, 11, 5, 12, 0, 0, TimeSpan.Zero);

    private static LibraryRoot Root(long files, bool online = true, double? scanned = null, string kind = "internal") =>
        new(1, "E:\\Camera", "Camera", kind, true, online, files, 2_500_000_000, scanned);

    [Fact]
    public void RowSummaryCountsFilesSizeAndLastScan()
    {
        WithInvariantCulture(() =>
        {
            Assert.Equal("1 file · 2.5 GB · not scanned yet", LocationPresentation.Summary(Root(1), Now));
            Assert.Equal("12,400 files · 2.5 GB · scanned 3 days ago",
                LocationPresentation.Summary(Root(12_400, scanned: Now.AddDays(-3).ToUnixTimeSeconds()), Now));
            Assert.Equal("Camera, offline, 1 file · 2.5 GB · not scanned yet", LocationPresentation.Spoken(Root(1, online: false), Now));
        });
    }

    [Theory]
    [InlineData(30, "just now")]
    [InlineData(60, "1 minute ago")]
    [InlineData(3_599, "59 minutes ago")]
    [InlineData(7_200, "2 hours ago")]
    [InlineData(86_400, "1 day ago")]
    [InlineData(86_400 * 8, "1 week ago")]
    [InlineData(86_400 * 45, "1 month ago")]
    [InlineData(86_400 * 800, "2 years ago")]
    public void RelativeTimesReadLikeTheMac(int seconds, string expected)
    {
        Assert.Equal(expected, LocationPresentation.Relative(TimeSpan.FromSeconds(seconds)));
    }

    [Fact]
    public void OfflineLocationsCannotRescanAndKindsHaveIcons()
    {
        Assert.Equal("Reconnect the drive to rescan", LocationPresentation.RescanTooltip(Root(5, online: false)));
        Assert.Equal("Scan this location again", LocationPresentation.RescanTooltip(Root(5)));
        Assert.Equal("\uE968", LocationPresentation.Glyph("network"));
        Assert.Equal("\uEDA2", LocationPresentation.Glyph("external"));
        Assert.Equal("\uE8B7", LocationPresentation.Glyph("internal"));
        Assert.Equal("not scanned yet", LocationPresentation.Scanned(double.NaN, Now));
    }

    [Fact]
    public void RootsListRoundTripsOverTheEngineWire()
    {
        var sent = IpcEvent.Now(new RootsListEvent(new RootsList("roots-1", new[] { Root(42, online: false, scanned: 1_700_000_000.5, kind: "external") })));
        var line = IpcCoder.Encode(sent);
        Assert.Contains("\"rootsList\"", line, StringComparison.Ordinal);
        Assert.Contains("\"requestID\":\"roots-1\"", line, StringComparison.Ordinal);
        var roots = Assert.IsType<RootsListEvent>(IpcCoder.Decode<IpcEvent>(line).Payload).Roots;
        var root = Assert.Single(roots.Roots);
        Assert.False(root.Online);
        Assert.Equal("external", root.Kind);
        Assert.Equal(42, root.FileCount);
        Assert.Equal(1_700_000_000.5, root.LastScanAt);
    }

    [Fact]
    public void DatesCoverWholeLocalDaysInclusive()
    {
        var zone = TimeZoneInfo.CreateCustomTimeZone("plus5", TimeSpan.FromHours(5), "plus5", "plus5");
        var day = new DateTimeOffset(2024, 3, 9, 17, 45, 0, TimeSpan.FromHours(-8));
        var (from, to) = SearchDateRange.Unix(day, day, zone);
        Assert.Equal(new DateTimeOffset(2024, 3, 9, 0, 0, 0, TimeSpan.FromHours(5)).ToUnixTimeSeconds(), from);
        Assert.Equal(86_399.999, to!.Value - from!.Value, 3);

        var (onlyFrom, none) = SearchDateRange.Unix(day, null, zone);
        Assert.Equal(from, onlyFrom);
        Assert.Null(none);
        var (noStart, onlyTo) = SearchDateRange.Unix(null, day, zone);
        Assert.Null(noStart);
        Assert.Equal(to, onlyTo);
        Assert.Equal((null, null), SearchDateRange.Unix(null, null, zone));
    }

    [Fact]
    public void DatesOutOfOrderOrBefore1970AreRefusedOrClamped()
    {
        var utc = TimeZoneInfo.Utc;
        Assert.Throws<InvalidOperationException>(() => SearchDateRange.Unix(new DateTimeOffset(2024, 3, 10, 0, 0, 0, TimeSpan.Zero), new DateTimeOffset(2024, 3, 9, 0, 0, 0, TimeSpan.Zero), utc));
        Assert.Throws<InvalidOperationException>(() => SearchDateRange.Unix(null, new DateTimeOffset(1960, 1, 1, 0, 0, 0, TimeSpan.Zero), utc));
        Assert.Equal(0, SearchDateRange.Unix(new DateTimeOffset(1960, 1, 1, 0, 0, 0, TimeSpan.Zero), null, utc).From);
    }

    [Fact]
    public void SearchSendsCaptureWindowOnlyWhenChosen()
    {
        var plain = JsonSerializer.Serialize(CatalogEmbeddingSpace.SearchRequest("beach", "keyword"), IpcCoder.Options);
        Assert.DoesNotContain("captured", plain, StringComparison.Ordinal);
        var dated = CatalogEmbeddingSpace.SearchRequest("beach", "keyword", null, 1_700_000_000, 1_700_086_399.999);
        Assert.Equal(1_700_000_000, dated.CapturedFrom);
        Assert.Equal(1_700_086_399.999, dated.CapturedTo);
        var json = JsonSerializer.Serialize(dated, IpcCoder.Options);
        Assert.Contains("\"capturedFrom\":1700000000", json, StringComparison.Ordinal);
        Assert.Contains("\"capturedTo\":1700086399.999", json, StringComparison.Ordinal);
    }

    [Fact]
    public void DateNoteNamesTheChosenDays()
    {
        WithInvariantCulture(() =>
        {
            var march9 = new DateTimeOffset(2024, 3, 9, 0, 0, 0, TimeSpan.Zero);
            var march12 = march9.AddDays(3);
            Assert.Equal("Taken 9 Mar 2024 – 12 Mar 2024", SearchDateRange.Describe(march9, march12));
            Assert.Equal("Taken on or after 9 Mar 2024", SearchDateRange.Describe(march9, null));
            Assert.Equal("Taken on or before 12 Mar 2024", SearchDateRange.Describe(null, march12));
            Assert.Equal("", SearchDateRange.Describe(null, null));
        });
    }

    private static void WithInvariantCulture(Action body)
    {
        var culture = CultureInfo.CurrentCulture;
        CultureInfo.CurrentCulture = CultureInfo.InvariantCulture;
        try { body(); }
        finally { CultureInfo.CurrentCulture = culture; }
    }
}
