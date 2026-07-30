using FileID.ViewModels;
using System.Diagnostics;
using Xunit;

namespace FileID.App.Tests;

public sealed class EngineHealthWaiterTests
{
    [Fact]
    public async Task ExactNoncePidAndGenerationAreRequired()
    {
        var waiters = new GenerationHealthWaiters();
        var waiter = waiters.Register("nonce-a", generation: 7, pid: 42);

        Assert.False(waiters.TryResolve("nonce-b", pid: 42, generation: 7));
        Assert.False(waiters.TryResolve("nonce-a", pid: 43, generation: 7));
        Assert.False(waiters.TryResolve("nonce-a", pid: 42, generation: 8));
        Assert.Equal(1, waiters.Count);
        Assert.False(waiter.Task.IsCompleted);

        Assert.True(waiters.TryResolve("nonce-a", pid: 42, generation: 7));
        await waiter.Task;
        Assert.Equal(0, waiters.Count);
    }

    [Fact]
    public async Task ReplyBeforeAwaitIsRetained()
    {
        var waiters = new GenerationHealthWaiters();
        var waiter = waiters.Register("early", generation: 3, pid: 99);

        Assert.True(waiters.TryResolve("early", pid: 99, generation: 3));

        await waiter.Task;
        Assert.True(waiter.Task.IsCompletedSuccessfully);
        Assert.Equal(0, waiters.Count);
    }

    [Fact]
    public async Task ConcurrentWaitersResolveIndependently()
    {
        var waiters = new GenerationHealthWaiters();
        var first = waiters.Register("first", generation: 4, pid: 100);
        var second = waiters.Register("second", generation: 4, pid: 100);

        Assert.True(waiters.TryResolve("second", pid: 100, generation: 4));
        Assert.False(first.Task.IsCompleted);
        await second.Task;
        Assert.Equal(1, waiters.Count);

        Assert.True(waiters.TryResolve("first", pid: 100, generation: 4));
        await first.Task;
        Assert.Equal(0, waiters.Count);
    }

    [Fact]
    public async Task SendTimeoutAndCancellationFailuresRemoveTheirWaiters()
    {
        var waiters = new GenerationHealthWaiters();
        var send = waiters.Register("send", generation: 1, pid: 5);
        var timeout = waiters.Register("timeout", generation: 1, pid: 5);
        var cancellation = waiters.Register("cancel", generation: 1, pid: 5);

        Assert.True(waiters.TryFail("send", new IOException("write failed")));
        Assert.True(waiters.TryFail("timeout", new TimeoutException("late")));
        Assert.True(waiters.TryFail(
            "cancel",
            new OperationCanceledException("cancelled")));

        await Assert.ThrowsAsync<IOException>(() => send.Task);
        await Assert.ThrowsAsync<TimeoutException>(() => timeout.Task);
        await Assert.ThrowsAnyAsync<OperationCanceledException>(
            () => cancellation.Task);
        Assert.Equal(0, waiters.Count);
    }

    [Fact]
    public async Task CleanupRetiresOnlyTheOldGeneration()
    {
        var waiters = new GenerationHealthWaiters();
        var oldFirst = waiters.Register("old-1", generation: 8, pid: 101);
        var oldSecond = waiters.Register("old-2", generation: 8, pid: 101);
        var current = waiters.Register("current", generation: 9, pid: 202);
        var stopped = new InvalidOperationException("stopped");

        Assert.Equal(2, waiters.FailGeneration(8, stopped));
        await Assert.ThrowsAsync<InvalidOperationException>(() => oldFirst.Task);
        await Assert.ThrowsAsync<InvalidOperationException>(() => oldSecond.Task);
        Assert.False(current.Task.IsCompleted);
        Assert.Equal(1, waiters.Count);

        Assert.True(waiters.TryResolve("current", pid: 202, generation: 9));
        await current.Task;
    }

    [Fact]
    public void HealthTargetRejectsProcessOrGenerationReplacement()
    {
        using var captured = new Process();
        using var replacement = new Process();

        Assert.True(EngineClient.IsHealthTargetCurrent(
            4, 4, captured, captured, capturedHasExited: false));
        Assert.False(EngineClient.IsHealthTargetCurrent(
            4, 5, captured, captured, capturedHasExited: false));
        Assert.False(EngineClient.IsHealthTargetCurrent(
            4, 4, captured, replacement, capturedHasExited: false));
        Assert.False(EngineClient.IsHealthTargetCurrent(
            4, 4, captured, captured, capturedHasExited: true));
    }

    [Fact]
    public void EngineClientRegistersBeforeWriteAndRetiresBeforeGenerationChange()
    {
        var root = FindRepoRoot();
        var client = File.ReadAllText(Path.Combine(
            root,
            "platforms",
            "windows",
            "src",
            "FileID.App",
            "ViewModels",
            "EngineClient.cs"));

        var probe = client.IndexOf(
            "private async Task ProbeCommandChannelAsync",
            StringComparison.Ordinal);
        var register = client.IndexOf(
            "_healthWaiters.Register(requestId, generation, pid)",
            probe,
            StringComparison.Ordinal);
        var send = client.IndexOf(
            "SendCommandAsync(new HealthCheckCommand(requestId), ct)",
            register,
            StringComparison.Ordinal);
        var awaitReply = client.IndexOf(
            "waiter.Task.WaitAsync(timeout, ct)",
            send,
            StringComparison.Ordinal);
        Assert.True(
            probe >= 0 && register > probe && send > register && awaitReply > send,
            "Health waiters must be installed before write and timed only after flush.");

        var cleanup = client.IndexOf("private void Cleanup()", StringComparison.Ordinal);
        var retire = client.IndexOf(
            "_healthWaiters.FailGeneration(",
            cleanup,
            StringComparison.Ordinal);
        var generationChange = client.IndexOf(
            "Interlocked.Increment(ref _spawnGeneration)",
            retire,
            StringComparison.Ordinal);
        Assert.True(
            cleanup >= 0 && retire > cleanup && generationChange > retire,
            "Cleanup must fault old health waiters before publishing a new generation.");

        Assert.Contains("case HealthCheckResultEvent:", client, StringComparison.Ordinal);
        Assert.Contains("health.Result.RequestId", client, StringComparison.Ordinal);
        Assert.Contains("health.Result.Pid", client, StringComparison.Ordinal);
        var decode = client.IndexOf(
            "ev = IpcCoder.Decode<IpcEvent>(line)",
            StringComparison.Ordinal);
        var resolve = client.IndexOf(
            "_healthWaiters.TryResolve(",
            decode,
            StringComparison.Ordinal);
        var dispatch = client.IndexOf(
            "_ui.TryEnqueue(() => Apply(ev, generation))",
            resolve,
            StringComparison.Ordinal);
        Assert.True(
            decode >= 0 && resolve > decode && dispatch > resolve,
            "Health replies must resolve on the stdout loop before UI dispatch.");
        Assert.Contains("HandleTransportFailureAsync(", client, StringComparison.Ordinal);
        Assert.Contains("\"stdout EOF\"", client, StringComparison.Ordinal);
    }

    private static string FindRepoRoot()
    {
        for (var directory = new DirectoryInfo(AppContext.BaseDirectory);
             directory is not null;
             directory = directory.Parent)
        {
            if (File.Exists(Path.Combine(directory.FullName, "AGENTS.md"))
                && Directory.Exists(Path.Combine(
                    directory.FullName,
                    "platforms",
                    "windows")))
            {
                return directory.FullName;
            }
        }
        throw new DirectoryNotFoundException(
            "Could not find the FileID repository root.");
    }
}
