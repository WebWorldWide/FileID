using FileID.IpcSchema;

namespace FileID.ViewModels;

/// <summary>Describe Files ▸ Name style (macOS NameStyle): the saved style, the proposeNames
/// request and the preview rows' rules. The engine proposes; the app shows, edits and applies
/// through renameFiles.</summary>
internal static class NameStylePolicy
{
    public const string DefaultStyle = "short";
    public const int DefaultMaxLength = 40;
    public const bool DefaultIncludePeople = true;
    public const bool DefaultIncludeEvent = true;
    public const bool DefaultIncludeDate = false;
    public const int MinLength = 4;
    public const int MaxLength = 120;
    public const int MaxFiles = 1000;
    public static readonly TimeSpan PreviewDelay = TimeSpan.FromMilliseconds(300);

    public static IReadOnlyList<(string Id, string Title)> Styles { get; } =
        new[] { ("short", "Short"), ("descriptive", "Descriptive"), ("dateFirst", "Date first") };

    public static bool IsStyle(string? style) => Styles.Any(s => s.Id == style);

    public static string SanitizeStyle(string? style) => IsStyle(style) ? style! : DefaultStyle;

    public static int ClampLength(double length) =>
        double.IsNaN(length) ? DefaultMaxLength : (int)Math.Clamp(Math.Round(length), MinLength, MaxLength);

    public static bool DateToggleEnabled(string style) => style != "dateFirst";

    public static CatalogNaming Naming(string style, int maxLength, bool includePeople, bool includeEvent, bool includeDate)
    {
        style = SanitizeStyle(style);
        return new CatalogNaming(style, Math.Clamp(maxLength, MinLength, MaxLength), includePeople, includeEvent,
            includeDate && DateToggleEnabled(style));
    }

    /// <summary>The Library selection when there is one, otherwise <paramref name="fallback"/>;
    /// deduplicated and capped at the engine's 1,000.</summary>
    public static IReadOnlyList<long> FileIds(IReadOnlyList<long> selection, IReadOnlyList<long> fallback) =>
        (selection.Count > 0 ? selection : fallback).Where(id => id > 0).Distinct().Take(MaxFiles).ToArray();

    public static CatalogRequest Request(string requestId, IReadOnlyList<long> fileIds, CatalogNaming naming) =>
        new(requestId, "proposeNames", null, null, null, null, null, fileIds, Naming: naming);

    public static string ReasonLabel(string? reason) => reason switch
    {
        "chapter" => "From your chapter",
        "proposedName" => "From the AI name",
        "description" => "From the description",
        "tags" => "From tags",
        "event" => "From your event",
        "unchanged" => "Already well named",
        "budget" => "Too long for the length limit",
        "reserved" => "Not allowed as a file name",
        "collision" => "No free name in this folder",
        _ => "Nothing to name it from yet",
    };

    /// <summary>Rows for a reply. A file unticked in the last preview stays unticked; rows
    /// without a proposed name can't be ticked.</summary>
    public static IReadOnlyList<NameProposalRow> Rows(IEnumerable<CatalogHit> hits, IReadOnlyList<NameProposalRow> previous)
    {
        var unticked = previous.Where(r => r.CanCheck && !r.Checked).Select(r => r.FileId).ToHashSet();
        return hits.Where(h => h.Kind == "proposedName")
            .Select(h => new NameProposalRow(h.FileID, h.Path, h.Text, h.EvidenceID ?? "noEvidence")
            {
                Edited = h.Text,
                Checked = h.Text.Length > 0 && !unticked.Contains(h.FileID),
            })
            .ToArray();
    }

    public static string? ValidName(string? text)
    {
        var name = (text ?? string.Empty).Trim();
        if (name.Length == 0 || name is "." or ".." || name.IndexOfAny(['/', '\\', '\0']) >= 0) return null;
        return name;
    }

    public static IReadOnlyList<RenameEntry> Renames(IEnumerable<NameProposalRow> rows) =>
        rows.Where(r => r.Checked && r.CanCheck)
            .Select(r => (Row: r, Name: ValidName(r.Edited)))
            .Where(x => x.Name is not null && x.Name != x.Row.CurrentName)
            .Select(x => new RenameEntry(x.Row.FileId, x.Name!))
            .ToArray();

    /// <summary>The renames that put back the original names of the files the engine confirmed.</summary>
    public static IReadOnlyList<RenameEntry> UndoEntries(
        IEnumerable<NameProposalRow> rows, IEnumerable<RenameEntry> sent, IEnumerable<long> confirmed)
    {
        var landed = confirmed.ToHashSet();
        var byId = rows.GroupBy(r => r.FileId).ToDictionary(g => g.Key, g => g.First());
        return sent.Where(e => landed.Contains(e.FileId) && byId.ContainsKey(e.FileId))
            .Select(e => new RenameEntry(e.FileId, byId[e.FileId].CurrentName))
            .ToArray();
    }

    public static string AccessibleName(NameProposalRow row)
    {
        var reason = ReasonLabel(row.Reason);
        var because = char.ToLowerInvariant(reason[0]) + reason[1..];
        return row.CanCheck
            ? $"{row.CurrentName}, rename to {row.Edited.Trim()}, {because}"
            : $"{row.CurrentName}, keeps its name, {because}";
    }
}

internal sealed record NameProposalRow(long FileId, string Path, string Proposed, string Reason)
{
    public string Edited { get; set; } = string.Empty;
    public bool Checked { get; set; }
    public bool CanCheck => Proposed.Length > 0;
    public string CurrentName => System.IO.Path.GetFileName(Path.Replace('\\', '/'));
}
