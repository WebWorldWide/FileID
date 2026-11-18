using System;
using System.Collections.Generic;
using System.Text.RegularExpressions;

namespace FileID.ViewModels;

/// <summary>The helper writes the sentence; the app only checks it is one.</summary>
internal static partial class SearchStrengthPolicy
{
    public const int MinQueryLength = 3;

    private const int MaxLength = 80;

    [GeneratedRegex(@"^(No matches\.|No good matches, \d{1,9} weaker\.|\d{1,9} good match(es)?(, \d{1,9} weaker)?\.)$", RegexOptions.CultureInvariant)]
    private static partial Regex SummaryShape();

    public static string? Label(string? query, string? message)
    {
        if (query is null || query.Trim().Length < MinQueryLength) return null;
        var text = message?.Trim();
        if (string.IsNullOrEmpty(text) || text.Length > MaxLength) return null;
        return SummaryShape().IsMatch(text) ? text : null;
    }

    [GeneratedRegex(@"^No good matches, (\d{1,9}) weaker\.$", RegexOptions.CultureInvariant)]
    private static partial Regex WeakerOnlyShape();

    [GeneratedRegex(@"^(\d{1,9}) good match(?:es)?(?:, (\d{1,9}) weaker)?\.$", RegexOptions.CultureInvariant)]
    private static partial Regex GoodShape();

    public static (int Good, int Weaker)? Counts(string? message)
    {
        var text = message?.Trim();
        if (string.IsNullOrEmpty(text) || text.Length > MaxLength) return null;
        if (text == "No matches.") return (0, 0);
        if (WeakerOnlyShape().Match(text) is { Success: true } weak) return (0, int.Parse(weak.Groups[1].Value, System.Globalization.CultureInfo.InvariantCulture));
        if (GoodShape().Match(text) is { Success: true } good)
        {
            return (int.Parse(good.Groups[1].Value, System.Globalization.CultureInfo.InvariantCulture),
                good.Groups[2].Success ? int.Parse(good.Groups[2].Value, System.Globalization.CultureInfo.InvariantCulture) : 0);
        }
        return null;
    }

    public readonly record struct Decision(bool KeywordOnly, string? Label, int HiddenWeaker)
    {
        public bool AddKeywordRows => !KeywordOnly;
    }

    /// <summary>The helper's rows in its order, then the keyword rows it lacks in theirs, de-duplicated by file id with no extra cap.</summary>
    public static IReadOnlyList<T> AppendKeywordRows<T>(IReadOnlyList<T> engineRows, IReadOnlyList<T> keywordRows, Func<T, long> id)
    {
        if (keywordRows.Count == 0) return engineRows;
        var known = new HashSet<long>(engineRows.Count);
        foreach (var row in engineRows) known.Add(id(row));
        var combined = new List<T>(engineRows.Count + keywordRows.Count);
        combined.AddRange(engineRows);
        foreach (var row in keywordRows)
        {
            if (known.Add(id(row))) combined.Add(row);
        }
        return combined;
    }

    /// <summary>Otherwise only the keywordCount keyword matches.</summary>
    public static Decision Decide(string? query, string? message, int keywordCount, bool showWeaker)
    {
        var label = Label(query, message);
        if (label is null || Counts(message) is not { } counts) return new Decision(false, label, 0);
        if (counts.Good > 0 || showWeaker) return new Decision(false, label, 0);
        var shown = counts.Weaker == 0 ? "No matches"
            : keywordCount > 0 ? $"{keywordCount} by name, tag or text"
            : "No good matches";
        return new Decision(true, shown, counts.Weaker);
    }

    public static string WeakerButtonText(int hidden) => $"Show {hidden} weaker matches";

    public static string NoMatchesText(string query, int hiddenWeaker) =>
        hiddenWeaker > 0
            ? $"No matches for \u201C{query}\u201D. Nothing has that in its name, tags or text, and no picture matches it well. {hiddenWeaker} weaker look-alikes were set aside."
            : $"No matches for \u201C{query}\u201D. Nothing has that in its name, tags or text, and no picture matches it. Try other words.";

    public static string CountText(string plainCount, string? summary) => summary ?? plainCount;

    public static string AccessibleName(string countText, string? summary) =>
        string.IsNullOrEmpty(countText) ? string.Empty
        : summary is null ? countText
        : $"Search results. {summary}";
}
