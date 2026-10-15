namespace FileID.ViewModels;

internal static class CommandWriteQueue
{
    public static Task Enqueue(Task predecessor, Action write, CancellationToken ct) => Task.Run(async () =>
    {
        try { await predecessor.ConfigureAwait(false); }
        catch { }
        ct.ThrowIfCancellationRequested();
        write();
    }, CancellationToken.None);
}
