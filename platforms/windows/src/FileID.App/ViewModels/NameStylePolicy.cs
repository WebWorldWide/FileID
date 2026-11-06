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
        UnscannedCompanion => "Its partner file isn't scanned yet",
        _ => "Nothing to name it from yet",
    };

    public const string UnscannedCompanion = "unscannedCompanion";
    private const string GroupMarker = "|group:";

    /// <summary>Splits a proposeNames evidenceID into its reason and the companion group's
    /// leader file ID ("proposedName|group:11"), matching both engines.</summary>
    public static (string Reason, long? Group) ParseReason(string? evidenceId)
    {
        var text = evidenceId ?? "noEvidence";
        var marker = text.IndexOf(GroupMarker, StringComparison.Ordinal);
        if (marker < 0) return (text, null);
        return (text[..marker], long.TryParse(text[(marker + GroupMarker.Length)..], System.Globalization.NumberStyles.None,
            System.Globalization.CultureInfo.InvariantCulture, out var group) ? group : null);
    }

    /// <summary>Rows for a reply, companions kept together leader first. A file (or group)
    /// unticked in the last preview stays unticked; rows without a proposed name can't be ticked.</summary>
    public static IReadOnlyList<NameProposalRow> Rows(IEnumerable<CatalogHit> hits, IReadOnlyList<NameProposalRow> previous)
    {
        var unticked = previous.Where(r => r.CanCheck && !r.Checked).Select(r => r.GroupKey).ToHashSet();
        var rows = hits.Where(h => h.Kind == "proposedName")
            .Select(h =>
            {
                var (reason, group) = ParseReason(h.EvidenceID);
                return new NameProposalRow(h.FileID, h.Path, h.Text, reason)
                {
                    Group = group,
                    Edited = h.Text,
                    Checked = h.Text.Length > 0 && !unticked.Contains(group ?? h.FileID),
                };
            })
            .ToArray();
        var leaderStems = rows.Where(r => r.IsLeader).GroupBy(r => r.FileId).ToDictionary(g => g.Key, g => Stem(g.First().Proposed));
        foreach (var row in rows)
        {
            if (row.Group is { } group && leaderStems.TryGetValue(group, out var stem))
                row.Suffix = row.Proposed.StartsWith(stem, StringComparison.Ordinal) ? row.Proposed[stem.Length..] : string.Empty;
        }
        return rows;
    }

    /// <summary>The name before its last dot, like the engines' split (a leading dot is part of the stem).</summary>
    public static string Stem(string name)
    {
        var dot = name.LastIndexOf('.');
        return dot > 0 && dot + 1 < name.Length ? name[..dot] : name;
    }

    /// <summary>Ticking any member of a companion group ticks the whole group. Returns the rows changed.</summary>
    public static IReadOnlyList<NameProposalRow> SetChecked(IReadOnlyList<NameProposalRow> rows, long fileId, bool isChecked)
    {
        var key = rows.FirstOrDefault(r => r.FileId == fileId)?.GroupKey;
        if (key is null) return Array.Empty<NameProposalRow>();
        var changed = rows.Where(r => r.GroupKey == key && r.CanCheck).ToArray();
        foreach (var row in changed) row.Checked = isChecked;
        return changed;
    }

    /// <summary>Editing the leader's name gives every companion the same stem. Returns the followers changed.</summary>
    public static IReadOnlyList<NameProposalRow> Edit(IReadOnlyList<NameProposalRow> rows, long fileId, string text)
    {
        var row = rows.FirstOrDefault(r => r.FileId == fileId);
        if (row is null || row.IsFollower) return Array.Empty<NameProposalRow>();
        row.Edited = text;
        if (!row.IsLeader) return Array.Empty<NameProposalRow>();
        var stem = Stem(text.Trim());
        var followers = Followers(rows, row).Where(r => r.CanCheck).ToArray();
        foreach (var follower in followers) follower.Edited = stem + follower.Suffix;
        return followers;
    }

    public static IEnumerable<NameProposalRow> Followers(IEnumerable<NameProposalRow> rows, NameProposalRow leader) =>
        rows.Where(r => r.IsFollower && r.Group == leader.FileId);

    /// <summary>The leader's chip naming its companions' extensions: "+ ARW, XMP".</summary>
    public static string? CompanionChip(NameProposalRow row, IReadOnlyList<NameProposalRow> rows)
    {
        if (!row.IsLeader) return null;
        var extensions = Followers(rows, row)
            .Select(r => { var name = r.CurrentName; var stem = Stem(name); return stem.Length < name.Length ? name[(stem.Length + 1)..].ToUpperInvariant() : string.Empty; })
            .ToArray();
        return extensions.Length == 0 ? null : "+ " + string.Join(", ", extensions);
    }

    public static string? ValidName(string? text)
    {
        var name = (text ?? string.Empty).Trim();
        if (name.Length == 0 || name is "." or ".." || name.IndexOfAny(['/', '\\', '\0']) >= 0) return null;
        return name;
    }

    /// <summary>Ticked renames in row order, as one batch. A companion group goes whole or not
    /// at all: one invalid member name holds every member back.</summary>
    public static IReadOnlyList<RenameEntry> Renames(IEnumerable<NameProposalRow> rows)
    {
        var list = rows as IReadOnlyList<NameProposalRow> ?? rows.ToArray();
        var blocked = list.Where(r => r.Group is not null && r.Checked && r.CanCheck && ValidName(r.Edited) is null)
            .Select(r => r.GroupKey).ToHashSet();
        return list.Where(r => r.Checked && r.CanCheck && !blocked.Contains(r.GroupKey))
            .Select(r => (Row: r, Name: ValidName(r.Edited)))
            .Where(x => x.Name is not null && x.Name != x.Row.CurrentName)
            .Select(x => new RenameEntry(x.Row.FileId, x.Name!))
            .ToArray();
    }

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

    public static string AccessibleName(NameProposalRow row, IReadOnlyList<NameProposalRow>? rows = null)
    {
        var reason = ReasonLabel(row.Reason);
        var because = char.ToLowerInvariant(reason[0]) + reason[1..];
        var others = row.Group is { } group && rows is not null
            ? rows.Where(r => r.Group == group && r.FileId != row.FileId).Select(r => r.CurrentName).ToArray()
            : Array.Empty<string>();
        var together = others.Length == 0 ? string.Empty : ", together with " + string.Join(", ", others);
        return row.CanCheck
            ? $"{row.CurrentName}, rename to {row.Edited.Trim()}, {because}{together}"
            : $"{row.CurrentName}, keeps its name, {because}{together}";
    }
}

internal sealed record NameProposalRow(long FileId, string Path, string Proposed, string Reason)
{
    public string Edited { get; set; } = string.Empty;
    public bool Checked { get; set; }
    public long? Group { get; init; }
    public string Suffix { get; set; } = string.Empty;
    public bool CanCheck => Proposed.Length > 0;
    public long GroupKey => Group ?? FileId;
    public bool IsLeader => Group == FileId;
    public bool IsFollower => Group is { } group && group != FileId;
    public string CurrentName => System.IO.Path.GetFileName(Path.Replace('\\', '/'));
}
