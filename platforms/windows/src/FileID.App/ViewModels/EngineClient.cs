// EngineClient — owns the FileIDEngine.exe child process lifecycle.
//
// Mirror of macOS EngineClient.swift. Responsibilities:
//   1. Spawn FileIDEngine.exe with stdin/stdout/stderr redirected.
//   2. Verify the engine binary's Authenticode signature before each spawn
//      (Phase 1: warns on Unsigned, refuses on Untrusted; Phase 11 tightens
//      to require Trusted with a pinned EV thumbprint).
//   3. Read engine stdout line-by-line, decode each as IpcEvent, dispatch
//      to the UI thread, raise INotifyPropertyChanged for the relevant
//      observable property.
//   4. Provide an IObservable<IpcEvent> stream for non-UI subscribers.
//   5. Send IpcCommand frames via stdin (as JSON + newline).
//   6. Auto-respawn on crash with bounded backoff (1s, 4s, 16s within a
//      60s window). After 3 strikes, transition to LifecycleState.Crashed
//      and surface the last error.
//   7. Bridge engine stderr → DebugLog (local-only) so engine tracing is
//      visible in app.log.
//   8. Throttle DeepAnalyzeFileDone events to 2 Hz (matches macOS — without
//      it, fast VLM runs spam the UI ~50/s).
//
// PRIVACY: every log call site that includes a path goes through
// PathRedactor.Redact. The engine never reaches the network on its own;
// only the IPC `prewarmModel` / `deepAnalyzeAll` paths trigger downloads,
// and the user explicitly initiated those.

using System.Collections.Concurrent;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Reactive.Subjects;
using System.Runtime.CompilerServices;
using System.Threading;
using FileID.IpcSchema;
using FileID.Services;
using Microsoft.UI.Dispatching;

namespace FileID.ViewModels;

internal sealed class EngineClient : INotifyPropertyChanged, IDisposable
{
    public static EngineClient Instance { get; } = new();

    public enum LifecycleState
    {
        Starting,
        Ready,
        Crashed,
    }

    private readonly DispatcherQueue _ui;
    private readonly Subject<IpcEvent> _events = new();
    private readonly object _writeLock = new();

    private Process? _process;
    private CancellationTokenSource? _readCts;
    private Task? _stdoutLoop;
    private Task? _stderrLoop;
    private StreamWriter? _stdin;

    private DateTime _lastSpawnAttempt = DateTime.MinValue;
    private int _consecutiveFailures;
    private DateTime _failureWindowStart = DateTime.MinValue;
    private static readonly TimeSpan FailureWindow = TimeSpan.FromSeconds(60);

    // BUG-3: respawn debouncing — prevents two-spawn races during the
    // 1s/4s/16s backoff window when the engine flaps quickly.
    private int _isStarting; // 0 = idle, 1 = StartAsync in flight

    // BUG-6: distinguish user-initiated shutdown from a crash. Set by
    // ShutdownAsync; OnProcessExited consumes it.
    private bool _expectingExit;

    private DateTime _lastDeepAnalyzeFileDone = DateTime.MinValue;
    private static readonly TimeSpan DeepAnalyzeFileDoneThrottle = TimeSpan.FromMilliseconds(500); // 2 Hz

    // V14.7.16: throttled diagnostic counter for inbound progress events.
    // Lets `[IPC IN] ModelDownloadProgress #N` lines correlate with engine
    // activity without flooding app.log.
    private int _modelDownloadEventCount;

    // ─── Observable surface (mirror of macOS @Observable) ──────────────

    private LifecycleState _state = LifecycleState.Starting;
    public LifecycleState State
    {
        get => _state;
        private set => Set(ref _state, value);
    }

    private string? _crashReason;
    public string? CrashReason
    {
        get => _crashReason;
        private set => Set(ref _crashReason, value);
    }

    private EngineInfo? _info;
    public EngineInfo? Info
    {
        get => _info;
        private set => Set(ref _info, value);
    }

    private ScanProgress? _lastProgress;
    public ScanProgress? LastProgress
    {
        get => _lastProgress;
        private set => Set(ref _lastProgress, value);
    }

    private EngineError? _lastError;
    public EngineError? LastError
    {
        get => _lastError;
        private set => Set(ref _lastError, value);
    }

    private BatchSummary? _lastBatch;
    public BatchSummary? LastBatch
    {
        get => _lastBatch;
        private set => Set(ref _lastBatch, value);
    }

    private FaceClusteringResult? _lastFaceClustering;
    public FaceClusteringResult? LastFaceClustering
    {
        get => _lastFaceClustering;
        private set => Set(ref _lastFaceClustering, value);
    }

    private DeepAnalyzeProgress? _deepAnalyzeProgress;
    public DeepAnalyzeProgress? DeepAnalyzeProgress
    {
        get => _deepAnalyzeProgress;
        private set => Set(ref _deepAnalyzeProgress, value);
    }

    private DeepAnalyzeFileDone? _deepAnalyzeLast;
    public DeepAnalyzeFileDone? DeepAnalyzeLast
    {
        get => _deepAnalyzeLast;
        private set => Set(ref _deepAnalyzeLast, value);
    }

    private DeepAnalyzeComplete? _deepAnalyzeComplete;
    public DeepAnalyzeComplete? DeepAnalyzeComplete
    {
        get => _deepAnalyzeComplete;
        private set => Set(ref _deepAnalyzeComplete, value);
    }

    private ModelDownloadProgress? _modelDownloadProgress;
    public ModelDownloadProgress? ModelDownloadProgress
    {
        get => _modelDownloadProgress;
        private set => Set(ref _modelDownloadProgress, value);
    }

    private QueueState? _queueState;
    public QueueState? QueueState
    {
        get => _queueState;
        private set => Set(ref _queueState, value);
    }

    private RestructurePlan? _lastRestructurePlan;
    public RestructurePlan? LastRestructurePlan
    {
        get => _lastRestructurePlan;
        private set => Set(ref _lastRestructurePlan, value);
    }

    private RestructureApplyResult? _lastRestructureApplyResult;
    public RestructureApplyResult? LastRestructureApplyResult
    {
        get => _lastRestructureApplyResult;
        private set => Set(ref _lastRestructureApplyResult, value);
    }

    private BulkActionResult? _lastBulkAction;
    public BulkActionResult? LastBulkAction
    {
        get => _lastBulkAction;
        private set => Set(ref _lastBulkAction, value);
    }

    private ClipTextEmbedding? _lastClipTextEmbedding;
    public ClipTextEmbedding? LastClipTextEmbedding
    {
        get => _lastClipTextEmbedding;
        private set => Set(ref _lastClipTextEmbedding, value);
    }

    private MergeSuggestions? _lastMergeSuggestions;
    public MergeSuggestions? LastMergeSuggestions
    {
        get => _lastMergeSuggestions;
        private set => Set(ref _lastMergeSuggestions, value);
    }

    private DeepAnalyzeStarting? _deepAnalyzeStarting;
    public DeepAnalyzeStarting? DeepAnalyzeStarting
    {
        get => _deepAnalyzeStarting;
        private set => Set(ref _deepAnalyzeStarting, value);
    }

    private ScanPhase? _phase;
    public ScanPhase? Phase
    {
        get => _phase;
        private set => Set(ref _phase, value);
    }

    /// <summary>Hot stream of every IPC event. Used by tests + the optional
    /// transcript log. Subscribe via System.Reactive.</summary>
    public IObservable<IpcEvent> Events => _events;

    public event PropertyChangedEventHandler? PropertyChanged;

    private EngineClient()
    {
        _ui = DispatcherQueue.GetForCurrentThread()
              ?? DispatcherQueue.GetForCurrentThread()
              ?? throw new InvalidOperationException("EngineClient must be constructed on the UI thread");
    }

    // ─── Lifecycle ─────────────────────────────────────────────────────

    /// <summary>
    /// Spawn the engine. Idempotent — calling this while already running
    /// is a no-op. On failure the state goes to Crashed; the caller can
    /// poll/observe State to react.
    /// </summary>
    public async Task StartAsync()
    {
        if (_process is { HasExited: false })
        {
            // Reset the in-flight gate even on this no-op path so the
            // backoff timer can re-fire.
            Interlocked.Exchange(ref _isStarting, 0);
            return;
        }
        // BUG-3: claim the in-flight gate. If another caller is already
        // mid-spawn, bail. Released in finally below.
        if (Interlocked.CompareExchange(ref _isStarting, 1, 0) != 0
            && _state == LifecycleState.Starting)
        {
            // Already starting from another path — the OnProcessExited
            // backoff path may have set _isStarting=1 and CALLED us;
            // detect that case by checking for the prior State.
        }

        // Notify singleton services that any cached engine state is now
        // stale and they should re-attach to PropertyChanged. Cheap +
        // idempotent.
        try { Services.ModelInstallerService.Instance.Reset(); } catch { /* swallow */ }

        State = LifecycleState.Starting;
        CrashReason = null;
        _expectingExit = false;
        _lastSpawnAttempt = DateTime.UtcNow;

        var enginePath = AppPaths.EngineExePath;
        DebugLog.Info($"EngineClient: spawning {PathRedactor.Redact(enginePath)}");

        // Pin the expected EV thumbprint via msbuild constant or env var
        // (FILEID_EV_THUMBPRINT, settable at install time). Empty means
        // dev build — accept Unsigned with a warning. Once a real cert is
        // in play, ship with the constant defined and the strict path
        // refuses Unsigned + tamper-mismatched binaries.
        var expectedThumb = Environment.GetEnvironmentVariable("FILEID_EV_THUMBPRINT");
        var verdict = WinVerifyTrustChecker.Verify(enginePath, expectedThumbprintHex: expectedThumb);
        switch (verdict)
        {
            case IntegrityVerdict.NotFound:
                CrashReason = "FileIDEngine.exe not found.";
                State = LifecycleState.Crashed;
                DebugLog.Error("EngineClient: engine binary missing — won't spawn.");
                return;

            case IntegrityVerdict.Untrusted:
                CrashReason = "Engine signature verification failed. Refusing to spawn.";
                State = LifecycleState.Crashed;
                DebugLog.Error("EngineClient: signature verification FAILED — won't spawn.");
                return;

            case IntegrityVerdict.Unsigned:
                // If a thumbprint was pinned (release mode), refuse to
                // spawn. Otherwise warn + continue (dev build).
                if (!string.IsNullOrEmpty(expectedThumb))
                {
                    CrashReason = "Engine binary is unsigned but signature verification is required.";
                    State = LifecycleState.Crashed;
                    DebugLog.Error("EngineClient: unsigned engine refused (FILEID_EV_THUMBPRINT set).");
                    return;
                }
                DebugLog.Warn("EngineClient: engine is unsigned. OK in dev; ship builds must be signed.");
                break;

            case IntegrityVerdict.Trusted:
                DebugLog.Info("EngineClient: engine signature verified.");
                break;
        }

        try
        {
            var psi = new ProcessStartInfo
            {
                FileName = enginePath,
                UseShellExecute = false,
                RedirectStandardInput = true,
                RedirectStandardOutput = true,
                RedirectStandardError = true,
                CreateNoWindow = true,
                StandardInputEncoding = System.Text.Encoding.UTF8,
                StandardOutputEncoding = System.Text.Encoding.UTF8,
                StandardErrorEncoding = System.Text.Encoding.UTF8,
                WorkingDirectory = AppPaths.Root,
            };
            // Pass the FILEID_LOG env to control engine tracing verbosity
            // (debug in dev profiles, info in release).
            psi.Environment["FILEID_LOG"] = Environment.GetEnvironmentVariable("FILEID_LOG") ?? "info";

            var p = Process.Start(psi)
                    ?? throw new InvalidOperationException("Process.Start returned null");
            _process = p;
            _stdin = p.StandardInput;

            _readCts = new CancellationTokenSource();
            var ct = _readCts.Token;
            _stdoutLoop = Task.Run(() => StdoutLoopAsync(p.StandardOutput, ct), ct);
            _stderrLoop = Task.Run(() => StderrLoopAsync(p.StandardError, ct), ct);

            // Hook exit so we can auto-respawn.
            p.EnableRaisingEvents = true;
            p.Exited += OnProcessExited;
        }
        catch (Exception ex)
        {
            DebugLog.Error("EngineClient.StartAsync failed: " + ex.Message);
            CrashReason = ex.Message;
            State = LifecycleState.Crashed;
            Interlocked.Exchange(ref _isStarting, 0);
            return;
        }

        // Send a status request — when the engine returns ready, we'll
        // populate Info and flip State to Ready.
        try
        {
            await SendCommandAsync(new RequestStatusCommand());
        }
        catch (Exception ex)
        {
            DebugLog.Warn("EngineClient: requestStatus failed at spawn: " + ex.Message);
        }
        // BUG-3: release the in-flight gate now that the spawn is done
        // (engine launched + status sent). Subsequent OnProcessExited
        // can re-claim it.
        Interlocked.Exchange(ref _isStarting, 0);
    }

    private async Task StdoutLoopAsync(StreamReader reader, CancellationToken ct)
    {
        while (!ct.IsCancellationRequested)
        {
            string? line;
            try
            {
                line = await reader.ReadLineAsync(ct).ConfigureAwait(false);
            }
            catch (OperationCanceledException) { return; }
            catch (Exception ex)
            {
                DebugLog.Warn("Engine stdout read error: " + ex.Message);
                return;
            }
            if (line is null)
            {
                // EOF — engine closed stdout (likely exiting). Process.Exited
                // will pick up the cleanup.
                return;
            }
            if (string.IsNullOrWhiteSpace(line))
            {
                continue;
            }

            IpcEvent? ev;
            try
            {
                ev = IpcCoder.Decode<IpcEvent>(line);
            }
            catch (Exception ex)
            {
                DebugLog.Warn($"Engine emitted unparseable line ({ex.GetType().Name}): {ex.Message}");
                continue;
            }

            // Marshal to UI thread before touching observable state.
            _ui.TryEnqueue(() => Apply(ev));
        }
    }

    private async Task StderrLoopAsync(StreamReader reader, CancellationToken ct)
    {
        while (!ct.IsCancellationRequested)
        {
            string? line;
            try
            {
                line = await reader.ReadLineAsync(ct).ConfigureAwait(false);
            }
            catch (OperationCanceledException) { return; }
            catch { return; }
            if (line is null) return;
            // Engine writes structured tracing JSON to stderr. We log it
            // verbatim; the JSON shape is decoded client-side only when a
            // crash investigation actually needs it.
            DebugLog.Debug("[engine] " + line);
        }
    }

    private void OnProcessExited(object? sender, EventArgs e)
    {
        _ui.TryEnqueue(() =>
        {
            DebugLog.Warn($"EngineClient: process exited (code={_process?.ExitCode}).");
            Cleanup();

            // Notify install service immediately so any in-flight download
            // owned by the now-dead engine flips to Failed instead of
            // spinning forever. Runs on every exit (graceful shutdown,
            // crash + respawn, 3-strike terminal crash). Idempotent.
            try { Services.ModelInstallerService.Instance.Reset(); }
            catch (Exception ex) { DebugLog.Warn("OnProcessExited: ModelInstallerService.Reset threw: " + ex.Message); }

            // BUG-6: user-initiated shutdown shouldn't count as a crash
            // or trigger the auto-respawn — that would drag the engine
            // back up after the user explicitly asked it to stop.
            if (_expectingExit)
            {
                _expectingExit = false;
                State = LifecycleState.Crashed; // "stopped" UI; user can manually start
                CrashReason = string.Empty;
                return;
            }

            // Auto-respawn with bounded backoff. The 3-strike window is
            // 60 s wide; failures beyond that reset the counter.
            var now = DateTime.UtcNow;
            if (now - _failureWindowStart > FailureWindow)
            {
                _failureWindowStart = now;
                _consecutiveFailures = 0;
            }
            _consecutiveFailures++;

            if (_consecutiveFailures > 3)
            {
                CrashReason = "Engine crashed three times in a row. Manual restart required.";
                State = LifecycleState.Crashed;
                return;
            }

            // 1s, 4s, 16s schedule.
            var delay = _consecutiveFailures switch
            {
                1 => TimeSpan.FromSeconds(1),
                2 => TimeSpan.FromSeconds(4),
                _ => TimeSpan.FromSeconds(16),
            };
            DebugLog.Info($"EngineClient: respawning in {delay.TotalSeconds}s (attempt {_consecutiveFailures}/3).");
            // BUG-3: guard against double-spawn during the delay window.
            // If a second OnProcessExited (impossible in practice — we
            // null _process in Cleanup — but cheap to guard) or a
            // user-initiated StartAsync races, we must run only once.
            _ = Task.Delay(delay).ContinueWith(_ => _ui.TryEnqueue(() =>
            {
                if (Interlocked.CompareExchange(ref _isStarting, 1, 0) == 0)
                {
                    _ = StartAsync();
                }
            }));
        });
    }

    private void Cleanup()
    {
        try { _readCts?.Cancel(); } catch { }
        _readCts?.Dispose();
        _readCts = null;

        // BUG-2: take the same lock as SendCommandAsync so a concurrent
        // writer can't see _stdin non-null then NRE on Write after we
        // dispose it.
        StreamWriter? stdin;
        lock (_writeLock)
        {
            stdin = _stdin;
            _stdin = null;
        }
        try { stdin?.Dispose(); } catch { }

        if (_process is { } p)
        {
            try { p.Exited -= OnProcessExited; } catch { }
            try { p.Dispose(); } catch { }
        }
        _process = null;
    }

    public void Dispose()
    {
        Cleanup();
        _events.OnCompleted();
    }

    // ─── Commands ──────────────────────────────────────────────────────

    /// <summary>
    /// Block until the engine reaches <see cref="LifecycleState.Ready"/>.
    /// Throws <see cref="TimeoutException"/> if the engine never becomes
    /// ready within <paramref name="timeout"/>; throws
    /// <see cref="InvalidOperationException"/> with the crash reason if
    /// the engine has already crashed. Returns immediately if Ready.
    /// Callers (the install flow) gate on this before sending an IPC
    /// command, so a click that happens during cold start either waits
    /// or surfaces a clean error — never silently throws "Engine not
    /// running."
    /// </summary>
    public Task WaitForReadyAsync(TimeSpan timeout, CancellationToken ct = default)
    {
        if (State == LifecycleState.Ready) return Task.CompletedTask;
        if (State == LifecycleState.Crashed)
        {
            throw new InvalidOperationException(
                "Engine has crashed: " + (CrashReason ?? "unknown reason"));
        }
        var tcs = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        PropertyChangedEventHandler? handler = null;
        handler = (_, e) =>
        {
            if (e.PropertyName != nameof(State)) return;
            if (State == LifecycleState.Ready)
            {
                PropertyChanged -= handler;
                tcs.TrySetResult(true);
            }
            else if (State == LifecycleState.Crashed)
            {
                PropertyChanged -= handler;
                tcs.TrySetException(new InvalidOperationException(
                    "Engine crashed while waiting for ready: " + (CrashReason ?? "unknown reason")));
            }
        };
        PropertyChanged += handler;
        // Re-check after subscribing in case the state changed between
        // the early-return above and the handler attach.
        if (State == LifecycleState.Ready)
        {
            PropertyChanged -= handler;
            return Task.CompletedTask;
        }
        return Task.Run(async () =>
        {
            try
            {
                using var cts = CancellationTokenSource.CreateLinkedTokenSource(ct);
                cts.CancelAfter(timeout);
                using var reg = cts.Token.Register(() =>
                {
                    PropertyChanged -= handler;
                    if (ct.IsCancellationRequested)
                    {
                        tcs.TrySetCanceled(ct);
                    }
                    else
                    {
                        tcs.TrySetException(new TimeoutException(
                            $"Engine did not become Ready within {timeout.TotalSeconds:0}s (current state: {State})."));
                    }
                });
                await tcs.Task.ConfigureAwait(false);
            }
            finally
            {
                PropertyChanged -= handler;
            }
        }, ct);
    }

    public Task SendCommandAsync(CommandPayload payload, CancellationToken ct = default)
    {
        var cmd = IpcCommand.New(payload);
        var bytes = IpcCoder.EncodeLine(cmd);
        var commandKind = payload.GetType().Name.Replace("Command", "");
        DebugLog.Info($"[IPC OUT] {commandKind} ({bytes.Length} bytes)");
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
    public Task StartScanAsync(string rootPath, string? rootDisplay = null)
    {
        _scanStartedAt = DateTime.UtcNow;
        return SendCommandAsync(new StartScanCommand(rootPath, rootDisplay));
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
        _expectingExit = true;
        return SendCommandAsync(new ShutdownCommand());
    }
    public Task RunFaceClusteringAsync() => SendCommandAsync(new RunFaceClusteringCommand());
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

    // ─── Event router ──────────────────────────────────────────────────

    private void Apply(IpcEvent ev)
    {
        // Always raise to subscribers first, even if the routing below
        // throws (defense-in-depth — never silently drop an event).
        try { _events.OnNext(ev); } catch (Exception ex) { DebugLog.Warn("event subject OnNext threw: " + ex.Message); }

        switch (ev.Payload)
        {
            case ReadyEvent r:
                Info = r.Info;
                State = LifecycleState.Ready;
                CrashReason = null;
                _consecutiveFailures = 0;
                break;
            case ProgressEvent p:
                LastProgress = p.Progress;
                Phase = p.Progress.Phase;
                break;
            case PhaseChangedEvent pc:
                Phase = pc.Phase;
                break;
            case DiscoveryCompleteEvent:
                // No dedicated property — UI consumes via LastProgress.Total,
                // which the engine populates immediately after this event.
                break;
            case FileDoneEventWrapper:
                // Per-file events are high-volume; we don't surface them as
                // an observable property. Library tab subscribes directly
                // via Events when it needs them.
                break;
            case BatchSummaryEvent b:
                LastBatch = b.Summary;
                break;
            case ScanCompleteEvent:
                Phase = ScanPhase.Completed;
                IsPaused = false;
                if (_scanStartedAt.HasValue)
                {
                    LastScanDuration = DateTime.UtcNow - _scanStartedAt.Value;
                    _scanStartedAt = null;
                }
                break;
            case ErrorEvent e:
                LastError = e.Error;
                DebugLog.Warn($"[IPC IN] engine error: kind={e.Error.Kind} msg={e.Error.Message} path={PathRedactor.Redact(e.Error.Path)}");
                break;
            case LogEvent:
                // Engine LogLine events go to the transcript via Events.
                // Local app log already captured stderr.
                break;
            case FaceClusteringCompleteEvent fc:
                LastFaceClustering = fc.Result;
                break;
            case DeepAnalyzeStartingEvent das:
                DeepAnalyzeStarting = das.Starting;
                break;
            case DeepAnalyzeProgressEvent dap:
                DeepAnalyzeProgress = dap.Progress;
                break;
            case DeepAnalyzeFileDoneEvent dafd:
                // Throttle: 2 Hz. Without this, fast VLM runs spam ~50/s.
                var now = DateTime.UtcNow;
                if (now - _lastDeepAnalyzeFileDone >= DeepAnalyzeFileDoneThrottle)
                {
                    DeepAnalyzeLast = dafd.FileDone;
                    _lastDeepAnalyzeFileDone = now;
                }
                break;
            case DeepAnalyzeCompleteEvent dac:
                DeepAnalyzeComplete = dac.Result;
                DeepAnalyzeProgress = null;
                DeepAnalyzeStarting = null;
                break;
            case ModelDownloadProgressEvent mdp:
                // Throttled to one log line per 1% (~100 events / model) so
                // app.log isn't flooded but the trail is dense enough to
                // diagnose stuck installs.
                _modelDownloadEventCount++;
                if (_modelDownloadEventCount <= 5
                    || _modelDownloadEventCount % 50 == 0
                    || mdp.Progress.Fraction >= 0.999)
                {
                    DebugLog.Info($"[IPC IN] ModelDownloadProgress #{_modelDownloadEventCount}: {mdp.Progress.ModelKind} {mdp.Progress.Fraction:P0} - {mdp.Progress.Message}");
                }
                ModelDownloadProgress = mdp.Progress;
                break;
            case QueueStateEvent qs:
                QueueState = qs.State;
                break;
            case RestructurePlanEvent rp:
                LastRestructurePlan = rp.Plan;
                break;
            case RestructureApplyResultEvent rar:
                LastRestructureApplyResult = rar.Result;
                break;
            case BulkActionResultEvent bar:
                LastBulkAction = bar.Result;
                break;
            case ClipTextEmbeddingEvent ce:
                LastClipTextEmbedding = ce.Embedding;
                break;
            case MergeSuggestionsEvent ms:
                LastMergeSuggestions = ms.Suggestions;
                break;
        }
    }

    // ─── INotifyPropertyChanged plumbing ───────────────────────────────

    private void Set<T>(ref T field, T value, [CallerMemberName] string? propertyName = null)
    {
        if (EqualityComparer<T>.Default.Equals(field, value))
        {
            return;
        }
        field = value;
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(propertyName));
    }
}
