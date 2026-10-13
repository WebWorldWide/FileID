using FileID.IpcSchema;

namespace FileID.ViewModels;

internal sealed class ToolExecutionState
{
    private sealed record Owner(string RequestID, string OperationID, int Generation, TaskCompletionSource<ToolResponse> Completion);
    private readonly object _gate = new();
    private Owner? _owner;
    private Owner? _recoveryOwner;
    private ToolResponse? _lastResponse;
    private bool _stopping;
    public string? OperationID { get { lock (_gate) return _owner?.OperationID; } }
    public int Generation { get { lock (_gate) return _owner?.Generation ?? -1; } }
    public bool IsActive { get { lock (_gate) return _owner is not null; } }
    public bool IsStopping { get { lock (_gate) return _stopping; } }
    public bool NeedsRecovery { get { lock (_gate) return _recoveryOwner is not null; } }
    public ToolResponse? LastResponse { get { lock (_gate) return _lastResponse; } }
    public string Label
    {
        get
        {
            lock (_gate)
            {
                return _owner is not null
                ? _stopping ? "Stopping · awaiting final export results" : "Exporting · awaiting final export results"
                : _recoveryOwner is not null ? "Previous export outcome unknown · reconcile matching durable history" : "No export awaiting confirmation.";
            }
        }
    }
    public Task<ToolResponse> Begin(string requestID, string operationID, int generation)
    {
        lock (_gate)
        {
            if (_owner is not null || _recoveryOwner is not null)
                throw new InvalidOperationException("An export is awaiting confirmation or durable recovery. Check Last export before retrying.");
            var completion = new TaskCompletionSource<ToolResponse>(TaskCreationOptions.RunContinuationsAsynchronously);
            _owner = new Owner(requestID, operationID, generation, completion);
            _lastResponse = null;
            _stopping = false;
            _ = completion.Task.ContinueWith(task => _ = task.Exception, CancellationToken.None,
                TaskContinuationOptions.OnlyOnFaulted | TaskContinuationOptions.ExecuteSynchronously, TaskScheduler.Default);
            return completion.Task;
        }
    }
    public bool ConfirmUnsent(string requestID, string operationID, int generation, Exception error)
    {
        lock (_gate)
        {
            var owner = _owner ?? _recoveryOwner;
            if (owner is null || owner.RequestID != requestID || owner.OperationID != operationID || owner.Generation != generation) return false;
            _owner = null;
            _recoveryOwner = null;
            _stopping = false;
            owner.Completion.TrySetException(error);
            return true;
        }
    }
    public string RequireCancellationOwner(int generation)
    {
        lock (_gate)
        {
            return _owner is { } owner && owner.Generation == generation ? owner.OperationID
            : throw new InvalidOperationException("There is no current export to cancel in this engine.");
        }
    }
    public void BeginCancellation(int generation)
    {
        lock (_gate)
        {
            _ = RequireCancellationOwner(generation);
            if (_stopping) throw new InvalidOperationException("A stop is already awaiting the export's final response.");
            _stopping = true;
        }
    }
    public bool ApplyCancellationAck(string operationID, int generation, ToolResponse response)
    {
        lock (_gate)
        {
            if (_owner is not { } owner || owner.OperationID != operationID || owner.Generation != generation) return false;
            if (response.Status == "error") _stopping = false;
            return true;
        }
    }
    public void CancellationFailed(string operationID, int generation)
    {
        lock (_gate)
        {
            if (_owner is { } owner && owner.OperationID == operationID && owner.Generation == generation) _stopping = false;
        }
    }
    public static bool SupportsCancellation(IEnumerable<ToolCapability> capabilities) =>
        capabilities.Any(capability => capability is { Id: "cancel", Available: true }
            && capability.InputFormats.Count == 0 && capability.OutputFormats.Count == 0);
    public bool ApplyTerminal(ToolResponse response, int generation)
    {
        lock (_gate)
        {
            if (_owner is not { } owner || owner.RequestID != response.RequestID
                || owner.OperationID != response.OperationID || owner.Generation != generation || response.Status is not ("ok" or "error"))
            {
                return false;
            }

            _owner = null;
            _stopping = false;
            _lastResponse = response;
            owner.Completion.TrySetResult(response);
            return true;
        }
    }
    public bool InvalidateGeneration(int generation)
    {
        lock (_gate)
        {
            if (_owner is not { } owner || owner.Generation != generation) return false;
            _owner = null;
            _stopping = false;
            _recoveryOwner = owner;
            owner.Completion.TrySetException(new InvalidOperationException("The engine generation ended before confirming this export. Its outcome is unknown; reconcile durable history."));
            return true;
        }
    }
    public bool ReconcileHistory(ToolResponse response)
    {
        lock (_gate)
        {
            if (_owner is not null || _recoveryOwner is null || response.Status != "ok" || response.OperationID != _recoveryOwner.OperationID
                || response.Outputs.Any(output => output.State is not ("completed" or "failed" or "undone")))
            {
                return false;
            }

            _recoveryOwner = null;
            _lastResponse = response;
            return true;
        }
    }
}
