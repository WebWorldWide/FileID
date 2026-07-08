// ChangeLog — the session change log behind UndoStack and the
// SessionChangesSheet. Pins the log's contract: entries survive undo
// (as Undone), survive failure (as UndoFailed + retry), restructure
// pushes supersede older restructure entries, and the capacity bound
// drops history (never correctness).

using System;
using System.Linq;
using System.Threading.Tasks;
using FileID.Services;
using Xunit;

namespace FileID.App.Tests;

public class ChangeLogTests
{
    private static ChangeLog Log => ChangeLog.Instance;

    [Fact]
    public async Task Undo_MarksEntryUndone_AndKeepsItInHistory()
    {
        Log.Clear();
        var entry = Log.Push("rename 2 files", ChangeKind.Rename, () => Task.FromResult(true));
        Assert.Equal(1, Log.UndoableCount);

        var ok = await Log.UndoAsync(entry);

        Assert.True(ok);
        Assert.Equal(ChangeStatus.Undone, entry.Status);
        Assert.Equal(0, Log.UndoableCount);
        Assert.Single(Log.Snapshot()); // history retained
        Log.Clear();
    }

    [Fact]
    public async Task FailedUndo_MarksUndoFailed_KeepsEntry_AndRetryCanSucceed()
    {
        Log.Clear();
        var attempts = 0;
        var entry = Log.Push("trash 3 files", ChangeKind.Trash, () =>
        {
            attempts++;
            return Task.FromResult(attempts > 1); // fail first, succeed on retry
        });

        Assert.False(await Log.UndoAsync(entry));
        Assert.Equal(ChangeStatus.UndoFailed, entry.Status);
        Assert.NotNull(entry.StatusDetail);
        Assert.Single(Log.Snapshot()); // NOT silently dropped

        Assert.True(await Log.RetryAsync(entry));
        Assert.Equal(ChangeStatus.Undone, entry.Status);
        Log.Clear();
    }

    [Fact]
    public async Task ThrowingUndo_MarksUndoFailed_WithExceptionMessage()
    {
        Log.Clear();
        var entry = Log.Push("merge people", ChangeKind.PeopleMerge,
            () => Task.FromException<bool>(new InvalidOperationException("engine offline")));

        Assert.False(await Log.UndoAsync(entry));
        Assert.Equal(ChangeStatus.UndoFailed, entry.Status);
        Assert.Contains("engine offline", entry.StatusDetail);
        Log.Clear();
    }

    [Fact]
    public void RestructurePush_SupersedesOlderRestructureEntries()
    {
        Log.Clear();
        var first = Log.Push("reorganize 10 files", ChangeKind.Restructure, () => Task.FromResult(true));
        var rename = Log.Push("rename 1 file", ChangeKind.Rename, () => Task.FromResult(true));
        var second = Log.Push("reorganize 4 files", ChangeKind.Restructure, () => Task.FromResult(true));

        Assert.Equal(ChangeStatus.NotUndoable, first.Status);
        Assert.Contains("Superseded", first.StatusDetail);
        Assert.Equal(ChangeStatus.Undoable, rename.Status);   // untouched
        Assert.Equal(ChangeStatus.Undoable, second.Status);
        Log.Clear();
    }

    [Fact]
    public void CapacityBound_DropsOldestHistory()
    {
        Log.Clear();
        for (var i = 0; i < 520; i++)
        {
            Log.Push($"op {i}", ChangeKind.Other, () => Task.FromResult(true));
        }
        var snapshot = Log.Snapshot();
        Assert.Equal(500, snapshot.Count);
        Assert.Equal("op 519", snapshot[0].Label);           // newest kept
        Assert.DoesNotContain(snapshot, e => e.Label == "op 0"); // oldest dropped
        Log.Clear();
    }

    [Fact]
    public async Task ConcurrentUndo_OnSameEntry_RunsReverseExactlyOnce()
    {
        Log.Clear();
        var runs = 0;
        var entry = Log.Push("trash 1 file", ChangeKind.Trash, async () =>
        {
            System.Threading.Interlocked.Increment(ref runs);
            await Task.Delay(50);
            return true;
        });

        var results = await Task.WhenAll(Log.UndoAsync(entry), Log.UndoAsync(entry));

        Assert.Equal(1, runs);
        Assert.Single(results.Where(r => r));
        Assert.Equal(ChangeStatus.Undone, entry.Status);
        Log.Clear();
    }

    [Fact]
    public async Task UndoStackFacade_ReflectsChangeLog_AndSkipsNonUndoable()
    {
        Log.Clear();
        UndoStack.Instance.Push("op-a", ChangeKind.Rename, () => Task.FromResult(true));
        var b = Log.Push("op-b", ChangeKind.Trash, () => Task.FromResult(true));
        Assert.True(UndoStack.Instance.CanUndo);
        Assert.Equal("op-b", UndoStack.Instance.TopLabel);

        // Undo b directly through the log; the facade's "top" moves to a.
        Assert.True(await Log.UndoAsync(b));
        Assert.Equal("op-a", UndoStack.Instance.TopLabel);

        Assert.Equal("op-a", await UndoStack.Instance.UndoAsync());
        Assert.False(UndoStack.Instance.CanUndo);
        Assert.Equal(2, Log.Snapshot().Count); // both retained as history
        Log.Clear();
    }
}
