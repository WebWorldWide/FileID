using FileID.Services;
using Xunit;

namespace FileID.App.Tests;

public sealed class BulkActionTimeoutTests
{
    [Theory]
    [InlineData(0, 30)]
    [InlineData(25, 31)]
    [InlineData(100_000, 4_030)]
    [InlineData(1_000_000, 7_200)]
    public void TimeoutScalesWithFileCountAndIsBounded(int fileCount, double expectedSeconds)
        => Assert.Equal(expectedSeconds, BulkActionTimeout.ForFileCount(fileCount).TotalSeconds);

    [Fact]
    public void MaximumMatchesTheDocumentedTwoHourSafetyBound()
        => Assert.Equal(TimeSpan.FromHours(2), BulkActionTimeout.Maximum);
}
