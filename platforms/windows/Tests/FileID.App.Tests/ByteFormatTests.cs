using System.Globalization;
using FileID.Services;
using FileID.Views.Cleanup;
using Xunit;

namespace FileID.App.Tests;

public sealed class ByteFormatTests
{
    [Theory]
    [InlineData(0L, "Zero KB")]
    [InlineData(1L, "1 byte")]
    [InlineData(999L, "999 bytes")]
    [InlineData(1_000L, "1 KB")]
    [InlineData(12_345L, "12 KB")]
    [InlineData(999_600L, "1 MB")]
    [InlineData(3_460_000L, "3.5 MB")]
    [InlineData(1_234_567_890L, "1.23 GB")]
    [InlineData(2_500_000_000_000L, "2.5 TB")]
    public void File_UsesFinderDecimalUnits(long bytes, string expected)
    {
        var previous = CultureInfo.CurrentCulture;
        CultureInfo.CurrentCulture = CultureInfo.InvariantCulture;
        try
        {
            Assert.Equal(expected, ByteFormat.File(bytes));
        }
        finally
        {
            CultureInfo.CurrentCulture = previous;
        }
    }

    [Fact]
    public void CleanupHeadline_MatchesMacOsCopy()
    {
        var previous = CultureInfo.CurrentCulture;
        CultureInfo.CurrentCulture = CultureInfo.InvariantCulture;
        try
        {
            Assert.Equal("No extra copies found", CleanupView.FormatHeadline(0, 0, 0));
            Assert.Equal("No extra copies found · 3 skipped", CleanupView.FormatHeadline(0, 0, 3));
            Assert.Equal(
                "1,204 files with extra copies · 1.23 GB can be freed · 2 skipped",
                CleanupView.FormatHeadline(1204, 1_234_567_890, 2));
            Assert.Equal("Move to Recycle Bin", CleanupView.FormatDeleteButton(0, 0));
            Assert.Equal("Move 3 to Recycle Bin · 43 KB", CleanupView.FormatDeleteButton(3, 43_000));
            Assert.Equal("Moved 3 files to the Recycle Bin · freed 43 KB", CleanupView.FormatTrashResult(3, 43_000));
        }
        finally
        {
            CultureInfo.CurrentCulture = previous;
        }
    }
}
