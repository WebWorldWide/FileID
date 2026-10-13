using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class WorkbenchDispatchTests
{
    [Theory]
    [InlineData(false, false)]
    [InlineData(true, false)]
    [InlineData(false, true)]
    public async Task GatedFifoUnsentOwnerReleasesOnlyAfterActualQueueResolves(bool timeout, bool retire)
    {
        var state = new ToolExecutionState();
        var completion = state.Begin("execute", "operation", 7);
        using var release = new ManualResetEventSlim();
        using var lifetime = new CancellationTokenSource();
        var entered = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var first = CommandWriteQueue.Enqueue(Task.CompletedTask, () =>
        {
            entered.SetResult();
            Assert.True(release.Wait(TimeSpan.FromSeconds(10)));
        }, CancellationToken.None);
        await entered.Task.WaitAsync(TimeSpan.FromSeconds(10));
        var writes = 0;
        var confirmed = 0;
        var dispatch = WorkbenchDispatch.ObserveAsync((started, ct) => CommandWriteQueue.Enqueue(first, () =>
        {
            started();
            Interlocked.Increment(ref writes);
        }, ct), error =>
        {
            if (state.ConfirmUnsent("execute", "operation", 7, error)) Interlocked.Increment(ref confirmed);
        }, lifetime.Token);
        try
        {
            if (timeout) await Assert.ThrowsAsync<TimeoutException>(() => dispatch.WaitAsync(TimeSpan.Zero));
            lifetime.Cancel();
            await Assert.ThrowsAnyAsync<OperationCanceledException>(() => dispatch.WaitAsync(lifetime.Token));
            Assert.Equal(0, Volatile.Read(ref confirmed));
            Assert.True(state.IsActive);
            Assert.False(completion.IsCompleted);
            if (retire)
            {
                Assert.True(state.InvalidateGeneration(7));
                Assert.True(state.NeedsRecovery);
            }
        }
        finally { release.Set(); }
        await first;
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => dispatch);
        if (retire) await Assert.ThrowsAsync<InvalidOperationException>(() => completion);
        else await Assert.ThrowsAnyAsync<OperationCanceledException>(() => completion);
        Assert.Equal(0, writes);
        Assert.Equal(1, confirmed);
        Assert.False(state.IsActive);
        Assert.False(state.NeedsRecovery);
        Assert.Null(state.LastResponse);
        var next = state.Begin("next", "next-operation", 8);
        Assert.False(state.ConfirmUnsent("execute", "operation", 7, new IOException("late old failure")));
        Assert.True(state.IsActive);
        Assert.True(state.ConfirmUnsent("next", "next-operation", 8, new OperationCanceledException()));
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => next);
    }

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task PostStartCancellationOrWriteFailureRetainsUnknownOwnership(bool cancelAwait)
    {
        var state = new ToolExecutionState();
        var completion = state.Begin("execute", "operation", 7);
        using var output = new MemoryStream();
        using var release = new ManualResetEventSlim();
        using var lifetime = new CancellationTokenSource();
        var entered = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var confirmed = 0;
        var dispatch = WorkbenchDispatch.ObserveAsync((started, ct) => CommandWriteQueue.Enqueue(Task.CompletedTask, () =>
        {
            started();
            output.WriteByte(1);
            entered.SetResult();
            Assert.True(release.Wait(TimeSpan.FromSeconds(10)));
            throw new IOException("Generated fixture: partial write/flush failure");
        }, ct), error =>
        {
            if (state.ConfirmUnsent("execute", "operation", 7, error)) Interlocked.Increment(ref confirmed);
        }, lifetime.Token);
        try
        {
            await entered.Task.WaitAsync(TimeSpan.FromSeconds(10));
            if (cancelAwait)
            {
                lifetime.Cancel();
                await Assert.ThrowsAnyAsync<OperationCanceledException>(() => dispatch.WaitAsync(lifetime.Token));
            }
            Assert.True(state.IsActive);
            Assert.False(completion.IsCompleted);
        }
        finally { release.Set(); }
        await Assert.ThrowsAsync<IOException>(() => dispatch);
        Assert.Equal(1, output.Length);
        Assert.Equal(0, confirmed);
        Assert.True(state.IsActive);
        Assert.Null(state.LastResponse);
        Assert.True(state.InvalidateGeneration(7));
        await Assert.ThrowsAsync<InvalidOperationException>(() => completion);
        Assert.True(state.NeedsRecovery);
    }
}
