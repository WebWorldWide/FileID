using FileID.IpcSchema;

namespace FileID.ViewModels;

internal static class ChatFrontendPolicy
{
    // The original retrieval-only adapter has neither generation nor effective cancellation.
    public static bool SupportsModelGeneration => false;
    public static bool SupportsCancellation => false;

    public static bool UseModelForSend(bool requested) => SupportsModelGeneration && requested;

    public static bool ShouldReplaceCitations(string? action, ChatResponse response) =>
        response.Status != "error" && (action != "history" || response.Hits.Count > 0);

    /// <summary>The line under the composer, or empty when the engine message
    /// only repeats the latest answer already shown in the conversation.</summary>
    public static string StatusLine(ChatResponse response)
    {
        var lastAnswer = response.Messages.LastOrDefault(message => message.Role != "user")?.Text;
        if (!string.IsNullOrWhiteSpace(lastAnswer)
            && string.Equals(response.Message.Trim(), lastAnswer.Trim(), StringComparison.Ordinal))
        {
            return string.Empty;
        }
        return $"{response.Status} · {response.Message}";
    }
}
