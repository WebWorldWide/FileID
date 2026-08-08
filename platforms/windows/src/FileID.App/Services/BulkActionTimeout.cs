using System;

namespace FileID.Services;

internal static class BulkActionTimeout
{
    internal static TimeSpan Maximum { get; } = TimeSpan.FromHours(2);

    internal static TimeSpan ForFileCount(int fileCount)
    {
        ArgumentOutOfRangeException.ThrowIfNegative(fileCount);
        var seconds = Math.Clamp(30 + fileCount / 25.0, 30, Maximum.TotalSeconds);
        return TimeSpan.FromSeconds(seconds);
    }
}
