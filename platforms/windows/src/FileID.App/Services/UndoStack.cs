// UndoStack — command facade over the session ChangeLog.
//
// Ctrl+Z and the history UI share ordered entries and retryable undo state.
// History remains local to the current process.

using System;
using System.ComponentModel;
using System.Threading;
using System.Threading.Tasks;

namespace FileID.Services;

internal sealed class UndoStack : INotifyPropertyChanged
{
    private sealed class EventRegistration(Action dispose) : IDisposable
    {
        private Action? _dispose = dispose;

        public void Dispose() => Interlocked.Exchange(ref _dispose, null)?.Invoke();
    }
    public static UndoStack Instance { get; } = new();

    private UndoStack() => ChangeLog.Instance.PropertyChanged += (_, _) => OnChanged();

    public bool CanUndo => ChangeLog.Instance.MostRecentUndoable is not null;
    public string TopLabel => ChangeLog.Instance.MostRecentUndoable?.Label ?? string.Empty;

    public void Push(string label, Func<Task<bool>> reverse) =>
        Push(label, ChangeKind.Other, reverse);

    public void Push(string label, ChangeKind kind, Func<Task<bool>> reverse) =>
        ChangeLog.Instance.Push(label, kind, reverse);

    public async Task<string?> UndoAsync()
    {
        var entry = ChangeLog.Instance.MostRecentUndoable;
        return entry is not null && await ChangeLog.Instance.UndoAsync(entry)
            ? entry.Label
            : null;
    }

    public void Clear() => ChangeLog.Instance.Clear();

    public event PropertyChangedEventHandler? PropertyChanged;

    private void OnChanged()
    {
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(CanUndo)));
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(TopLabel)));
    }

    /// <summary>
    /// Helper: subscribe to the next `BulkActionResult` whose action
    /// starts with the given prefix (e.g. "trashFiles:") + push an
    /// undo entry that calls `reverse(batchId)`. Used by Library +
    /// Cleanup trash buttons + the People merge flows.
    /// </summary>
    public static IDisposable CaptureNextBulkResult(
        string actionPrefix,
        string undoLabel,
        Func<string, Task<bool>> reverse)
    {
        var ec = ViewModels.EngineClient.Instance;
        var consumed = 0;
        System.ComponentModel.PropertyChangedEventHandler? handler = null;
        var registration = new EventRegistration(() => ec.PropertyChanged -= handler);
        handler = (_, ev) => DebugLog.SafeRun(nameof(CaptureNextBulkResult), () =>
        {
            if (ev.PropertyName != nameof(ViewModels.EngineClient.LastBulkAction)) return;
            var result = ec.LastBulkAction;
            if (result is null || !result.Action.StartsWith(actionPrefix, StringComparison.Ordinal)) return;
            if (Interlocked.CompareExchange(ref consumed, 1, 0) != 0) return;

            var separator = result.Action.IndexOf(':');
            var batchId = separator >= 0 ? result.Action[(separator + 1)..] : string.Empty;
            registration.Dispose();
            if (batchId.Length == 0) return;
            Instance.Push(undoLabel, () => reverse(batchId));
        });
        ec.PropertyChanged += handler;
        return registration;
    }
}
