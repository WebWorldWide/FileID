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

    public static string CountText(string plainCount, string? summary) => summary ?? plainCount;

    /// <summary>What a screen reader says for the count line.</summary>
    public static string AccessibleName(string countText, string? summary) =>
        string.IsNullOrEmpty(countText) ? string.Empty
        : summary is null ? countText
        : $"Search results. {summary}";
}
