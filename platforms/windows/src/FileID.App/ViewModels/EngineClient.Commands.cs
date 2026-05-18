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
        Interlocked.Exchange(ref _expectingExit, 1);
        try
        {
            await SendCommandAsync(new ShutdownCommand()).ConfigureAwait(false);
        }
        catch
        {
            Interlocked.Exchange(ref _expectingExit, 0);
            throw;
        }
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

    /// <summary>tell the engine to re-probe CUDA/cuDNN
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

    // RunAutoPilotAsync + AwaitPhaseAsync + ClearAutoPilot removed
    // along with the AutoPilot button. macOS has no equivalent explicit
    // pipeline button — auto-advance from scan → face clustering is the
    // standard behavior (wired in EngineClient.Apply's ScanCompleteEvent
    // case). Deep Analyze stays manual on both platforms (gated on the
    // user naming ≥1 person first).

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
