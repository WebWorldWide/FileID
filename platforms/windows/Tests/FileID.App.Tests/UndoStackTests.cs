using System;
using System.ComponentModel;
using System.Threading.Tasks;
using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public class UndoStackTests
{
    // UndoStack is a singleton (UndoStack.Instance). To keep test isolation
    // we drain it at the start of each test by undoing whatever's queued.
    private static async Task DrainAsync()
    {
        while (UndoStack.Instance.CanUndo)
        {
            await UndoStack.Instance.UndoAsync();
        }
    }

    [Fact]
    public async Task Push_IncreasesCanUndo()
    {
        await DrainAsync();
        Assert.False(UndoStack.Instance.CanUndo);
        UndoStack.Instance.Push("Trash 1 file", () => Task.FromResult(true));
        Assert.True(UndoStack.Instance.CanUndo);
        await DrainAsync();
    }

    [Fact]
    public async Task TopLabel_MatchesMostRecentPush()
    {
        await DrainAsync();
        UndoStack.Instance.Push("op-a", () => Task.FromResult(true));
        UndoStack.Instance.Push("op-b", () => Task.FromResult(true));
        Assert.Equal("op-b", UndoStack.Instance.TopLabel);
        await DrainAsync();
    }

    [Fact]
    public async Task UndoAsync_InvokesReverseAndPops()
    {
        await DrainAsync();
        bool reverseCalled = false;
        UndoStack.Instance.Push("op-x", () =>
        {
            reverseCalled = true;
            return Task.FromResult(true);
        });

        var label = await UndoStack.Instance.UndoAsync();
        Assert.Equal("op-x", label);
        Assert.True(reverseCalled);
        Assert.False(UndoStack.Instance.CanUndo);
    }

    [Fact]
    public async Task UndoAsync_ReturnsNullWhenEmpty()
    {
        await DrainAsync();
        var label = await UndoStack.Instance.UndoAsync();
        Assert.Null(label);
    }

    [Fact]
    public async Task Capacity_KeepsEveryEntryUndoableUpToTheLogCap()
    {
        await DrainAsync();
        ChangeLog.Instance.Clear();
        // The old 16-entry stack silently dropped older undoables; the
        // session change log keeps all of them (cap 500 — pinned in
        // ChangeLogTests.CapacityBound_DropsOldestHistory).
        for (int i = 0; i < 20; i++)
        {
            int captured = i;
            UndoStack.Instance.Push($"op-{captured}", () => Task.FromResult(true));
        }
        int count = 0;
        while (UndoStack.Instance.CanUndo)
        {
            await UndoStack.Instance.UndoAsync();
            count++;
        }
        Assert.Equal(20, count);
        ChangeLog.Instance.Clear();
    }

    [Fact]
    public void BulkCapture_RejectsFailedOrMalformedResults()
    {
        Assert.Null(UndoStack.GetUndoableBatchId(Result("trashFiles:failed", succeeded: 0, failed: 2)));
        Assert.Null(UndoStack.GetUndoableBatchId(Result("trashFiles:", succeeded: 1, failed: 0)));
        Assert.Null(UndoStack.GetUndoableBatchId(Result("trashFiles", succeeded: 1, failed: 0)));
    }

    [Fact]
    public void BulkCapture_AcceptsConfirmedFullAndPartialResults()
    {
        Assert.Equal("full", UndoStack.GetUndoableBatchId(Result("trashFiles:full", succeeded: 2, failed: 0)));
        Assert.Equal("partial", UndoStack.GetUndoableBatchId(Result("trashFiles: partial ", succeeded: 1, failed: 1)));
    }

    [Theory]
    [InlineData(1u, 0u, true)]
    [InlineData(3u, 1u, false)]
    [InlineData(0u, 1u, false)]
    [InlineData(0u, 0u, false)]
    public void RestoreRequiresAtLeastOneSuccessAndNoFailures(uint succeeded, uint failed, bool expected)
    {
        Assert.Equal(expected, EngineClient.IsSuccessfulRestoreResult(
            Result("restoreFromTrash", succeeded, failed)));
    }

    [Fact]
    public void ThrowingPropertySubscriber_DoesNotStarveLaterSubscribers()
    {
        ChangeLog.Instance.Clear();
        var observed = 0;
        PropertyChangedEventHandler throwing = (_, _) => throw new InvalidOperationException("broken undo pill");
        PropertyChangedEventHandler observing = (_, _) => observed++;
        UndoStack.Instance.PropertyChanged += throwing;
        UndoStack.Instance.PropertyChanged += observing;

        try
        {
            UndoStack.Instance.Push("rename one file", () => Task.FromResult(true));
            Assert.True(observed >= 2);
        }
        finally
        {
            UndoStack.Instance.PropertyChanged -= throwing;
            UndoStack.Instance.PropertyChanged -= observing;
            ChangeLog.Instance.Clear();
        }
    }

    private static BulkActionResult Result(string action, uint succeeded, uint failed)
        => new(action, succeeded, failed, Array.Empty<BulkActionItem>());
}
