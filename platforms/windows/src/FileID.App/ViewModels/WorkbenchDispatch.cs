namespace FileID.ViewModels;

internal static class WorkbenchDispatch
{
    public static Task ObserveAsync(Func<Action, CancellationToken, Task> enqueue, Action<Exception> confirmedUnsent, CancellationToken ct)
    {
        var task = ObserveCoreAsync(enqueue, confirmedUnsent, ct);
        _ = task.ContinueWith(completed => _ = completed.Exception, CancellationToken.None,
            TaskContinuationOptions.OnlyOnFaulted | TaskContinuationOptions.ExecuteSynchronously, TaskScheduler.Default);
        return task;
    }
    private static async Task ObserveCoreAsync(Func<Action, CancellationToken, Task> enqueue, Action<Exception> confirmedUnsent, CancellationToken ct)
    {
        var started = 0;
        try { await enqueue(() => Interlocked.Exchange(ref started, 1), ct).ConfigureAwait(false); }
        catch (Exception error)
        {
            if (Volatile.Read(ref started) == 0) confirmedUnsent(error);
            throw;
        }
    }
}
