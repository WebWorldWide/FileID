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
    /// <summary>Maximum size of a single IPC frame in bytes. Windows
    /// pipe buffers default to ~64 KB; flushing more than that in a
    /// single Write can deadlock if the engine's stdout reader hasn't
    /// drained its half. 1 MB is generous for every legitimate command
    /// today (each fits comfortably under 100 KB) — beyond that the
    /// caller should chunk explicitly.</summary>
    private const int MaxIpcFrameBytes = 1_000_000;

    public Task SendCommandAsync(CommandPayload payload, CancellationToken ct = default)
    {
        var cmd = IpcCommand.New(payload);
        var bytes = IpcCoder.EncodeLine(cmd);
        var commandKind = payload.GetType().Name.Replace("Command", "");
        DebugLog.Info($"[IPC OUT] {commandKind} ({bytes.Length} bytes)");

        // F.3: refuse to write a frame that risks pipe-buffer deadlock.
        if (bytes.Length > MaxIpcFrameBytes)
        {
            var msg = $"IPC frame too large: {commandKind} is {bytes.Length:N0} bytes (max {MaxIpcFrameBytes:N0}). Chunk the request into smaller batches.";
            DebugLog.Warn("[IPC OUT] " + msg);
            return Task.FromException(new InvalidOperationException(msg));
        }

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

        // The engine's stdin reader handles concurrent writers because
        // our writes are atomic per-line, but we still serialize through a
        // lock to make the byte order deterministic for log correlation.
        return Task.Run(() =>
        {
            ct.ThrowIfCancellationRequested();
            try
            {
                lock (_writeLock)
                {
                    if (_stdin is null)
                    {
                        DebugLog.Warn($"[IPC OUT] {commandKind} ABORTED — engine stdin is null (engine not running).");
                        throw new InvalidOperationException("Engine not running.");
                    }
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
        }, ct);
    }

    // FEAT-2: track scan duration locally so the SidebarProcessingControl
    // CompletedPanel can show "Scan complete — N files in 1m 23s." Used
    // to be hard-coded to "in 0s" because of a placeholder typo.
    private DateTime? _scanStartedAt;
    private TimeSpan _lastScanDuration;
    public TimeSpan LastScanDuration
    {
        get => _lastScanDuration;
        private set => Set(ref _lastScanDuration, value);
    }
    public Task StartScanAsync(string rootPath, string? rootDisplay = null, bool rescan = false)
    {
        _scanStartedAt = DateTime.UtcNow;
        return SendCommandAsync(new StartScanCommand(rootPath, rootDisplay, rescan));
    }

    /// <summary>Reset Phase + LastError before a fresh user action (e.g. retrying
    /// Start Scan after a failure). Without this, the sidebar's Failed branch
    /// keeps showing the previous error message because Phase is still
    /// <see cref="ScanPhase.Failed"/> at the moment of the new click.</summary>
    public void ClearPhaseAndError()
    {
        Phase = null;
        LastError = null;
        LastWarning = null;
    }

    private static bool IsNonFatalWarningKind(string? kind) => kind switch
    {
        "stages_skipped_missing_models" => true,
        "discovery_partial" => true,
        "checkpoint_failed_at_shutdown" => true,
        "cuda_dll_registration_failed" => true,
        _ => false,
    };

    /// <summary>Pre-flip Phase to <see cref="ScanPhase.Discovering"/> as soon
    /// as the user clicks Start Scan, so the sidebar transitions out of the
    /// idle panel before the engine's first PhaseChanged event lands. The
    /// engine's own Discovering event echoes the same value (no-op); any
    /// real phase transition takes over immediately afterwards.</summary>
    public void SetOptimisticScanningPhase()
    {
        Phase = ScanPhase.Discovering;
        LastError = null;
    }

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
    public Task PauseScanAsync()
    {
        IsPaused = true;
        return SendCommandAsync(new PauseScanCommand());
    }
    public Task ResumeScanAsync()
    {
        IsPaused = false;
        return SendCommandAsync(new ResumeScanCommand());
    }
    public Task CancelScanAsync()
    {
        IsPaused = false;
        return SendCommandAsync(new CancelScanCommand());
    }
    public Task RequestStatusAsync() => SendCommandAsync(new RequestStatusCommand());
    public Task ShutdownAsync()
    {
        // BUG-6: mark this exit as user-initiated so OnProcessExited
        // doesn't count it as a crash + auto-respawn.
        Interlocked.Exchange(ref _expectingExit, 1);
        return SendCommandAsync(new ShutdownCommand());
    }

    /// <summary>Send ShutdownCommand and wait for the engine process to
    /// actually exit (HasExited == true). Returns when the process is
    /// gone or after <paramref name="timeout"/> elapses (engine wedged —
    /// caller decides whether to proceed or surface an error). Used by
    /// RestartAsync and by the in-app wipe flow, which both need the
    /// SQLite file handle released before continuing.</summary>
    public async Task StopAndWaitForExitAsync(TimeSpan timeout, CancellationToken ct = default)
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
            if (_process is null || _process.HasExited)
            {
                DebugLog.Info($"[ENGINE] StopAndWaitForExitAsync: process exited after {sw.ElapsedMilliseconds}ms.");
                return;
            }
            await Task.Delay(100, ct).ConfigureAwait(false);
        }
        DebugLog.Warn($"[ENGINE] StopAndWaitForExitAsync: timed out after {sw.ElapsedMilliseconds}ms; process still alive.");
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
        await StopAndWaitForExitAsync(TimeSpan.FromSeconds(10), ct).ConfigureAwait(false);

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

    /// <summary>V14.9-G: tell the engine to re-probe CUDA/cuDNN
    /// availability. Engine replies with a <c>hardwareReprobed</c> event
    /// which lands on <see cref="LastHardwareReprobe"/>. Used by
    /// Settings → Performance "Verify install" so the user gets
    /// immediate feedback after installing cuDNN, without an engine
    /// restart.</summary>
    public Task VerifyCudaPackAsync() => SendCommandAsync(new VerifyCudaPackCommand());
    public Task DeepAnalyzeFileAsync(long fileId, string modelKind) =>
        SendCommandAsync(new DeepAnalyzeFileCommand(fileId, modelKind));
    public Task DeepAnalyzeFolderAsync(string pathPrefix, string modelKind) =>
        SendCommandAsync(new DeepAnalyzeFolderCommand(pathPrefix, modelKind));
    public Task DeepAnalyzeAllAsync(string modelKind, bool skipExisting) =>
        SendCommandAsync(new DeepAnalyzeAllCommand(modelKind, skipExisting));
    public Task DeepAnalyzeCancelAsync() => SendCommandAsync(new DeepAnalyzeCancelCommand());
    public Task PrewarmModelAsync(string modelKind)
    {
        DebugLog.Info($"[INSTALL] EngineClient.PrewarmModelAsync('{modelKind}') called. State={State}, _stdin={(_stdin is null ? "NULL" : "alive")}");
        return SendCommandAsync(new PrewarmModelCommand(modelKind));
    }
    public Task CancelPrewarmAsync()
    {
        DebugLog.Info("[INSTALL] EngineClient.CancelPrewarmAsync() called.");
        return SendCommandAsync(new CancelPrewarmCommand());
    }

    /// <summary>V14.9 A6: chains the four "AutoPilot" phases — Scan,
    /// face clustering, Deep Analyze, plan Restructure — into a single
    /// flow, advancing <see cref="CurrentAutoPilotStage"/> at each
    /// boundary. App-side orchestration (not engine-side) so we can
    /// reuse the existing per-phase IPC commands + events without an
    /// engine refactor. Cancellation: pass a CancellationToken; each
    /// stage checks before dispatching. The engine's per-stage cancel
    /// IPCs are wired separately (CancelScanAsync, DeepAnalyzeCancelAsync).</summary>
    public async Task RunAutoPilotAsync(
        string rootPath,
        string? rootDisplay = null,
        string vlmModelKind = "qwen2_5_vl_3b",
        bool skipExistingCaptions = true,
        CancellationToken ct = default)
    {
        if (string.IsNullOrEmpty(rootPath)) throw new ArgumentNullException(nameof(rootPath));
        if (CurrentAutoPilotStage is not null && CurrentAutoPilotStage != AutoPilotStage.Complete)
        {
            DebugLog.Warn("[AUTOPILOT] already in flight; ignoring duplicate run.");
            return;
        }
        try
        {
            // Stage 1: scan.
            CurrentAutoPilotStage = AutoPilotStage.Scanning;
            DebugLog.Info("[AUTOPILOT] stage=Scanning");
            await WaitForReadyAsync(TimeSpan.FromSeconds(30), ct).ConfigureAwait(false);
            ct.ThrowIfCancellationRequested();
            await StartScanAsync(rootPath, rootDisplay).ConfigureAwait(false);
            await AwaitPhaseAsync(ScanPhase.Completed, ct).ConfigureAwait(false);

            // Stage 2: face clustering.
            CurrentAutoPilotStage = AutoPilotStage.Clustering;
            DebugLog.Info("[AUTOPILOT] stage=Clustering");
            var clusteringTcs = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
            void OnFaceClustering(object? _, PropertyChangedEventArgs e)
            {
                if (e.PropertyName == nameof(LastFaceClustering) && LastFaceClustering is not null)
                    clusteringTcs.TrySetResult(true);
            }
            PropertyChanged += OnFaceClustering;
            try
            {
                await RunFaceClusteringAsync().ConfigureAwait(false);
                using var reg = ct.Register(() => clusteringTcs.TrySetCanceled(ct));
                await clusteringTcs.Task.ConfigureAwait(false);
            }
            finally { PropertyChanged -= OnFaceClustering; }

            // Stage 3: deep analyze.
            CurrentAutoPilotStage = AutoPilotStage.Captioning;
            DebugLog.Info("[AUTOPILOT] stage=Captioning");
            var deepAnalyzeTcs = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
            void OnDeepAnalyze(object? _, PropertyChangedEventArgs e)
            {
                if (e.PropertyName == nameof(DeepAnalyzeComplete) && DeepAnalyzeComplete is not null)
                    deepAnalyzeTcs.TrySetResult(true);
            }
            PropertyChanged += OnDeepAnalyze;
            try
            {
                await DeepAnalyzeAllAsync(vlmModelKind, skipExistingCaptions).ConfigureAwait(false);
                using var reg = ct.Register(() => deepAnalyzeTcs.TrySetCanceled(ct));
                await deepAnalyzeTcs.Task.ConfigureAwait(false);
            }
            finally { PropertyChanged -= OnDeepAnalyze; }

            // Stage 4: plan restructure.
            CurrentAutoPilotStage = AutoPilotStage.Planning;
            DebugLog.Info("[AUTOPILOT] stage=Planning");
            var planTcs = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
            void OnPlan(object? _, PropertyChangedEventArgs e)
            {
                if (e.PropertyName == nameof(LastRestructurePlan) && LastRestructurePlan is not null)
                    planTcs.TrySetResult(true);
            }
            PropertyChanged += OnPlan;
            try
            {
                await PlanRestructureAsync(rootPath).ConfigureAwait(false);
                using var reg = ct.Register(() => planTcs.TrySetCanceled(ct));
                await planTcs.Task.ConfigureAwait(false);
            }
            finally { PropertyChanged -= OnPlan; }

            CurrentAutoPilotStage = AutoPilotStage.Complete;
            DebugLog.Info("[AUTOPILOT] stage=Complete");
        }
        catch (OperationCanceledException)
        {
            DebugLog.Info("[AUTOPILOT] cancelled");
            CurrentAutoPilotStage = null;
            throw;
        }
        catch (Exception ex)
        {
            DebugLog.Warn("[AUTOPILOT] failed: " + ex.Message);
            CurrentAutoPilotStage = null;
            throw;
        }
    }

    /// <summary>Resolve when the engine's <see cref="Phase"/> reaches the
    /// target value (or transitions to <see cref="ScanPhase.Failed"/>,
    /// which throws). Used by AutoPilot to await each scan-phase
    /// completion before advancing.</summary>
    private Task AwaitPhaseAsync(ScanPhase target, CancellationToken ct)
    {
        if (Phase == target) return Task.CompletedTask;
        var tcs = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        PropertyChangedEventHandler? handler = null;
        handler = (_, e) =>
        {
            if (e.PropertyName != nameof(Phase)) return;
            if (Phase == target)
            {
                PropertyChanged -= handler;
                tcs.TrySetResult(true);
            }
            else if (Phase == ScanPhase.Failed)
            {
                PropertyChanged -= handler;
                tcs.TrySetException(new InvalidOperationException(
                    "Scan failed: " + (LastError?.Message ?? "unknown error")));
            }
        };
        PropertyChanged += handler;
        using var reg = ct.Register(() =>
        {
            PropertyChanged -= handler;
            tcs.TrySetCanceled(ct);
        });
        return tcs.Task;
    }

    /// <summary>Clear AutoPilot state — used by the cancel path or by
    /// the host UI when the user dismisses the tracker. Independent of
    /// the engine's own cancel IPCs (those still need to be called
    /// separately).</summary>
    public void ClearAutoPilot() => CurrentAutoPilotStage = null;
    public Task PlanRestructureAsync(string libraryRoot) =>
        SendCommandAsync(new PlanRestructureCommand(libraryRoot));
    public Task ApplyRestructureAsync(string libraryRoot, IReadOnlyList<RestructureMove> moves, bool useSymlinks) =>
        SendCommandAsync(new ApplyRestructureCommand(libraryRoot, moves, useSymlinks));

    public Task ApplyTagsAsync(IReadOnlyList<long> fileIds, IReadOnlyList<string> tags, string mode = "add") =>
        SendCommandAsync(new ApplyTagsCommand(fileIds, tags, mode));

    public Task RenameFilesAsync(IReadOnlyList<RenameEntry> renames) =>
        SendCommandAsync(new RenameFilesCommand(renames));

    public Task TrashFilesAsync(IReadOnlyList<long> fileIds) =>
        SendCommandAsync(new TrashFilesCommand(fileIds));

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

    public Task EmbedImageQueryAsync(long fileId, string queryId) =>
        SendCommandAsync(new EmbedImageQueryCommand(fileId, queryId));

    public Task RestoreFromTrashAsync(string batchId) =>
        SendCommandAsync(new RestoreFromTrashCommand(batchId));

    public Task RevertMergeAsync(long sourcePersonId, long destPersonId, IReadOnlyList<long> faceIdsToRevert) =>
        SendCommandAsync(new RevertMergeCommand(sourcePersonId, destPersonId, faceIdsToRevert));

}
