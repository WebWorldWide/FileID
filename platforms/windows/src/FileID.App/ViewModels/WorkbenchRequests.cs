using FileID.IpcSchema;

namespace FileID.ViewModels;

internal sealed class WorkbenchRequests : IDisposable
{
    private sealed record Pending(int Generation, string? ConversationID, TaskCompletionSource<object> Completion);
    private readonly Dictionary<(Type Type, string ID), Pending> _pending = new();
    private readonly object _gate = new();
    private bool _disposed;

    public Task<T> Register<T>(string id, int generation, string? conversationID = null)
    {
        lock (_gate)
        {
            ObjectDisposedException.ThrowIf(_disposed, this);
            var completion = new TaskCompletionSource<object>(TaskCreationOptions.RunContinuationsAsynchronously);
            if (!_pending.TryAdd((typeof(T), id), new Pending(generation, conversationID, completion)))
                throw new InvalidOperationException("This request ID is already pending.");
            return ConvertAsync<T>(completion.Task);
        }
    }

    private static async Task<T> ConvertAsync<T>(Task<object> task) => (T)await task.ConfigureAwait(false);

    public bool Apply<T>(string id, int generation, T response, string? conversationID = null, bool terminal = true) where T : notnull
    {
        lock (_gate)
        {
            var key = (typeof(T), id);
            if (!_pending.TryGetValue(key, out var pending) || pending.Generation != generation
                || pending.ConversationID != conversationID)
            {
                return false;
            }

            if (terminal)
            {
                _pending.Remove(key);
                pending.Completion.TrySetResult(response);
            }
            return true;
        }
    }

    public void Remove<T>(string id, Exception error)
    {
        lock (_gate)
        {
            if (_pending.Remove((typeof(T), id), out var pending)) pending.Completion.TrySetException(error);
        }
    }

    public void FailGeneration(int generation)
    {
        lock (_gate)
        {
            foreach (var key in _pending.Where(pair => pair.Value.Generation == generation).Select(pair => pair.Key).ToArray())
            {
                var pending = _pending[key];
                _pending.Remove(key);
                pending.Completion.TrySetException(new InvalidOperationException("The engine stopped before confirming the request. Refresh history before retrying changes."));
            }
        }
    }

    public void Dispose()
    {
        lock (_gate)
        {
            _disposed = true;
            foreach (var pending in _pending.Values)
                pending.Completion.TrySetException(new ObjectDisposedException(nameof(WorkbenchRequests)));
            _pending.Clear();
        }
    }

    public static bool IsChatBusy(string status) => status is "retrieving" or "queued" or "streaming";
}
