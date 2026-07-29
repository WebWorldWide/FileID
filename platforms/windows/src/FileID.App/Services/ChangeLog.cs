// ChangeLog — the session-wide record of every applied file mutation
// (renames, trash, restructure applies, people merges, tag writes), each
// with an inverse-action closure so the user can undo anything they didn't
// mean before closing the app.
//
// Supersedes UndoStack's bounded pop-and-drop LIFO as the storage layer;
// UndoStack remains as a facade over this log so Ctrl+Z and the Library
// undo pill keep their exact semantics. Differences from the old stack:
//   - Entries are never silently dropped: an undo failure marks the entry
//     UndoFailed (with the reason) and leaves it visible for retry.
//   - Undone entries stay in the list as a session activity record.
//   - Reverse capacity is 500 (vs 16); overflowing entries lose only their
//     reverse closure (→ NotUndoable "history limit"), not their history line.
//   - A new Restructure entry marks older Restructure entries NotUndoable:
//     the engine keeps a single truncate-per-batch inverse-move journal, so
//     only the latest apply is engine-undoable.
//
// Threading: Push arrives from EngineClient PropertyChanged handlers (the
// untrusted engine-event path) while reads/undo run on the UI thread — all
// list access is under _gate; reverse closures run OUTSIDE the lock
// (mirrors UndoStack's APP-1 note). Session-only by design, matching the
// macOS app's session-scoped Undo.

using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Threading.Tasks;

namespace FileID.Services;

internal enum ChangeKind
{
    Rename,
    Trash,
    Restructure,
    RestructureShortcuts,
    PeopleMerge,
    Tags,
    Other,
}

internal enum ChangeStatus
{
    Undoable,
    Undoing,
    Undone,
    UndoFailed,
    NotUndoable,
}

internal sealed class ChangeLogEntry : INotifyPropertyChanged
{
    internal ChangeLogEntry(
        string label,
        ChangeKind kind,
        Func<Task<bool>>? reverse,
        ChangeStatus status = ChangeStatus.Undoable,
        string? statusDetail = null)
    {
        Id = Guid.NewGuid().ToString("N");
        Timestamp = DateTimeOffset.Now;
        Label = label;
        Kind = kind;
        _reverse = reverse;
        _status = status;
        _statusDetail = statusDetail;
    }

    public string Id { get; }
    public DateTimeOffset Timestamp { get; }
    public string Label { get; }
    public ChangeKind Kind { get; }

    private ChangeStatus _status;
    public ChangeStatus Status
    {
        get => _status;
        internal set
        {
            if (_status == value) return;
            _status = value;
            RaisePropertyChanged(nameof(Status));
        }
    }

    private string? _statusDetail;
    /// <summary>Human-readable reason for UndoFailed / NotUndoable.</summary>
    public string? StatusDetail
    {
        get => _statusDetail;
        internal set
        {
            if (_statusDetail == value) return;
            _statusDetail = value;
            RaisePropertyChanged(nameof(StatusDetail));
        }
    }

    private Func<Task<bool>>? _reverse;
    internal Func<Task<bool>>? TakeReverseForUndo()
    {
        var r = _reverse;
        _reverse = null;
        return r;
    }

    internal void RestoreReverseAfterFailure(Func<Task<bool>> reverse) => _reverse = reverse;

    internal bool HasReverse => _reverse is not null;

    internal void DropReverse() => _reverse = null;

    private bool _supersededDuringUndo;
    internal void MarkSupersededDuringUndo() => _supersededDuringUndo = true;

    internal bool ConsumeSupersededDuringUndo()
    {
        var superseded = _supersededDuringUndo;
        _supersededDuringUndo = false;
        return superseded;
    }

    public event PropertyChangedEventHandler? PropertyChanged;

    private void RaisePropertyChanged(string propertyName)
    {
        var subscribers = PropertyChanged;
        if (subscribers is null) return;
        var args = new PropertyChangedEventArgs(propertyName);
        foreach (PropertyChangedEventHandler subscriber in subscribers.GetInvocationList())
        {
            try { subscriber(this, args); }
            catch (Exception ex) { DebugLog.Warn($"Change-log entry subscriber failed for {propertyName}: {ex.Message}"); }
        }
    }
}

internal sealed class ChangeLog : INotifyPropertyChanged
{
    public static ChangeLog Instance { get; } = new();

    private const int ReverseCapacity = 500;
    private readonly object _gate = new();
    // Newest first.
    private readonly LinkedList<ChangeLogEntry> _entries = new();
    private bool _undoInFlight;

    /// <summary>Entries still undoable — drives the close-confirm gate and
    /// the shell badge.</summary>
    public int UndoableCount
    {
        get
        {
            lock (_gate)
            {
                var n = 0;
                foreach (var e in _entries)
                {
                    if (e.Status == ChangeStatus.Undoable) n++;
                }
                return n;
            }
        }
    }

    /// <summary>Entries whose outcome is still pending at close time. A failed
    /// undo remains pending because it can be retried, and an in-flight undo
    /// remains pending until its terminal result is known.</summary>
    public int PendingCount
    {
        get
        {
            lock (_gate)
            {
                var n = 0;
                foreach (var e in _entries)
                {
                    if (IsPending(e.Status)) n++;
                }
                return n;
            }
        }
    }

    public bool IsUndoInFlight
    {
        get { lock (_gate) { return _undoInFlight; } }
    }

    public int Count { get { lock (_gate) { return _entries.Count; } } }

    public ChangeLogEntry? MostRecentUndoable
    {
        get
        {
            lock (_gate)
            {
                if (_undoInFlight) return null;
                foreach (var e in _entries)
                {
                    if (e.Status == ChangeStatus.Undoable) return e;
                }
                return null;
            }
        }
    }

    /// <summary>Stable copy for rendering, newest first. Entry Status may
    /// still change after the snapshot (entries notify their own
    /// PropertyChanged).</summary>
    public IReadOnlyList<ChangeLogEntry> Snapshot()
    {
        lock (_gate)
        {
            var list = new List<ChangeLogEntry>(_entries.Count);
            list.AddRange(_entries);
            return list;
        }
    }

    public ChangeLogEntry Push(string label, ChangeKind kind, Func<Task<bool>> reverse)
    {
        var entry = new ChangeLogEntry(label, kind, reverse);
        lock (_gate)
        {
            if (kind == ChangeKind.Restructure)
            {
                // The engine's restructure_undo journal is truncated at each
                // apply — only the latest batch can be replayed.
                foreach (var e in _entries)
                {
                    if (e.Kind != ChangeKind.Restructure) continue;
                    if (e.Status is ChangeStatus.Undoable or ChangeStatus.UndoFailed)
                    {
                        MarkRestructureSuperseded(e);
                    }
                    else if (e.Status == ChangeStatus.Undoing)
                    {
                        e.MarkSupersededDuringUndo();
                    }
                }
            }
            _entries.AddFirst(entry);
            TrimReverseClosuresLocked();
        }
        OnChanged();
        return entry;
    }

    public ChangeLogEntry RecordNotUndoable(string label, ChangeKind kind, string detail)
    {
        var entry = new ChangeLogEntry(
            label,
            kind,
            reverse: null,
            status: ChangeStatus.NotUndoable,
            statusDetail: detail);
        lock (_gate)
        {
            _entries.AddFirst(entry);
        }
        OnChanged();
        return entry;
    }

    /// <summary>Run the entry's inverse action. Returns true on success.
    /// Failure marks the entry UndoFailed with the reason and re-arms the
    /// closure so the user can retry (e.g. after closing an Explorer window
    /// that locked the file).</summary>
    public Task<bool> UndoAsync(ChangeLogEntry entry)
        => UndoCoreAsync(entry, retryFailed: false);

    /// <summary>Retry a failed undo without exposing a transient Undoable state
    /// that another global undo could consume first.</summary>
    public Task<bool> RetryAsync(ChangeLogEntry entry)
        => UndoCoreAsync(entry, retryFailed: true);

    private async Task<bool> UndoCoreAsync(ChangeLogEntry entry, bool retryFailed)
    {
        Func<Task<bool>>? reverse;
        lock (_gate)
        {
            var eligible = retryFailed
                ? entry.Status == ChangeStatus.UndoFailed
                : entry.Status == ChangeStatus.Undoable;
            if (!eligible || _undoInFlight) return false;
            reverse = entry.TakeReverseForUndo();
            if (reverse is null) return false;
            _undoInFlight = true;
            entry.StatusDetail = null;
            entry.Status = ChangeStatus.Undoing;
        }
        OnChanged();
        try
        {
            var ok = await reverse().ConfigureAwait(false);
            lock (_gate)
            {
                CompleteUndoLocked(
                    entry,
                    reverse,
                    ok,
                    "The engine couldn't reverse this change.");
            }
            OnChanged();
            return ok;
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"Undo of '{entry.Label}' threw: {ex.Message}");
            lock (_gate)
            {
                CompleteUndoLocked(entry, reverse, succeeded: false, ex.Message);
            }
            OnChanged();
            return false;
        }
    }

    private void CompleteUndoLocked(
        ChangeLogEntry entry,
        Func<Task<bool>> reverse,
        bool succeeded,
        string failureDetail)
    {
        _undoInFlight = false;
        var superseded = entry.ConsumeSupersededDuringUndo();
        if (succeeded)
        {
            entry.StatusDetail = null;
            entry.Status = ChangeStatus.Undone;
            return;
        }

        if (superseded)
        {
            MarkRestructureSuperseded(entry);
            return;
        }

        entry.StatusDetail = failureDetail;
        entry.RestoreReverseAfterFailure(reverse);
        entry.Status = ChangeStatus.UndoFailed;
    }

    public void Clear()
    {
        lock (_gate)
        {
            if (_entries.Count == 0) return;
            foreach (var e in _entries) e.DropReverse();
            _entries.Clear();
        }
        OnChanged();
    }

    public event PropertyChangedEventHandler? PropertyChanged;
    /// <summary>Coarse "something changed" signal for views; re-snapshot on
    /// the dispatcher when it fires (it may arrive off the UI thread).</summary>
    public event EventHandler? Changed;

    private void OnChanged()
    {
        RaisePropertyChanged(nameof(UndoableCount));
        RaisePropertyChanged(nameof(PendingCount));
        RaisePropertyChanged(nameof(IsUndoInFlight));
        RaisePropertyChanged(nameof(Count));

        var subscribers = Changed;
        if (subscribers is null) return;
        foreach (EventHandler subscriber in subscribers.GetInvocationList())
        {
            try { subscriber(this, EventArgs.Empty); }
            catch (Exception ex) { DebugLog.Warn($"Change-log subscriber failed: {ex.Message}"); }
        }
    }

    private void RaisePropertyChanged(string propertyName)
    {
        var subscribers = PropertyChanged;
        if (subscribers is null) return;
        var args = new PropertyChangedEventArgs(propertyName);
        foreach (PropertyChangedEventHandler subscriber in subscribers.GetInvocationList())
        {
            try { subscriber(this, args); }
            catch (Exception ex) { DebugLog.Warn($"Change-log property subscriber failed for {propertyName}: {ex.Message}"); }
        }
    }

    private static bool IsPending(ChangeStatus status)
        => status is ChangeStatus.Undoable or ChangeStatus.Undoing or ChangeStatus.UndoFailed;

    private static void MarkRestructureSuperseded(ChangeLogEntry entry)
    {
        entry.DropReverse();
        entry.StatusDetail = "Superseded — only the most recent restructure can be undone.";
        entry.Status = ChangeStatus.NotUndoable;
    }

    private void TrimReverseClosuresLocked()
    {
        var reverseCount = 0;
        foreach (var entry in _entries)
        {
            if (entry.HasReverse) reverseCount++;
        }
        if (reverseCount <= ReverseCapacity) return;

        for (var node = _entries.Last; node is not null && reverseCount > ReverseCapacity; node = node.Previous)
        {
            var entry = node.Value;
            if (!entry.HasReverse) continue;
            entry.DropReverse();
            entry.StatusDetail = "History limit — this older change can no longer be undone.";
            entry.Status = ChangeStatus.NotUndoable;
            reverseCount--;
        }
    }
}
