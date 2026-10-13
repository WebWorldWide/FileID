using FileID.IpcSchema;

namespace FileID.ViewModels;

internal sealed class ChatPresentationRevision
{
    internal readonly record struct HistoryStamp(string RequestID, string ConversationID, int Generation, long Revision);
    private readonly int _threadID = Environment.CurrentManagedThreadId;
    private long _revision;

    // Checking the revision and redrawing must share the captured presentation thread.
    private void VerifyAccess()
    {
        if (Environment.CurrentManagedThreadId != _threadID)
        {
            throw new InvalidOperationException("Chat presentation revisions require their owning UI thread.");
        }
    }
    public HistoryStamp CaptureHistory(ChatRequest request, int generation)
    {
        VerifyAccess();
        return new HistoryStamp(request.RequestID, request.ConversationID, generation, _revision);
    }
    public void Invalidate()
    {
        VerifyAccess();
        _revision++;
    }
    public bool TryRenderHistory(HistoryStamp stamp, ChatResponse response,
        string? conversationID, int generation, Action<ChatResponse> render)
    {
        VerifyAccess();
        if (stamp.Revision != _revision || stamp.RequestID != response.RequestID || stamp.ConversationID != response.ConversationID
            || stamp.ConversationID != conversationID || stamp.Generation != generation)
        {
            return false;
        }
        render(response);
        return true;
    }
}
