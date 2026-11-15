namespace FileID.ViewModels;

/// <summary>Decides whether a bulk-action reply answers one waiter. A reply that
/// carries the id of the command it answers goes only to the waiter that sent
/// that command; a reply without an id (an older engine) is matched by the
/// action-name prefix, as before.</summary>
internal sealed class BulkResultMatcher(string actionPrefix)
{
    private readonly object _lock = new();
    private readonly HashSet<string> _sentIds = new(StringComparer.Ordinal);

    public void NoteSent(string commandId)
    {
        ArgumentException.ThrowIfNullOrEmpty(commandId);
        lock (_lock) _sentIds.Add(commandId);
    }

    public bool Accepts(string? action, string? commandId)
    {
        if (!string.IsNullOrEmpty(commandId))
        {
            lock (_lock)
            {
                if (_sentIds.Count > 0) return _sentIds.Contains(commandId);
            }
        }

        return action is not null && action.StartsWith(actionPrefix, StringComparison.Ordinal);
    }
}
