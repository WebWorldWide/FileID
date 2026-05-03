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

    private DateTime _lastDeepAnalyzeFileDone = DateTime.MinValue;
    private static readonly TimeSpan DeepAnalyzeFileDoneThrottle = TimeSpan.FromMilliseconds(500); // 2 Hz

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
            return;
        }

        // Notify singleton services that any cached engine state is now
        // stale and they should re-attach to PropertyChanged. Cheap +
        // idempotent.
        try { Services.ModelInstallerService.Instance.Reset(); } catch { /* swallow */ }

        State = LifecycleState.Starting;
        CrashReason = null;
        _lastSpawnAttempt = DateTime.UtcNow;

        var enginePath = AppPaths.EngineExePath;
        DebugLog.Info($"EngineClient: spawning {PathRedactor.Redact(enginePath)}");

        // Phase 1: don't pin a thumbprint (no EV cert yet). Phase 11
        // supplies the published EV thumbprint and tightens the gate.
        var verdict = WinVerifyTrustChecker.Verify(enginePath, expectedThumbprintHex: null);
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
            _ = Task.Delay(delay).ContinueWith(_ => _ui.TryEnqueue(() => _ = StartAsync()));
        });
    }

    private void Cleanup()
    {
        try { _readCts?.Cancel(); } catch { }
        _readCts?.Dispose();
        _readCts = null;

        try { _stdin?.Dispose(); } catch { }
        _stdin = null;

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

    public Task SendCommandAsync(CommandPayload payload, CancellationToken ct = default)
    {
        var cmd = IpcCommand.New(payload);
        var bytes = IpcCoder.EncodeLine(cmd);
        // The engine's stdin reader handles concurrent writers because
        // our writes are atomic per-line, but we still serialize through a
        // lock to make the byte order deterministic for log correlation.
        return Task.Run(() =>
        {
            ct.ThrowIfCancellationRequested();
            lock (_writeLock)
            {
                if (_stdin is null)
                {
                    throw new InvalidOperationException("Engine not running.");
                }
                _stdin.BaseStream.Write(bytes, 0, bytes.Length);
                _stdin.BaseStream.Flush();
            }
        }, ct);
    }

    public Task StartScanAsync(string rootPath, string? rootDisplay = null) =>
        SendCommandAsync(new StartScanCommand(rootPath, rootDisplay));

    public Task PauseScanAsync() => SendCommandAsync(new PauseScanCommand());
    public Task ResumeScanAsync() => SendCommandAsync(new ResumeScanCommand());
    public Task CancelScanAsync() => SendCommandAsync(new CancelScanCommand());
    public Task RequestStatusAsync() => SendCommandAsync(new RequestStatusCommand());
    public Task ShutdownAsync() => SendCommandAsync(new ShutdownCommand());
    public Task RunFaceClusteringAsync() => SendCommandAsync(new RunFaceClusteringCommand());
    public Task DeepAnalyzeFileAsync(long fileId, string modelKind) =>
        SendCommandAsync(new DeepAnalyzeFileCommand(fileId, modelKind));
    public Task DeepAnalyzeFolderAsync(string pathPrefix, string modelKind) =>
        SendCommandAsync(new DeepAnalyzeFolderCommand(pathPrefix, modelKind));
    public Task DeepAnalyzeAllAsync(string modelKind, bool skipExisting) =>
        SendCommandAsync(new DeepAnalyzeAllCommand(modelKind, skipExisting));
    public Task DeepAnalyzeCancelAsync() => SendCommandAsync(new DeepAnalyzeCancelCommand());
    public Task PrewarmModelAsync(string modelKind) =>
        SendCommandAsync(new PrewarmModelCommand(modelKind));
    public Task CancelPrewarmAsync() => SendCommandAsync(new CancelPrewarmCommand());
    public Task PlanRestructureAsync(string libraryRoot) =>
        SendCommandAsync(new PlanRestructureCommand(libraryRoot));
    public Task ApplyRestructureAsync(string libraryRoot, IReadOnlyList<RestructureMove> moves, bool useSymlinks) =>
        SendCommandAsync(new ApplyRestructureCommand(libraryRoot, moves, useSymlinks));
    public Task AutoPilotAsync(string libraryRoot, string? vlmModelKind = null) =>
        SendCommandAsync(new AutoPilotCommand(libraryRoot, vlmModelKind));

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

    public Task FindMergeSuggestionsAsync() =>
        SendCommandAsync(new FindMergeSuggestionsCommand());

    public Task EmbedImageQueryAsync(long fileId, string queryId) =>
        SendCommandAsync(new EmbedImageQueryCommand(fileId, queryId));

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
                break;
            case ErrorEvent e:
                LastError = e.Error;
                DebugLog.Warn($"engine error: kind={e.Error.Kind} msg={e.Error.Message} path={PathRedactor.Redact(e.Error.Path)}");
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
