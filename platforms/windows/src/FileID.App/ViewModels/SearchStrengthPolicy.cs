using System.Text.RegularExpressions;

namespace FileID.ViewModels;

/// <summary>Decides when the Library shows the helper's search summary
/// ("3 good matches, 1 weaker.") in place of the plain result count. The
/// helper writes the sentence; the app only checks it is one.</summary>
internal static partial class SearchStrengthPolicy
{
    /// <summary>Shorter searches show the plain count, as on the Mac.</summary>
    public const int MinQueryLength = 3;

    private const int MaxLength = 80;

    [GeneratedRegex(@"^(No matches\.|No good matches, \d{1,9} weaker\.|\d{1,9} good match(es)?(, \d{1,9} weaker)?\.)$", RegexOptions.CultureInvariant)]
    private static partial Regex SummaryShape();

    /// <summary>The summary to show for a typed search, or null to keep the
    /// plain count: the search is too short, the helper sent no message, or
    /// the message is some other note (for example "Visual similarity
    /// results.").</summary>
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

    /// <summary>The helper's good and weaker counts, or null when
    /// <paramref name="message"/> is not one of its summary sentences.</summary>
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

    /// <summary>How a typed search is shown. <see cref="KeywordOnly"/> keeps the
    /// name, tag and text matches and holds the weaker look-alikes back behind
    /// "Show N weaker matches" (<see cref="HiddenWeaker"/>).</summary>
    public readonly record struct Decision(bool KeywordOnly, string? Label, int HiddenWeaker);

    /// <summary>The Mac's rule (LibrarySearchOutcome.decide). No summary, or a
    /// search shorter than <see cref="MinQueryLength"/>: today's behaviour
    /// (the helper's list, <see cref="Label"/>). Good matches, or the user asked
    /// for the weaker ones: the helper's list and summary. Otherwise only the
    /// <paramref name="keywordCount"/> keyword matches.</summary>
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

    /// <summary>The empty-state sentence when a search found no keyword match
    /// (the Mac's wording, title and message joined).</summary>
    public static string NoMatchesText(string query, int hiddenWeaker) =>
        hiddenWeaker > 0
            ? $"No matches for \u201C{query}\u201D. Nothing has that in its name, tags or text, and no picture matches it well. {hiddenWeaker} weaker look-alikes were set aside."
            : $"No matches for \u201C{query}\u201D. Nothing has that in its name, tags or text, and no picture matches it. Try other words.";

    public static string CountText(string plainCount, string? summary) => summary ?? plainCount;

    /// <summary>What a screen reader says for the count line.</summary>
    public static string AccessibleName(string countText, string? summary) =>
        string.IsNullOrEmpty(countText) ? string.Empty
        : summary is null ? countText
        : $"Search results. {summary}";
}
