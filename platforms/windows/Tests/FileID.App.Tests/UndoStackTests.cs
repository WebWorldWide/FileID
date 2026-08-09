using System;
using System.Threading.Tasks;
using FileID.Services;
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
    public async Task Capacity_DropsOldestEntriesPast16()
    {
        await DrainAsync();
        // Push 20; only the most recent 16 should remain.
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
        Assert.Equal(16, count);
    }
}
