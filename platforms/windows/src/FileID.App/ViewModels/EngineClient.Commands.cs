// Outbound IPC command facade + AutoPilot orchestration for EngineClient.
// Partial class split from EngineClient.cs: lifecycle code (spawn, stdout loop, event router) stays apart from the per-command surface.

using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Linq;
using System.Reactive.Linq;
using System.Threading;
using FileID.IpcSchema;
using FileID.Services;

namespace FileID.ViewModels;

internal sealed partial class EngineClient
{
    /// 64 MiB, symmetric with the engine's command-read cap (main.rs MAX_FRAME_BYTES) and the inbound read cap (MaxFrameChars). A 1 MiB cap rejected a large applyRestructure (>~3.5k moves), the same move set the engine sent in restructurePlan, leaving a big reorganize unappliable; 64 MiB carries a ~200k-move whole-library apply.
    private const int MaxIpcFrameBytes = 64 * 1024 * 1024;
    private sealed record ScanStartRequest(
        ScanPhase? PreviousPhase,
        TaskCompletionSource<bool> Confirmation);
    private sealed record DeepAnalyzeOperation(
        TaskCompletionSource<DeepAnalyzeComplete>? Completion);
    private readonly GenerationOwnedOperationSlot<ScanStartRequest> _scanStartSlot = new();
    private readonly GenerationOwnedOperationSlot<DeepAnalyzeOperation> _deepAnalyzeSlot = new();
    private long _scanPresentationRevision;
    private long _deepAnalyzeRevision;
    private long _scanControlRevision;
    private Task _writeTail = Task.CompletedTask;

    private GenerationOwnedOperationSlot<DeepAnalyzeOperation>.Owner TryReserveDeepAnalyzeCommand(
        TaskCompletionSource<DeepAnalyzeComplete>? completion = null)
    {
        if (!_deepAnalyzeSlot.TryReserve(
            SpawnGeneration,
            Interlocked.Increment(ref _deepAnalyzeRevision),
            new DeepAnalyzeOperation(completion),
            out var owner))
        {
            throw new InvalidOperationException("FileID is already describing files.");
        }

        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(DeepAnalyzeCommandInFlight)));
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(DeepAnalyzeCommandAttemptId)));
        return owner;
    }

    private void ReleaseDeepAnalyzeOwner(GenerationOwnedOperationSlot<DeepAnalyzeOperation>.Owner owner)
    {
        if (_deepAnalyzeSlot.Release(owner))
        {
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(DeepAnalyzeCommandInFlight)));
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(DeepAnalyzeCommandAttemptId)));
        }
    }

    private bool CompleteDeepAnalyzeCommand(int generation, DeepAnalyzeComplete result)
    {
        var owner = _deepAnalyzeSlot.Current;
        if (owner is null) return true;
        if (owner.Generation != generation || !_deepAnalyzeSlot.Release(owner)) return false;

        owner.Payload.Completion?.TrySetResult(result);
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(DeepAnalyzeCommandInFlight)));
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(DeepAnalyzeCommandAttemptId)));
        return true;
    }

    private void RetireDeepAnalyzeGeneration(int generation)
    {
        if (_deepAnalyzeSlot.ReleaseGeneration(generation) is not { } owner) return;
        owner.Payload.Completion?.TrySetException(
            new InvalidOperationException("FileID restarted before describing finished. Try again."));
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(DeepAnalyzeCommandInFlight)));
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(DeepAnalyzeCommandAttemptId)));
    }


    public Task SendCommandAsync(CommandPayload payload, CancellationToken ct = default)
        => SendCommandAsync(payload, null, ct);

    private Task SendCommandAsync(
        CommandPayload payload,
        Action? onWriteStarted,
        CancellationToken ct)
    {
        var commandKind = payload.GetType().Name.Replace("Command", "");
        if (State != LifecycleState.Ready
            && !(payload is HealthCheckCommand && State == LifecycleState.Starting))
        {
            DebugLog.Warn($"[IPC OUT] {commandKind} ABORTED — engine not ready (state={State})");
            return Task.FromException(new InvalidOperationException("FileID is still getting ready. Try again in a moment."));
        }

        var generation = SpawnGeneration;
        var commandIdObserver = _bulkCommandIdObserver.Value;
        Task queued;
        lock (_writeLock)
        {
            var predecessor = _writeTail;
            queued = CommandWriteQueue.Enqueue(predecessor, () =>
            {
                var command = IpcCommand.New(payload);
                var bytes = IpcCoder.EncodeLine(command);
                DebugLog.Info($"[IPC OUT] {commandKind} ({bytes.Length} bytes)");
                if (bytes.Length > MaxIpcFrameBytes)
                {
                    var message = $"IPC frame too large: {commandKind} {bytes.Length:N0} bytes (max {MaxIpcFrameBytes:N0}). Chunk request into smaller batches.";
                    DebugLog.Warn("[IPC OUT] " + message);
                    throw new InvalidOperationException(message);
                }

                ct.ThrowIfCancellationRequested();
                lock (_writeLock)
                {
                    if (generation != SpawnGeneration)
                    {
                        throw new InvalidOperationException("Engine generation changed before the queued command could be written.");
                    }
                    if (_stdin is null)
                    {
                        DebugLog.Warn($"[IPC OUT] {commandKind} ABORTED — engine stdin is null (engine not running).");
                        throw new InvalidOperationException("FileID is still getting ready. Try again in a moment.");
                    }

                    onWriteStarted?.Invoke();
                    commandIdObserver?.Invoke(command.Id);
                    _stdin.BaseStream.Write(bytes, 0, bytes.Length);
                    _stdin.BaseStream.Flush();
                }

                DebugLog.Info($"[IPC OUT] {commandKind} flushed to engine stdin.");
            }, ct);
            _writeTail = queued;
        }

        return queued;
    }

    // Scan duration is tracked locally for the CompletedPanel's "Scan complete — N files in 1m 23s."
    private DateTime? _scanStartedAt;
    private TimeSpan _lastScanDuration;
    public TimeSpan LastScanDuration
    {
        get => _lastScanDuration;
        private set => Set(ref _lastScanDuration, value);
    }

    public ulong LastScanProcessedFiles { get; private set; }
    public async Task StartScanAsync(string rootPath, string? rootDisplay = null, bool rescan = false)
    {
        if (GpuDeviceRemoved)
        {
            throw new InvalidOperationException(GpuRestartRequiredMessage);
        }

        if (Phase is ScanPhase.Discovering or ScanPhase.Tagging or ScanPhase.PostScan)
        {
            throw new InvalidOperationException("A scan is already in progress.");
        }

        var revision = Interlocked.Increment(ref _scanPresentationRevision);
        if (!_scanStartSlot.TryReserve(
            SpawnGeneration,
            revision,
            new ScanStartRequest(
                Phase,
                new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously)),
            out var owner))
        {
            throw new InvalidOperationException("A scan start is already being sent.");
        }

        _scanStartedAt = DateTime.UtcNow;
        _shownPhaseRank = -1;
        // Clear stale Deep Analyze latches so the pipeline strip doesn't jump a fresh (re)scan to "Done" off a prior session's DeepAnalyzeComplete; done here because it's the path for ALL scan starts (incl. "Force re-tag").
        DeepAnalyzeComplete = null;
        DeepAnalyzeProgress = null;
        DeepAnalyzeStarting = null;
        _shownPhaseRank = -1;
        Phase = ScanPhase.Discovering;
        LastError = null;
        LastWarning = null;
        try
        {
            await SendCommandAsync(new StartScanCommand(rootPath, rootDisplay, rescan)).ConfigureAwait(false);
        }
        catch
        {
            if (_scanStartSlot.Release(owner)
                && owner.Generation == SpawnGeneration
                && owner.Revision == Volatile.Read(ref _scanPresentationRevision))
            {
                Phase = owner.Payload.PreviousPhase;
                _scanStartedAt = null;
            }
            throw;
        }
    }

    /// <summary>Resets Phase + LastError before a fresh user action; otherwise the sidebar's Failed branch keeps showing the previous error because Phase is still <see cref="ScanPhase.Failed"/>.</summary>
    public void ClearPhaseAndError()
    {
        Phase = null;
        LastError = null;
        LastWarning = null;
        _shownPhaseRank = -1;
    }

    /// <summary>Full UI-state reset for wipe-and-rescan (Phase, LastProgress, LastBatch, LastError, LastWarning, LastScanDuration). Without it the sidebar shows the previous scan's "Completed" panel during the multi-second wipe window. Call BEFORE the shutdown so the transition is immediate.</summary>
    public void ResetForWipe()
    {
        Phase = null;
        LastError = null;
        LastWarning = null;
        LastProgress = null;
        LastBatch = null;
        LastScanDuration = TimeSpan.Zero;
        _scanStartedAt = null;
        IsPaused = false;
        _shownPhaseRank = -1;
    }

    private void ResetProcessBoundScanState()
    {
        Phase = null;
        LastProgress = null;
        LastBatch = null;
        IsPaused = false;
        LastScanDuration = TimeSpan.Zero;
        LastScanProcessedFiles = 0;
        _scanStartedAt = null;
        _lastProgressPhase = null;
        _shownPhaseRank = -1;
    }

    // internal so FileID.App.Tests can assert the classification headlessly; the EngineClient singleton needs a UI-thread DispatcherQueue.
    internal static bool IsNonFatalWarningKind(string? kind) => kind switch
    {
        "stages_skipped_missing_models" => true,
        "discovery_partial" => true,
        "checkpoint_failed_at_shutdown" => true,
        "cuda_dll_registration_failed" => true,
        // The VLM server rejected our image payload but the batch fell back to the per-file CLI path, so tags still land (slower): a warning, not an error.
        "vlm_server_payload_rejected" => true,
        // An incremental rescan that found nothing new, and a second Deep Analyze bounced because one is running, are informational notices, not errors.
        "scan_already_running" => true,
        "rescan_no_changes" => true,
        "deep_analyze_already_running" => true,
        // A concurrent RunFaceClustering bounced off the engine's single-flight guard: a benign "already busy" notice.
        "face_clustering_busy" => true,
        // A grouping pass that a scan took over runs again after that scan, and a scan that fell back to the slower CPU path still completes: notices, not failures.
        "face_clustering_cancelled" => true,
        "scan_slower_path" => true,
        // IPC parity: the engine reports undecodable command frames (as macOS always has). Diagnostic, not actionable.
        "command_decode_failed" => true,
        _ => false,
    };

    // Optimistic pause flag, flipped on the IPC send so the sidebar can bind IsPaused without waiting for a ScanProgress event (which doesn't surface pause state). Cleared on resume, cancel and scan complete.
    private bool _isPaused;
    public bool IsPaused
    {
        get => _isPaused;
        private set => Set(ref _isPaused, value);
    }
    private async Task EnsureScanStartConfirmedAsync()
    {
        var owner = _scanStartSlot.Current;
        if (owner is null)
        {
            if (Phase is ScanPhase.Discovering or ScanPhase.Tagging or ScanPhase.PostScan) return;
            throw new InvalidOperationException("No active scan has been confirmed by the engine.");
        }

        if (owner.Generation != SpawnGeneration)
            throw new InvalidOperationException("The engine changed before the scan start was confirmed.");

        await owner.Payload.Confirmation.Task.WaitAsync(TimeSpan.FromSeconds(30)).ConfigureAwait(false);
    }

    private async Task SendScanControlAsync(CommandPayload payload, bool paused)
    {
        await EnsureScanStartConfirmedAsync();
        var generation = SpawnGeneration;
        var revision = Interlocked.Increment(ref _scanControlRevision);
        var previousPaused = IsPaused;
        IsPaused = paused;

        try
        {
            await SendCommandAsync(payload).ConfigureAwait(false);
        }
        catch
        {
            if (generation == SpawnGeneration
                && Interlocked.Read(ref _scanControlRevision) == revision)
            {
                IsPaused = previousPaused;
            }
            throw;
        }
    }

    public Task PauseScanAsync()
    {
        return SendScanControlAsync(new PauseScanCommand(), paused: true);
    }
    public Task ResumeScanAsync()
    {
        return SendScanControlAsync(new ResumeScanCommand(), paused: false);
    }
    public async Task CancelScanAsync()
    {
        // Clear the in-flight indicators only once the cancel has been handed to the engine: clearing first left the sidebar reading Idle over a still-running scan when the send failed. The engine follows with PhaseChanged(Cancelled) and possibly a final Progress, both no-ops on the cleared state. Without the clear, _scanStartedAt + LastProgress + LastBatch + IsPaused kept prior-scan values and the "Scan complete" panel showed stale numbers.
        var generation = SpawnGeneration;
        await SendCommandAsync(new CancelScanCommand()).ConfigureAwait(false);

        void ClearScanState()
        {
            if (generation != SpawnGeneration) return;
            IsPaused = false;
            _scanStartedAt = null;
            LastProgress = null;
            LastBatch = null;
        }

        if (_ui.HasThreadAccess)
        {
            ClearScanState();
        }
        else
        {
            _ui.TryEnqueue(ClearScanState);
        }
    }
    public Task RequestStatusAsync() => SendCommandAsync(new RequestStatusCommand());
    public async Task ShutdownAsync()
    {
        var expectedExitProcess = _process;
        if (expectedExitProcess is not null)
        {
            _expectedExitProcesses[expectedExitProcess] = DateTime.UtcNow.Ticks;
        }

        try
        {
            await SendCommandAsync(new ShutdownCommand()).ConfigureAwait(false);
        }
        catch
        {
            if (expectedExitProcess is not null)
            {
                _expectedExitProcesses.TryRemove(expectedExitProcess, out _);
            }
            throw;
        }
    }

    /// <summary>Sends ShutdownCommand and waits for the process to exit (HasExited), or until <paramref name="timeout"/> elapses (engine wedged; the caller decides). Used by RestartAsync and the wipe flow, which need the SQLite file handle released.</summary>
    public async Task<bool> StopAndWaitForExitAsync(TimeSpan timeout, CancellationToken ct = default)
    {
        try
        {
            await ShutdownAsync().ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            DebugLog.Warn("[ENGINE] StopAndWaitForExitAsync: ShutdownAsync threw: " + ex.Message);
        }

        var sw = System.Diagnostics.Stopwatch.StartNew();
        while (sw.Elapsed < timeout && !ct.IsCancellationRequested)
        {
            if (Volatile.Read(ref _isStarting) == 0
                && (_process is not { } process || ProcessHasExited(process)))
            {
                DebugLog.Info($"[ENGINE] StopAndWaitForExitAsync: process exited after {sw.ElapsedMilliseconds}ms.");
                return true;
            }
            await Task.Delay(100, ct).ConfigureAwait(false);
        }
        ct.ThrowIfCancellationRequested();
        var stopped = Volatile.Read(ref _isStarting) == 0
            && (_process is not { } current || ProcessHasExited(current));
        if (stopped)
        {
            DebugLog.Info($"[ENGINE] StopAndWaitForExitAsync: process exited after {sw.ElapsedMilliseconds}ms.");
            return true;
        }
        DebugLog.Warn($"[ENGINE] StopAndWaitForExitAsync: timed out after {sw.ElapsedMilliseconds}ms; process still alive.");
        return false;
    }

    /// <summary>Cleanly stops and respawns the engine (e.g. after a Performance Pack install: RuntimeProbe runs once at startup, so a fresh process is the only way to switch DLLs on the search path). Throws TimeoutException if the engine isn't Ready within 60 s (10 s shutdown + 30 s startup + 20 s slack); State is left where the FSM landed and the caller can retry.</summary>
    public async Task RestartAsync(CancellationToken ct = default)
    {
        DebugLog.Info("[ENGINE] RestartAsync requested.");
        if (!await StopAndWaitForExitAsync(TimeSpan.FromSeconds(10), ct).ConfigureAwait(false))
        {
            throw new TimeoutException("Engine restart was aborted because the current process did not stop.");
        }

        // Force a fresh spawn: StartAsync is idempotent for a running process, and if the backoff path already started one this is a no-op (the _isStarting gate dedupes).
        DebugLog.Info("[ENGINE] RestartAsync: requesting fresh spawn.");
        try { await StartAsync().ConfigureAwait(false); }
        catch (Exception ex)
        {
            DebugLog.Warn("[ENGINE] StartAsync threw during restart: " + ex.Message);
        }

        await WaitForReadyAsync(TimeSpan.FromSeconds(30), ct).ConfigureAwait(false);
        DebugLog.Info("[ENGINE] RestartAsync complete; engine is Ready.");
    }
    public Task RunFaceClusteringAsync() => SendCommandAsync(new RunFaceClusteringCommand());

    public Task WipeLibraryAsync() => SendCommandAsync(new WipeLibraryCommand());

    /// <summary>Sends wipeLibrary and awaits the engine's libraryWiped reply. The engine truncates every table on its single writer connection, so no shutdown/restart is needed and it can't race the OS file lock the way deleting fileid.sqlite from the app process does. Throws TimeoutException if no reply lands within <paramref name="timeout"/>.</summary>
    public async Task<LibraryWiped> WipeLibraryAndWaitAsync(TimeSpan timeout, CancellationToken ct = default)
    {
        var tcs = new TaskCompletionSource<LibraryWiped>(TaskCreationOptions.RunContinuationsAsynchronously);
        PropertyChangedEventHandler? handler = null;
        handler = (_, e) =>
        {
            if (e.PropertyName == nameof(LastLibraryWiped) && LastLibraryWiped is { } r)
            {
                PropertyChanged -= handler;
                tcs.TrySetResult(r);
            }
        };
        // Reset first so a second identical wipe still raises PropertyChanged (records compare by value; an equal reply wouldn't re-fire Set()).
        LastLibraryWiped = null;
        PropertyChanged += handler;
        try
        {
            await WipeLibraryAsync().ConfigureAwait(false);
            using var cts = CancellationTokenSource.CreateLinkedTokenSource(ct);
            cts.CancelAfter(timeout);
            using var reg = cts.Token.Register(() =>
            {
                PropertyChanged -= handler;
                tcs.TrySetException(new TimeoutException(
                    $"Engine did not confirm wipeLibrary within {timeout.TotalSeconds:0}s."));
            });
            return await tcs.Task.ConfigureAwait(false);
        }
        finally
        {
            PropertyChanged -= handler;
        }
    }

    // A reply carrying commandID (IPC 1.28.0) goes only to the wait that sent that command (BulkResultMatcher); a reply without one (an older engine) is matched by `actionPrefix` against the single shared LastBulkAction slot (trashFiles carries an undo-batch suffix, which StartsWith ignores). A prefix match alone would let two concurrent same-prefix waits both resolve off the first reply, so the per-prefix gate stays: at most one same-prefix wait is live.
    private static readonly AsyncLocal<Action<string>?> _bulkCommandIdObserver = new();
    private readonly System.Collections.Concurrent.ConcurrentDictionary<string, SemaphoreSlim> _bulkWaitGates = new();

    /// <summary>Runs a bulk command and awaits its <c>BulkActionResult</c>, matched by the engine's action prefix ("trashFiles", "applyTags", "renameFiles", "restoreFromTrash"), so callers surface Succeeded/Failed instead of the silent-failure class ("user thinks files were deleted but they weren't"). Throws TimeoutException if no matching reply lands. The UndoStack listener captures the same result independently.</summary>
    public async Task<BulkActionResult> WaitForBulkActionResultAsync(
        string actionPrefix,
        Func<Task> send,
        TimeSpan timeout,
        Func<IDisposable?>? beforeSend = null,
        CancellationToken ct = default)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(actionPrefix);
        ArgumentNullException.ThrowIfNull(send);
        if (timeout < TimeSpan.Zero && timeout != Timeout.InfiniteTimeSpan)
            throw new ArgumentOutOfRangeException(nameof(timeout));

        var gate = _bulkWaitGates.GetOrAdd(actionPrefix, _ => new SemaphoreSlim(1, 1));
        if (!await gate.WaitAsync(TimeSpan.Zero, ct).ConfigureAwait(false))
            throw new InvalidOperationException($"A {actionPrefix} operation is already awaiting its engine result.");

        var generation = SpawnGeneration;
        var matcher = new BulkResultMatcher(actionPrefix);
        var terminal = new TaskCompletionSource<BulkActionResult>(TaskCreationOptions.RunContinuationsAsynchronously);
        PropertyChangedEventHandler? handler = null;
        PropertyChangedEventHandler? lifecycleHandler = null;
        IDisposable? sendRegistration = null;
        var sendCompleted = false;
        var releaseOnExit = true;
        var released = 0;

        void ReleaseOwnership()
        {
            if (Interlocked.Exchange(ref released, 1) != 0) return;
            PropertyChanged -= handler;
            PropertyChanged -= lifecycleHandler;
            sendRegistration?.Dispose();
            gate.Release();
        }

        handler = (_, e) =>
        {
            if (e.PropertyName == nameof(LastBulkAction)
                && LastBulkAction is { } result
                && matcher.Accepts(result.Action, result.CommandId))
            {
                terminal.TrySetResult(result);
            }
        };

        lifecycleHandler = (_, e) =>
        {
            if ((e.PropertyName == nameof(SpawnGeneration) && SpawnGeneration != generation)
                || (e.PropertyName == nameof(State) && State == LifecycleState.Crashed))
            {
                terminal.TrySetException(new InvalidOperationException(
                    "The engine generation changed before the bulk action was confirmed."));
            }
        };

        try
        {
            LastBulkAction = null;
            sendRegistration = beforeSend?.Invoke();
            PropertyChanged += handler;
            PropertyChanged += lifecycleHandler;
            _bulkCommandIdObserver.Value = matcher.NoteSent;
            try { await send().ConfigureAwait(false); }
            finally { _bulkCommandIdObserver.Value = null; }
            sendCompleted = true;
            return timeout == Timeout.InfiniteTimeSpan
                ? await terminal.Task.WaitAsync(ct).ConfigureAwait(false)
                : await terminal.Task.WaitAsync(timeout, ct).ConfigureAwait(false);
        }
        catch (TimeoutException) when (sendCompleted)
        {
            releaseOnExit = false;
            _ = terminal.Task.ContinueWith(
                _ => ReleaseOwnership(),
                CancellationToken.None,
                TaskContinuationOptions.ExecuteSynchronously,
                TaskScheduler.Default);
            ReleaseAfterLateReplyHold(ReleaseOwnership);
            throw;
        }
        catch (OperationCanceledException) when (sendCompleted)
        {
            releaseOnExit = false;
            _ = terminal.Task.ContinueWith(
                _ => ReleaseOwnership(),
                CancellationToken.None,
                TaskContinuationOptions.ExecuteSynchronously,
                TaskScheduler.Default);
            ReleaseAfterLateReplyHold(ReleaseOwnership);
            throw;
        }
        finally
        {
            if (releaseOnExit) ReleaseOwnership();
        }
    }

    // A wait that gave up keeps its prefix reserved so a late reply can't be read as the next operation's result. A reply that never comes must not block that kind of action for the session, so the reservation also ends after this hold.
    private static void ReleaseAfterLateReplyHold(Action release) =>
        _ = Task.Delay(BulkActionTimeout.LateReplyHold).ContinueWith(
            _ => release(),
            CancellationToken.None,
            TaskContinuationOptions.ExecuteSynchronously,
            TaskScheduler.Default);

    public Task VerifyCudaPackAsync() => SendCommandAsync(new VerifyCudaPackCommand());
    /// <summary>Sends deepAnalyzeFile and awaits the terminal <c>DeepAnalyzeComplete</c> (the single-file handler always emits one: success, failure, AND the no-model early return), so a stuck or no-model run surfaces instead of leaving the stream card open with no result. A single VLM caption can be slow, so the timeout is generous and a no-response is a warning (the run may still be in flight), not an error.</summary>
    public async Task DeepAnalyzeFileAsync(long fileId, string modelKind)
    {
        var completion = new TaskCompletionSource<FileID.IpcSchema.DeepAnalyzeComplete>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        var owner = TryReserveDeepAnalyzeCommand(completion);
        DeepAnalyzeComplete = null;
        try
        {
            try
            {
                await SendCommandAsync(new DeepAnalyzeFileCommand(fileId, modelKind)).ConfigureAwait(false);
            }
            catch
            {
                ReleaseDeepAnalyzeOwner(owner);
                throw;
            }

            FileID.IpcSchema.DeepAnalyzeComplete result;
            try
            {
                result = await completion.Task.WaitAsync(DeepAnalyzeFileTimeout).ConfigureAwait(false);
            }
            catch (TimeoutException)
            {
                _ = completion.Task.ContinueWith(
                    task => _ = task.Exception,
                    CancellationToken.None,
                    TaskContinuationOptions.OnlyOnFaulted | TaskContinuationOptions.ExecuteSynchronously,
                    TaskScheduler.Default);
                _ui.TryEnqueue(() => LastWarning = new EngineError(
                    "deep_analyze_no_confirm",
                    $"Describing this file hasn't finished in {DeepAnalyzeFileTimeout.TotalMinutes:0} minutes. A large model can be slow; stop and try again if it stays stuck.",
                    null,
                    modelKind));
                return;
            }

            if (!result.Cancelled && result.Failed > 0)
            {
                _ui.TryEnqueue(() => LastWarning = new EngineError(
                    "deep_analyze_file_failed",
                    "FileID couldn't describe this file. Its format may not be supported, or the description model may not be downloaded.",
                    null,
                    modelKind));
            }
        }
        catch (InvalidOperationException) when (owner.Generation != SpawnGeneration)
        {
            _ui.TryEnqueue(() => LastWarning = new EngineError(
                "deep_analyze_engine_restarted",
                "FileID restarted before describing finished. Try again.",
                null,
                modelKind));
        }
    }
    private static readonly TimeSpan DeepAnalyzeFileTimeout = TimeSpan.FromMinutes(5);
    public async Task DeepAnalyzeFolderAsync(string pathPrefix, string modelKind)
    {
        var owner = TryReserveDeepAnalyzeCommand();
        try
        {
            await SendCommandAsync(new DeepAnalyzeFolderCommand(pathPrefix, modelKind)).ConfigureAwait(false);
        }
        catch
        {
            ReleaseDeepAnalyzeOwner(owner);
            throw;
        }
    }

    public async Task DeepAnalyzeAllAsync(
        string modelKind,
        bool skipExisting,
        bool tagsOnly = false,
        bool proposeRenames = true)
    {
        var owner = TryReserveDeepAnalyzeCommand();
        try
        {
            var excluded = AppViewModel.Instance.Settings.DeepAnalyzeExcludedFolders;
            await SendCommandAsync(new DeepAnalyzeAllCommand(modelKind, skipExisting, tagsOnly, proposeRenames, ExcludedFolders: excluded.Count > 0 ? [.. excluded] : null))
                .ConfigureAwait(false);
        }
        catch
        {
            ReleaseDeepAnalyzeOwner(owner);
            throw;
        }
    }
    public Task DeepAnalyzeCancelAsync() => SendCommandAsync(new DeepAnalyzeCancelCommand());
    /// <summary>No-progress (stall) window for a prewarm/pack install. Total wall time is NOT capped (large downloads are legitimately long); the watchdog fires only when no <c>ModelDownloadProgress</c> lands for this long, and each event re-arms it. 120 s sits ABOVE the engine's 60 s read-timeout + resume cycle (downloader.rs), so a transient HuggingFace stall self-heals (the engine errors the dead read, resumes from .part, re-emits progress) before this fires; only a genuinely wedged engine alarms.</summary>
    private static readonly TimeSpan PrewarmNoProgressTimeout = TimeSpan.FromSeconds(120);

    /// <summary>Absolute backstop: a pathological engine could dribble a byte every 89 s forever, so the watch is capped well above any realistic pack download on a slow link.</summary>
    private static readonly TimeSpan PrewarmAbsoluteCeiling = TimeSpan.FromHours(2);

    // Per-model-kind stall-guard cancellation (mirrors the engine's per-kind cancel registry): a single global flag would let a per-row Cancel kill the stall guards of every other concurrent download during Install All. `_prewarmCancelAll` is the legacy cancel-everything path (CancelPrewarmAsync(null)).
    private readonly System.Collections.Concurrent.ConcurrentDictionary<string, byte> _prewarmCancelledKinds = new();
    private int _prewarmCancelAllRequested;

    internal bool IsPrewarmCancelled(string modelKind) =>
        Interlocked.CompareExchange(ref _prewarmCancelAllRequested, 0, 0) == 1
        || _prewarmCancelledKinds.ContainsKey(modelKind);

    /// <summary>Erases a kind's cancel mark. Call only from explicit user Install/Retry entry points: a prewarm dispatch REQUESTED before the cancel can park in WaitForReadyAsync for up to 75 s during engine cold start while the CancelPrewarm IPC faults, so the mark is the only record the cancel happened.</summary>
    internal void ClearPrewarmCancelMark(string modelKind) =>
        _prewarmCancelledKinds.TryRemove(modelKind, out _);

    public Task PrewarmModelAsync(string modelKind, bool clearCancelMark = true)
    {
        DebugLog.Info($"[INSTALL] EngineClient.PrewarmModelAsync('{modelKind}') called. State={State}, _stdin={(_stdin is null ? "NULL" : "alive")}");
        // Direct callers (Settings pack, Deep Analyze installer, auto-installers) are fresh intent: clear this kind's cancel mark and any pending cancel-all so stall guards re-arm. ModelInstallerService.PrewarmAsync passes false: its installAction cleared the mark at click time, and clearing again at dispatch would erase a Cancel that arrived while the dispatch waited for Ready.
        if (clearCancelMark)
        {
            Interlocked.Exchange(ref _prewarmCancelAllRequested, 0);
            _prewarmCancelledKinds.TryRemove(modelKind, out _);
        }
        var send = SendCommandAsync(new PrewarmModelCommand(modelKind));
        // Detached stall guard: keeps PrewarmModelAsync fire-and-forget while still surfacing a wedged install to LastError when nothing else is watching.
        _ = StartPrewarmStallGuardAsync(modelKind, send);
        return send;
    }

    private async Task StartPrewarmStallGuardAsync(string modelKind, Task send)
    {
        try
        {
            await send.ConfigureAwait(false);
        }
        catch
        {
            return;
        }

        var startedAt = DateTime.UtcNow;
        var lastSeenAt = startedAt;
        // Each ModelDownloadProgress is a fresh record, so a reference change reliably means the engine emitted progress.
        var lastProgress = ModelDownloadProgress;

        // Per-kind terminal latch. The shared ModelDownloadProgress slot is overwritten by OTHER concurrent installs, so polling it only at wake time can MISS this kind's terminal (fraction >= 1.0), after which the guard would false-fire "stopped responding" for a finished model (the clip_text symptom). Subscribing for the loop's lifetime and latching the first terminal event makes a completed install always stop the guard.
        var reachedTerminal = 0;
        void OnProgress(object? _, PropertyChangedEventArgs e)
        {
            if (e.PropertyName != nameof(ModelDownloadProgress)) return;
            if (ModelDownloadProgress is { } p
                && string.Equals(p.ModelKind, modelKind, StringComparison.Ordinal)
                && p.Fraction >= 1.0)
            {
                Interlocked.Exchange(ref reachedTerminal, 1);
            }
        }
        PropertyChanged += OnProgress;
        // Catch a terminal that already landed between `await send` and here. Use >= 1.0, not 0.999: in-progress events are clamped to min(0.999) (engine prewarm.rs), so 1.0 latches ONLY on the genuine terminal; a near-done value must not silence the guard during the engine's no-progress finalize phase (concat / SHA-256 / extract).
        if (ModelDownloadProgress is { } seed
            && string.Equals(seed.ModelKind, modelKind, StringComparison.Ordinal)
            && seed.Fraction >= 1.0)
        {
            Interlocked.Exchange(ref reachedTerminal, 1);
        }

        try
        {
            while (true)
            {
                if (IsPrewarmCancelled(modelKind)) return;
                if (Interlocked.CompareExchange(ref reachedTerminal, 0, 0) == 1) return;

                await Task.Delay(PrewarmNoProgressTimeout).ConfigureAwait(false);

                if (IsPrewarmCancelled(modelKind)) return;
                if (Interlocked.CompareExchange(ref reachedTerminal, 0, 0) == 1) return;

                var current = ModelDownloadProgress;
                // A relevant engine error already routed to LastError (prewarm failures emit EngineError); stop watching so a generic stall message doesn't pile on the specific one.
                if (LastError is { } err
                    && string.Equals(err.ModelKind, modelKind, StringComparison.Ordinal))
                {
                    return;
                }
                // Any new progress (different slot reference) means the engine is alive: re-arm the window.
                if (!ReferenceEquals(current, lastProgress))
                {
                    lastProgress = current;
                    lastSeenAt = DateTime.UtcNow;
                }

                var now = DateTime.UtcNow;
                if (now - lastSeenAt >= PrewarmNoProgressTimeout || now - startedAt >= PrewarmAbsoluteCeiling)
                {
                    var stalled = now - startedAt >= PrewarmAbsoluteCeiling
                        ? $"The install for '{modelKind}' is still running after {PrewarmAbsoluteCeiling.TotalHours:0} hours — something is wrong. Cancel and try again."
                        : $"The install for '{modelKind}' stopped responding (no progress for {PrewarmNoProgressTimeout.TotalSeconds:0}s). Check your connection, then cancel and retry.";
                    DebugLog.Warn($"[INSTALL] prewarm stall guard firing for '{modelKind}': {stalled}");
                    _ui.TryEnqueue(() => LastError = new EngineError("model_install_stalled", stalled, null, modelKind));
                    return;
                }
            }
        }
        finally
        {
            PropertyChanged -= OnProgress;
        }
    }

    public Task CancelPrewarmAsync(string? modelKind = null)
    {
        DebugLog.Info($"[INSTALL] EngineClient.CancelPrewarmAsync(modelKind={modelKind ?? "<all>"}) called.");
        if (modelKind is null) Interlocked.Exchange(ref _prewarmCancelAllRequested, 1);
        else _prewarmCancelledKinds[modelKind] = 1;
        return SendCommandAsync(new CancelPrewarmCommand(modelKind));
    }

    public Task PlanRestructureAsync(string libraryRoot)
    {
        LastError = null;
        LastWarning = null;
        return SendCommandAsync(new PlanRestructureCommand(libraryRoot));
    }
    private bool _restructureApplyUsesSymlinks;
    private int _restructureCommandInFlight;
    internal bool LastRestructureResultWasUndo { get; private set; }
    internal bool LastRestructureResultWasShortcutApply { get; private set; }
    internal string? RestructureOperationRoot { get; private set; }

    public async Task ApplyRestructureAsync(string libraryRoot, IReadOnlyList<RestructureMove> moves, bool useSymlinks)
    {
        if (Interlocked.CompareExchange(ref _restructureCommandInFlight, 1, 0) != 0)
            throw new InvalidOperationException("Organize Folders is already working. Wait for it to finish.");
        LastError = null;
        LastWarning = null;
        _restructureApplyUsesSymlinks = useSymlinks;
        RestructureOperationRoot = libraryRoot;
        try { await SendCommandAsync(new ApplyRestructureCommand(libraryRoot, moves, useSymlinks)).ConfigureAwait(false); }
        catch { Interlocked.Exchange(ref _restructureCommandInFlight, 0); throw; }
    }
    /// <summary>Reverses the most recent applyRestructure by replaying the engine's on-disk undo journal. Reply lands on LastRestructureApplyResult; cancelled or failed work stays retryable.</summary>
    public async Task UndoRestructureAsync(string libraryRoot)
    {
        if (Interlocked.CompareExchange(ref _restructureCommandInFlight, 1, 0) != 0)
            throw new InvalidOperationException("Organize Folders is already working. Wait for it to finish.");
        await SendRestructureUndoCoreAsync(libraryRoot).ConfigureAwait(false);
    }

    private async Task SendRestructureUndoCoreAsync(string libraryRoot)
    {
        RestructureOperationRoot = libraryRoot;
        LastError = null;
        LastWarning = null;
        // Clear the flag if the send faults (engine not Ready); otherwise it latches and mis-attributes the next apply's result as the undo's.
        UndoRestructureInFlight = true;
        try
        {
            await SendCommandAsync(new UndoRestructureCommand(libraryRoot, UndoRestructureShortcutToken)).ConfigureAwait(false);
        }
        catch
        {
            UndoRestructureInFlight = false;
            Interlocked.Exchange(ref _restructureCommandInFlight, 0);
            throw;
        }
    }

    internal async Task<RestructureApplyResult> UndoRestructureAndWaitAsync(
        string libraryRoot, CancellationToken ct = default)
    {
        await WaitForReadyAsync(TimeSpan.FromSeconds(30), ct).ConfigureAwait(false);
        var generation = SpawnGeneration;
        if (Interlocked.CompareExchange(ref _restructureCommandInFlight, 1, 0) != 0)
            throw new InvalidOperationException("Organize Folders is already working. Wait for it to finish.");
        UndoRestructureInFlight = true;
        var completion = new TaskCompletionSource<RestructureApplyResult>(TaskCreationOptions.RunContinuationsAsynchronously);
        using var subscription = Events.Subscribe(ev =>
        {
            if (SpawnGeneration != generation) return;
            if (ev.Payload is RestructureApplyResultEvent result) completion.TrySetResult(result.Result);
            else if (ev.Payload is ErrorEvent { Error.Kind: "undo_restructure" } error)
                completion.TrySetException(new InvalidOperationException(error.Error.Message));
        }, error => completion.TrySetException(error),
            () => completion.TrySetException(new InvalidOperationException("The engine event stream closed.")));
        void OnLifecycleChanged(object? sender, PropertyChangedEventArgs e)
            => DebugLog.SafeRun(nameof(OnLifecycleChanged), () =>
            {
                if (SpawnGeneration != generation || State != LifecycleState.Ready)
                    completion.TrySetException(new InvalidOperationException("FileID stopped before confirming the undo. Check the folder, then try Undo move again."));
            });
        PropertyChanged += OnLifecycleChanged;
        var sendStarted = false;
        try
        {
            if (SpawnGeneration != generation)
                throw new InvalidOperationException("FileID restarted before the undo started. Try Undo move again.");
            sendStarted = true;
            await SendRestructureUndoCoreAsync(libraryRoot).ConfigureAwait(false);
            return await completion.Task.WaitAsync(TimeSpan.FromMinutes(30), ct).ConfigureAwait(false);
        }
        finally
        {
            PropertyChanged -= OnLifecycleChanged;
            if (!sendStarted)
            {
                UndoRestructureInFlight = false;
                Interlocked.Exchange(ref _restructureCommandInFlight, 0);
            }
        }
    }

    public Task ApplyTagsAsync(IReadOnlyList<long> fileIds, IReadOnlyList<string> tags, string mode = "add") =>
        SendCommandAsync(new ApplyTagsCommand(fileIds, tags, mode));

    public Task RenameFilesAsync(IReadOnlyList<RenameEntry> renames) =>
        SendCommandAsync(new RenameFilesCommand(renames));

    public Task TrashFilesAsync(IReadOnlyList<long> fileIds) =>
        SendCommandAsync(new TrashFilesCommand(fileIds));

    internal static TrashFilesCommand CreateExactTrashCommand(
        IReadOnlyList<ExactTrashIdentity> identities)
    {
        if (identities.Count == 0) throw new ArgumentException("No exact trash identities were supplied.", nameof(identities));
        var ids = identities.Select(identity => identity.FileId).ToArray();
        if (ids.Any(id => id <= 0) || ids.Distinct().Count() != ids.Length)
            throw new ArgumentException("Exact trash identities must have unique positive file IDs.", nameof(identities));
        return new TrashFilesCommand(ids, identities.ToArray());
    }

    public Task TrashExactFilesAsync(IReadOnlyList<ExactTrashIdentity> identities) =>
        SendCommandAsync(CreateExactTrashCommand(identities));

    public Task ReassignFaceAsync(long faceId, long destinationPersonId) =>
        SendCommandAsync(new ReassignFaceCommand(faceId, destinationPersonId));

    public Task MergeClustersAsync(long sourcePersonId, long destinationPersonId) =>
        SendCommandAsync(new MergeClustersCommand(sourcePersonId, destinationPersonId));

    public Task EmbedTextQueryAsync(string query, string queryId) =>
        SendCommandAsync(new EmbedTextQueryCommand(query, queryId));

    public Task RenamePersonAsync(long personId, string? title, string? first, string? middle, string? last, string? suffix) =>
        SendCommandAsync(new RenamePersonCommand(personId, title, first, middle, last, suffix));

    public Task MarkPersonsAsUnknownAsync(System.Collections.Generic.IReadOnlyList<long> personIds) =>
        SendCommandAsync(new MarkPersonsAsUnknownCommand(personIds));

    public Task FindMergeSuggestionsAsync() =>
        SendCommandAsync(new FindMergeSuggestionsCommand());

    /// <summary>Sends findMergeSuggestions and awaits the matching <c>mergeSuggestions</c> reply (lands on <see cref="LastMergeSuggestions"/>), so the SuggestedMergesSheet shows "looking…" → result/timeout instead of sitting on the placeholder while clustering still runs. Throws TimeoutException if no reply lands within <paramref name="timeout"/>.</summary>
    public async Task<MergeSuggestions> WaitForMergeSuggestionsAsync(TimeSpan timeout, CancellationToken ct = default)
    {
        var tcs = new TaskCompletionSource<MergeSuggestions>(TaskCreationOptions.RunContinuationsAsynchronously);
        PropertyChangedEventHandler? handler = null;
        handler = (_, e) =>
        {
            if (e.PropertyName == nameof(LastMergeSuggestions) && LastMergeSuggestions is { } r)
            {
                PropertyChanged -= handler;
                tcs.TrySetResult(r);
            }
        };
        // Do NOT reset LastMergeSuggestions to null here: that fires PropertyChanged → SuggestedMergesSheet.Render() with a null result, flashing "No likely merges found." over "Looking…". Unlike LastLibraryWiped/LastBulkAction (value-type records that CAN be value-equal across replies), each MergeSuggestions carries a fresh Pairs list, so the handler still fires on the next reply.
        PropertyChanged += handler;
        try
        {
            await SendCommandAsync(new FindMergeSuggestionsCommand(), ct).ConfigureAwait(false);
            using var cts = CancellationTokenSource.CreateLinkedTokenSource(ct);
            cts.CancelAfter(timeout);
            using var reg = cts.Token.Register(() =>
            {
                PropertyChanged -= handler;
                tcs.TrySetException(new TimeoutException(
                    $"Engine did not return merge suggestions within {timeout.TotalSeconds:0}s."));
            });
            return await tcs.Task.ConfigureAwait(false);
        }
        finally
        {
            PropertyChanged -= handler;
        }
    }

    public Task MarkPersonsDifferentAsync(long sourcePersonId, long destinationPersonId, long sourceAnchorFaceId, long destinationAnchorFaceId) =>
        SendCommandAsync(new MarkPersonsDifferentCommand(sourcePersonId, destinationPersonId, sourceAnchorFaceId, destinationAnchorFaceId));

    public Task EmbedImageQueryAsync(long fileId, string queryId) =>
        SendCommandAsync(new EmbedImageQueryCommand(fileId, queryId));

    /// <summary>Sends restoreFromTrash and awaits the matching <c>BulkActionResult</c> (prefix "restoreFromTrash"), surfacing partial/total failure or non-response to LastError/LastWarning so the user isn't told "restored" when it timed out or some entries couldn't come back. The UndoStack listener captures the same reply independently.</summary>
    public async Task<bool> RestoreFromTrashAsync(string batchId)
    {
        try
        {
            var result = await WaitForBulkActionResultAsync(
                "restoreFromTrash",
                () => SendCommandAsync(new RestoreFromTrashCommand(batchId)),
                TimeSpan.FromSeconds(30)).ConfigureAwait(false);
            if (result.Failed > 0)
            {
                var first = result.Messages?.FirstOrDefault(m => !m.Ok)?.Message;
                var detail = string.IsNullOrWhiteSpace(first) ? "" : $" — {first}";
                _ui.TryEnqueue(() => LastWarning = new EngineError(
                    "restore_partial_failure",
                    $"Restored {result.Succeeded}; {result.Failed} couldn't be brought back{detail}.",
                    null));
            }
            return result.Failed == 0 && result.Succeeded > 0;
        }
        catch (TimeoutException)
        {
            _ui.TryEnqueue(() => LastError = new EngineError(
                "restore_no_confirm",
                "The engine didn't confirm the restore within 30 seconds. The files may or may not have been restored — re-run the scan to check before retrying.",
                null));
            throw;
        }
        catch (Exception ex)
        {
            _ui.TryEnqueue(() => LastError = new EngineError("restore_failed", $"Restore failed: {ex.Message}", null));
            throw;
        }
    }

    public Task RevertMergeAsync(long sourcePersonId, long destPersonId, IReadOnlyList<long> faceIdsToRevert) =>
        SendCommandAsync(new RevertMergeCommand(sourcePersonId, destPersonId, faceIdsToRevert));

    /// <summary>Asks the engine to render a video keyframe out-of-process; the reply lands on <see cref="LastThumbnailGenerated"/>. <paramref name="modifiedAt"/> is echoed back so ThumbnailService can write the result under its (path, modifiedAt) cache key.</summary>
    public Task GenerateVideoThumbnailAsync(string path, double? modifiedAt) =>
        SendCommandAsync(new GenerateVideoThumbnailCommand(path, modifiedAt));

}
