using FileID.IpcSchema;

namespace FileID.ViewModels;

/// <summary>Chat message and citation layout rules (macOS ChatWorkbench):
/// who said what, which answer carries the citation chips, and whether a new
/// reply only adds to the messages already shown.</summary>
internal static class ChatPresentation
{
    public const int MaxChips = 8;

    public static bool IsUser(ChatMessage message) => message.Role == "user";

    public static string RoleLabel(ChatMessage message) => IsUser(message) ? "You" : "FileID";

    /// <summary>The newest answer; citation chips sit under it only.</summary>
    public static string? LastAnswerID(IReadOnlyList<ChatMessage> messages) =>
        messages.LastOrDefault(message => !IsUser(message))?.Id;

    public static IReadOnlyList<CatalogHit> ChipHits(IReadOnlyList<CatalogHit> hits) => hits.Take(MaxChips).ToArray();

    /// <summary>True when <paramref name="next"/> keeps every shown message in
    /// order, so the views can be updated and extended instead of rebuilt.</summary>
    public static bool ExtendsShown(IReadOnlyList<string> shown, IReadOnlyList<ChatMessage> next)
    {
        if (next.Count < shown.Count) return false;
        for (var i = 0; i < shown.Count; i++)
        {
            if (shown[i] != next[i].Id) return false;
        }
        return true;
    }

    public static string SourcesCount(int count) => count == 0 ? "None yet" : count.ToString(System.Globalization.CultureInfo.CurrentCulture);

    public static string ChipMeta(CatalogHit hit)
    {
        var meta = HitPresentation.SourceMeta(hit);
        return meta.Length == 0 ? "" : "· " + meta;
    }
}
