// UndoStack — bounded LIFO of reversible destructive actions.
//
// Records the last 16 destructive ops (rename / trash / restructure /
// merge-clusters) with a reverse-op closure. Ctrl+Z on MainWindow pops
// the top entry + invokes its reverse. Best-effort: an entry is silently
// dropped if its reverse fails (e.g., the file was deleted from the
// Recycle Bin between trash + undo).
//
// Per-entry state is kept compact (a label + a reverse-action async
// delegate). The LIFO is process-local — restart loses the history,
// matching the macOS app's session-only Undo behavior.

using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.CompilerServices;
using System.Threading.Tasks;

namespace FileID.Services;

internal sealed class UndoStack : INotifyPropertyChanged
{
    public static UndoStack Instance { get; } = new();

    private const int Capacity = 16;
    private readonly LinkedList<UndoEntry> _entries = new();
    // APP-1: `Push` is invoked from EngineClient PropertyChanged handlers (the
    // "untrusted" engine-event path) while `UndoAsync`/`Clear`/`CanUndo`/
    // `TopLabel` run on the UI thread. A LinkedList is not thread-safe, so the
    // unsynchronized mix could corrupt list pointers (NRE / InvalidOperation)
    // and fast-fail the process. All `_entries` access is now under `_gate`;
    // the async reverse closure runs OUTSIDE the lock.
    private readonly object _gate = new();

    public bool CanUndo { get { lock (_gate) { return _entries.Count > 0; } } }
    public string TopLabel
    {
        get { lock (_gate) { return _entries.Count == 0 ? string.Empty : _entries.First!.Value.Label; } }
    }

    public void Push(string label, Func<Task<bool>> reverse)
    {
        lock (_gate)
        {
            _entries.AddFirst(new UndoEntry(label, reverse));
            while (_entries.Count > Capacity) _entries.RemoveLast();
        }
        OnChanged();
    }

    /// <summary>Pop + invoke. Returns the label that was undone, or null on failure / empty.</summary>
    public async Task<string?> UndoAsync()
    {
        UndoEntry entry;
        lock (_gate)
        {
            if (_entries.Count == 0) return null;
            entry = _entries.First!.Value;
            _entries.RemoveFirst();
        }
        OnChanged();
        try
        {
            var ok = await entry.Reverse();
            return ok ? entry.Label : null;
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"Undo of '{entry.Label}' threw: {ex.Message}");
            return null;
        }
    }

    public void Clear()
    {
        lock (_gate)
        {
            if (_entries.Count == 0) return;
            _entries.Clear();
        }
        OnChanged();
    }

    public event PropertyChangedEventHandler? PropertyChanged;

    private void OnChanged()
    {
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(CanUndo)));
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(TopLabel)));
    }

    private sealed record UndoEntry(string Label, Func<Task<bool>> Reverse);

    /// <summary>
    /// Helper: subscribe to the next `BulkActionResult` whose action
    /// starts with the given prefix (e.g. "trashFiles:") + push an
    /// undo entry that calls `reverse(batchId)`. Used by Library +
    /// Cleanup trash buttons + the People merge flows.
    /// </summary>
    public static void CaptureNextBulkResult(string actionPrefix, string undoLabel,
        Func<string, Task<bool>> reverse)
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
            Instance.Push(undoLabel, () => reverse(batchId));
        };
        ec.PropertyChanged += once;

        _ = Task.Delay(TimeSpan.FromSeconds(30)).ContinueWith(_ =>
        {
            // Only detach if we haven't already consumed a reply. This
            // prevents the timeout from racing the reply handler and
            // erroneously detaching after a successful Push.
            if (System.Threading.Interlocked.CompareExchange(ref consumed, 1, 0) != 0) return;
            try { ec.PropertyChanged -= once; } catch { /* swallow */ }
        });
    }
}
