using FileID.IpcSchema;

namespace FileID.ViewModels;

internal enum ChatResponseOwner { None, Request, CancellationAcknowledgement }

internal sealed class ChatRequestState
{
    private ChatRequest? _request;
    private string? _cancelID;
    public int Generation { get; private set; }
    public bool CanStop => _request?.Action == "send" && _cancelID is null;
    public bool HasActiveSend => _request?.Action == "send";
    public string? Action => _request?.Action;
    public string? RequestID => _request?.RequestID;

    public void Begin(ChatRequest request, int generation)
    {
        if (HasActiveSend) throw new InvalidOperationException("Wait for the current request's own completion before starting another request.");
        _request = request;
        _cancelID = null;
        Generation = generation;
    }

    public ChatRequest Cancel(int generation)
    {
        if (!CanStop || Generation != generation) throw new InvalidOperationException("There is no current response to stop in this engine.");
        _cancelID = Guid.NewGuid().ToString();
        return new ChatRequest(_cancelID, _request!.ConversationID, "cancel");
    }

    public ChatResponseOwner GetResponseOwner(ChatResponse response, int generation)
    {
        if (_request is null || Generation != generation || response.ConversationID != _request.ConversationID) return ChatResponseOwner.None;
        if (_cancelID is not null && response.RequestID == _cancelID) return ChatResponseOwner.CancellationAcknowledgement;
        return response.RequestID == _request.RequestID ? ChatResponseOwner.Request : ChatResponseOwner.None;
    }

    public bool Apply(ChatResponse response, int generation)
    {
        var owner = GetResponseOwner(response, generation);
        if (owner == ChatResponseOwner.None) return false;
        if (owner == ChatResponseOwner.CancellationAcknowledgement)
        {
            if (WorkbenchRequests.IsChatBusy(response.Status)) return false;
            _cancelID = null;
            return true;
        }
        if (!WorkbenchRequests.IsChatBusy(response.Status)) { _request = null; _cancelID = null; }
        return true;
    }

    public void CancelFailed() => _cancelID = null;
    public void Reset() { _request = null; _cancelID = null; }
}
