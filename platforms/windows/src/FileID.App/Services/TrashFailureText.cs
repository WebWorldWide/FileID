using FileID.IpcSchema;

namespace FileID.Services;

internal static class TrashFailureText
{
    public const string Changed = "changed";
    public const string Protected = "protected";
    public const string Missing = "missing";
    public const string Failed = "failed";
    public const string ReadOnly = "readOnly";

    public static string Kind(BulkActionItem item)
    {
        if (IsReadOnlyDrive(item.Message)) return ReadOnly;
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

    private static bool IsReadOnlyDrive(string? message) =>
        message is not null && message.Contains("is a read-only drive", StringComparison.Ordinal);

    public static string Sentence(string kind, int count)
    {
        var one = count == 1;
        var files = one ? "1 file" : $"{count:N0} files";
        return kind switch
        {
            Changed => $"{files} changed since the scan, so FileID left {(one ? "it" : "them")} alone. Scan again, then retry.",
            Protected => $"{files} {(one ? "is" : "are")} in a protected location that FileID doesn't change.",
            Missing => $"{files} {(one ? "is" : "are")} no longer there.",
            ReadOnly => $"{files} {(one ? "is" : "are")} on a read-only drive, so FileID can't move {(one ? "it" : "them")}.",
            _ => $"{files} couldn't be moved. {(one ? "It" : "They")} may be open in another app, or you may not have permission."
        };
    }

    public static string Summary(BulkActionResult result)
    {
        var changed = 0;
        var isProtected = 0;
        var missing = 0;
        var failed = 0;
        var readOnly = 0;
        string? readOnlySentence = null;
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
                case ReadOnly:
                    readOnly++;
                    readOnlySentence ??= item.Message!.Trim();
                    break;
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
        if (readOnly > 0) parts.Add(readOnly == 1 || readOnlySentence!.EndsWith("nothing was changed.", StringComparison.Ordinal) ? readOnlySentence! : $"{readOnlySentence} ({readOnly:N0} files)");
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
