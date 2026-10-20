using FileID.Views.Library;
using Xunit;

namespace FileID.App.Tests;

public sealed class PreviewDecodeSizeTests
{
    [Theory]
    [InlineData(1000.0, 1.0, 1000)]
    [InlineData(1000.0, 1.5, 1500)]
    [InlineData(1000.0, 2.0, 2000)]
    [InlineData(2600.0, 2.0, 4096)]
    [InlineData(0.0, 1.0, 1)]
    [InlineData(800.0, 0.0, 800)]
    public void DecodeWidth_IsDisplayWidthInPhysicalPixelsCapped(double dip, double scale, int expected)
        => Assert.Equal(expected, FilePreviewSheet.PreviewDecodePixels(dip, scale));
}
