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
using FileID.IpcSchema;

namespace FileID.Services;

internal sealed class UndoStack : INotifyPropertyChanged
{
    public static UndoStack Instance { get; } = new();

    private UndoStack()
    {
        ChangeLog.Instance.Changed += (_, _) => OnChanged();
    }

    public bool CanUndo => ChangeLog.Instance.MostRecentUndoable is not null;
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
        RaisePropertyChanged(nameof(CanUndo));
        RaisePropertyChanged(nameof(TopLabel));
    }

    private void RaisePropertyChanged(string propertyName)
    {
        var subscribers = PropertyChanged;
        if (subscribers is null) return;
        var args = new PropertyChangedEventArgs(propertyName);
        foreach (PropertyChangedEventHandler subscriber in subscribers.GetInvocationList())
        {
            try { subscriber(this, args); }
            catch (Exception ex) { DebugLog.Warn($"Undo-stack subscriber failed for {propertyName}: {ex.Message}"); }
        }
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
        once = (_, ev) => DebugLog.SafeRun("UndoStack.CaptureNextBulkResult", () =>
        {
            if (ev.PropertyName != nameof(ViewModels.EngineClient.LastBulkAction)) return;
            var bar = ec.LastBulkAction;
            if (bar is null) return;
            if (!bar.Action.StartsWith(actionPrefix, StringComparison.Ordinal)) return;

            if (System.Threading.Interlocked.CompareExchange(ref consumed, 1, 0) != 0) return;

            ec.PropertyChanged -= once;
            var batchId = GetUndoableBatchId(bar);
            if (batchId is null) return;
            var historyLabel = bar.Failed == 0
                ? undoLabel
                : $"{undoLabel} — {bar.Succeeded:N0} changed";
            Instance.Push(historyLabel, kind, () => reverse(batchId));
        });
        ec.PropertyChanged += once;

        void Cancel()
        {
            if (System.Threading.Interlocked.CompareExchange(ref consumed, 1, 0) != 0) return;
            try { ec.PropertyChanged -= once; } catch { /* swallow */ }
        }
        _ = Task.Delay(timeout ?? TimeSpan.FromSeconds(30)).ContinueWith(_ => Cancel());
        return new CallbackRegistration(Cancel);
    }

    internal static string? GetUndoableBatchId(BulkActionResult result)
    {
        if (result.Succeeded == 0 || string.IsNullOrWhiteSpace(result.Action)) return null;
        var colonIdx = result.Action.IndexOf(':');
        if (colonIdx < 0) return null;
        var batchId = result.Action[(colonIdx + 1)..].Trim();
        return batchId.Length == 0 ? null : batchId;
    }

    private sealed class CallbackRegistration(Action cancel) : IDisposable
    {
        private Action? _cancel = cancel;

        public void Dispose() => System.Threading.Interlocked.Exchange(ref _cancel, null)?.Invoke();
    }
}
