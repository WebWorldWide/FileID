// UndoStack — the Ctrl+Z facade over ChangeLog.
//
// Historically a bounded 16-entry LIFO of reverse-op closures; the storage
// moved to ChangeLog (the session change log with per-entry undo + the
// close-time review sheet) so the Library undo pill, Ctrl+Z, and the
// changes sheet all see the same record. The public surface here is
// unchanged — call sites and semantics (undo the most recent still-undoable
// action) are preserved. One deliberate improvement: a failed reverse no
// longer silently drops the entry; it stays visible in the changes sheet
// as "undo failed" with a retry.
//
// Threading unchanged (APP-1): Push arrives from EngineClient
// PropertyChanged handlers; ChangeLog gates all list access and runs
// reverse closures outside its lock.

using System;
using System.ComponentModel;
using System.Threading.Tasks;

namespace FileID.Services;

internal sealed class UndoStack : INotifyPropertyChanged
{
    public static UndoStack Instance { get; } = new();

    private UndoStack()
    {
        ChangeLog.Instance.Changed += (_, _) => OnChanged();
    }

    public bool CanUndo => ChangeLog.Instance.UndoableCount > 0;
    public string TopLabel => ChangeLog.Instance.MostRecentUndoable?.Label ?? string.Empty;

    public void Push(string label, Func<Task<bool>> reverse)
        => Push(label, ChangeKind.Other, reverse);

    public void Push(string label, ChangeKind kind, Func<Task<bool>> reverse)
        => ChangeLog.Instance.Push(label, kind, reverse);

    /// <summary>Undo the most recent still-undoable action. Returns the label
    /// that was undone, or null on failure / empty.</summary>
    public async Task<string?> UndoAsync()
    {
        var entry = ChangeLog.Instance.MostRecentUndoable;
        if (entry is null) return null;
        var ok = await ChangeLog.Instance.UndoAsync(entry).ConfigureAwait(false);
        return ok ? entry.Label : null;
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
        Func<string, Task<bool>> reverse,
        ChangeKind kind = ChangeKind.Other,
        TimeSpan? timeout = null)
    {
        var ec = ViewModels.EngineClient.Instance;

        // BUG-7: previous version had a race — if the timeout fired,
        // the next BulkActionResult would match the next registered
        // handler instead, causing cross-talk between unrelated bulk
        // actions. Use a single guard int that is consumed atomically:
        // either the engine reply path wins, or the timeout path wins,
        // and the loser is a no-op.
        int consumed = 0; // 0 = pending, 1 = consumed
        System.ComponentModel.PropertyChangedEventHandler? once = null;
        once = (_, ev) =>
        {
            if (ev.PropertyName != nameof(ViewModels.EngineClient.LastBulkAction)) return;
            var bar = ec.LastBulkAction;
            if (bar is null) return;
            if (!bar.Action.StartsWith(actionPrefix, StringComparison.Ordinal)) return;

            if (System.Threading.Interlocked.CompareExchange(ref consumed, 1, 0) != 0) return;

            // Action is "trashFiles:<uuid>". A missing/empty suffix (no colon,
            // or a trailing ':' with nothing after it) yields no batch id; skip
            // rather than push an undo entry whose reverse can never resolve.
            // IndexOf+Substring is bounds-safe — never throws on a malformed suffix.
            var colonIdx = bar.Action.IndexOf(':');
            var batchId = colonIdx >= 0 ? bar.Action.Substring(colonIdx + 1) : string.Empty;
            ec.PropertyChanged -= once;
            if (batchId.Length == 0) return;
            Instance.Push(undoLabel, kind, () => reverse(batchId));
        };
        ec.PropertyChanged += once;

        void Cancel()
        {
            if (System.Threading.Interlocked.CompareExchange(ref consumed, 1, 0) != 0) return;
            try { ec.PropertyChanged -= once; } catch { /* swallow */ }
        }
        _ = Task.Delay(timeout ?? TimeSpan.FromSeconds(30)).ContinueWith(_ => Cancel());
        return new CallbackRegistration(Cancel);
    }

    private sealed class CallbackRegistration(Action cancel) : IDisposable
    {
        private Action? _cancel = cancel;

        public void Dispose() => System.Threading.Interlocked.Exchange(ref _cancel, null)?.Invoke();
    }
}
