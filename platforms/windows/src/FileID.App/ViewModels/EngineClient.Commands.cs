// Outbound IPC command facade + AutoPilot orchestration for EngineClient.
// Split from EngineClient.cs as a partial class so the lifecycle code (spawn,
// stdout loop, event router) stays separate from the per-command surface.

using System.Collections.Generic;
using System.ComponentModel;
using System.Threading;
using FileID.IpcSchema;
using FileID.Services;

namespace FileID.ViewModels;

internal sealed partial class EngineClient
{
    // 64 MiB, symmetric with the engine's command-read cap (main.rs MAX_FRAME_BYTES)
    // and the inbound read cap (MaxFrameBytes). The old 1 MiB cap rejected a large
    // applyRestructure (>~3.5k moves) — the same move set the engine just sent in
    // restructurePlan — leaving a big reorganize unappliable. Bumped 32→64 MiB
    // (R3-07B/R5-12) to carry a ~200k-move whole-library apply. (audit E10)
    private const int MaxIpcFrameBytes = 64 * 1024 * 1024;
    private readonly object _writeQueueLock = new();
    private Task _writeTail = Task.CompletedTask;
    internal const string GpuRestartRequiredMessage =
        "Windows reset the GPU while FileID was using it. Restart FileID's engine before scanning again.";

    public Task SendCommandAsync(CommandPayload payload, CancellationToken ct = default) =>
        SendCommandAsync(payload, onWriteStarted: null, ct: ct);

    private Task SendCommandAsync(
        CommandPayload payload, Action? onWriteStarted, CancellationToken ct = default)
    {
        var commandKind = payload.GetType().Name.Replace("Command", "");

        // F.2: precondition — engine must be Ready. Without this, callers
        // get the generic "Engine not running" later and have no clue if
        // the engine is starting (wait), crashed (give up), or already
        // shut down (abandon). Throw early so the message is meaningful.
        if (State != LifecycleState.Ready)
        {
            var msg = $"Engine not ready (state={State}). Wait for Ready or call WaitForReadyAsync first.";
            DebugLog.Warn($"[IPC OUT] {commandKind} ABORTED — {msg}");
            return Task.FromException(new InvalidOperationException(msg));
        }
        if (GpuDeviceRemoved && RequiresHealthyGpu(payload))
        {
            DebugLog.Warn($"[IPC OUT] {commandKind} ABORTED — {GpuRestartRequiredMessage}");
            return Task.FromException(new InvalidOperationException(GpuRestartRequiredMessage));
        }

        var generation = SpawnGeneration;
        lock (_writeQueueLock)
        {
            var predecessor = _writeTail;
            var queued = SendCommandAfterAsync(
                predecessor, generation, payload, commandKind, onWriteStarted, ct);
            _writeTail = queued;
            return queued;
        }
    }

    private async Task SendCommandAfterAsync(
        Task predecessor,
        int generation,
        CommandPayload payload,
        string commandKind,
        Action? onWriteStarted,
        CancellationToken ct)
    {
        try
        {
            await predecessor.ConfigureAwait(false);
        }
        catch
        {
            // A failed command does not poison the FIFO for later commands.
        }

        await Task.Run(() =>
        {
            ct.ThrowIfCancellationRequested();
            var cmd = IpcCommand.New(payload);
            var bytes = IpcCoder.EncodeLine(cmd);
            DebugLog.Info($"[IPC OUT] {commandKind} ({bytes.Length} bytes)");
            if (bytes.Length > MaxIpcFrameBytes)
            {
                var msg = $"IPC frame too large: {commandKind} is {bytes.Length:N0} bytes (max {MaxIpcFrameBytes:N0}). Chunk the request into smaller batches.";
                DebugLog.Warn("[IPC OUT] " + msg);
                throw new InvalidOperationException(msg);
            }
            try
            {
                lock (_writeLock)
                {
                    if (generation != SpawnGeneration)
                    {
                        var msg = $"Engine changed while {commandKind} was queued; refusing to send it to the replacement process.";
                        DebugLog.Warn($"[IPC OUT] {commandKind} ABORTED — {msg}");
                        throw new InvalidOperationException(msg);
                    }
                    if (_stdin is null)
                    {
                        DebugLog.Warn($"[IPC OUT] {commandKind} ABORTED — engine stdin is null (engine not running).");
                        throw new InvalidOperationException("Engine not running.");
                    }
                    onWriteStarted?.Invoke();
                    _stdin.BaseStream.Write(bytes, 0, bytes.Length);
                    _stdin.BaseStream.Flush();
                }
                DebugLog.Info($"[IPC OUT] {commandKind} flushed to engine stdin.");
            }
            catch (Exception ex)
            {
                DebugLog.Warn($"[IPC OUT] {commandKind} threw on send: {ex.Message}");
                throw;
            }
        }, ct).ConfigureAwait(false);
    }

    internal static bool RequiresHealthyGpu(CommandPayload payload) => payload is
        StartScanCommand
        or DeepAnalyzeFileCommand
        or DeepAnalyzeFolderCommand
        or DeepAnalyzeAllCommand
        or EmbedTextQueryCommand;

    // FEAT-2: track scan duration locally so the SidebarProcessingControl
    // CompletedPanel can show "Scan complete — N files in 1m 23s." Used
    // to be hard-coded to "in 0s" because of a placeholder typo.
    private sealed class ScanStartPresentation
    {
        internal ScanPhase? PreviousPhase { get; init; }
        internal EngineError? PreviousError { get; init; }
        internal DateTime? PreviousStartedAt { get; init; }
        internal int PreviousShownPhaseRank { get; init; }
        internal long Revision { get; set; }
    }

    private readonly GenerationOwnedOperationSlot<ScanStartPresentation> _scanStartSlot = new();
    private long _scanPresentationRevision;
    private long _scanControlRevision;
    private DateTime? _scanStartedAt;
    private TimeSpan _lastScanDuration;
    public TimeSpan LastScanDuration
    {
        get => _lastScanDuration;
        private set => Set(ref _lastScanDuration, value);
    }

    /// Authoritative processed-file count from the last ScanComplete (the
    /// engine's final total). The completed-scan summary reads this instead of
    /// LastProgress.Processed, which can be throttle-stale by up to one batch.
    private ulong _lastScanProcessedFiles;
    public ulong LastScanProcessedFiles
    {
        get => _lastScanProcessedFiles;
        private set => Set(ref _lastScanProcessedFiles, value);
    }
    public async Task StartScanAsync(string rootPath, string? rootDisplay = null, bool rescan = false,
        IReadOnlyList<string>? excludedPaths = null)
    {
        if (State != LifecycleState.Ready)
        {
            throw new InvalidOperationException(
                $"Engine not ready (state={State}). Wait for Ready or call WaitForReadyAsync first.");
        }
        if (GpuDeviceRemoved)
        {
            throw new InvalidOperationException(GpuRestartRequiredMessage);
        }
        if (Phase is ScanPhase.Discovering or ScanPhase.Tagging or ScanPhase.PostScan)
        {
            throw new InvalidOperationException("A scan is already active.");
        }

        var presentation = new ScanStartPresentation
        {
            PreviousPhase = Phase,
            PreviousError = LastError,
            PreviousStartedAt = _scanStartedAt,
            PreviousShownPhaseRank = _shownPhaseRank,
        };
        if (!_scanStartSlot.TryReserve(SpawnGeneration, 0, presentation, out var owner))
        {
            throw new InvalidOperationException("A scan start is already awaiting confirmation from the engine.");
        }

        presentation.Revision = Interlocked.Increment(ref _scanPresentationRevision);
        Interlocked.Increment(ref _scanControlRevision);
        if (!ReferenceEquals(_scanStartSlot.Current, owner) || owner.Generation != SpawnGeneration)
        {
            _scanStartSlot.Release(owner);
            throw new InvalidOperationException("The engine changed while the scan was starting.");
        }

        _scanStartedAt = DateTime.UtcNow;
        _shownPhaseRank = -1;
        Phase = ScanPhase.Discovering;
        LastError = null;

        string[]? exclusions = excludedPaths is { Count: > 0 }
            ? System.Linq.Enumerable.ToArray(excludedPaths)
            : null;
        try
        {
            await SendCommandAsync(new StartScanCommand(rootPath, rootDisplay, rescan, exclusions))
                .ConfigureAwait(false);
        }
        catch
        {
            if (_scanStartSlot.Release(owner)
                && owner.Generation == SpawnGeneration
                && Interlocked.Read(ref _scanPresentationRevision) == presentation.Revision)
            {
                _ui.TryEnqueue(() =>
                {
                    if (owner.Generation != SpawnGeneration
                        || Interlocked.Read(ref _scanPresentationRevision) != presentation.Revision)
                    {
                        return;
                    }
                    Phase = presentation.PreviousPhase;
                    if (LastError is null) LastError = presentation.PreviousError;
                    _scanStartedAt = presentation.PreviousStartedAt;
                    _shownPhaseRank = presentation.PreviousShownPhaseRank;
                });
            }
            throw;
        }
    }

    private void ObserveAuthoritativeScanEvent(int generation)
    {
        if (generation != SpawnGeneration) return;
        Interlocked.Increment(ref _scanPresentationRevision);
        _scanStartSlot.ReleaseGeneration(generation);
        DeepAnalyzeComplete = null;
        DeepAnalyzeProgress = null;
        DeepAnalyzeStarting = null;
    }

    private void RetireScanStartGeneration(int generation)
    {
        Interlocked.Increment(ref _scanPresentationRevision);
        _scanStartSlot.ReleaseGeneration(generation);
    }

    private void RejectScanStartCommand(int generation)
    {
        var owner = _scanStartSlot.Current;
        if (owner is null || owner.Generation != generation || !_scanStartSlot.Release(owner))
        {
            return;
        }
        var presentation = owner.Payload;
        _ui.TryEnqueue(() =>
        {
            if (owner.Generation != SpawnGeneration
                || Interlocked.Read(ref _scanPresentationRevision) != presentation.Revision)
            {
                return;
            }
            Phase = presentation.PreviousPhase;
            _scanStartedAt = presentation.PreviousStartedAt;
            _shownPhaseRank = presentation.PreviousShownPhaseRank;
            Interlocked.Increment(ref _scanPresentationRevision);
        });
    }

    /// <summary>Immediately purge cataloged rows under the given excluded
    /// folders (files on disk untouched) and await the engine's
    /// <c>BulkActionResult</c> reply (action "purgeExcluded",
    /// Succeeded = purged row count) so Settings can surface the count
    /// instead of fire-and-forgetting.</summary>
    public Task<BulkActionResult> PurgeExcludedAndWaitAsync(
        IReadOnlyList<string> excludedPaths, CancellationToken ct = default) =>
        WaitForBulkActionResultAsync(
            "purgeExcluded",
            () => SendCommandAsync(new PurgeExcludedCommand(excludedPaths), ct),
            TimeSpan.FromSeconds(30),
            ct);

    /// <summary>Reset Phase + LastError before a fresh user action (e.g. retrying
    /// Start Scan after a failure). Without this, the sidebar's Failed branch
    /// keeps showing the previous error message because Phase is still
    /// <see cref="ScanPhase.Failed"/> at the moment of the new click.</summary>
    public void ClearPhaseAndError()
    {
        Phase = null;
        LastError = null;
        LastWarning = null;
        _shownPhaseRank = -1;
    }

    /// <summary>full UI-state reset for the wipe-and-rescan flow.
    /// Phase=null + LastProgress=null + LastBatch=null + LastError=null
    /// + LastWarning=null + LastScanDuration=zero. Without this, the
    /// sidebar continues to show the previous scan's "Completed" panel
    /// (with its file count + duration) during the multi-second wipe
    /// window — the user reports "the old scan stats are still there
    /// after I wipe", which reads as broken even though the engine is
    /// in fact tearing down. Call BEFORE the shutdown so the visual
    /// transition is immediate.</summary>
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
        Interlocked.Increment(ref _scanControlRevision);
        _shownPhaseRank = -1;
    }

    // internal (not private) so FileID.App.Tests can assert the classification
    // headlessly — the EngineClient singleton itself needs a UI-thread
    // DispatcherQueue and can't be constructed in a test worker.
    internal static bool IsNonFatalWarningKind(string? kind) => kind switch
    {
        "stages_skipped_missing_models" => true,
        "discovery_partial" => true,
        "checkpoint_failed_at_shutdown" => true,
        "cuda_dll_registration_failed" => true,
        // A2: the VLM server rejected our image payload but the batch fell back
        // to the per-file CLI path — tags still land, just slower. Surface as a
        // warning, not a scary error.
        "vlm_server_payload_rejected" => true,
        // #21: an incremental rescan found nothing new — informational, not an
        // error. #10: a second Deep Analyze bounced because one is already
        // running — a benign "already busy" notice, not a failure.
        "rescan_no_changes" => true,
        "scan_already_running" => true,
        "deep_analyze_already_running" => true,
        // A concurrent RunFaceClustering bounced off the engine's single-flight
        // guard — a manual Re-cluster while clustering is already running is a
        // benign "already busy" notice, not a scary red error.
        "face_clustering_busy" => true,
        // IPC parity: the engine now reports undecodable command frames
        // (macOS has always emitted this kind). Diagnostic, not actionable.
        "command_decode_failed" => true,
        _ => false,
    };

    // FEAT-1: optimistic pause flag — flipped here on the IPC send so
    // the sidebar UI can bind to IsPaused without waiting for the next
    // ScanProgress event (which doesn't currently surface pause state
    // anyway). Cleared on resume + cancel + scan complete.
    private bool _isPaused;
    public bool IsPaused
    {
        get => _isPaused;
        private set => Set(ref _isPaused, value);
    }
    private async Task EnsureScanStartConfirmedAsync()
    {
        var generation = SpawnGeneration;
        for (var attempt = 0; attempt < 500; attempt++)
        {
            if (_scanStartSlot.Current is null)
            {
                if (generation != SpawnGeneration)
                {
                    throw new InvalidOperationException("The engine changed while the scan was starting.");
                }
                return;
            }
            await Task.Delay(10);
        }
        throw new TimeoutException("The engine did not confirm the scan start within 5 seconds.");
    }

    public async Task PauseScanAsync()
    {
        await EnsureScanStartConfirmedAsync();
        var generation = SpawnGeneration;
        var previous = IsPaused;
        var revision = Interlocked.Increment(ref _scanControlRevision);
        IsPaused = true;
        try
        {
            await SendCommandAsync(new PauseScanCommand()).ConfigureAwait(false);
        }
        catch
        {
            RollbackScanControl(generation, revision, () => IsPaused = previous);
            throw;
        }
    }

    public async Task ResumeScanAsync()
    {
        await EnsureScanStartConfirmedAsync();
        var generation = SpawnGeneration;
        var previous = IsPaused;
        var revision = Interlocked.Increment(ref _scanControlRevision);
        IsPaused = false;
        try
        {
            await SendCommandAsync(new ResumeScanCommand()).ConfigureAwait(false);
        }
        catch
        {
            RollbackScanControl(generation, revision, () => IsPaused = previous);
            throw;
        }
    }

    public async Task CancelScanAsync()
    {
        await EnsureScanStartConfirmedAsync();
        var generation = SpawnGeneration;
        var previousPaused = IsPaused;
        var previousStartedAt = _scanStartedAt;
        var previousProgress = LastProgress;
        var previousBatch = LastBatch;
        var revision = Interlocked.Increment(ref _scanControlRevision);
        IsPaused = false;
        _scanStartedAt = null;
        LastProgress = null;
        LastBatch = null;
        try
        {
            await SendCommandAsync(new CancelScanCommand()).ConfigureAwait(false);
        }
        catch
        {
            RollbackScanControl(generation, revision, () =>
            {
                IsPaused = previousPaused;
                _scanStartedAt = previousStartedAt;
                LastProgress = previousProgress;
                LastBatch = previousBatch;
            });
            throw;
        }
    }

    private void RollbackScanControl(int generation, long revision, Action rollback)
    {
        if (generation != SpawnGeneration || Interlocked.Read(ref _scanControlRevision) != revision)
        {
            return;
        }
        _ui.TryEnqueue(() =>
        {
            if (generation == SpawnGeneration
                && Interlocked.Read(ref _scanControlRevision) == revision)
            {
                rollback();
            }
        });
    }
    public Task RequestStatusAsync() => SendCommandAsync(new RequestStatusCommand());
    public async Task ShutdownAsync()
    {
        // BUG-6: mark this exit as user-initiated so OnProcessExited
        // doesn't count it as a crash + auto-respawn.
        //
        // the flag has to be paired with the IPC actually landing.
        // The previous version set _expectingExit=1 unconditionally, then
        // SendCommandAsync would abort if State != Ready (engine already
        // gone), leaving the flag latched at 1. The NEXT time the engine
        // spawned and then crashed for any real reason, OnProcessExited
        // would see the leftover flag and treat the genuine crash as a
        // user-initiated exit — no auto-respawn, engine stays dead. Now
        // we set the flag only AFTER SendCommandAsync succeeds, and clear
        // it if SendCommandAsync throws.
        Interlocked.Exchange(ref _restartAfterExpectedExit, 0);
        var expectedProcess = _process;
        Interlocked.Exchange(ref _expectedExitProcess, expectedProcess);
        Interlocked.Exchange(ref _expectingExitAtTicks, DateTime.UtcNow.Ticks);
        Interlocked.Exchange(ref _expectingExit, 1);
        try
        {
            await SendCommandAsync(new ShutdownCommand()).ConfigureAwait(false);
        }
        catch
        {
            Interlocked.CompareExchange(ref _expectedExitProcess, null, expectedProcess);
            Interlocked.Exchange(ref _expectingExit, 0);
            throw;
        }
    }

    /// <summary>Send ShutdownCommand and wait for the engine process to
    /// actually exit (HasExited == true). Returns false on timeout so callers
    /// cannot mistake a live engine for a safely stopped one.</summary>
    public async Task<bool> StopAndWaitForExitAsync(
        TimeSpan timeout,
        bool restartAfterLateExit = false,
        CancellationToken ct = default)
    {
        try
        {
            await ShutdownAsync().ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            DebugLog.Warn("[ENGINE] StopAndWaitForExitAsync: ShutdownAsync threw: " + ex.Message);
            // Even if the IPC send failed, OnProcessExited may still fire.
        }

        var sw = System.Diagnostics.Stopwatch.StartNew();
        while (sw.Elapsed < timeout && !ct.IsCancellationRequested)
        {
            var process = _process;
            if ((process is null || process.HasExited) && Volatile.Read(ref _isStarting) == 0)
            {
                DebugLog.Info($"[ENGINE] StopAndWaitForExitAsync: process exited and no start is in flight after {sw.ElapsedMilliseconds}ms.");
                return true;
            }
            await Task.Delay(100, ct).ConfigureAwait(false);
        }
        ct.ThrowIfCancellationRequested();
        Interlocked.Exchange(ref _restartAfterExpectedExit, restartAfterLateExit ? 1 : 0);
        if (restartAfterLateExit && (_process is null || _process.HasExited))
        {
            if (Interlocked.Exchange(ref _restartAfterExpectedExit, 0) == 1)
            {
                await StartAfterLateExpectedExitAsync().ConfigureAwait(false);
            }
        }
        DebugLog.Warn($"[ENGINE] StopAndWaitForExitAsync: timed out after {sw.ElapsedMilliseconds}ms; process or startup is still active.");
        return false;
    }

    /// <summary>Cleanly stop the engine and respawn it. Used after a
    /// Performance Pack install so the new EP is picked up — the
    /// RuntimeProbe runs once at startup, so a fresh process is the only
    /// way to switch DLLs on the search path.
    ///
    /// Throws TimeoutException if the engine doesn't reach Ready within
    /// 60 s (10 s shutdown + 30 s startup + 20 s slack). On timeout,
    /// State is left where the FSM happened to land — caller can retry.</summary>
    public async Task RestartAsync(CancellationToken ct = default)
    {
        DebugLog.Info("[ENGINE] RestartAsync requested.");
        if (!await StopAndWaitForExitAsync(
                TimeSpan.FromSeconds(10), ct: ct, restartAfterLateExit: true).ConfigureAwait(false))
        {
            throw new TimeoutException("The existing engine did not stop; restart was aborted.");
        }

        // Force a fresh spawn. StartAsync is idempotent if a process is
        // already running, but here we explicitly want a new one. If the
        // backoff path already kicked off StartAsync, this call is a
        // no-op (the _isStarting gate dedupes).
        DebugLog.Info("[ENGINE] RestartAsync: requesting fresh spawn.");
        try { await StartAsync().ConfigureAwait(false); }
        catch (Exception ex)
        {
            DebugLog.Warn("[ENGINE] StartAsync threw during restart: " + ex.Message);
        }

        // Wait for the new process to reach Ready.
        await WaitForReadyAsync(TimeSpan.FromSeconds(30), ct).ConfigureAwait(false);
        DebugLog.Info("[ENGINE] RestartAsync complete; engine is Ready.");
    }
    public Task RunFaceClusteringAsync() => SendCommandAsync(new RunFaceClusteringCommand());

    /// <summary>Fire-and-forget wipeLibrary command (no wait for the reply).</summary>
    public Task WipeLibraryAsync() => SendCommandAsync(new WipeLibraryCommand());

    /// <summary>Send wipeLibrary and await the engine's libraryWiped reply.
    /// The engine truncates every table on its single writer connection, so
    /// this needs no shutdown/restart and can't race the OS file-lock the way
    /// deleting fileid.sqlite from the app process does. Throws TimeoutException
    /// if no reply lands within <paramref name="timeout"/>.</summary>
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
        // Reset first so a second identical wipe still raises PropertyChanged
        // (records compare by value; an equal reply wouldn't re-fire Set()).
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

    // R7: BulkActionResult replies are correlated only by `actionPrefix` against
    // the single shared LastBulkAction slot — the engine echoes no per-request id
    // (only trashFiles carries an undo-batch suffix, which the StartsWith match
    // ignores). Two concurrent same-prefix waits would both subscribe a handler
    // against that one slot and both resolve off whichever reply lands first, so
    // the later op silently reports the earlier op's Succeeded/Failed and its own
    // reply is dropped. Reserve one wait per prefix and reject overlap; a timed-out
    // reservation remains owned until its late terminal or an engine transition.
    // Per-request IDs remain a deferred cross-platform IPC-schema change.
    private readonly System.Collections.Concurrent.ConcurrentDictionary<string, SemaphoreSlim> _bulkWaitGates = new();

    /// <summary>Run a bulk command and await its <c>BulkActionResult</c> reply,
    /// matched by the action prefix the engine tags replies with
    /// (e.g. "trashFiles", "applyTags", "renameFiles", "restoreFromTrash"). Mirrors
    /// <see cref="WipeLibraryAndWaitAsync"/>: callers can then surface
    /// Succeeded/Failed instead of fire-and-forgetting (the silent-failure class —
    /// "user thinks files were deleted but they weren't"). Throws TimeoutException
    /// if no matching reply lands. The separate UndoStack listener still captures
    /// the same result for undo independently.</summary>
    public Task<BulkActionResult> WaitForBulkActionResultAsync(
        string actionPrefix,
        Func<Task> send,
        TimeSpan timeout,
        CancellationToken ct) =>
        WaitForBulkActionResultAsync(actionPrefix, send, timeout, beforeSend: null, ct: ct);

    public async Task<BulkActionResult> WaitForBulkActionResultAsync(
        string actionPrefix,
        Func<Task> send,
        TimeSpan timeout,
        Func<IDisposable?>? beforeSend = null,
        CancellationToken ct = default)
    {
        // Permit only one handler + one in-flight command per prefix. Queueing is
        // unsafe after a timeout because the late terminal from the first command
        // could resolve the queued command; reject promptly and require the prior
        // terminal or an engine transition to retire that ownership.
        var gate = _bulkWaitGates.GetOrAdd(actionPrefix, _ => new SemaphoreSlim(1, 1));
        if (!await gate.WaitAsync(TimeSpan.Zero, ct).ConfigureAwait(false))
        {
            throw new InvalidOperationException(
                $"A prior '{actionPrefix}' operation is still active or awaiting its terminal result. Restart the engine if it does not finish.");
        }
        var tcs = new TaskCompletionSource<BulkActionResult>(TaskCreationOptions.RunContinuationsAsynchronously);
        var terminal = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var leaseReleased = 0;
        IDisposable? sendRegistration = null;
        PropertyChangedEventHandler? handler = null;
        void ReleaseLease()
        {
            if (Interlocked.CompareExchange(ref leaseReleased, 1, 0) != 0) return;
            PropertyChanged -= handler;
            sendRegistration?.Dispose();
            gate.Release();
        }
        handler = (_, e) =>
        {
            if (e.PropertyName == nameof(LastBulkAction)
                && LastBulkAction is { } r
                && r.Action is { } a
                && a.StartsWith(actionPrefix, StringComparison.Ordinal))
            {
                tcs.TrySetResult(r);
                terminal.TrySetResult();
            }
            else if (e.PropertyName == nameof(State) && State != LifecycleState.Ready)
            {
                tcs.TrySetException(new InvalidOperationException(
                    $"Engine stopped before confirming '{actionPrefix}'."));
                terminal.TrySetResult();
            }
        };
        var releaseAfterReturn = true;
        var sendCompleted = false;
        try
        {
            // Reset first so a value-equal reply still re-fires PropertyChanged.
            // Inside the try so the finally always releases the gate even if a
            // PropertyChanged subscriber throws during the reset.
            LastBulkAction = null;
            // Register Undo first so its handler consumes a successful terminal
            // before this waiter's release path can dispose the registration.
            sendRegistration = beforeSend?.Invoke();
            PropertyChanged += handler;
            await send().ConfigureAwait(false);
            sendCompleted = true;
            using var cts = CancellationTokenSource.CreateLinkedTokenSource(ct);
            cts.CancelAfter(timeout);
            using var reg = cts.Token.Register(() =>
            {
                tcs.TrySetException(new TimeoutException(
                    $"Engine did not confirm '{actionPrefix}' within {timeout.TotalSeconds:0}s."));
            });
            return await tcs.Task.ConfigureAwait(false);
        }
        catch (TimeoutException) when (sendCompleted)
        {
            releaseAfterReturn = false;
            _ = terminal.Task.ContinueWith(
                _ => ReleaseLease(),
                CancellationToken.None,
                TaskContinuationOptions.ExecuteSynchronously,
                TaskScheduler.Default);
            throw;
        }
        finally
        {
            if (releaseAfterReturn) ReleaseLease();
        }
    }

    /// <summary>tell the engine to re-probe CUDA/cuDNN
    /// availability. Engine replies with a <c>hardwareReprobed</c> event
    /// which lands on <see cref="LastHardwareReprobe"/>. Used by
    /// Settings → Performance "Verify install" so the user gets
    /// immediate feedback after installing cuDNN, without an engine
    /// restart.</summary>
    public Task VerifyCudaPackAsync() => SendCommandAsync(new VerifyCudaPackCommand());

    private sealed class DeepAnalyzeOperation
    {
        internal DeepAnalyzeOperation(string modelKind, bool awaitCompletion)
        {
            ModelKind = modelKind;
            Completion = awaitCompletion
                ? new TaskCompletionSource<FileID.IpcSchema.DeepAnalyzeComplete>(
                    TaskCreationOptions.RunContinuationsAsynchronously)
                : null;
        }

        internal string ModelKind { get; }
        internal TaskCompletionSource<FileID.IpcSchema.DeepAnalyzeComplete>? Completion { get; }
        internal int HasStarted;
        internal int SendBegan;
        internal int TerminalState;
    }

    private readonly GenerationOwnedOperationSlot<DeepAnalyzeOperation> _deepAnalyzeCommandSlot = new();
    private readonly object _deepAnalyzeTerminalLock = new();

    private bool TryReserveDeepAnalyzeCommand(
        string modelKind,
        bool awaitCompletion,
        out GenerationOwnedOperationSlot<DeepAnalyzeOperation>.Owner owner)
    {
        if (!_deepAnalyzeCommandSlot.TryReserve(
                SpawnGeneration, 0, new DeepAnalyzeOperation(modelKind, awaitCompletion), out owner))
        {
            return false;
        }
        DeepAnalyzeComplete = null;
        DeepAnalyzeLast = null;
        DeepAnalyzeProgress = null;
        DeepAnalyzeStarting = null;
        NotifyDeepAnalyzeCommandOwnershipChanged();
        return true;
    }

    private void NotifyDeepAnalyzeCommandOwnershipChanged()
    {
        void Raise()
        {
            try
            {
                PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(DeepAnalyzeCommandInFlight)));
                PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(DeepAnalyzeCommandAttemptId)));
            }
            catch (Exception ex)
            {
                DebugLog.Warn("Deep Analyze ownership notification threw: " + ex.Message);
            }
        }
        if (_ui.HasThreadAccess) Raise();
        else _ui.TryEnqueue(Raise);
    }

    private bool ReleaseDeepAnalyzeCommand(
        GenerationOwnedOperationSlot<DeepAnalyzeOperation>.Owner owner,
        Exception? error = null)
    {
        if (!_deepAnalyzeCommandSlot.Release(owner)) return false;
        if (error is not null) owner.Payload.Completion?.TrySetException(error);
        NotifyDeepAnalyzeCommandOwnershipChanged();
        return true;
    }

    private void MarkDeepAnalyzeCommandStarted(int generation)
    {
        var owner = _deepAnalyzeCommandSlot.Current;
        if (owner is not null
            && owner.Generation == generation
            && Volatile.Read(ref owner.Payload.TerminalState) == 0)
        {
            Volatile.Write(ref owner.Payload.HasStarted, 1);
        }
    }

    private void FenceRejectedDeepAnalyzeCommand(int generation, string message)
    {
        var owner = _deepAnalyzeCommandSlot.Current;
        if (owner is null
            || owner.Generation != generation
            || Volatile.Read(ref owner.Payload.HasStarted) != 0)
        {
            return;
        }
        if (Interlocked.CompareExchange(ref owner.Payload.TerminalState, 1, 0) != 0)
        {
            return;
        }
        owner.Payload.Completion?.TrySetException(new InvalidOperationException(message));
        DebugLog.Warn("Deep Analyze was rejected as busy; restarting the engine before allowing another attempt.");
        _ = RestartAfterDeepAnalyzeFenceAsync(owner, message);
    }

    private bool CompleteDeepAnalyzeCommand(
        int generation,
        FileID.IpcSchema.DeepAnalyzeComplete result,
        Action publishPresentation)
    {
        Exception? publicationError = null;
        bool released;
        lock (_deepAnalyzeTerminalLock)
        {
            var owner = _deepAnalyzeCommandSlot.Current;
            if (owner is null
                || owner.Generation != generation
                || Interlocked.CompareExchange(ref owner.Payload.TerminalState, 2, 0) != 0)
            {
                return false;
            }
            try
            {
                publishPresentation();
            }
            catch (Exception ex)
            {
                publicationError = ex;
            }
            owner.Payload.Completion?.TrySetResult(result);
            released = _deepAnalyzeCommandSlot.Release(owner);
        }
        if (released) NotifyDeepAnalyzeCommandOwnershipChanged();
        if (publicationError is not null) throw publicationError;
        return released;
    }

    private void RetireDeepAnalyzeGeneration(int generation)
    {
        GenerationOwnedOperationSlot<DeepAnalyzeOperation>.Owner? owner;
        lock (_deepAnalyzeTerminalLock)
        {
            owner = _deepAnalyzeCommandSlot.ReleaseGeneration(generation);
        }
        if (owner is null) return;
        owner.Payload.Completion?.TrySetException(new InvalidOperationException(
            "The engine stopped before Deep Analyze completed."));
        NotifyDeepAnalyzeCommandOwnershipChanged();
    }

    private void HandleDeepAnalyzeSendFailure(
        GenerationOwnedOperationSlot<DeepAnalyzeOperation>.Owner owner, Exception error)
    {
        if (Volatile.Read(ref owner.Payload.SendBegan) == 0)
        {
            ReleaseDeepAnalyzeCommand(owner);
            return;
        }
        if (Interlocked.CompareExchange(ref owner.Payload.TerminalState, 1, 0) != 0)
        {
            return;
        }
        owner.Payload.Completion?.TrySetCanceled();
        DebugLog.Warn("Deep Analyze send outcome is uncertain; restarting the engine before allowing another attempt.");
        _ = RestartAfterDeepAnalyzeFenceAsync(owner, error.Message);
    }

    private async Task RestartAfterDeepAnalyzeFenceAsync(
        GenerationOwnedOperationSlot<DeepAnalyzeOperation>.Owner owner, string reason)
    {
        if (!ReferenceEquals(_deepAnalyzeCommandSlot.Current, owner)) return;
        try
        {
            await RestartAsync().ConfigureAwait(false);
        }
        catch (Exception restartError)
        {
            DebugLog.Error(
                $"Engine recovery after fenced Deep Analyze attempt failed: {restartError.Message}; " +
                $"reason: {reason}");
        }
    }

    internal bool DeepAnalyzeCommandInFlight => _deepAnalyzeCommandSlot.Current is not null;
    internal long DeepAnalyzeCommandAttemptId => _deepAnalyzeCommandSlot.Current?.AttemptId ?? 0;

    public async Task DeepAnalyzeFileAsync(long fileId, string modelKind)
    {
        if (!TryReserveDeepAnalyzeCommand(modelKind, awaitCompletion: true, out var owner))
        {
            throw new InvalidOperationException("A Deep Analyze operation is already running.");
        }
        try
        {
            DeepAnalyzeComplete = null;
            LastWarning = null;
            await SendCommandAsync(
                new DeepAnalyzeFileCommand(fileId, modelKind),
                () => Volatile.Write(ref owner.Payload.SendBegan, 1)).ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            HandleDeepAnalyzeSendFailure(owner, ex);
            throw;
        }

        using var timeout = new CancellationTokenSource();
        var timeoutTask = Task.Delay(DeepAnalyzeFileTimeout, timeout.Token);
        var completionTask = owner.Payload.Completion!.Task;
        if (await Task.WhenAny(completionTask, timeoutTask).ConfigureAwait(false) != completionTask)
        {
            _ui.TryEnqueue(() =>
            {
                if (!ReferenceEquals(_deepAnalyzeCommandSlot.Current, owner)) return;
                LastWarning = new EngineError(
                    "deep_analyze_no_confirm",
                    $"Deep Analyze hasn't responded in {DeepAnalyzeFileTimeout.TotalMinutes:0} minutes. It may still be running on a large model — check the stream, or cancel and retry if it stays stuck.",
                    null,
                    modelKind);
            });
            return;
        }
        timeout.Cancel();
        var result = await completionTask.ConfigureAwait(false);
        if (!result.Cancelled && result.Failed > 0)
        {
            _ui.TryEnqueue(() =>
            {
                var current = _deepAnalyzeCommandSlot.Current;
                if (SpawnGeneration != owner.Generation
                    || current is not null && current.AttemptId > owner.AttemptId)
                {
                    return;
                }
                LastWarning = new EngineError(
                    "deep_analyze_file_failed",
                    "Deep Analyze couldn't process this file. It may be an unsupported format, or the model isn't installed yet.",
                    null,
                    modelKind);
            });
        }
    }

    private static readonly TimeSpan DeepAnalyzeFileTimeout = TimeSpan.FromMinutes(5);

    public async Task DeepAnalyzeFolderAsync(string pathPrefix, string modelKind)
    {
        if (!TryReserveDeepAnalyzeCommand(modelKind, awaitCompletion: false, out var owner))
        {
            throw new InvalidOperationException("A Deep Analyze operation is already running.");
        }
        try
        {
            await SendCommandAsync(
                new DeepAnalyzeFolderCommand(pathPrefix, modelKind),
                () => Volatile.Write(ref owner.Payload.SendBegan, 1));
        }
        catch (Exception ex)
        {
            HandleDeepAnalyzeSendFailure(owner, ex);
            throw;
        }
    }

    public async Task DeepAnalyzeAllAsync(string modelKind, bool skipExisting, bool tagsOnly = false,
        bool proposeRenames = true, IReadOnlyList<long>? fileIds = null)
    {
        if (!TryReserveDeepAnalyzeCommand(modelKind, awaitCompletion: false, out var owner))
        {
            throw new InvalidOperationException("A Deep Analyze operation is already running.");
        }
        try
        {
            await SendCommandAsync(
                new DeepAnalyzeAllCommand(modelKind, skipExisting, tagsOnly, proposeRenames, fileIds),
                () => Volatile.Write(ref owner.Payload.SendBegan, 1));
        }
        catch (Exception ex)
        {
            HandleDeepAnalyzeSendFailure(owner, ex);
            throw;
        }
    }
    public Task DeepAnalyzeCancelAsync() => SendCommandAsync(new DeepAnalyzeCancelCommand());
    /// <summary>No-progress (stall) window for a prewarm/pack install. A large
    /// pack download is legitimately long, so we do NOT cap total wall time —
    /// instead the watchdog only fires when NO <c>ModelDownloadProgress</c>
    /// event lands for this long, which means the engine wedged. Each progress
    /// event re-arms the window (any progress = engine alive), so a healthy
    /// multi-GB download never false-fails. 120 s (was 90 s) sits ABOVE the
    /// engine's own 60 s read-timeout + resume cycle (downloader.rs): a transient
    /// HuggingFace stall self-heals — the engine errors the dead read at ~60 s,
    /// resumes from the .part, and re-emits progress — re-arming this window
    /// before it fires, so the user is no longer told to "cancel and retry"
    /// (interrupting recovery) mid-self-heal. Only a genuinely wedged engine
    /// (no progress for a full 120 s) still alarms.</summary>
    private static readonly TimeSpan PrewarmNoProgressTimeout = TimeSpan.FromSeconds(120);

    /// <summary>Absolute backstop. Even with a stall watchdog, a pathological
    /// engine could dribble one byte every 89 s forever; this caps the total
    /// watch at a generous ceiling so the UI is never wedged indefinitely. Set
    /// well above any realistic pack download on a slow link.</summary>
    private static readonly TimeSpan PrewarmAbsoluteCeiling = TimeSpan.FromHours(2);

    // Per-model-kind stall-guard cancellation (mirrors the engine's per-kind
    // cancel registry). A single global flag here would let a per-row Cancel kill
    // the stall guards of every OTHER concurrently-downloading model during
    // Install All, even though those downloads keep running. `_prewarmCancelAll`
    // is the legacy cancel-everything path (CancelPrewarmAsync(null)).
    private readonly System.Collections.Concurrent.ConcurrentDictionary<string, byte> _prewarmCancelledKinds = new();
    private int _prewarmCancelAllRequested;

    internal bool IsPrewarmCancelled(string modelKind) =>
        Interlocked.CompareExchange(ref _prewarmCancelAllRequested, 0, 0) == 1
        || _prewarmCancelledKinds.ContainsKey(modelKind);

    /// <summary>Erase a kind's cancel mark. Call only from explicit user
    /// Install/Retry entry points (the ModelInstallerService installActions):
    /// the mark must survive a prewarm dispatch that was REQUESTED before the
    /// cancel — during engine cold start the dispatch parks in
    /// WaitForReadyAsync for up to 75 s while the CancelPrewarm IPC faults, so
    /// the mark is the only record the cancel ever happened (C9).</summary>
    internal void ClearPrewarmCancelMark(string modelKind) =>
        _prewarmCancelledKinds.TryRemove(modelKind, out _);

    public Task PrewarmModelAsync(string modelKind, bool clearCancelMark = true)
    {
        DebugLog.Info($"[INSTALL] EngineClient.PrewarmModelAsync('{modelKind}') called. State={State}, _stdin={(_stdin is null ? "NULL" : "alive")}");
        // Direct callers (Settings performance pack, Deep Analyze installer,
        // auto-installers) are fresh intent: clear this kind's cancel mark and
        // any pending cancel-all so its (and others') stall guards re-arm.
        // ModelInstallerService.PrewarmAsync passes false: its installAction
        // cleared the mark at click time, and clearing again at dispatch would
        // erase a Cancel that arrived while the dispatch was parked waiting
        // for engine Ready — the lost-cancel half of C7 (C9).
        if (clearCancelMark)
        {
            Interlocked.Exchange(ref _prewarmCancelAllRequested, 0);
            _prewarmCancelledKinds.TryRemove(modelKind, out _);
        }
        var send = SendCommandAsync(new PrewarmModelCommand(modelKind));
        // Detached stall guard — keeps PrewarmModelAsync fire-and-forget (callers
        // like ModelInstallerService schedule their own UI-slot watchdog after
        // this returns) while still surfacing a wedged install to LastError when
        // nothing else is watching (Settings / auto-installer paths).
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
            // The IPC send itself failed; SendCommandAsync's awaiter already
            // throws to the caller (which surfaces it). Nothing to watch.
            return;
        }

        var startedAt = DateTime.UtcNow;
        var lastSeenAt = startedAt;
        // Each ModelDownloadProgress event is a fresh record instance, so a
        // reference change is a reliable "the engine emitted progress" signal.
        var lastProgress = ModelDownloadProgress;

        // Per-kind terminal latch. The shared ModelDownloadProgress slot is
        // overwritten by OTHER concurrent installs (Install-All), so polling it
        // only at wake time can MISS this kind's terminal (fraction >= 1.0)
        // event — after which, once the other downloads go quiet, the guard would
        // false-fire a "stopped responding" toast for a model that actually
        // finished (the reported clip_text symptom). Subscribing for the loop's
        // lifetime and latching the first terminal event for THIS kind makes a
        // completed install always stop the guard, regardless of what later
        // overwrites the slot. (The 90s→120s bump alone does NOT fix this.)
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
        // Catch a terminal that already landed between `await send` and here.
        // Use >= 1.0 (not 0.999): in-progress events are clamped to min(0.999)
        // (engine prewarm.rs), so 1.0 latches ONLY on the genuine terminal —
        // a clamped near-done value must not silence the guard while the engine
        // is still in its no-progress finalize phase (concat / SHA-256 / extract).
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
                // The latch catches this kind's terminal even when a later,
                // other-kind event has overwritten the shared slot.
                if (Interlocked.CompareExchange(ref reachedTerminal, 0, 0) == 1) return;

                var current = ModelDownloadProgress;
                // A relevant engine error already routed to LastError (prewarm
                // failures emit EngineError); stop watching so we don't pile a
                // generic stall message on top of the specific one.
                if (LastError is { } err
                    && string.Equals(err.ModelKind, modelKind, StringComparison.Ordinal))
                {
                    return;
                }
                // Any new progress (different slot reference) means the engine is
                // alive — re-arm the window.
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

    // RunAutoPilotAsync + AwaitPhaseAsync + ClearAutoPilot removed
    // along with the AutoPilot button. macOS has no equivalent explicit
    // pipeline button — auto-advance from scan → face clustering is the
    // standard behavior (wired in EngineClient.Apply's ScanCompleteEvent
    // case). Deep Analyze stays manual on both platforms (gated on the
    // user naming ≥1 person first).

    public async Task PlanRestructureAsync(string libraryRoot)
    {
        LastError = null;
        await SendCommandAsync(new PlanRestructureCommand(libraryRoot, SupportsPagedPlans: true));
    }

    public async Task ApplyRestructureAsync(string libraryRoot, IReadOnlyList<RestructureMove> moves,
        bool useSymlinks, string? planId = null)
    {
        LastError = null;
        _pendingRestructureApplyRoot = libraryRoot;
        _pendingRestructureApplyUndoable = !useSymlinks;
        try
        {
            await SendCommandAsync(new ApplyRestructureCommand(libraryRoot, moves, useSymlinks, planId));
        }
        catch
        {
            if (string.Equals(_pendingRestructureApplyRoot, libraryRoot, StringComparison.OrdinalIgnoreCase))
            {
                _pendingRestructureApplyRoot = null;
                _pendingRestructureApplyUndoable = false;
            }
            throw;
        }
    }
    /// <summary>Reverse the most recent applyRestructure — the engine replays its
    /// on-disk undo journal. A partial result stays retryable. (R2)</summary>
    public async Task UndoRestructureAsync(string libraryRoot)
    {
        if (UndoRestructureInFlight)
        {
            throw new InvalidOperationException("A Restructure undo is already running.");
        }
        // Clear the prior terminal error so a value-identical retry still raises
        // PropertyChanged when its new error arrives.
        LastError = null;
        // Clear the flag if the send faults (engine not Ready) — else it latches and
        // mis-attributes the next apply's result as the undo's. (audit R2-app)
        UndoRestructureInFlight = true;
        try
        {
            var undoRoot = UndoRestructureRoot ?? libraryRoot;
            await SendCommandAsync(new UndoRestructureCommand(undoRoot)).ConfigureAwait(false);
        }
        catch
        {
            UndoRestructureInFlight = false;
            throw;
        }
    }

    /// <summary>Send Undo and wait for its terminal engine result. A command-frame
    /// write is not success: partial, rejected, crashed, and timed-out undos return
    /// false so ChangeLog keeps the entry retryable.</summary>
    public async Task<bool> UndoRestructureAndWaitAsync(
        string libraryRoot,
        TimeSpan? timeout = null,
        CancellationToken ct = default)
    {
        var tcs = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        var ownerGeneration = SpawnGeneration;
        PropertyChangedEventHandler? handler = null;
        handler = (_, e) =>
        {
            if (e.PropertyName == nameof(LastRestructureApplyResult)
                && LastRestructureApplyResultWasUndo
                && LastRestructureApplyResult is { } result)
            {
                tcs.TrySetResult(result.Failed == 0 && string.IsNullOrWhiteSpace(result.PrivilegeError));
            }
            else if (e.PropertyName == nameof(LastError)
                     && LastError?.Kind == "undo_restructure")
            {
                tcs.TrySetResult(false);
            }
            else if (e.PropertyName == nameof(State)
                     && (State == LifecycleState.Crashed || SpawnGeneration != ownerGeneration))
            {
                tcs.TrySetResult(false);
            }
        };

        PropertyChanged += handler;
        try
        {
            await UndoRestructureAsync(libraryRoot).ConfigureAwait(false);
            using var timeoutCts = CancellationTokenSource.CreateLinkedTokenSource(ct);
            timeoutCts.CancelAfter(timeout ?? TimeSpan.FromMinutes(30));
            try
            {
                return await tcs.Task.WaitAsync(timeoutCts.Token).ConfigureAwait(false);
            }
            catch (OperationCanceledException) when (!ct.IsCancellationRequested)
            {
                DebugLog.Warn("Restructure undo timed out while awaiting its terminal engine result.");
                return false;
            }
        }
        finally
        {
            PropertyChanged -= handler;
        }
    }

    public Task ApplyTagsAsync(IReadOnlyList<long> fileIds, IReadOnlyList<string> tags, string mode = "add") =>
        SendCommandAsync(new ApplyTagsCommand(fileIds, tags, mode));

    public Task RenameFilesAsync(IReadOnlyList<RenameEntry> renames) =>
        SendCommandAsync(new RenameFilesCommand(renames));

    public Task TrashFilesAsync(IReadOnlyList<long> fileIds) =>
        SendCommandAsync(new TrashFilesCommand(fileIds));

    public Task TrashExactFilesAsync(IReadOnlyList<ExactTrashIdentity> identities) =>
        SendCommandAsync(CreateExactTrashCommand(identities));

    internal static TrashFilesCommand CreateExactTrashCommand(
        IReadOnlyList<ExactTrashIdentity> identities)
    {
        ArgumentNullException.ThrowIfNull(identities);
        if (identities.Count == 0)
        {
            throw new ArgumentException("Exact Trash requires at least one identity.", nameof(identities));
        }
        var ids = identities.Select(identity => identity.FileId).ToArray();
        if (ids.Distinct().Count() != ids.Length)
        {
            throw new ArgumentException("Exact Trash identities must have unique file IDs.", nameof(identities));
        }
        return new TrashFilesCommand(ids, identities.ToArray());
    }

    public Task MergeClustersAsync(long sourcePersonId, long destinationPersonId) =>
        SendCommandAsync(new MergeClustersCommand(sourcePersonId, destinationPersonId));

    public Task EmbedTextQueryAsync(string query, string queryId) =>
        SendCommandAsync(new EmbedTextQueryCommand(query, queryId));

    public Task RenamePersonAsync(long personId, string? title, string? first, string? middle, string? last, string? suffix) =>
        SendCommandAsync(new RenamePersonCommand(personId, title, first, middle, last, suffix));

    /// <summary>FEAT-CRIT-1: bulk mark-as-unknown for People multi-select mode.</summary>
    public Task MarkPersonsAsUnknownAsync(System.Collections.Generic.IReadOnlyList<long> personIds) =>
        SendCommandAsync(new MarkPersonsAsUnknownCommand(personIds));

    public Task FindMergeSuggestionsAsync() =>
        SendCommandAsync(new FindMergeSuggestionsCommand());

    /// <summary>Send findMergeSuggestions and await the engine's matching
    /// <c>mergeSuggestions</c> reply (lands on <see cref="LastMergeSuggestions"/>).
    /// Mirrors the awaited-bounded pattern of <see cref="WaitForBulkActionResultAsync"/>:
    /// the SuggestedMergesSheet can show "looking…" → result/timeout instead of
    /// sitting forever on the placeholder when clustering is still running on the
    /// engine. The IPC wire shape is unchanged. Throws TimeoutException if no reply
    /// lands within <paramref name="timeout"/>.</summary>
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
            else if (e.PropertyName == nameof(LastError)
                && IsMergeSuggestionTerminalError(LastError)
                && LastError is { } error)
            {
                PropertyChanged -= handler;
                tcs.TrySetException(new InvalidOperationException(error.Message));
            }
        };
        LastError = null;
        // Do NOT reset LastMergeSuggestions to null here. That fires
        // PropertyChanged → SuggestedMergesSheet.Render() with a null result,
        // flashing "No likely merges found." over the "Looking…" placeholder before
        // the real reply lands. Unlike LastLibraryWiped/LastBulkAction (value-type
        // records that CAN be value-equal across replies, so they need the reset),
        // each MergeSuggestions reply carries a fresh Pairs list ⇒ never value-equal
        // to the prior ⇒ the handler below still fires on the next reply.
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

    internal static bool IsMergeSuggestionTerminalError(EngineError? error)
        => error?.Kind == "find_merge_suggestions_failed";

    public Task MarkPersonsDifferentAsync(long sourcePersonId, long destinationPersonId, long sourceAnchorFaceId, long destinationAnchorFaceId) =>
        SendCommandAsync(new MarkPersonsDifferentCommand(sourcePersonId, destinationPersonId, sourceAnchorFaceId, destinationAnchorFaceId));

    public Task EmbedImageQueryAsync(long fileId, string queryId) =>
        SendCommandAsync(new EmbedImageQueryCommand(fileId, queryId));

    /// <summary>Send restoreFromTrash and await the engine's matching
    /// <c>BulkActionResult</c> reply (action prefix "restoreFromTrash"), surfacing
    /// any partial/total failure or non-response to LastError/LastWarning so the
    /// user isn't told "restored" when the engine timed out or some entries
    /// couldn't come back. Mirrors the awaited-bounded pattern in
    /// <see cref="WaitForBulkActionResultAsync"/>; the IPC wire shape is
    /// unchanged (still a single restoreFromTrash command). The UndoStack
    /// listener captures the same reply independently.</summary>
    public async Task RestoreFromTrashAsync(string batchId)
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

    /// <summary>Ask the engine to render a video keyframe out-of-process; it
    /// replies with a <c>thumbnailGenerated</c> event that lands on
    /// <see cref="LastThumbnailGenerated"/>. <paramref name="modifiedAt"/> is
    /// the file's modified-unix time, echoed back so ThumbnailService can
    /// write the result under its (path, modifiedAt) cache key.</summary>
    public Task GenerateVideoThumbnailAsync(string path, double? modifiedAt) =>
        SendCommandAsync(new GenerateVideoThumbnailCommand(path, modifiedAt));

}
