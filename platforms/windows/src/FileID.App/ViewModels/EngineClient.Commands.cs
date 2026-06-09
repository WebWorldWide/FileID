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
    // 32 MiB, symmetric with the engine's command-read cap (main.rs MAX_FRAME_BYTES)
    // and the inbound read cap (MaxFrameChars). The old 1 MiB cap rejected a large
    // applyRestructure (>~3.5k moves) — the same move set the engine just sent in
    // restructurePlan — leaving a big reorganize unappliable. (audit E10)
    private const int MaxIpcFrameBytes = 32 * 1024 * 1024;

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

    /// Authoritative processed-file count from the last ScanComplete (the
    /// engine's final total). The completed-scan summary reads this instead of
    /// LastProgress.Processed, which can be throttle-stale by up to one batch.
    public ulong LastScanProcessedFiles { get; private set; }
    public Task StartScanAsync(string rootPath, string? rootDisplay = null, bool rescan = false)
    {
        _scanStartedAt = DateTime.UtcNow;
        _shownPhaseRank = -1;
        // Clear stale Deep Analyze latches so the pipeline strip doesn't jump a
        // fresh (re)scan straight to "Done" off a prior session's
        // DeepAnalyzeComplete. Done here — the common path for ALL scan starts
        // (incl. Settings "Force re-tag") — not only the optimistic-UI hook.
        DeepAnalyzeComplete = null;
        DeepAnalyzeProgress = null;
        DeepAnalyzeStarting = null;
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
        "deep_analyze_already_running" => true,
        // A concurrent RunFaceClustering bounced off the engine's single-flight
        // guard — a manual Re-cluster while clustering is already running is a
        // benign "already busy" notice, not a scary red error.
        "face_clustering_busy" => true,
        _ => false,
    };

    /// <summary>Pre-flip Phase to <see cref="ScanPhase.Discovering"/> as soon
    /// as the user clicks Start Scan, so the sidebar transitions out of the
    /// idle panel before the engine's first PhaseChanged event lands. The
    /// engine's own Discovering event echoes the same value (no-op); any
    /// real phase transition takes over immediately afterwards.</summary>
    public void SetOptimisticScanningPhase()
    {
        _shownPhaseRank = -1;
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
        // Optimistic UI flip: clear the "in-flight" indicators immediately
        // so the sidebar drops back to Idle within microseconds. The engine
        // will follow up with PhaseChanged(Cancelled) + a possible final
        // Progress event; both are no-ops on the already-cleared state.
        // Without this, _scanStartedAt + LastProgress + LastBatch + IsPaused
        // retained their prior-scan values until the next scan started, so
        // the sidebar's "Scan complete — N files in MM:SS" panel showed the
        // STALE values from before the cancel.
        IsPaused = false;
        _scanStartedAt = null;
        LastProgress = null;
        LastBatch = null;
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
        Interlocked.Exchange(ref _expectingExitAtTicks, DateTime.UtcNow.Ticks);
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

    /// <summary>Run a bulk command and await its <c>BulkActionResult</c> reply,
    /// matched by the action prefix the engine tags replies with
    /// (e.g. "trashFiles", "applyTags", "renameFiles", "restoreFromTrash"). Mirrors
    /// <see cref="WipeLibraryAndWaitAsync"/>: callers can then surface
    /// Succeeded/Failed instead of fire-and-forgetting (the silent-failure class —
    /// "user thinks files were deleted but they weren't"). Throws TimeoutException
    /// if no matching reply lands. The separate UndoStack listener still captures
    /// the same result for undo independently.</summary>
    public async Task<BulkActionResult> WaitForBulkActionResultAsync(
        string actionPrefix, Func<Task> send, TimeSpan timeout, CancellationToken ct = default)
    {
        var tcs = new TaskCompletionSource<BulkActionResult>(TaskCreationOptions.RunContinuationsAsynchronously);
        PropertyChangedEventHandler? handler = null;
        handler = (_, e) =>
        {
            if (e.PropertyName == nameof(LastBulkAction)
                && LastBulkAction is { } r
                && r.Action is { } a
                && a.StartsWith(actionPrefix, StringComparison.Ordinal))
            {
                PropertyChanged -= handler;
                tcs.TrySetResult(r);
            }
        };
        // Reset first so a value-equal reply still re-fires PropertyChanged.
        LastBulkAction = null;
        PropertyChanged += handler;
        try
        {
            await send().ConfigureAwait(false);
            using var cts = CancellationTokenSource.CreateLinkedTokenSource(ct);
            cts.CancelAfter(timeout);
            using var reg = cts.Token.Register(() =>
            {
                PropertyChanged -= handler;
                tcs.TrySetException(new TimeoutException(
                    $"Engine did not confirm '{actionPrefix}' within {timeout.TotalSeconds:0}s."));
            });
            return await tcs.Task.ConfigureAwait(false);
        }
        finally
        {
            PropertyChanged -= handler;
        }
    }

    /// <summary>tell the engine to re-probe CUDA/cuDNN
    /// availability. Engine replies with a <c>hardwareReprobed</c> event
    /// which lands on <see cref="LastHardwareReprobe"/>. Used by
    /// Settings → Performance "Verify install" so the user gets
    /// immediate feedback after installing cuDNN, without an engine
    /// restart.</summary>
    public Task VerifyCudaPackAsync() => SendCommandAsync(new VerifyCudaPackCommand());
    /// <summary>Send deepAnalyzeFile and await the engine's terminal
    /// <c>DeepAnalyzeComplete</c> reply (the single-file handler always emits
    /// one — on success, analyze failure, AND the no-model early return), so a
    /// stuck or no-model run surfaces instead of fire-and-forgetting (the user
    /// otherwise sees the stream card stay open with no result and no error).
    /// Mirrors the awaited-bounded pattern in <see cref="WaitForBulkActionResultAsync"/>;
    /// the IPC wire shape is unchanged. A single VLM caption can be slow, so the
    /// timeout is generous and a no-response is surfaced as a warning (the run
    /// may still be in flight) rather than a hard error.</summary>
    public async Task DeepAnalyzeFileAsync(long fileId, string modelKind)
    {
        var tcs = new TaskCompletionSource<FileID.IpcSchema.DeepAnalyzeComplete>(TaskCreationOptions.RunContinuationsAsynchronously);
        PropertyChangedEventHandler? handler = null;
        handler = (_, e) =>
        {
            if (e.PropertyName == nameof(DeepAnalyzeComplete) && DeepAnalyzeComplete is { } r)
            {
                PropertyChanged -= handler;
                tcs.TrySetResult(r);
            }
        };
        DeepAnalyzeComplete = null;
        PropertyChanged += handler;
        try
        {
            await SendCommandAsync(new DeepAnalyzeFileCommand(fileId, modelKind)).ConfigureAwait(false);
            using var cts = new CancellationTokenSource(DeepAnalyzeFileTimeout);
            using var reg = cts.Token.Register(() =>
            {
                PropertyChanged -= handler;
                tcs.TrySetException(new TimeoutException(
                    $"Engine did not confirm deepAnalyzeFile({fileId}) within {DeepAnalyzeFileTimeout.TotalSeconds:0}s."));
            });
            var result = await tcs.Task.ConfigureAwait(false);
            if (!result.Cancelled && result.Failed > 0)
            {
                // This runs on the ConfigureAwait(false) thread-pool continuation;
                // marshal the observable write to the UI thread so its
                // PropertyChanged never fires off-thread into x:Bind. (audit A12)
                _ui.TryEnqueue(() => LastWarning = new EngineError(
                    "deep_analyze_file_failed",
                    "Deep Analyze couldn't process this file. It may be an unsupported format, or the model isn't installed yet.",
                    null,
                    modelKind));
            }
        }
        catch (TimeoutException)
        {
            _ui.TryEnqueue(() => LastWarning = new EngineError(
                "deep_analyze_no_confirm",
                $"Deep Analyze hasn't responded in {DeepAnalyzeFileTimeout.TotalMinutes:0} minutes. It may still be running on a large model — check the stream, or cancel and retry if it stays stuck.",
                null,
                modelKind));
        }
        finally
        {
            PropertyChanged -= handler;
        }
    }

    /// <summary>Ceiling for a single-file Deep Analyze before we surface a
    /// "no response" warning. Generous: a 7B VLM captioning one image on CPU
    /// can run well over a minute, and we must NOT abort a healthy slow run —
    /// this only guards a genuinely wedged engine.</summary>
    private static readonly TimeSpan DeepAnalyzeFileTimeout = TimeSpan.FromMinutes(5);
    public Task DeepAnalyzeFolderAsync(string pathPrefix, string modelKind) =>
        SendCommandAsync(new DeepAnalyzeFolderCommand(pathPrefix, modelKind));
    // tagsOnly = the fast background auto-tag pass (one VLM call/file). The
    // manual Deep Analyze pass leaves it false → full caption + rename + tags.
    public Task DeepAnalyzeAllAsync(string modelKind, bool skipExisting, bool tagsOnly = false, bool proposeRenames = true) =>
        SendCommandAsync(new DeepAnalyzeAllCommand(modelKind, skipExisting, tagsOnly, proposeRenames));
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

    private bool IsPrewarmCancelled(string modelKind) =>
        Interlocked.CompareExchange(ref _prewarmCancelAllRequested, 0, 0) == 1
        || _prewarmCancelledKinds.ContainsKey(modelKind);

    public Task PrewarmModelAsync(string modelKind)
    {
        DebugLog.Info($"[INSTALL] EngineClient.PrewarmModelAsync('{modelKind}') called. State={State}, _stdin={(_stdin is null ? "NULL" : "alive")}");
        // A fresh prewarm means the user wants downloads: clear this kind's cancel
        // mark and any pending cancel-all so its (and others') stall guards re-arm.
        Interlocked.Exchange(ref _prewarmCancelAllRequested, 0);
        _prewarmCancelledKinds.TryRemove(modelKind, out _);
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
        };
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
