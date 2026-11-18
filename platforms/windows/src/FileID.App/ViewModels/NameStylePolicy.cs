using FileID.IpcSchema;

namespace FileID.ViewModels;

internal static class NameStylePolicy
{
    public const string DefaultStyle = "short";
    public const int DefaultMaxLength = 40;
    public const bool DefaultIncludePeople = true;
    public const bool DefaultIncludeEvent = true;
    public const bool DefaultIncludeDate = false;
    public const bool DefaultLearn = true;
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

    public static CatalogNaming Naming(string style, int maxLength, bool includePeople, bool includeEvent, bool includeDate, bool learn = false)
    {
        style = SanitizeStyle(style);
        return new CatalogNaming(style, Math.Clamp(maxLength, MinLength, MaxLength), includePeople, includeEvent,
            includeDate && DateToggleEnabled(style), learn);
    }

    /// <summary>The Library selection when there is one, otherwise fallback; deduplicated and capped at the engine's 1,000.</summary>
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
    private const string LearnedMarker = "|learned:";

    public static (string Reason, IReadOnlyList<string> Learned) SplitLearned(string reason)
    {
        var marker = reason.IndexOf(LearnedMarker, StringComparison.Ordinal);
        if (marker < 0) return (reason, Array.Empty<string>());
        return (reason[..marker], reason[(marker + LearnedMarker.Length)..].Split(',', StringSplitOptions.RemoveEmptyEntries));
    }

    public static string ReasonChip(NameProposalRow row) =>
        row.Learned.Count == 0 ? ReasonLabel(row.Reason) : ReasonLabel(row.Reason) + " · your style";

    public static string? LearnedHelp(IReadOnlyList<string> keys)
    {
        var parts = keys.Select(key => key switch
        {
            "dropWords" => "leaves out words you remove",
            "people" => "leaves out people's names",
            "words" => "keeps it as short as you do",
            "leadingDate" or "trailingDate" => "puts the date where you do",
            "dateFormat" => "writes the date your way",
            "separator" => "uses your word separator",
            "case" => "uses your capitals",
            _ => null,
        }).OfType<string>().ToArray();
        return parts.Length == 0 ? null : "Learned from your renames: " + string.Join(", ", parts);
    }

    public static string? PreferenceLabel(string key, string value) => (key, value) switch
    {
        ("separator", "_") => "underscores between words",
        ("separator", " ") => "spaces between words",
        ("case", "title") => "Capitalised Words",
        ("case", "upper") => "CAPITALS",
        ("leadingDate", "end") => "the date at the end",
        ("trailingDate", "start") => "the date first",
        ("leadingDate" or "trailingDate", "none") => "no date",
        ("dateFormat", "compact") => "dates like 20230704",
        ("dateFormat", "underscore") => "dates like 2023_07_04",
        ("dateFormat", "dotted") => "dates like 2023.07.04",
        ("dateFormat", "month") => "dates like 2023-07",
        ("people", _) => "no people's names",
        ("words", _) => $"at most {value} {(value == "1" ? "word" : "words")}",
        ("dropWords", _) => $"without \u201C{value}\u201D",
        _ => null,
    };

    public static (string Text, long Samples) LearnedSummary(IEnumerable<CatalogHit> hits, bool enabled)
    {
        var list = hits as IReadOnlyList<CatalogHit> ?? hits.ToArray();
        var samples = list.Where(h => h.Kind == "nameSamples")
            .Select(h => long.TryParse(h.Text, System.Globalization.NumberStyles.None, System.Globalization.CultureInfo.InvariantCulture, out var n) ? n : 0)
            .FirstOrDefault();
        if (!enabled) return ("Names follow the style above only.", samples);
        if (samples == 0) return ("Edit names before you rename them and FileID will follow your style.", 0);
        var count = $"{samples.ToString("N0", System.Globalization.CultureInfo.CurrentCulture)} rename{(samples == 1 ? "" : "s")}";
        var items = list.Where(h => h.Kind == "namePreference")
            .Select(h => PreferenceLabel(h.EvidenceID ?? string.Empty, h.Text)).OfType<string>().ToArray();
        return (items.Length == 0 ? $"No clear preference yet from {count}." : $"From {count}: {string.Join(", ", items)}.", samples);
    }

    public static IReadOnlyList<CatalogNameEdit> Edits(IEnumerable<NameProposalRow> rows, IEnumerable<long> renamed)
    {
        var landed = renamed.ToHashSet();
        return rows.Where(r => r.Checked && r.CanCheck && !r.IsFollower && landed.Contains(r.FileId))
            .Select(r => (Row: r, Name: ValidName(r.Edited)))
            .Where(x => x.Name is not null)
            .Select(x => new CatalogNameEdit(x.Row.FileId, x.Row.Proposed, x.Name!))
            .ToArray();
    }

    public static CatalogRequest LearnRequest(string requestId, IReadOnlyList<CatalogNameEdit> edits, CatalogNaming naming) =>
        new(requestId, "learnNames", null, null, null, null, null, null, Naming: naming, NameEdits: edits);

    public static CatalogRequest? UnlearnRequest(string requestId, IEnumerable<CatalogNameEdit> edits, IEnumerable<long> putBack)
    {
        var back = putBack.ToHashSet();
        var undone = edits.Where(e => back.Contains(e.FileID)).ToArray();
        return undone.Length == 0 ? null : new(requestId, "unlearnNames", null, null, null, null, null, null, NameEdits: undone);
    }

    public static CatalogRequest PreferencesRequest(string requestId, bool reset) =>
        new(requestId, reset ? "resetNamePreferences" : "namePreferences", null, null, null, null, null, null);

    public static (string Reason, long? Group) ParseReason(string? evidenceId)
    {
        var text = evidenceId ?? "noEvidence";
        var marker = text.IndexOf(GroupMarker, StringComparison.Ordinal);
        if (marker < 0) return (text, null);
        return (text[..marker], long.TryParse(text[(marker + GroupMarker.Length)..], System.Globalization.NumberStyles.None,
            System.Globalization.CultureInfo.InvariantCulture, out var group) ? group : null);
    }

    public static IReadOnlyList<NameProposalRow> Rows(IEnumerable<CatalogHit> hits, IReadOnlyList<NameProposalRow> previous)
    {
        var unticked = previous.Where(r => r.CanCheck && !r.Checked).Select(r => r.GroupKey).ToHashSet();
        var rows = hits.Where(h => h.Kind == "proposedName")
            .Select(h =>
            {
                var (tagged, group) = ParseReason(h.EvidenceID);
                var (reason, learned) = SplitLearned(tagged);
                return new NameProposalRow(h.FileID, h.Path, h.Text, reason)
                {
                    Group = group,
                    Learned = learned,
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

    public static string Stem(string name)
    {
        var dot = name.LastIndexOf('.');
        return dot > 0 && dot + 1 < name.Length ? name[..dot] : name;
    }

    public static IReadOnlyList<NameProposalRow> SetChecked(IReadOnlyList<NameProposalRow> rows, long fileId, bool isChecked)
    {
        var key = rows.FirstOrDefault(r => r.FileId == fileId)?.GroupKey;
        if (key is null) return Array.Empty<NameProposalRow>();
        var changed = rows.Where(r => r.GroupKey == key && r.CanCheck).ToArray();
        foreach (var row in changed) row.Checked = isChecked;
        return changed;
    }

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

    /// <summary>Ticked renames in row order, as one batch.</summary>
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
        var reason = ReasonChip(row);
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
    public IReadOnlyList<string> Learned { get; init; } = Array.Empty<string>();
    public string Suffix { get; set; } = string.Empty;
    public bool CanCheck => Proposed.Length > 0;
    public long GroupKey => Group ?? FileId;
    public bool IsLeader => Group == FileId;
    public bool IsFollower => Group is { } group && group != FileId;
    public string CurrentName => System.IO.Path.GetFileName(Path.Replace('\\', '/'));
}
