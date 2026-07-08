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
//   - Capacity is 500 (vs 16); overflowing entries lose only their reverse
//     closure (→ NotUndoable "history limit"), not their history line.
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
    PeopleMerge,
    Tags,
    Other,
}

internal enum ChangeStatus
{
    Undoable,
    Undone,
    UndoFailed,
    NotUndoable,
}

internal sealed class ChangeLogEntry : INotifyPropertyChanged
{
    internal ChangeLogEntry(string label, ChangeKind kind, Func<Task<bool>> reverse)
    {
        Id = Guid.NewGuid().ToString("N");
        Timestamp = DateTimeOffset.Now;
        Label = label;
        Kind = kind;
        _reverse = reverse;
        _status = ChangeStatus.Undoable;
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
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(Status)));
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
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(StatusDetail)));
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

    internal void DropReverse() => _reverse = null;

    public event PropertyChangedEventHandler? PropertyChanged;
}

internal sealed class ChangeLog : INotifyPropertyChanged
{
    public static ChangeLog Instance { get; } = new();

    private const int Capacity = 500;
    private readonly object _gate = new();
    // Newest first.
    private readonly LinkedList<ChangeLogEntry> _entries = new();

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

    public int Count { get { lock (_gate) { return _entries.Count; } } }

    public ChangeLogEntry? MostRecentUndoable
    {
        get
        {
            lock (_gate)
            {
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
                    if (e.Kind == ChangeKind.Restructure && e.Status == ChangeStatus.Undoable)
                    {
                        e.Status = ChangeStatus.NotUndoable;
                        e.StatusDetail = "Superseded — only the most recent restructure can be undone.";
                        e.DropReverse();
                    }
                }
            }
            _entries.AddFirst(entry);
            while (_entries.Count > Capacity)
            {
                var oldest = _entries.Last!.Value;
                _entries.RemoveLast();
                // Keep nothing alive that could pin big closures; the entry
                // itself is dropped from history at the cap.
                oldest.DropReverse();
            }
        }
        OnChanged();
        return entry;
    }

    /// <summary>Run the entry's inverse action. Returns true on success.
    /// Failure marks the entry UndoFailed with the reason and re-arms the
    /// closure so the user can retry (e.g. after closing an Explorer window
    /// that locked the file).</summary>
    public async Task<bool> UndoAsync(ChangeLogEntry entry)
    {
        Func<Task<bool>>? reverse;
        lock (_gate)
        {
            if (entry.Status != ChangeStatus.Undoable) return false;
            // Taking the closure marks the entry in-flight: a concurrent
            // UndoAsync on the same entry sees null and no-ops without
            // touching Status (the first attempt owns the outcome).
            reverse = entry.TakeReverseForUndo();
            if (reverse is null) return false;
        }
        try
        {
            var ok = await reverse().ConfigureAwait(false);
            lock (_gate)
            {
                if (ok)
                {
                    entry.Status = ChangeStatus.Undone;
                    entry.StatusDetail = null;
                }
                else
                {
                    entry.StatusDetail = "The engine couldn't reverse this change.";
                    entry.RestoreReverseAfterFailure(reverse);
                    entry.Status = ChangeStatus.UndoFailed;
                }
            }
            OnChanged();
            return ok;
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"Undo of '{entry.Label}' threw: {ex.Message}");
            lock (_gate)
            {
                entry.StatusDetail = ex.Message;
                entry.RestoreReverseAfterFailure(reverse);
                entry.Status = ChangeStatus.UndoFailed;
            }
            OnChanged();
            return false;
        }
    }

    /// <summary>Retry a failed undo: re-arm and run again.</summary>
    public Task<bool> RetryAsync(ChangeLogEntry entry)
    {
        lock (_gate)
        {
            if (entry.Status == ChangeStatus.UndoFailed)
            {
                entry.Status = ChangeStatus.Undoable;
            }
        }
        OnChanged();
        return UndoAsync(entry);
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
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(UndoableCount)));
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(Count)));
        Changed?.Invoke(this, EventArgs.Empty);
    }
}
