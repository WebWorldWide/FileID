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
    public async Task SessionHistoryRetainsEntriesBeyondLegacyUndoStackCapacity()
    {
        await DrainAsync();
        // Session history keeps every entry; reverse callbacks are capped separately.
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
    }

    private static readonly string[] LibraryOnly = { "library" };
    private static readonly string[] LibraryThenCleanup = { "library", "cleanup" };

    [Fact]
    public async Task EachPageUndoesItsOwnNewestChange()
    {
        await DrainAsync();
        var undone = new System.Collections.Generic.List<string>();
        UndoStack.Instance.Push("library trash", ChangePage.Library, () => { undone.Add("library"); return Task.FromResult(true); });
        UndoStack.Instance.Push("cleanup trash", ChangePage.Cleanup, () => { undone.Add("cleanup"); return Task.FromResult(true); });
        UndoStack.Instance.Push("people merge", ChangePage.People, () => { undone.Add("people"); return Task.FromResult(true); });

        Assert.Equal("library trash", UndoStack.Instance.TopLabelOn(ChangePage.Library));
        Assert.Equal("people merge", UndoStack.Instance.TopLabel);

        Assert.Equal("library trash", await UndoStack.Instance.UndoAsync(ChangePage.Library));
        Assert.Equal(LibraryOnly, undone);
        Assert.False(UndoStack.Instance.CanUndoOn(ChangePage.Library));
        Assert.True(UndoStack.Instance.CanUndoOn(ChangePage.Cleanup));
        Assert.True(UndoStack.Instance.CanUndoOn(ChangePage.People));
        Assert.Null(await UndoStack.Instance.UndoAsync(ChangePage.Library));

        Assert.Equal("cleanup trash", await UndoStack.Instance.UndoAsync(ChangePage.Cleanup));
        Assert.Equal(LibraryThenCleanup, undone);
        Assert.Equal("people merge", UndoStack.Instance.TopLabel);
        await DrainAsync();
    }

    [Fact]
    public async Task AChangeWithNoPageIsLeftToTheRecentChangesList()
    {
        await DrainAsync();
        UndoStack.Instance.Push("folder move", () => Task.FromResult(true));

        Assert.True(UndoStack.Instance.CanUndo);
        Assert.False(UndoStack.Instance.CanUndoOn(ChangePage.Library));
        Assert.False(UndoStack.Instance.CanUndoOn(ChangePage.Cleanup));
        Assert.Equal(string.Empty, UndoStack.Instance.TopLabelOn(ChangePage.Cleanup));
        await DrainAsync();
    }

    [Theory]
    [InlineData("library", nameof(ChangePage.Library))]
    [InlineData("cleanup", nameof(ChangePage.Cleanup))]
    [InlineData("people", nameof(ChangePage.People))]
    [InlineData("deepanalyze", nameof(ChangePage.DeepAnalyze))]
    public void CtrlZActsOnTheShowingPage(string tab, string page)
    {
        Assert.Equal(page, UndoStack.PageForTab(tab)?.ToString());
    }

    [Theory]
    [InlineData("settings")]
    [InlineData("restructure")]
    [InlineData("chat")]
    [InlineData(null)]
    public void CtrlZDoesNothingOnPagesWithNoChangesOfTheirOwn(string? tab)
    {
        Assert.Null(UndoStack.PageForTab(tab));
    }
}
