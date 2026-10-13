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
}
