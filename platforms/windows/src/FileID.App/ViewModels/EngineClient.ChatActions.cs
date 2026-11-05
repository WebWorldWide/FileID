using System.ComponentModel;
using FileID.IpcSchema;
using FileID.Services;

namespace FileID.ViewModels;

internal sealed partial class EngineClient
{
    private static readonly HashSet<string> PlanFailureKinds = new(StringComparer.Ordinal)
    {
        "restructure_busy", "plan_restructure_failed", "db_unavailable",
    };

    private static readonly HashSet<string> ApplyFailureKinds = new(StringComparer.Ordinal)
    {
        "restructure_busy", "apply_restructure", "db_unavailable",
    };

    /// <summary>Describes up to 100 chosen files (Chat's describeFiles with a selection).</summary>
    public async Task DescribeFilesAsync(IReadOnlyList<long> fileIDs, string modelKind)
    {
        if (fileIDs.Count == 0) throw new ArgumentException("Choose at least one file to describe.", nameof(fileIDs));
        var owner = TryReserveDeepAnalyzeCommand();
        try
        {
            await SendCommandAsync(new DeepAnalyzeAllCommand(modelKind, SkipExisting: false, FileIDs: fileIDs.Take(ChatActionPolicy.MaxDescribeSelection).ToArray()))
                .ConfigureAwait(false);
        }
        catch
        {
            ReleaseDeepAnalyzeOwner(owner);
            throw;
        }
    }

    public Task<RestructurePlan> PlanRestructureAndWaitAsync(string libraryRoot, CancellationToken ct = default) =>
        WaitForRestructureReplyAsync<RestructurePlan>(
            () => PlanRestructureAsync(libraryRoot),
            ev => ev is RestructurePlanEvent plan ? plan.Plan : null,
            PlanFailureKinds,
            TimeSpan.FromMinutes(30),
            ct);

    public Task<RestructureApplyResult> ApplyRestructureAndWaitAsync(string libraryRoot, IReadOnlyList<RestructureMove> moves, CancellationToken ct = default) =>
        WaitForRestructureReplyAsync<RestructureApplyResult>(
            () => ApplyRestructureAsync(libraryRoot, moves, useSymlinks: false),
            ev => ev is RestructureApplyResultEvent result ? result.Result : null,
            ApplyFailureKinds,
            TimeSpan.FromHours(2),
            ct);

    private async Task<T> WaitForRestructureReplyAsync<T>(
        Func<Task> send, Func<EventPayload?, T?> reply, IReadOnlySet<string> failures, TimeSpan timeout, CancellationToken ct)
        where T : class
    {
        await WaitForReadyAsync(TimeSpan.FromSeconds(30), ct).ConfigureAwait(false);
        var generation = SpawnGeneration;
        var completion = new TaskCompletionSource<T>(TaskCreationOptions.RunContinuationsAsynchronously);
        using var subscription = Events.Subscribe(ev =>
        {
            if (SpawnGeneration != generation) return;
            if (reply(ev.Payload) is { } value)
            {
                completion.TrySetResult(value);
            }
            else if (ev.Payload is ErrorEvent error && failures.Contains(error.Error.Kind))
            {
                completion.TrySetException(new InvalidOperationException(error.Error.Kind == "restructure_busy"
                    ? "Another folder job is already running. Wait for it to finish, then try again."
                    : error.Error.Message));
            }
        }, error => completion.TrySetException(error),
            () => completion.TrySetException(new InvalidOperationException("The engine event stream closed.")));
        void OnLifecycleChanged(object? sender, PropertyChangedEventArgs e)
            => DebugLog.SafeRun(nameof(OnLifecycleChanged), () =>
            {
                if (SpawnGeneration != generation || State != LifecycleState.Ready)
                    completion.TrySetException(new InvalidOperationException("FileID's engine restarted before this finished. Check Organize Folders, then try again."));
            });
        PropertyChanged += OnLifecycleChanged;
        try
        {
            await send().ConfigureAwait(false);
            return await completion.Task.WaitAsync(timeout, ct).ConfigureAwait(false);
        }
        finally
        {
            PropertyChanged -= OnLifecycleChanged;
        }
    }
}
