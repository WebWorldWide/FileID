using FileID.IpcSchema;
using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class ToolPreviewStateTests
{
    private static readonly string[] PngFormats = { "png" };
    [Fact]
    public void InputChangeInvalidatesPreviewButPreservesIndependentUndoReceipt()
    {
        var state = new ToolPreviewState();
        state.Apply("history", Response("old", "ok", "completed"));
        state.Apply("preview", Response("preview", "ok", "pending"));
        Assert.True(state.CanExecute);
        state.Invalidate();
        Assert.False(state.CanExecute);
        Assert.Equal("old", state.UndoID);
    }

    [Fact]
    public void ExecuteAttemptCannotBeSentTwiceEvenIfFirstAttemptTimesOut()
    {
        var state = new ToolPreviewState();
        state.Apply("preview", Response("preview", "ok", "pending"));
        state.BeginExecute();
        Assert.False(state.CanExecute);
        Assert.Throws<InvalidOperationException>(state.BeginExecute);
        state.Apply("execute", Response("preview", "error", "completed"));
        Assert.False(state.CanExecute);
        Assert.Equal("preview", state.UndoID);
        state.Apply("preview", Response("fresh", "ok", "pending"));
        Assert.True(state.CanExecute);
    }

    [Fact]
    public void OnlyRealCompletedOutputsEnableUndoAndFailedUndoPreservesReceipt()
    {
        var state = new ToolPreviewState();
        state.Apply("history", Response("pending", "ok", "pending"));
        Assert.Null(state.UndoID);
        state.Apply("history", Response("partial", "ok", "completed"));
        state.Apply("undo", new ToolResponse("r", "error", "Export was edited", Array.Empty<ToolOutput>(), Array.Empty<ToolCapability>()));
        Assert.Equal("partial", state.UndoID);
        state.Apply("undo", Response("partial", "ok", "undone"));
        Assert.Null(state.UndoID);
    }

    [Fact]
    public void MissingOrUnavailableCapabilityCannotPreview()
    {
        var available = new ToolCapability("photo", true, PngFormats, PngFormats, "actual backend details");
        Assert.True(ToolPreviewState.Supports(available, "png", 4096));
        Assert.False(ToolPreviewState.Supports(null, "png", 4096));
        Assert.False(ToolPreviewState.Supports(available with { Available = false }, "png", 4096));
        Assert.False(ToolPreviewState.Supports(available, "mp4", 4096));
        Assert.False(ToolPreviewState.Supports(available, "png", 0));
        Assert.False(ToolPreviewState.Supports(available, "png", 8193));
    }

    [Fact]
    public void FailedPreviewAndHistoryCannotTurnIntoAnExecutePlan()
    {
        var state = new ToolPreviewState();
        state.Apply("preview", Response("bad", "error", "pending"));
        Assert.False(state.CanExecute);
        state.Apply("history", Response("receipt", "ok", "completed"));
        Assert.False(state.CanExecute);
        Assert.Equal("receipt", state.UndoID);
    }

    private static ToolResponse Response(string id, string status, string outputState) =>
        new("r", status, "", new[] { new ToolOutput(1, "source", "output", outputState, "receipt") }, Array.Empty<ToolCapability>(), id);
}
