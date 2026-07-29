// ChangeLog — the session change log behind UndoStack and the
// SessionChangesSheet. Pins the log's contract: entries survive undo
// (as Undone), survive failure (as UndoFailed + retry), restructure
// pushes supersede older restructure entries, and reverse capacity
// expires closures without dropping session history.

using System;
using System.ComponentModel;
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
    public async Task RestructurePush_SupersedesFailedRestructureUndo()
    {
        Log.Clear();
        var failed = Log.Push(
            "reorganize 10 files",
            ChangeKind.Restructure,
            () => Task.FromResult(false));
        Assert.False(await Log.UndoAsync(failed));
        Assert.Equal(ChangeStatus.UndoFailed, failed.Status);

        var current = Log.Push(
            "reorganize 4 files",
            ChangeKind.Restructure,
            () => Task.FromResult(true));

        Assert.Equal(ChangeStatus.NotUndoable, failed.Status);
        Assert.Contains("Superseded", failed.StatusDetail);
        Assert.Equal(ChangeStatus.Undoable, current.Status);
        Log.Clear();
    }

    [Fact]
    public async Task NonUndoableRestructureRecord_PreservesRealMoveUndoState()
    {
        Log.Clear();
        var realMove = Log.Push(
            "reorganize 10 files",
            ChangeKind.Restructure,
            () => Task.FromResult(false));
        var shortcut = Log.RecordNotUndoable(
            "create 3 restructure shortcuts",
            ChangeKind.Restructure,
            "Shortcuts leave originals in place; remove links manually.");

        Assert.Equal(ChangeStatus.Undoable, realMove.Status);
        Assert.Equal(ChangeStatus.NotUndoable, shortcut.Status);
        Assert.Equal(1, Log.PendingCount);

        Assert.False(await Log.UndoAsync(realMove));
        var secondShortcut = Log.RecordNotUndoable(
            "create 2 restructure shortcuts",
            ChangeKind.Restructure,
            "Shortcuts leave originals in place; remove links manually.");

        Assert.Equal(ChangeStatus.UndoFailed, realMove.Status);
        Assert.Equal(ChangeStatus.NotUndoable, secondShortcut.Status);
        Assert.Equal(1, Log.PendingCount);
        Assert.Equal(3, Log.Count);
        Log.Clear();
    }

    [Fact]
    public async Task TokenizedShortcutUndo_NeverAliasesRealMoveRestructureJournal()
    {
        Log.Clear();
        var failedRealMove = Log.Push(
            "reorganize 10 files",
            ChangeKind.Restructure,
            () => Task.FromResult(false));
        Assert.False(await Log.UndoAsync(failedRealMove));

        var shortcuts = Log.Push(
            "create 3 restructure shortcuts",
            ChangeKind.RestructureShortcuts,
            () => Task.FromResult(true));

        Assert.Equal(ChangeStatus.UndoFailed, failedRealMove.Status);
        Assert.Equal(ChangeStatus.Undoable, shortcuts.Status);
        Assert.Equal(2, Log.PendingCount);

        var currentRealMove = Log.Push(
            "reorganize 4 files",
            ChangeKind.Restructure,
            () => Task.FromResult(true));

        Assert.Equal(ChangeStatus.NotUndoable, failedRealMove.Status);
        Assert.Equal(ChangeStatus.Undoable, shortcuts.Status);
        Assert.Equal(ChangeStatus.Undoable, currentRealMove.Status);
        Assert.Equal(2, Log.PendingCount);
        Log.Clear();
    }

    [Fact]
    public void ReverseCapacity_ExpiresClosuresWithoutDroppingHistory()
    {
        Log.Clear();
        for (var i = 0; i < 520; i++)
        {
            Log.Push($"op {i}", ChangeKind.Other, () => Task.FromResult(true));
        }
        var snapshot = Log.Snapshot();
        Assert.Equal(520, snapshot.Count);
        Assert.Equal("op 519", snapshot[0].Label);
        Assert.Equal("op 0", snapshot[^1].Label);
        Assert.Equal(20, snapshot.Count(entry => entry.Status == ChangeStatus.NotUndoable));
        Assert.Equal(500, Log.UndoableCount);
        Assert.Equal(500, Log.PendingCount);
        Log.Clear();
    }

    [Fact]
    public async Task Undo_IsGloballySingleFlight_AndPendingUntilTerminal()
    {
        Log.Clear();
        var started = new TaskCompletionSource<bool>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        var release = new TaskCompletionSource<bool>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        var older = Log.Push("older rename", ChangeKind.Rename, () => Task.FromResult(true));
        var current = Log.Push("current rename", ChangeKind.Rename, async () =>
        {
            started.TrySetResult(true);
            await release.Task;
            return true;
        });

        var undo = Log.UndoAsync(current);
        await started.Task;

        Assert.Equal(ChangeStatus.Undoing, current.Status);
        Assert.True(Log.IsUndoInFlight);
        Assert.Equal(2, Log.PendingCount);
        Assert.False(UndoStack.Instance.CanUndo);
        Assert.False(await Log.UndoAsync(older));

        release.TrySetResult(true);
        Assert.True(await undo);
        Assert.Equal(ChangeStatus.Undone, current.Status);
        Assert.False(Log.IsUndoInFlight);
        Assert.Equal(1, Log.PendingCount);
        Assert.True(UndoStack.Instance.CanUndo);
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
        Assert.Single(results, r => r);
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

    [Fact]
    public async Task ThrowingSubscribers_CannotReclassifyACompletedUndoOrStarveLaterSubscribers()
    {
        Log.Clear();
        var reverseRuns = 0;
        var changedNotifications = 0;
        var propertyNotifications = 0;
        EventHandler throwingChanged = (_, _) => throw new InvalidOperationException("broken view");
        EventHandler observingChanged = (_, _) => changedNotifications++;
        PropertyChangedEventHandler throwingProperty = (_, _) => throw new InvalidOperationException("broken binding");
        PropertyChangedEventHandler observingProperty = (_, _) => propertyNotifications++;
        Log.Changed += throwingChanged;
        Log.Changed += observingChanged;
        Log.PropertyChanged += throwingProperty;
        Log.PropertyChanged += observingProperty;

        try
        {
            var entry = Log.Push("rename one file", ChangeKind.Rename, () =>
            {
                reverseRuns++;
                return Task.FromResult(true);
            });
            var entryNotifications = 0;
            PropertyChangedEventHandler throwingEntry = (_, _) => throw new InvalidOperationException("broken row");
            PropertyChangedEventHandler observingEntry = (_, _) => entryNotifications++;
            entry.PropertyChanged += throwingEntry;
            entry.PropertyChanged += observingEntry;
            try
            {
                var changedAfterPush = changedNotifications;
                var propertiesAfterPush = propertyNotifications;

                Assert.True(await Log.UndoAsync(entry));

                Assert.Equal(ChangeStatus.Undone, entry.Status);
                Assert.Equal(1, reverseRuns);
                Assert.True(entryNotifications > 0);
                Assert.True(changedNotifications > changedAfterPush);
                Assert.True(propertyNotifications > propertiesAfterPush);
                Assert.False(await Log.UndoAsync(entry));
                Assert.Equal(1, reverseRuns);
            }
            finally
            {
                entry.PropertyChanged -= throwingEntry;
                entry.PropertyChanged -= observingEntry;
            }
        }
        finally
        {
            Log.Changed -= throwingChanged;
            Log.Changed -= observingChanged;
            Log.PropertyChanged -= throwingProperty;
            Log.PropertyChanged -= observingProperty;
            Log.Clear();
        }
    }
}
