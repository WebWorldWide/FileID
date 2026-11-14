using FileID.IpcSchema;

namespace FileID.Services;

/// <summary>Plain sentences for files that could not be moved to the
/// Recycle Bin, from the reason FileID's helper gives for each one.</summary>
internal static class TrashFailureText
{
    public const string Changed = "changed";
    public const string Protected = "protected";
    public const string Missing = "missing";
    public const string Failed = "failed";

    /// <summary>The helper's reason for one file; an older helper that sends
    /// none is read from its fixed wording, as the Mac app does.</summary>
    public static string Kind(BulkActionItem item)
    {
        if (item.Reason is Changed or Protected or Missing or Failed) return item.Reason;
        var text = item.Message ?? string.Empty;
        if (text.StartsWith("This file belongs to a managed media library", StringComparison.Ordinal)
            || text.StartsWith("This location is read-only", StringComparison.Ordinal))
        {
            return Protected;
        }
        if (text.StartsWith("File changed since", StringComparison.Ordinal)
            || text.EndsWith("Nothing was moved.", StringComparison.Ordinal))
        {
            return Changed;
        }
        if (text.StartsWith("not found", StringComparison.OrdinalIgnoreCase)
            || text.StartsWith("File row not found", StringComparison.Ordinal))
        {
            return Missing;
        }
        return string.Empty;
    }

    public static string Sentence(string kind, int count)
    {
        var one = count == 1;
        var files = one ? "1 file" : $"{count:N0} files";
        return kind switch
        {
            Changed => $"{files} changed since the scan, so FileID left {(one ? "it" : "them")} alone. Scan again, then retry.",
            Protected => $"{files} {(one ? "is" : "are")} in a protected location that FileID doesn't change.",
            Missing => $"{files} {(one ? "is" : "are")} no longer there.",
            _ => $"{files} couldn't be moved. {(one ? "It" : "They")} may be open in another app, or you may not have permission."
        };
    }

    /// <summary>One sentence per kind of failure, most useful first. A file
    /// with no reason and unfamiliar wording keeps the helper's own message.
    /// Empty when nothing failed.</summary>
    public static string Summary(BulkActionResult result)
    {
        var changed = 0;
        var isProtected = 0;
        var missing = 0;
        var failed = 0;
        string? other = null;
        var others = 0;
        foreach (var item in result.Messages ?? Array.Empty<BulkActionItem>())
        {
            if (item.Ok) continue;
            switch (Kind(item))
            {
                case Changed: changed++; break;
                case Protected: isProtected++; break;
                case Missing: missing++; break;
                case Failed: failed++; break;
                default:
                    if (string.IsNullOrWhiteSpace(item.Message))
                    {
                        failed++;
                    }
                    else
                    {
                        other ??= item.Message.Trim();
                        others++;
                    }
                    break;
            }
        }

        var parts = new List<string>(5);
        if (changed > 0) parts.Add(Sentence(Changed, changed));
        if (isProtected > 0) parts.Add(Sentence(Protected, isProtected));
        if (missing > 0) parts.Add(Sentence(Missing, missing));
        if (failed > 0) parts.Add(Sentence(Failed, failed));
        if (other is not null)
        {
            var text = other.EndsWith('.') || other.EndsWith('!') || other.EndsWith('?') ? other : other + ".";
            parts.Add(others == 1 ? text : $"{text} ({others:N0} files)");
        }
        if (parts.Count == 0 && result.Failed > 0) parts.Add(Sentence(Failed, (int)Math.Min(result.Failed, int.MaxValue)));
        return string.Join(" ", parts);
    }
}
