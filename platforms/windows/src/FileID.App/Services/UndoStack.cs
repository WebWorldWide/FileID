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

    public bool CanUndo => _entries.Count > 0;
    public string TopLabel => _entries.Count == 0 ? string.Empty : _entries.First!.Value.Label;

    public void Push(string label, Func<Task<bool>> reverse)
    {
        _entries.AddFirst(new UndoEntry(label, reverse));
        while (_entries.Count > Capacity) _entries.RemoveLast();
        OnChanged();
    }

    /// <summary>Pop + invoke. Returns the label that was undone, or null on failure / empty.</summary>
    public async Task<string?> UndoAsync()
    {
        if (_entries.Count == 0) return null;
        var entry = _entries.First!.Value;
        _entries.RemoveFirst();
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
        if (_entries.Count == 0) return;
        _entries.Clear();
        OnChanged();
    }

    public event PropertyChangedEventHandler? PropertyChanged;

    private void OnChanged()
    {
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(CanUndo)));
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(TopLabel)));
    }

    private sealed record UndoEntry(string Label, Func<Task<bool>> Reverse);
}
