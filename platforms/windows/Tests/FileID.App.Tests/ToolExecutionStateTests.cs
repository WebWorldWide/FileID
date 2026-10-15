using FileID.IpcSchema;
using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class ToolExecutionStateTests
{
    private static readonly string[] Mp4Formats = ["mp4"];

    [Fact]
    public async Task CancelTargetsExactOperationAndEngineGenerationUntilExecutionFinal()
    {
        var state = new ToolExecutionState();
        var pending = state.Begin("execute", "preview-operation", 3);
        Assert.Equal("preview-operation", state.RequireCancellationOwner(3));
        Assert.Throws<InvalidOperationException>(() => state.RequireCancellationOwner(4));
        await Assert.ThrowsAsync<InvalidOperationException>(() => state.Begin("other", "new-preview", 3));
        Assert.False(state.ApplyTerminal(Response("execute", "other"), 3));
        Assert.False(state.ApplyTerminal(Response("execute", "preview-operation"), 4));
        Assert.True(state.IsActive);
        var final = Response("execute", "preview-operation");
        Assert.True(state.ApplyTerminal(final, 3));
        Assert.Same(final, await pending);
        Assert.False(state.IsActive);
        Assert.Throws<InvalidOperationException>(() => state.RequireCancellationOwner(3));
    }

    [Theory]
    [InlineData(true)]
    [InlineData(false)]
    public async Task TimeoutOrUnloadedPanelLeavesBoundedClientOwnershipForLateTerminal(bool timeout)
    {
        var state = new ToolExecutionState();
        var pending = state.Begin("execute", "operation", 3);
        if (timeout)
        {
            await Assert.ThrowsAsync<TimeoutException>(() => pending.WaitAsync(TimeSpan.Zero));
        }
        else
        {
            using var lifetime = new CancellationTokenSource();
            lifetime.Cancel();
            await Assert.ThrowsAnyAsync<OperationCanceledException>(() => pending.WaitAsync(lifetime.Token));
        }
        Assert.True(state.IsActive);
        Assert.False(pending.IsCompleted);
        Assert.Null(state.LastResponse);
        var final = Response("execute", "operation");
        Assert.True(state.ApplyTerminal(final, 3));
        Assert.Same(final, await pending);
        Assert.Same(final, state.LastResponse);
        Assert.False(state.IsActive);
    }

    [Fact]
    public async Task StaleDuplicateAndMismatchedTerminalsCannotReleaseANewPreview()
    {
        var state = new ToolExecutionState();
        var first = state.Begin("old-request", "old-operation", 3);
        var old = Response("old-request", "old-operation");
        Assert.True(state.ApplyTerminal(old, 3));
        await first;
        Assert.False(state.ApplyTerminal(old, 3));
        var pending = state.Begin("new-request", "new-operation", 4);
        Assert.False(state.ApplyTerminal(old, 3));
        Assert.False(state.ApplyTerminal(Response("old-request", "new-operation"), 4));
        Assert.False(state.ApplyTerminal(Response("new-request", "old-operation"), 4));
        Assert.False(state.ApplyTerminal(Response("new-request", "new-operation", "running"), 4));
        Assert.True(state.IsActive);
        Assert.True(state.ApplyTerminal(Response("new-request", "new-operation"), 4));
        await pending;
    }

    [Fact]
    public async Task ObservedGenerationRetirementIsUnknownUntilMatchingDurableRecovery()
    {
        var state = new ToolExecutionState();
        var old = state.Begin("old-request", "old-operation", 3);
        Assert.False(state.InvalidateGeneration(2));
        Assert.True(state.InvalidateGeneration(3));
        await Assert.ThrowsAsync<InvalidOperationException>(() => old);
        Assert.True(state.NeedsRecovery);
        Assert.Null(state.LastResponse);
        Assert.False(state.ApplyTerminal(Response("old-request", "old-operation"), 3));
        await Assert.ThrowsAsync<InvalidOperationException>(() => state.Begin("new-request", "new-operation", 4));
        Assert.False(state.ReconcileHistory(Response("history", "other")));
        Assert.False(state.ReconcileHistory(Response("history", "old-operation", "error")));
        Assert.False(state.ReconcileHistory(Response("history", "old-operation") with
        {
            Outputs = new[] { new ToolOutput(1, "source", "output", "pending", "Unconfirmed") }
        }));
        Assert.True(state.ReconcileHistory(Response("history", "old-operation")));
        Assert.False(state.NeedsRecovery);
        var next = state.Begin("new-request", "new-operation", 4);
        Assert.False(state.ApplyTerminal(Response("old-request", "old-operation"), 3));
        Assert.True(state.ApplyTerminal(Response("new-request", "new-operation"), 4));
        await next;
    }

    [Theory]
    [InlineData(true)]
    [InlineData(false)]
    public async Task CancelAcknowledgementNeverCompletesExecuteInEitherTerminalOrder(bool acknowledgementFirst)
    {
        using var requests = new WorkbenchRequests();
        var state = new ToolExecutionState();
        var execute = state.Begin("execute", "operation", 3);
        state.BeginCancellation(3);
        Assert.StartsWith("Stopping", state.Label, StringComparison.Ordinal);
        var cancel = requests.Register<ToolResponse>("cancel", 3);
        var ack = Response("cancel", "operation");
        var final = new ToolResponse("execute", "error", "Cancelled; completed outputs remain recoverable", new[]
        {
            new ToolOutput(1, "source", "output", "completed", "Original preserved")
        }, Array.Empty<ToolCapability>(), "operation");
        if (acknowledgementFirst)
        {
            Assert.True(requests.Apply("cancel", 3, ack));
            Assert.True(state.ApplyCancellationAck("operation", 3, await cancel));
            Assert.False(state.ApplyTerminal(ack, 3));
            Assert.False(execute.IsCompleted);
            Assert.False(state.ReconcileHistory(Response("history", "operation")));
            Assert.True(state.IsActive);
        }
        Assert.True(state.ApplyTerminal(final, 3));
        var result = await execute;
        Assert.Equal("error", result.Status);
        Assert.Contains("recoverable", result.Message, StringComparison.Ordinal);
        var plan = new ToolPreviewState();
        plan.Apply("execute", result);
        Assert.Equal("operation", plan.UndoID);
        Assert.False(state.IsActive);
        if (!acknowledgementFirst)
        {
            var next = state.Begin("new-execute", "new-preview", 3);
            Assert.True(requests.Apply("cancel", 3, ack));
            Assert.False(state.ApplyCancellationAck("operation", 3, await cancel));
            Assert.Equal("new-preview", state.OperationID);
            Assert.StartsWith("Exporting", state.Label, StringComparison.Ordinal);
            Assert.True(state.ApplyTerminal(Response("new-execute", "new-preview"), 3));
            await next;
        }
    }

    [Fact]
    public async Task RejectedStopRestoresExportingWithoutReleasingExecuteOwnership()
    {
        var state = new ToolExecutionState();
        var execute = state.Begin("execute", "operation", 3);
        state.BeginCancellation(3);
        Assert.True(state.ApplyCancellationAck("operation", 3, Response("cancel", "operation", "error")));
        Assert.True(state.IsActive);
        Assert.False(state.IsStopping);
        Assert.StartsWith("Exporting", state.Label, StringComparison.Ordinal);
        Assert.True(state.ApplyTerminal(Response("execute", "operation"), 3));
        await execute;
    }

    [Theory]
    [InlineData("cancel", true, true)]
    [InlineData("cancel", false, false)]
    [InlineData("video", true, false)]
    [InlineData("videoEnhancement", true, false)]
    public void CancellationRequiresItsOwnAvailableCapability(string id, bool available, bool supported)
    {
        var capability = new ToolCapability(id, available, Array.Empty<string>(), Array.Empty<string>(), "Interruptible adapter");
        Assert.Equal(supported, ToolExecutionState.SupportsCancellation(new[] { capability }));
        Assert.False(ToolPreviewState.Supports(capability, "mp4", 4096));
    }

    [Fact]
    public void CancellationCapabilityCannotMasqueradeAsAnExportRecipe()
    {
        var capability = new ToolCapability("cancel", true, Array.Empty<string>(), Mp4Formats, "Invalid cancel capability");
        Assert.False(ToolExecutionState.SupportsCancellation(new[] { capability }));
        Assert.False(ToolPreviewState.Supports(capability, "mp4", 4096));
    }

    [Theory]
    [InlineData("video", true, "mp4", true)]
    [InlineData("video", false, "mp4", false)]
    [InlineData("video", true, "webm", false)]
    [InlineData("videoEnhancement", true, "mp4", false)]
    public void VideoRecipesRequireAvailableCanonicalCapabilityAndReportedFormat(string id, bool available, string format, bool supported)
    {
        var capability = new ToolCapability(id, available, Mp4Formats, Mp4Formats, "Native conversion");
        Assert.Equal(supported, ToolPreviewState.Supports(capability, format, 4096));
    }

    private static ToolResponse Response(string requestID, string operationID, string status = "ok") =>
        new(requestID, status, "Engine result", Array.Empty<ToolOutput>(), Array.Empty<ToolCapability>(), operationID);
}
