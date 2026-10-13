using FileID.IpcSchema;

namespace FileID.ViewModels;

internal sealed class ChatSendState
{
    private sealed record Owner(ChatRequest Request, int Generation, TaskCompletionSource<ChatResponse> Completion);
    private readonly object _gate = new();
    private Owner? _owner;
    private ChatResponse? _lastResponse;
    public ChatRequest? Request { get { lock (_gate) return _owner?.Request; } }
    public int Generation { get; private set; } = -1;
    public bool IsActive { get { lock (_gate) return _owner is not null; } }
    public ChatResponse? LastResponse { get { lock (_gate) return _lastResponse; } }
    public Task<ChatResponse> Begin(ChatRequest request, int generation)
    {
        lock (_gate)
        {
            if (_owner is not null) throw new InvalidOperationException("A local Chat send is still unconfirmed. Stop it or refresh read-only history before retrying changes.");
            var completion = new TaskCompletionSource<ChatResponse>(TaskCreationOptions.RunContinuationsAsynchronously);
            _owner = new Owner(request, generation, completion);
            Generation = generation;
            _lastResponse = null;
            _ = completion.Task.ContinueWith(task => _ = task.Exception, CancellationToken.None,
                TaskContinuationOptions.OnlyOnFaulted | TaskContinuationOptions.ExecuteSynchronously, TaskScheduler.Default);
            return completion.Task;
        }
    }
    public bool Apply(ChatResponse response, int generation)
    {
        lock (_gate)
        {
            if (_owner is not { } owner || owner.Generation != generation || owner.Request.RequestID != response.RequestID
                || owner.Request.ConversationID != response.ConversationID
                || (!WorkbenchRequests.IsChatBusy(response.Status) && response.Status is not ("completed" or "cancelled" or "error")))
            {
                return false;
            }

            _lastResponse = response;
            if (!WorkbenchRequests.IsChatBusy(response.Status))
            {
                _owner = null;
                owner.Completion.TrySetResult(response);
            }
            return true;
        }
    }
    public bool ConfirmUnsent(ChatRequest request, int generation, Exception error)
    {
        lock (_gate)
        {
            if (_owner is not { } owner || owner.Generation != generation || owner.Request.RequestID != request.RequestID
                || owner.Request.ConversationID != request.ConversationID)
            {
                return false;
            }

            _owner = null;
            _lastResponse = null;
            owner.Completion.TrySetException(error);
            return true;
        }
    }
    public bool InvalidateGeneration(int generation)
    {
        lock (_gate)
        {
            if (_owner is not { } owner || owner.Generation != generation) return false;
            _owner = null;
            _lastResponse = null;
            owner.Completion.TrySetException(new InvalidOperationException("The engine generation ended before confirming Chat. Refresh saved history; no successful response was inferred."));
            return true;
        }
    }
    public void GuardMutation(ChatRequest request)
    {
        lock (_gate)
        {
            if (_owner is { } owner && request.ConversationID == owner.Request.ConversationID && request.Action == "clear")
                throw new InvalidOperationException("Stop the unconfirmed Chat send before deleting this conversation.");
        }
    }
}
