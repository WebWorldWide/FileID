// Owns the FileIDEngine.exe child process: spawns it after verifying its Authenticode signature (warns on Unsigned, refuses on Untrusted), reads stdout as IpcEvent frames,
// dispatches them to the UI thread (INotifyPropertyChanged plus an IObservable<IpcEvent> for non-UI subscribers), sends IpcCommand frames on stdin, bridges stderr to DebugLog,
// throttles DeepAnalyzeFileDone to 2 Hz, and auto-respawns on crash with bounded backoff (1s, 4s, 16s within 60s) before settling in LifecycleState.Crashed after 3 strikes.
// Privacy: every log call site that includes a path goes through PathRedactor.Redact. The engine never reaches the network on its own; only the user-initiated IPC `prewarmModel` / `deepAnalyzeAll` paths trigger downloads.

using System.Collections.Concurrent;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Reactive.Subjects;
using System.Runtime.CompilerServices;
using System.Text;
using System.Threading;
using FileID.IpcSchema;
using FileID.Services;
using Microsoft.UI.Dispatching;

namespace FileID.ViewModels;

internal sealed partial class EngineClient : INotifyPropertyChanged, IDisposable
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
    private readonly GenerationHealthWaiters _healthWaiters = new();
    private int _transportFailureGeneration = -1;

    // Bounded stdout framing: a wedged engine that never emits a newline cannot grow an unbounded buffer. 64 MiB fits a ~200k-move restructurePlan; the engine caps outbound frames at the same size (sink.rs MAX_FRAME_BYTES) and sends a structured ipc_frame_too_large error instead, so all four caps stay symmetric. An oversize drop surfaces as a visible error (see StdoutLoopAsync).
    private const int MaxFrameBytes = 64 * 1024 * 1024;

    /// <summary>Per-loop stdout framing state, owned by one StdoutLoopAsync invocation so an overlapping loop from a respawn can't race another's buffer/resync flag.</summary>
    internal sealed class StdoutFraming
    {
        public readonly StringBuilder Buffer = new();
        public readonly char[] Chunk = new char[16 * 1024];
        public readonly Encoder Utf8Encoder = Encoding.UTF8.GetEncoder();
        public long BufferedBytes;
        // Leading buffer chars already confirmed newline-free, so a multi-MB frame isn't rescanned from index 0 on every chunk (O(n^2) pegged a core for minutes on a large restructurePlan).
        public int Scanned;
        public bool Resyncing;
        public bool OversizeDropped;
    }

    private Process? _process;
    private int _spawnGeneration;
    private int _gpuDeviceRemovedGeneration = -1;
    private CancellationTokenSource? _readCts;
    private Task? _stdoutLoop;
    private Task? _stderrLoop;
    private StreamWriter? _stdin;

    private DateTime _lastSpawnAttempt = DateTime.MinValue;
    private int _consecutiveFailures;
    private DateTime _failureWindowStart = DateTime.MinValue;
    private static readonly TimeSpan FailureWindow = TimeSpan.FromSeconds(60);
    // The engine must stay Ready at least this long before a crash counts as recovery and clears the strike counter; a shorter Ready→crash is a flap that must keep ticking toward the 3-strike cap.
    private DateTime _lastReadyAt = DateTime.MinValue;
    private static readonly TimeSpan StabilitySettle = TimeSpan.FromSeconds(30);

    private int _isStarting; // 0 = idle, 1 = StartAsync in flight

    // Distinguishes user-initiated shutdown from a crash: set by ShutdownAsync, consumed by OnProcessExited. A concurrent map rather than a bool because OnProcessExited fires on whichever thread detects exit, and bool reads can tear on weakly-ordered ARM64.
    private readonly ConcurrentDictionary<Process, long> _expectedExitProcesses = new();
    private static readonly TimeSpan ExpectingExitWindow = TimeSpan.FromSeconds(60);

    private DateTime _lastDeepAnalyzeFileDone = DateTime.MinValue;
    private static readonly TimeSpan DeepAnalyzeFileDoneThrottle = TimeSpan.FromMilliseconds(500); // 2 Hz

    private int _autoDeepAnalyzeInFlight;

    // Re-entrancy gate for the auto-triggered face-clustering pass. A rescan emits a SECOND ScanComplete, and the engine spawns one clustering task per IPC with no dedup, so two passes would race the same persons / face_prints.person_id writes. Released on FaceClusteringComplete or a clustering error.
    private int _faceClusterAutoInFlight;

    // Throttle for scan FileDone events: a fast scan emits hundreds per second, so sample every Nth rather than inflate UI work for every subscriber.
    private int _scanFileDoneEventCounter;
    private const int ScanFileDoneSampleN = 5;

    // ScanProgress throttle (10 Hz): the engine emits 100+ per second on a fast scan. Phase transitions bypass it.
    private DateTime _lastProgressEmit = DateTime.MinValue;
    private ScanPhase? _lastProgressPhase;

    // Highest phase rank shown during the current scan. Discovery and tagging run concurrently and their ProgressEvents interleave, so without this the displayed Phase (and sidebar label/icon/dot) flickers Discovering<->Tagging. A ProgressEvent may only ADVANCE it; the authoritative PhaseChangedEvent sets Phase directly and re-syncs this latch. Reset to -1 at each scan start (EngineClient.Commands.cs).
    private int _shownPhaseRank = -1;

    private static int PhaseRank(ScanPhase phase) => phase switch
    {
        ScanPhase.Idle => 0,
        ScanPhase.Discovering => 1,
        ScanPhase.Tagging => 2,
        ScanPhase.PostScan => 3,
        ScanPhase.Completed => 4,
        // Terminal states sit above the progression so a late interleaved ProgressEvent can't clamp them away.
        ScanPhase.Cancelled => 5,
        ScanPhase.Failed => 5,
        _ => 0,
    };
    private static readonly TimeSpan ProgressThrottle = TimeSpan.FromMilliseconds(100); // 10 Hz

    private int _modelDownloadEventCount;

    // Monotonic Apply-call counter for the [APPLY:N] enter/exit tracing. A native fast-fail killed the app with no managed exception or crash dump; the highest-numbered `enter` without a matching `exit` in app.log identifies the killer event.
    private int _applySeq;

    private LifecycleState _state = LifecycleState.Starting;
    internal const string StoppedReason = "Engine stopped";
    public LifecycleState State
    {
        get => _state;
        private set => Set(ref _state, value);
    }
    public int SpawnGeneration => Volatile.Read(ref _spawnGeneration);

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
        private set
        {
            if (ReferenceEquals(_lastError, value)) return;
            _lastError = value;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(LastError)));
        }
    }

    public const string GpuRestartRequiredMessage =
        "The graphics device was removed while FileID was using it. Restart the engine before starting another scan.";

    private bool _gpuDeviceRemoved;
    public bool GpuDeviceRemoved
    {
        get => _gpuDeviceRemoved;
        private set => Set(ref _gpuDeviceRemoved, value);
    }

    private EngineError? _lastWarning;
    public EngineError? LastWarning
    {
        get => _lastWarning;
        set => Set(ref _lastWarning, value);
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

    public bool DeepAnalyzeCommandInFlight => _deepAnalyzeSlot.Current is not null;
    public long DeepAnalyzeCommandAttemptId => _deepAnalyzeSlot.Current?.AttemptId ?? 0;

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
    private ToolResponse? _lastToolResponse;
    public ToolResponse? LastToolResponse
    {
        get => _lastToolResponse;
        private set => Set(ref _lastToolResponse, value);
    }
    private CatalogResponse? _lastCatalogResponse;
    public CatalogResponse? LastCatalogResponse
    {
        get => _lastCatalogResponse;
        private set => Set(ref _lastCatalogResponse, value);
    }
    public RestructurePlan? LastRestructurePlan
    {
        get => _lastRestructurePlan;
        private set => Set(ref _lastRestructurePlan, value);
    }

    private RestructureApplyResult? _lastRestructureApplyResult;
    public RestructureApplyResult? LastRestructureApplyResult
    {
        get => _lastRestructureApplyResult;
        private set
        {
            if (ReferenceEquals(_lastRestructureApplyResult, value)) return;
            _lastRestructureApplyResult = value;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(LastRestructureApplyResult)));
        }
    }

    private bool _canUndoRestructure;
    public bool CanUndoRestructure
    {
        get => _canUndoRestructure;
        private set => Set(ref _canUndoRestructure, value);
    }
    /// Set by UndoRestructureAsync so the next RestructureApplyResult is read as
    /// the undo's reply (clears CanUndoRestructure) rather than a fresh apply.
    internal bool UndoRestructureInFlight { get; set; }
    private string? UndoRestructureShortcutToken { get; set; }

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

    private ThumbnailGenerated? _lastThumbnailGenerated;
    public ThumbnailGenerated? LastThumbnailGenerated
    {
        get => _lastThumbnailGenerated;
        private set => Set(ref _lastThumbnailGenerated, value);
    }

    private HardwareReprobed? _lastHardwareReprobe;
    public HardwareReprobed? LastHardwareReprobe
    {
        get => _lastHardwareReprobe;
        private set => Set(ref _lastHardwareReprobe, value);
    }

    private LibraryWiped? _lastLibraryWiped;
    public LibraryWiped? LastLibraryWiped
    {
        get => _lastLibraryWiped;
        set => Set(ref _lastLibraryWiped, value);
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

    public IObservable<IpcEvent> Events => _events;

    public event PropertyChangedEventHandler? PropertyChanged;

    private EngineClient()
    {
        // The singleton MUST be first-touched on the UI thread (App.OnLaunched ensures this): from a thread-pool thread GetForCurrentThread returns null with no recovery and every later _ui.TryEnqueue would silently no-op. Throw early instead.
        _ui = DispatcherQueue.GetForCurrentThread()
              ?? throw new InvalidOperationException(
                  "EngineClient must be constructed on the UI thread. "
                  + "First-touch the singleton from App.OnLaunched, not from a Task.Run continuation.");
    }

    public async Task StartAsync()
    {
        if (!_ui.HasThreadAccess)
        {
            var completion = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
            if (!_ui.TryEnqueue(async () =>
            {
                try
                {
                    await StartCoreAsync();
                    completion.TrySetResult();
                }
                catch (Exception ex)
                {
                    completion.TrySetException(ex);
                }
            }))
            {
                throw new InvalidOperationException("The UI dispatcher is unavailable; the engine cannot be started safely.");
            }
            await completion.Task.ConfigureAwait(false);
            return;
        }

        await StartCoreAsync();
    }

    private async Task StartCoreAsync()
    {
        if (_process is { HasExited: false })
        {
            // Engine is already running. Don't touch _isStarting: another caller may hold the gate while completing a spawn we collided with.
            return;
        }
        // Strict CAS gate: losing the CAS returns immediately; otherwise a second StartAsync could spawn a parallel engine on a shared stdin/stdout pair (the second engine crashed on bind or fed corrupt IPC).
        if (Interlocked.CompareExchange(ref _isStarting, 1, 0) != 0)
        {
            DebugLog.Info("EngineClient.StartAsync: spawn already in flight; skipping.");
            return;
        }

        // Every path below (signature verdicts, hash failures, the spawn catch) must release `_isStarting` or the gate latches at 1 and OnProcessExited's respawn can't claim it, hence try/finally.
        try
        {
            if (_process is { HasExited: true } exitedProcess && ReferenceEquals(exitedProcess, _process))
            {
                ResetProcessBoundScanState();
                Phase = ScanPhase.Idle;
                LastError = null;
                LastWarning = null;
                Cleanup();
            }

            try { Services.ModelInstallerService.Instance.Reset(); } catch { /* swallow */ }

            State = LifecycleState.Starting;
            CrashReason = null;
            _lastSpawnAttempt = DateTime.UtcNow;

            var enginePath = AppPaths.EngineExePath;
            DebugLog.Info($"EngineClient: spawning {PathRedactor.Redact(enginePath)}");

            // Expected EV thumbprint from FILEID_SIGN_THUMBPRINT (msbuild constant or env var). Empty means a dev build: accept Unsigned with a warning; with a real cert the strict path refuses Unsigned and tampered binaries.
            var expectedThumb = Environment.GetEnvironmentVariable("FILEID_SIGN_THUMBPRINT");
            var expectedSubject = ReleaseSigningPolicy.ExpectedSignerSubject;
            var requireSignedEngine = ReleaseSigningPolicy.RequireSignedEngine
                || !string.IsNullOrWhiteSpace(expectedThumb);
            var expectedSignerPublicKey = ReleaseSigningPolicy.ExpectedSignerPublicKeySha256;
            if (ReleaseSigningPolicy.RequireSignedEngine)
            {
                if (string.IsNullOrWhiteSpace(expectedSignerPublicKey))
                {
                    CrashReason = "Engine signature verification failed because the release signer identity is missing.";
                    State = LifecycleState.Crashed;
                    DebugLog.Error("EngineClient: signed-release signer public-key policy is missing.");
                    return;
                }

                var appAssemblyPath = System.Reflection.Assembly.GetExecutingAssembly().Location;
                var appVerdict = await Task.Run(() => WinVerifyTrustChecker.Verify(
                    appAssemblyPath,
                    expectedSignerSubject: expectedSubject,
                    expectedSignerPublicKeySha256: expectedSignerPublicKey));
                if (appVerdict != IntegrityVerdict.Trusted)
                {
                    CrashReason = "Engine signature verification failed because the FileID app signer could not be verified.";
                    State = LifecycleState.Crashed;
                    DebugLog.Error("EngineClient: signed-release app assembly did not match the approved signer identity.");
                    return;
                }
            }
            FileStream? spawnPin = null;
            try
            {
                spawnPin = new FileStream(enginePath, FileMode.Open, FileAccess.Read, FileShare.Read);
            }
            catch (FileNotFoundException)
            {
            }
            catch (DirectoryNotFoundException)
            {
            }
            catch (Exception ex)
            {
                CrashReason = "Engine binary could not be pinned for verification: " + ex.Message;
                State = LifecycleState.Crashed;
                DebugLog.Error("EngineClient: failed to lease engine image before verification.");
                return;
            }
            using var spawnPinLease = spawnPin;
            // Off the UI thread: WinVerifyTrust hashes the multi-MB engine binary AND can make an OCSP/CRL round-trip, on startup before first paint and on every crash-respawn. Task.Run keeps the gate (the spawn still waits for the verdict) while unblocking the UI.
            var verdict = await Task.Run(() => WinVerifyTrustChecker.Verify(
                enginePath,
                expectedThumbprintHex: expectedThumb,
                expectedSignerSubject: expectedSubject,
                expectedSignerPublicKeySha256: expectedSignerPublicKey));
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
                    if (requireSignedEngine)
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

            // TOCTOU mitigation. Hash the binary AFTER WinVerifyTrust returns, then re-hash and compare immediately before Process.Start; if an adversary swaps the binary in between, we abort. Skipped in dev (no thumbprint pinned) because rebuilds change the hash legitimately. Hashing runs off the UI thread (~95 MB).
            byte[]? preSpawnHash = null;
            if (!string.IsNullOrEmpty(expectedThumb))
            {
                try
                {
                    preSpawnHash = await Task.Run(() =>
                    {
                        using var sha = System.Security.Cryptography.SHA256.Create();
                        using var fs = System.IO.File.OpenRead(enginePath);
                        return sha.ComputeHash(fs);
                    });
                }
                catch (Exception ex)
                {
                    CrashReason = "Pre-spawn binary hash failed: " + ex.Message;
                    State = LifecycleState.Crashed;
                    DebugLog.Error("EngineClient: pre-spawn hash failed — refusing to spawn.");
                    return;
                }
            }

            try
            {
                if (preSpawnHash is not null)
                {
                    try
                    {
                        using var sha = System.Security.Cryptography.SHA256.Create();
                        using var fs = System.IO.File.OpenRead(enginePath);
                        var nowHash = sha.ComputeHash(fs);
                        if (!System.Linq.Enumerable.SequenceEqual(preSpawnHash, nowHash))
                        {
                            CrashReason = "Engine binary changed between Verify and spawn — refusing.";
                            State = LifecycleState.Crashed;
                            DebugLog.Error("EngineClient: TOCTOU detected on engine binary — refusing to spawn.");
                            return;
                        }
                    }
                    catch (Exception ex)
                    {
                        CrashReason = "Post-verify hash failed: " + ex.Message;
                        State = LifecycleState.Crashed;
                        DebugLog.Error("EngineClient: post-verify hash failed — refusing to spawn.");
                        return;
                    }
                }

                var psi = new ProcessStartInfo
                {
                    FileName = enginePath,
                    UseShellExecute = false,
                    RedirectStandardInput = true,
                    RedirectStandardOutput = true,
                    RedirectStandardError = true,
                    CreateNoWindow = true,
                    // `Encoding.UTF8` is the BOM-emitting variant: its StreamWriter pushes `EF BB BF` into the engine's stdin, which trips serde_json ("expected value at line 1 column 1") and surfaced as a red toast on every cold launch. `new UTF8Encoding(false)` is the same UTF-8 without the preamble.
                    StandardInputEncoding = new System.Text.UTF8Encoding(encoderShouldEmitUTF8Identifier: false),
                    StandardOutputEncoding = new System.Text.UTF8Encoding(encoderShouldEmitUTF8Identifier: false),
                    StandardErrorEncoding = new System.Text.UTF8Encoding(encoderShouldEmitUTF8Identifier: false),
                    WorkingDirectory = AppPaths.Root,
                };
                psi.Environment["FILEID_LOG"] = Environment.GetEnvironmentVariable("FILEID_LOG") ?? "info";
                if (AppViewModel.Instance.Settings.RestructureGranularityForEngine is { } granularity)
                {
                    psi.Environment["FILEID_RESTRUCTURE_GRANULARITY"] = granularity;
                }

                Process startedProcess;
                startedProcess = Process.Start(psi)
                    ?? throw new InvalidOperationException("Process.Start returned null");
                var p = startedProcess;
                _process = p;
                var generation = Interlocked.Increment(ref _spawnGeneration);
                PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(SpawnGeneration)));
                _stdin = p.StandardInput;

                _readCts = new CancellationTokenSource();
                var ct = _readCts.Token;
                var startupReady = new TaskCompletionSource<EngineInfo>(TaskCreationOptions.RunContinuationsAsynchronously);
                _stdoutLoop = Task.Run(() => StdoutLoopAsync(p.StandardOutput, p, generation, startupReady, ct), ct);
                _stderrLoop = Task.Run(() => StderrLoopAsync(p.StandardError, ct), ct);

                // Subscribe BEFORE enabling events: a process exiting in the gap would raise (and drop) Exited before the handler attaches, and the crash respawn would never fire.
                p.Exited += OnProcessExited;
                p.EnableRaisingEvents = true;
                _ = VerifyStartupChannelAsync(p, generation, startupReady.Task, ct);
            }
            catch (Exception ex)
            {
                DebugLog.Error("EngineClient.StartAsync failed: " + ex.Message);
                CrashReason = ex.Message;
                State = LifecycleState.Crashed;
                return;
            }

        }
        finally
        {
            Interlocked.Exchange(ref _isStarting, 0);
        }
    }

    /// <summary>Reads one newline-delimited engine frame, bounded to <see cref="MaxFrameBytes"/>. A frame over the cap before a newline is discarded and we resync to the next newline, so a never-terminating line can't OOM the UI. Null at EOF. All framing state lives in the caller-owned <paramref name="st"/>, so no loops share it.</summary>
    internal static async Task<string?> ReadBoundedFrameAsync(
        StreamReader reader, StdoutFraming st, CancellationToken ct, int maxFrameBytes = MaxFrameBytes)
    {
        while (true)
        {
            // Emit a completed frame if the buffer already holds a newline, scanning only the not-yet-scanned tail so a multi-MB buffer is never re-indexed per chunk.
            int nl = -1;
            for (int i = st.Scanned; i < st.Buffer.Length; i++)
            {
                if (st.Buffer[i] == '\n') { nl = i; break; }
            }
            if (nl >= 0)
            {
                string frame = st.Buffer.ToString(0, nl);
                st.Buffer.Remove(0, nl + 1);
                st.Scanned = 0;
                st.Utf8Encoder.Reset();
                st.BufferedBytes = st.Utf8Encoder.GetByteCount(st.Buffer.ToString().AsSpan(), flush: false);
                if (st.Resyncing)
                {
                    st.Resyncing = false;
                    continue;
                }
                if (Encoding.UTF8.GetByteCount(frame) > maxFrameBytes)
                {
                    st.OversizeDropped = true;
                    continue;
                }
                if (frame.Length > 0 && frame[^1] == '\r') frame = frame[..^1];
                return frame;
            }
            st.Scanned = st.Buffer.Length;
            if (st.BufferedBytes > maxFrameBytes)
            {
                DebugLog.Warn($"Engine emitted an oversize IPC frame (> {maxFrameBytes} UTF-8 bytes); discarding and resyncing.");
                st.Buffer.Clear();
                st.Scanned = 0;
                st.Utf8Encoder.Reset();
                st.BufferedBytes = 0;
                st.Resyncing = true;
                st.OversizeDropped = true;
            }
            int read = await reader.ReadAsync(st.Chunk.AsMemory(), ct).ConfigureAwait(false);
            if (read == 0)
            {
                if (!st.Resyncing && st.Buffer.Length > 0)
                {
                    string tail = st.Buffer.ToString();
                    st.Buffer.Clear();
                    st.Scanned = 0;
                    st.Utf8Encoder.Reset();
                    st.BufferedBytes = 0;
                    if (Encoding.UTF8.GetByteCount(tail) > maxFrameBytes)
                    {
                        st.OversizeDropped = true;
                        return null;
                    }
                    if (tail.Length > 0 && tail[^1] == '\r') tail = tail[..^1];
                    return tail;
                }
                return null;
            }
            // Detect a newline in the fresh chunk with a flat array scan (O(read)) rather than re-indexing the StringBuilder; if found, rewind Scanned to just before it so the indexed scan examines only this chunk (the O(n^2) on a large frame).
            int bufLenBefore = st.Buffer.Length;
            st.Buffer.Append(st.Chunk, 0, read);
            st.BufferedBytes += st.Utf8Encoder.GetByteCount(st.Chunk.AsSpan(0, read), flush: false);
            if (Array.IndexOf(st.Chunk, '\n', 0, read) >= 0)
            {
                st.Scanned = bufLenBefore;
            }
            else
            {
                st.Scanned = st.Buffer.Length;
            }
        }
    }

    private async Task StdoutLoopAsync(StreamReader reader, Process captured, int generation,
        TaskCompletionSource<EngineInfo> startupReady, CancellationToken ct)
    {
        var framing = new StdoutFraming();
        // No stdout idle watchdog: it killed healthy idle engines (e.g. after auto-install of llama runtimes the engine waits quietly for the user) and can't distinguish "hung" from "idle". Genuine hangs are caught by the engine's parent-PID watchdog, its GPU TDR detection (sticky cancellation + EngineError), and per-command timeouts on the C# side (WaitForReadyAsync, install stall guards).
        while (!ct.IsCancellationRequested)
        {
            string? line;
            try
            {
                line = await ReadBoundedFrameAsync(reader, framing, ct).ConfigureAwait(false);
            }
            catch (OperationCanceledException) { return; }
            catch (Exception ex)
            {
                DebugLog.Warn("Engine stdout read error: " + ex.Message);
                startupReady.TrySetException(ex);
                await HandleTransportFailureAsync(captured, generation, "stdout read failure", ex).ConfigureAwait(false);
                return;
            }
            if (framing.OversizeDropped)
            {
                framing.OversizeDropped = false;
                // Surface the drop through the normal dispatch path: Apply runs on the UI thread, so observable state is never written from this loop thread.
                var oversize = IpcEvent.Now(new ErrorEvent(new EngineError(
                    "ipc_frame_too_large",
                    "The engine sent a response too large for the app to read, so it was dropped. " +
                    "If this was an Organize Folders plan for a very large folder, the plan may be incomplete. " +
                    "Try a smaller folder.",
                    null)));
                _ui.TryEnqueue(() => Apply(oversize, generation));
            }
            if (line is null)
            {
                startupReady.TrySetException(new IOException("Engine stdout closed before startup completed."));
                if (!ct.IsCancellationRequested)
                    await HandleTransportFailureAsync(captured, generation, "stdout EOF").ConfigureAwait(false);
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

            if (!IsHealthTargetCurrent(generation, SpawnGeneration, captured, _process, ProcessHasExited(captured))) return;
            if (ev.Payload is ReadyEvent ready && ready.Info.Pid == captured.Id)
                startupReady.TrySetResult(ready.Info);
            if (ev.Payload is HealthCheckResultEvent health)
                _healthWaiters.TryResolve(health.Result.RequestId, health.Result.Pid, generation);
            _ui.TryEnqueue(() => Apply(ev, generation));
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
            // The engine redacts paths via redact_path_for_log, but as defense in depth the bridge also passes any Windows-shaped absolute path (drive letter + colon + backslash) through PathRedactor, best-effort.
            DebugLog.Info("[engine] " + RedactWindowsPathsInLine(line));
        }
    }

    private static string RedactWindowsPathsInLine(string line)
    {
        if (line.IndexOf('\\') < 0) return line;
        return s_pathInLine.Replace(line, m => PathRedactor.Redact(m.Value));
    }

    // Match a drive path (C:\…) or UNC path (\\server\…), terminating only on quotes / brackets / commas / line ends, NOT spaces: paths routinely contain spaces ("C:\Users\Bob Smith\…") and stopping at the first one leaked the remainder into the log. Over-matching trailing text is fine; PathRedactor still collapses the home prefix.
    private static readonly System.Text.RegularExpressions.Regex s_pathInLine =
        new(@"(?:[A-Za-z]:\\|\\\\)[^"",)\]}>\r\n]*",
            System.Text.RegularExpressions.RegexOptions.Compiled);

    private bool ConsumeExpectedExit(Process? exited)
    {
        if (exited is null || !_expectedExitProcesses.TryRemove(exited, out var requestedAtTicks))
        {
            return false;
        }

        var requestedAt = new DateTime(requestedAtTicks, DateTimeKind.Utc);
        return DateTime.UtcNow - requestedAt <= ExpectingExitWindow;
    }

    private void OnProcessExited(object? sender, EventArgs e)
    {
        // Capture the exit code from the process that ACTUALLY exited (sender), not the mutable _process: by the time this UI-thread callback runs it may point at a respawned process or be disposed, and reading ExitCode then throws and aborts cleanup.
        var exited = sender as System.Diagnostics.Process;
        int? exitCode = null;
        try { exitCode = exited?.ExitCode; } catch { /* not-exited / disposed */ }
        _ui.TryEnqueue(() =>
        {
            var expectedExit = ConsumeExpectedExit(exited);

            // Ignore a stale exit from a process we've already replaced. In RestartAsync the OLD process's Exited callback is queued to the UI thread and can run AFTER StartAsync installed the NEW _process/_stdin/_readCts; running Cleanup() then would tear down the fresh engine and mis-count it as a crash, wedging IPC with State stuck Starting. Dispose the dead sender and bail.
            if (!ReferenceEquals(sender, _process))
            {
                if (sender is Process dead)
                {
                    try { dead.Exited -= OnProcessExited; } catch { }
                    try { dead.Dispose(); } catch { }
                }
                return;
            }
            DebugLog.Warn($"EngineClient: process exited (code={exitCode?.ToString() ?? "?"}).");
            Cleanup();
            ResetProcessBoundScanState();

            // Notify the install service immediately so any in-flight download owned by the dead engine flips to Failed instead of spinning forever. Runs on every exit and is idempotent.
            try { Services.ModelInstallerService.Instance.Reset(); }
            catch (Exception ex) { DebugLog.Warn("OnProcessExited: ModelInstallerService.Reset threw: " + ex.Message); }

            // User-initiated shutdown must not count as a crash or auto-respawn. Interlocked.Exchange reads and clears atomically.
            if (expectedExit)
            {
                CrashReason = StoppedReason;
                State = LifecycleState.Crashed;
                return;
            }

            var now = DateTime.UtcNow;
            // A crash after a STABLE Ready (>= StabilitySettle) is genuine recovery and clears the strike state; a faster Ready→crash flap is not, and keeps ticking toward the 3-strike terminal cap.
            if (_lastReadyAt != DateTime.MinValue && now - _lastReadyAt >= StabilitySettle)
            {
                _consecutiveFailures = 0;
                _failureWindowStart = DateTime.MinValue;
            }
            _lastReadyAt = DateTime.MinValue;
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

            var delay = _consecutiveFailures switch
            {
                1 => TimeSpan.FromSeconds(1),
                2 => TimeSpan.FromSeconds(4),
                _ => TimeSpan.FromSeconds(16),
            };
            DebugLog.Info($"EngineClient: respawning in {delay.TotalSeconds}s (attempt {_consecutiveFailures}/3).");
            // No outer CAS: StartAsync has its own strict CAS, and setting _isStarting=1 here made it see "already starting" and bail, silently dropping every auto-respawn.
            _ = Task.Delay(delay).ContinueWith(_ => _ui.TryEnqueue(async () =>
            {
                try
                {
                    await StartAsync().ConfigureAwait(false);
                }
                catch (Exception ex)
                {
                    DebugLog.Error("EngineClient: respawn StartAsync threw: " + ex.Message);
                }
            }));
        });
    }

    private void RefreshPersistedRestructureUndo(string? excludedToken)
    {
        var next = PersistedUndoReader.ReadPersistedShortcutUndo(AppPaths.Root, excludedToken);
        UndoRestructureShortcutToken = next?.Token;
        CanUndoRestructure = next is not null;
    }

    private void Cleanup()
    {
        var retiringGeneration = SpawnGeneration;
        _workbenchRequests.FailGeneration(retiringGeneration);
        if (ToolExecution.InvalidateGeneration(retiringGeneration)) PublishToolExecution();
        if (ChatSend.InvalidateGeneration(retiringGeneration)) PublishChatSend();
        _healthWaiters.FailGeneration(retiringGeneration, new InvalidOperationException("The engine generation stopped."));
        RetireDeepAnalyzeGeneration(retiringGeneration);
        if (_scanStartSlot.ReleaseGeneration(SpawnGeneration) is { } scanStart)
        {
            scanStart.Payload.Confirmation.TrySetException(
                new InvalidOperationException("The engine stopped before confirming the scan start."));
        }
        Interlocked.Increment(ref _scanPresentationRevision);
        try { _readCts?.Cancel(); } catch { }
        _readCts?.Dispose();
        _readCts = null;

        // Take the same lock as SendCommandAsync so a concurrent writer can't see _stdin non-null and then NRE on Write after we dispose it.
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
        Interlocked.Increment(ref _spawnGeneration);
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(SpawnGeneration)));

        // Release the auto-advance gates on every engine teardown: a crash mid-clustering or mid-deep-analyze never emits the Complete/Failed event that clears them, so a respawned engine would skip auto face-clustering / deep analyze for the rest of the session. Cleanup() is the single teardown chokepoint.
        Interlocked.Exchange(ref _faceClusterAutoInFlight, 0);
        Interlocked.Exchange(ref _autoDeepAnalyzeInFlight, 0);
        // Likewise the undo affordance: a crash mid-undo never emits the terminal restructureApplyResult, so the next apply's result would be mis-attributed as the dead undo's.
        UndoRestructureInFlight = false;
        Interlocked.Exchange(ref _restructureCommandInFlight, 0);
    }

    public void Dispose()
    {
        _workbenchRequests.Dispose();
        Cleanup();
        _events.OnCompleted();
    }

    /// <summary>
    /// Blocks until the engine reaches <see cref="LifecycleState.Ready"/>. Throws <see cref="TimeoutException"/> on timeout, or <see cref="InvalidOperationException"/> with the crash reason if it already crashed; returns immediately if Ready. Install flows gate on this so a click during cold start waits or errors cleanly rather than throwing "Engine not running."
    /// </summary>
    internal static bool IsHealthTargetCurrent(int capturedGeneration, int currentGeneration,
        Process? captured, Process? current, bool capturedHasExited)
        => captured is not null && ReferenceEquals(captured, current)
            && capturedGeneration == currentGeneration && !capturedHasExited;

    private static bool ProcessHasExited(Process process)
    {
        try { return process.HasExited; }
        catch (InvalidOperationException) { return true; }
    }

    public async Task ProbeCommandChannelAsync(TimeSpan timeout, CancellationToken ct = default)
    {
        ArgumentOutOfRangeException.ThrowIfLessThanOrEqual(timeout, TimeSpan.Zero);
        ct.ThrowIfCancellationRequested();
        var captured = _process ?? throw new InvalidOperationException("FileID is still getting ready. Try again in a moment.");
        var generation = SpawnGeneration;
        if (!IsHealthTargetCurrent(generation, SpawnGeneration, captured, _process, ProcessHasExited(captured)))
            throw new InvalidOperationException("The engine changed before its health check.");
        var requestId = Guid.NewGuid().ToString("N");
        var waiter = _healthWaiters.Register(requestId, generation, captured.Id);
        try
        {
            if (waiter.Task.IsFaulted) await waiter.Task.ConfigureAwait(false);
            if (!IsHealthTargetCurrent(generation, SpawnGeneration, captured, _process, ProcessHasExited(captured)))
                throw new InvalidOperationException("The engine changed before its health check was sent.");
            await SendCommandAsync(new HealthCheckCommand(requestId), ct).ConfigureAwait(false);
            await waiter.Task.WaitAsync(timeout, ct).ConfigureAwait(false);
            if (!IsHealthTargetCurrent(generation, SpawnGeneration, captured, _process, ProcessHasExited(captured)))
                throw new InvalidOperationException("The engine changed during its health check.");
        }
        catch (Exception ex)
        {
            _healthWaiters.TryFail(requestId, ex);
            try { await waiter.Task.ConfigureAwait(false); } catch { }
            throw;
        }
    }

    private async Task VerifyStartupChannelAsync(Process captured, int generation, Task<EngineInfo> ready, CancellationToken ct)
    {
        try
        {
            await ready.WaitAsync(TimeSpan.FromSeconds(30), ct).ConfigureAwait(false);
            if (!IsHealthTargetCurrent(generation, SpawnGeneration, captured, _process, ProcessHasExited(captured))) return;
            await ProbeCommandChannelAsync(TimeSpan.FromSeconds(15), ct).ConfigureAwait(false);
            _ui.TryEnqueue(() =>
            {
                if (!IsHealthTargetCurrent(generation, SpawnGeneration, captured, _process, ProcessHasExited(captured))) return;
                _lastReadyAt = DateTime.UtcNow;
                CrashReason = null;
                State = LifecycleState.Ready;
            });
        }
        catch (OperationCanceledException) when (ct.IsCancellationRequested) { }
        catch (Exception ex)
        {
            await HandleTransportFailureAsync(captured, generation, "startup health check failed", ex).ConfigureAwait(false);
        }
    }

    private async Task HandleTransportFailureAsync(Process captured, int generation, string reason, Exception? failure = null)
    {
        if (!IsHealthTargetCurrent(generation, SpawnGeneration, captured, _process, ProcessHasExited(captured))) return;
        if (Interlocked.Exchange(ref _transportFailureGeneration, generation) == generation) return;
        _healthWaiters.FailGeneration(generation, failure ?? new IOException("The engine command channel closed."));
        if (_expectedExitProcesses.ContainsKey(captured)) return;
        DebugLog.Warn($"Engine transport failure: {reason} ({failure?.GetType().Name ?? "closed"}).");
        try
        {
            if (!IsHealthTargetCurrent(generation, SpawnGeneration, captured, _process, ProcessHasExited(captured))) return;
            captured.Kill(entireProcessTree: true);
            await captured.WaitForExitAsync().WaitAsync(TimeSpan.FromSeconds(5)).ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            DebugLog.Warn("Engine transport recovery failed: " + ex.GetType().Name);
            _ui.TryEnqueue(() =>
            {
                if (generation != SpawnGeneration || !ReferenceEquals(captured, _process)) return;
                CrashReason = "The engine command channel closed. Stop the engine or restart FileID before continuing.";
                State = LifecycleState.Crashed;
            });
        }
        if (generation == SpawnGeneration && ReferenceEquals(captured, _process) && ProcessHasExited(captured))
            OnProcessExited(captured, EventArgs.Empty);
    }

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

    private void ObserveAuthoritativeScanEvent(int generation)
    {
        var owner = _scanStartSlot.Current;
        if (owner is null || owner.Generation != generation)
        {
            return;
        }

        owner.Payload.Confirmation.TrySetResult(true);
        if (_scanStartSlot.Release(owner))
        {
            Interlocked.Increment(ref _scanPresentationRevision);
        }
    }

    private void RejectScanStartCommand(int generation)
    {
        var owner = _scanStartSlot.Current;
        if (owner is null || owner.Generation != generation)
        {
            return;
        }

        owner.Payload.Confirmation.TrySetException(
            new InvalidOperationException("The engine rejected the scan start."));
        if (_scanStartSlot.Release(owner)
            && owner.Generation == SpawnGeneration
            && owner.Revision == Volatile.Read(ref _scanPresentationRevision))
        {
            Phase = owner.Payload.PreviousPhase;
            _scanStartedAt = null;
        }
    }

    private void Apply(IpcEvent ev, int generation)
    {
        if (generation != SpawnGeneration) return;
        // Per-event diagnostic tracing (see _applySeq). Logs only the event TYPE, never the payload, which can contain user paths.
        var applySeq = Interlocked.Increment(ref _applySeq);
        var applyEventName = ev.Payload?.GetType().Name ?? "<null>";
        DebugLog.Info($"[APPLY:{applySeq}] enter {applyEventName} tid={Environment.CurrentManagedThreadId}");

        // Top-level try so a throw inside Set<T> → PropertyChanged fan-out can't escape Apply into the dispatcher loop. The inner try catches per-arm routing exceptions and writes a crash dump; the outer catches anything else. Worst case: log and carry on.
        try
        {
            // Always raise to subscribers first, even if routing below throws. Scan FileDone events are sampled (every Nth) since a fast scan emits hundreds per second.
            bool publishToSubject = true;
            if (ev.Payload is FileDoneEventWrapper)
            {
                var n = Interlocked.Increment(ref _scanFileDoneEventCounter);
                publishToSubject = (n % ScanFileDoneSampleN) == 0;
            }
            if (publishToSubject)
            {
                try { _events.OnNext(ev); } catch (Exception ex) { DebugLog.Warn("event subject OnNext threw: " + ex.Message); }
            }

            try
            {
                ApplyWorkbenchResponse(ev.Payload, generation);
                switch (ev.Payload)
                {
                    case ReadyEvent r:
                        Info = r.Info;
                        if (GpuDeviceRemoved && _gpuDeviceRemovedGeneration != generation)
                        {
                            GpuDeviceRemoved = false;
                            _gpuDeviceRemovedGeneration = -1;
                            if (LastError?.Kind == "gpu_device_removed") LastError = null;
                            if (Phase == ScanPhase.Failed)
                            {
                                Phase = null;
                                _shownPhaseRank = -1;
                            }
                        }
                        break;
                    case ProgressEvent p:
                        ObserveAuthoritativeScanEvent(generation);
                        // Discovery and tagging emit ProgressEvents CONCURRENTLY during the pipeline overlap, and a late Discovering event carries processed=0, eta=None, fps=0 and its own memory reading; letting it replace LastProgress made the sidebar's Tagged / ETA / Memory flicker. Gate the whole event on a monotonic phase rank: drop any ProgressEvent below the latch. Tagging events carry the live discovered count, so "Discovered" keeps climbing. The authoritative PhaseChangedEvent below also syncs the latch.
                        var progRank = PhaseRank(p.Progress.Phase);
                        if (progRank < _shownPhaseRank) break;
                        _shownPhaseRank = progRank;
                        var nowProg = DateTime.UtcNow;
                        if (nowProg - _lastProgressEmit >= ProgressThrottle
                            || p.Progress.Phase != _lastProgressPhase)
                        {
                            LastProgress = p.Progress;
                            _lastProgressEmit = nowProg;
                            _lastProgressPhase = p.Progress.Phase;
                        }
                        Phase = p.Progress.Phase;
                        break;
                    case PhaseChangedEvent pc:
                        ObserveAuthoritativeScanEvent(generation);
                        Phase = pc.Phase;
                        // Authoritative phase boundary: sync the latch so a late interleaved ProgressEvent can't pull the displayed phase back below it.
                        _shownPhaseRank = PhaseRank(pc.Phase);
                        // On cancel, also clear in-flight tracking state; otherwise the CompletedPanel (bound to LastScanDuration + LastProgress) shows the prior scan's numbers.
                        if (pc.Phase == ScanPhase.Cancelled)
                        {
                            IsPaused = false;
                            _scanStartedAt = null;
                            LastProgress = null;
                            LastBatch = null;
                        }
                        // Faces persist incrementally (dbwriter commits per batch) but auto-clustering fires only on ScanComplete, so a Failed scan would leave detected faces with no persons row. Fire the idempotent, zero-face-safe auto-cluster there too. A user-Cancelled scan instead DEFERS clustering to a manual re-cluster, since auto-firing on cancel races the engine's teardown.
                        if (pc.Phase == ScanPhase.Failed)
                        {
                            _ = AutoTriggerFaceClusteringAsync();
                        }
                        break;
                    case DiscoveryCompleteEvent:
                        break;
                    case FileDoneEventWrapper:
                        break;
                    case BatchSummaryEvent b:
                        LastBatch = b.Summary;
                        break;
                    case ScanCompleteEvent sce:
                        ObserveAuthoritativeScanEvent(generation);
                        LastScanProcessedFiles = sce.Result.ProcessedFiles;
                        IsPaused = false;
                        if (_scanStartedAt.HasValue)
                        {
                            LastScanDuration = DateTime.UtcNow - _scanStartedAt.Value;
                            _scanStartedAt = null;
                        }
                        // The engine emits scanComplete for cancelled scans too (carrying final counts). Cancelled is the user's terminal phase: do not overwrite it with Completed or auto-advance to clustering.
                        if (Phase == ScanPhase.Cancelled || Phase == ScanPhase.Failed)
                        {
                            break;
                        }
                        Phase = ScanPhase.Completed;
                        _shownPhaseRank = PhaseRank(ScanPhase.Completed);
                        // Auto-advance to face clustering: the engine has no such hook, so the app fires the IPC on ScanComplete; the handler is a no-op with zero face_prints, so it is safe on a library with no images. Deep Analyze stays manual.
                        _ = AutoTriggerFaceClusteringAsync();
                        break;
                    case ErrorEvent e:
                        if (e.Error.Kind == "gpu_device_removed")
                        {
                            _gpuDeviceRemovedGeneration = generation;
                            GpuDeviceRemoved = true;
                        }
                        if (e.Error.Kind == "scan_already_running")
                        {
                            RejectScanStartCommand(generation);
                        }

                        if (e.Error.Kind is "apply_restructure" or "undo_restructure")
                        {
                            UndoRestructureInFlight = false;
                            Interlocked.Exchange(ref _restructureCommandInFlight, 0);
                        }
                        if (IsNonFatalWarningKind(e.Error.Kind))
                        {
                            LastWarning = e.Error;
                            DebugLog.Info($"[IPC IN] engine warning: kind={e.Error.Kind} msg={e.Error.Message}");
                        }
                        else
                        {
                            LastError = e.Error;
                            DebugLog.Warn($"[IPC IN] engine error: kind={e.Error.Kind} msg={e.Error.Message} path={PathRedactor.Redact(e.Error.Path)}");
                        }
                        // A clustering FAILURE must release the auto gate, else auto-clustering stays suppressed for the session. Match the EXACT failure kind, not Contains("cluster"), which also matched the "face_clustering_busy" bounce (a pass still running, the opposite of a failure) and wrongly cleared the gate. The in-flight pass emits its own FaceClusteringComplete / face_clustering_failed.
                        if (e.Error.Kind == "face_clustering_failed" || (e.Error.Kind.StartsWith("face_clustering_", StringComparison.Ordinal) && e.Error.Kind != "face_clustering_busy"))
                        {
                            Interlocked.Exchange(ref _faceClusterAutoInFlight, 0);
                        }
                        break;
                    case LogEvent:
                        break;
                    case FaceClusteringCompleteEvent fc:
                        LastFaceClustering = fc.Result;
                        Interlocked.Exchange(ref _faceClusterAutoInFlight, 0); // release the auto gate
                        break;
                    case DeepAnalyzeStartingEvent das:
                        DeepAnalyzeStarting = das.Starting;
                        // Clear the previous run's terminal result. DeepAnalyzeComplete is otherwise cleared only on the single-file path, so on a 2nd+ "Analyze All" run the stale Complete makes the view's SyncStream clobber live progress every tick (buttons and status text fight at ~4 Hz, Cancel greyed out for the whole run).
                        DeepAnalyzeComplete = null;
                        // Also clear the last-file result so a new run starts with no carried-over DeepAnalyzeLast; otherwise the view's run-start _proposedNameCount reset is undone when SyncStream reprocesses the stale last (over-counting smart-renames).
                        DeepAnalyzeLast = null;
                        break;
                    case DeepAnalyzeProgressEvent dap:
                        DeepAnalyzeProgress = dap.Progress;
                        break;
                    case DeepAnalyzeFileDoneEvent dafd:
                        var now = DateTime.UtcNow;
                        if (now - _lastDeepAnalyzeFileDone >= DeepAnalyzeFileDoneThrottle)
                        {
                            DeepAnalyzeLast = dafd.FileDone;
                            _lastDeepAnalyzeFileDone = now;
                        }
                        break;
                    case DeepAnalyzeCompleteEvent dac:
                        if (!CompleteDeepAnalyzeCommand(generation, dac.Result)) break;
                        DeepAnalyzeComplete = dac.Result;
                        DeepAnalyzeProgress = null;
                        DeepAnalyzeStarting = null;
                        Interlocked.Exchange(ref _autoDeepAnalyzeInFlight, 0);
                        break;
                    case ModelDownloadProgressEvent mdp:
                        // Throttled to one log line per 1% (~100 events per model) so app.log isn't flooded but stuck installs stay diagnosable.
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
                    case ToolResponseEvent tools:
                        LastToolResponse = tools.Response;
                        break;
                    case CatalogResponseEvent catalog:
                        LastCatalogResponse = catalog.Response;
                        break;
                    case RestructurePlanEvent rp:
                        LastRestructurePlan = rp.Plan;
                        break;
                    case HealthCheckResultEvent:
                        break;
                    case RestructureApplyResultEvent rar:
                        try
                        {
                            LastRestructureResultWasUndo = UndoRestructureInFlight;
                            LastRestructureResultWasShortcutApply = !UndoRestructureInFlight && _restructureApplyUsesSymlinks;
                            LastRestructureResultWasShortcutUndo = UndoRestructureInFlight && UndoRestructureShortcutToken is not null;
                            if (UndoRestructureInFlight)
                            {
                                UndoRestructureInFlight = false;
                                if (UndoRestructureShortcutToken is not null)
                                {
                                    RefreshPersistedRestructureUndo(UndoRestructureShortcutToken);
                                }
                                else
                                {
                                    CanUndoRestructure = rar.Result.Failed > 0
                                        || (rar.Result.Cancelled && rar.Result.Remaining.GetValueOrDefault() > 0);
                                }
                            }
                            else
                            {
                                UndoRestructureShortcutToken = _restructureApplyUsesSymlinks
                                    ? rar.Result.ShortcutUndoToken
                                        ?? PersistedUndoReader.ReadPersistedShortcutUndo(AppPaths.Root)?.Token
                                    : null;
                                CanUndoRestructure = UndoRestructureShortcutToken is not null
                                    || (!_restructureApplyUsesSymlinks && rar.Result.Applied > 0);
                            }
                            if (!LastRestructureResultWasUndo
                                && RestructureUndoPolicy.ShouldRecord(LastRestructureResultWasShortcutApply, rar.Result.Applied, CanUndoRestructure)
                                && RestructureOperationRoot is { Length: > 0 } operationRoot)
                            {
                                DebugLog.SafeRun("EngineClient.RecordRestructureChange", () =>
                                {
                                    ChangeLog.Instance.Push($"Moved {rar.Result.Applied:N0} files into new folders", ChangeKind.Restructure, async () =>
                                    {
                                        var undoResult = await UndoRestructureAndWaitAsync(operationRoot);
                                        return !undoResult.Cancelled && undoResult.Failed == 0 && undoResult.Applied > 0;
                                    });
                                });
                            }
                            LastRestructureApplyResult = rar.Result;
                        }
                        finally { Interlocked.Exchange(ref _restructureCommandInFlight, 0); }
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
                    case HardwareReprobedEvent hr:
                        if (hr.Result is null)
                        {
                            DebugLog.Warn("HardwareReprobedEvent with null Result; dropped.");
                            break;
                        }
                        LastHardwareReprobe = hr.Result;
                        if (Info is { } prevInfo && hr.Result.Hardware is { } hw)
                        {
                            Info = new EngineInfo(
                                prevInfo.Version,
                                prevInfo.Pid,
                                prevInfo.WorkerCap,
                                prevInfo.PhysicalMemoryGB,
                                hw);
                        }
                        break;
                    case LibraryWipedEvent lw:
                        LastLibraryWiped = lw.Result;
                        break;
                    case ThumbnailGeneratedEvent tg:
                        LastThumbnailGenerated = tg.Generated;
                        DebugLog.Info($"[IPC IN] thumbnailGenerated path={PathRedactor.Redact(tg.Generated.Path)} ({tg.Generated.Bytes.Length} b64 chars)");
                        break;
                    default:
                        // Unknown TAGS never get here (the strict decoder throws and the read loop logs it). This catches a DTO added to the schema without a routing arm, so the gap is visible rather than silently dropping state.
                        DebugLog.Warn($"[IPC IN] event type with no Apply routing: {ev.Payload?.GetType().Name ?? "<null>"}");
                        break;
                }
            }
            catch (Exception ex)
            {
                // Route through WriteCrashDump so a routing-side fault leaves a forensic artifact; a null deref or malformed payload in one arm must NOT tear down the UI.
                DebugLog.Error($"EngineClient.Apply({ev.Payload?.GetType().Name ?? "<null>"}) threw: {ex}");
                try { DebugLog.WriteCrashDump($"EngineClient.Apply({ev.Payload?.GetType().Name ?? "<null>"})", ex, terminating: false); }
                catch { /* swallow */ }
            }

        }
        catch (Exception outerEx)
        {
            // Outer-frame catch, the last defense before the dispatcher loop (e.g. the inner catch failing to write a dump on a full disk). Log only, never re-throw.
            try { DebugLog.Error($"EngineClient.Apply OUTER catch: {outerEx}"); }
            catch { /* truly nothing we can do */ }
        }

        // Matching exit line for [APPLY:N] enter. After a native fast-fail, the highest seq without an exit identifies the offending event; PropertyChanged fires synchronously, so the trailing [ENGINE-SUB:Class] line identifies the offending subscriber.
        DebugLog.Info($"[APPLY:{applySeq}] exit {applyEventName}");
    }

    /// <summary>Scan-complete → face-clustering auto-advance. Fire-and-forget so Apply returns quickly; failures are logged and swallowed because the scan itself succeeded.</summary>
    private async Task AutoTriggerFaceClusteringAsync()
    {
        // Skip if an auto-clustering pass is already in flight (e.g. a rescan completing while the prior pass runs); released on FaceClusteringComplete or a clustering error.
        if (Interlocked.CompareExchange(ref _faceClusterAutoInFlight, 1, 0) != 0)
        {
            DebugLog.Info("[AUTO-ADVANCE] face clustering already in flight — skipping duplicate trigger");
            return;
        }
        try
        {
            await Task.Yield(); // let the rest of Apply complete first
            DebugLog.Info("[AUTO-ADVANCE] scan complete → triggering face clustering");
            await RunFaceClusteringAsync().ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            Interlocked.Exchange(ref _faceClusterAutoInFlight, 0);
            DebugLog.Warn("[AUTO-ADVANCE] face clustering trigger threw: " + ex.Message);
        }
    }

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
