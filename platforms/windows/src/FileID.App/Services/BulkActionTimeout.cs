using System;

namespace FileID.Services;

internal static class BulkActionTimeout
{
    internal static TimeSpan Maximum { get; } = TimeSpan.FromHours(2);

    /// <summary>How long a wait that gave up keeps its kind of action
    /// reserved for a late reply before the next one may start.</summary>
    internal static TimeSpan LateReplyHold { get; } = TimeSpan.FromMinutes(10);

    private static readonly TimeSpan TrashFloor = TimeSpan.FromMinutes(5);

    /// <summary>Moving files to the Recycle Bin can stall on a slow or
    /// sleeping drive, so it gets five minutes on top of the per-file
    /// allowance, still under <see cref="Maximum"/>.</summary>
    internal static TimeSpan ForTrash(int fileCount)
    {
        var wait = ForFileCount(fileCount) + TrashFloor;
        return wait > Maximum ? Maximum : wait;
    }

    internal static TimeSpan ForFileCount(int fileCount)
    {
        ArgumentOutOfRangeException.ThrowIfNegative(fileCount);
        var seconds = Math.Clamp(30 + fileCount / 25.0, 30, Maximum.TotalSeconds);
        return TimeSpan.FromSeconds(seconds);
    }
}
