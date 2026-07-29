using System;
using System.Collections.Generic;
using System.Linq;
using FileID.IpcSchema;

namespace FileID.Services;

internal static class BulkActionResultTruth
{
    internal static IReadOnlyList<long> ConfirmedSuccessfulFileIds(
        BulkActionResult result,
        IEnumerable<long> requestedFileIds)
    {
        var requested = requestedFileIds.Distinct().ToArray();
        if (result.Succeeded == 0) return Array.Empty<long>();

        var requestedSet = requested.ToHashSet();
        var confirmed = new HashSet<long>();
        foreach (var item in result.Messages)
        {
            if (!item.Ok) continue;
            if (item.FileId is not long fileId
                || !requestedSet.Contains(fileId)
                || !confirmed.Add(fileId))
            {
                return Array.Empty<long>();
            }
        }

        if ((uint)confirmed.Count != result.Succeeded)
        {
            return Array.Empty<long>();
        }

        return requested.Where(confirmed.Contains).ToArray();
    }

    internal static bool ConfirmsExactSuccess(
        BulkActionResult result,
        IReadOnlyCollection<long> expectedFileIds)
    {
        var expected = expectedFileIds.Distinct().ToArray();
        if (expected.Length != expectedFileIds.Count
            || result.Failed != 0
            || result.Succeeded != (uint)expected.Length)
        {
            return false;
        }

        return ConfirmedSuccessfulFileIds(result, expected).Count == expected.Length;
    }
}
