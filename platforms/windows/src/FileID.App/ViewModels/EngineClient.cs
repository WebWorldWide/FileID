// EngineClient — owns the FileIDEngine.exe child process lifecycle.
//
// Responsibilities:
//   1. Spawn FileIDEngine.exe with stdin/stdout/stderr redirected.
//   2. Verify the engine binary's Authenticode signature before each spawn;
//      signed releases require the same signer public key as the app assembly.
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

    internal const string StoppedReason = "Engine stopped";

    private readonly DispatcherQueue _ui;
    private readonly Subject<IpcEvent> _events = new();
    private readonly object _writeLock = new();

    // S4: bounded engine-stdout framing — a wedged/garbled engine that never
    // emits a newline can't grow an unbounded read buffer and OOM the UI process.
    // The 1 MiB cap this once used silently DROPPED the restructurePlan event (one
    // move per file × ~300 B) above ~3.5k moves, leaving the Restructure tab empty
    // on a real "tens of thousands of files" library. The engine caps OUTBOUND
    // frames too (sink.rs MAX_FRAME_BYTES) — over-cap it substitutes a structured
    // ipc_frame_too_large error rather than emitting a frame this reader would drop
    // — so all four caps are kept symmetric. Bumped 32→64 MiB (R3-07B/R5-12) to hold
    // a full ~200k-move whole-library plan while still bounding a runaway line. An
    // oversize drop is also surfaced as a visible error (see StdoutLoopAsync), never silent.
    private const int MaxFrameBytes = 64 * 1024 * 1024;

    /// <summary>Per-loop stdout framing state (#22). Owned by a single
    /// StdoutLoopAsync invocation — never shared across loops, so an overlapping
    /// loop from a respawn can't race another's buffer/resync flag.</summary>
    internal sealed class StdoutFraming
    {
        public readonly StringBuilder Buffer = new();
        public readonly char[] Chunk = new char[16 * 1024];
        // How many leading buffer chars are already confirmed newline-free, so a
        // multi-MB frame isn't rescanned from index 0 on every chunk (the old
        // O(n^2) that pegged a core for minutes on a large restructurePlan). (audit A0)
        public int Scanned;
        public long Utf8Bytes;
        public bool Resyncing;
        // Set when an over-cap frame was discarded, so StdoutLoopAsync can surface
        // a one-shot visible error instead of failing silently.
        public bool OversizeDropped;
    }

    private Process? _process;
    private CancellationTokenSource? _readCts;
    private Task? _stdoutLoop;
    private Task? _stderrLoop;
    private StreamWriter? _stdin;
    private readonly GenerationHealthWaiters _healthWaiters = new();
    private readonly object _transportFailureLock = new();
    private Task _transportFailureTask = Task.CompletedTask;
    private int _transportFailureGeneration = -1;
    private static readonly TimeSpan HealthProbeTimeout = TimeSpan.FromSeconds(5);

    private DateTime _lastSpawnAttempt = DateTime.MinValue;
    private int _consecutiveFailures;
    private DateTime _failureWindowStart = DateTime.MinValue;
    private static readonly TimeSpan FailureWindow = TimeSpan.FromSeconds(60);
    // R5-07: the engine must stay continuously Ready at least this long before a
    // subsequent crash counts as recovery (and clears the strike counter). A
    // shorter Ready→crash is a flap that must keep ticking toward the 3-strike
    // terminal cap instead of resetting every ~1s.
    private DateTime _lastReadyAt = DateTime.MinValue;
    private static readonly TimeSpan StabilitySettle = TimeSpan.FromSeconds(30);

    // Monotonic engine-process generation. Incremented on every teardown
    // (crash, user stop, restart) in Cleanup(). Lets a view whose in-flight
    // guard is keyed to a command sent to a PARTICULAR engine process detect
    // that the process is gone — its result/error event can never arrive —
    // and release the guard instead of staying wedged for the session.
    private int _spawnGeneration;
    public int SpawnGeneration => Volatile.Read(ref _spawnGeneration);
    private int _gpuDeviceRemovedGeneration = -1;

    // BUG-3: respawn debouncing — prevents two-spawn races during the
    // 1s/4s/16s backoff window when the engine flaps quickly.
    private int _isStarting; // 0 = idle, 1 = StartAsync in flight
    private readonly EngineLifecycleCoordinator _lifecycle = new();
    private readonly SemaphoreSlim _lifecycleGate = new(1, 1);

    // BUG-6: distinguish user-initiated shutdown from a crash. Set by
    // ShutdownAsync; OnProcessExited consumes it. Uses int + Interlocked
    // (instead of bool) so reads/writes are atomic across threads on
    // ARM64 — bool reads can theoretically tear on weakly-ordered
    // architectures, and OnProcessExited fires on whichever thread
    // detects process exit (not always the UI thread).
    private int _expectingExit; // 0 = false, 1 = true
    private Process? _expectedExitProcess;
    // When _expectingExit was set (UTC ticks). A shutdown request that never
    // produces an exit would otherwise latch the flag forever, so a real crash
    // much later gets mis-read as user-initiated and never respawns. Only honor
    // the flag if the exit follows the request within ExpectingExitWindow.
    private long _expectingExitAtTicks;
    private readonly object _expectedExitRestartGate = new();
    private int _restartAfterExpectedExit;
    private long _restartAfterExpectedExitRevision = -1;
    private static readonly TimeSpan ExpectingExitWindow = TimeSpan.FromSeconds(60);

    // Re-entrancy gate for any auto-triggered Deep Analyze pass. Released
    // in the DeepAnalyzeCompleteEvent arm.
    private int _autoDeepAnalyzeInFlight;

    // PAR-111: re-entrancy gate for the auto-triggered face-clustering pass.
    // A rescan emits a SECOND ScanComplete in the same session; without this
    // gate each one fire-and-forgets another RunFaceClustering, and the engine
    // spawns one clustering task per IPC with no dedup of its own — so two
    // passes race the same persons / face_prints.person_id writes. Acquired in
    // AutoTriggerFaceClusteringAsync, released on FaceClusteringComplete or a
    // clustering error. Mirrors macOS EngineClient.faceClusteringInFlight.
    private int _faceClusterAutoInFlight;

    // Observable counterpart of _faceClusterAutoInFlight: true while a clustering
    // pass is in progress, false otherwise. Set on the UI thread (inside Apply())
    // so PropertyChanged fires safely. Mirrors macOS faceClusteringInFlight @Published.
    private bool _faceClusteringInFlight;
    public bool FaceClusteringInFlight
    {
        get => _faceClusteringInFlight;
        private set => Set(ref _faceClusteringInFlight, value);
    }

    // Throttle for scan FileDone events. A fast scan can emit hundreds per
    // second; publishing each through the Rx Subject inflates UI work for
    // every subscriber (LibraryView, transcript, etc.). Sample every Nth
    // event so subscribers still see "files are flowing" without
    // re-running expensive layouts at scan-throughput rate.
    private int _scanFileDoneEventCounter;
    private const int ScanFileDoneSampleN = 5;

    // PerfAudit-#8: ScanProgress throttle. Engine emits one Progress
    // per discovery/tagging batch; on a fast scan that's 100+ events/s.
    // Throttle at 10 Hz so the sidebar's progress bar / counters don't
    // re-render at scan-throughput rate. Phase transitions bypass the
    // throttle (rare; user-visible).
    private DateTime _lastProgressEmit = DateTime.MinValue;
    private ScanPhase? _lastProgressPhase;

    // Highest phase rank shown during the current scan. Discovery and tagging
    // run concurrently, so their ProgressEvents interleave; without this the
    // displayed Phase — and the sidebar label/icon/pipeline dot bound to it —
    // flicker Discovering<->Tagging several times a second. A ProgressEvent may
    // only ADVANCE the displayed phase, never regress it within a scan. The
    // authoritative PhaseChangedEvent still sets Phase directly and re-syncs
    // this latch. Reset to -1 at each scan start (see EngineClient.Commands.cs).
    private int _shownPhaseRank = -1;

    private static int PhaseRank(ScanPhase phase) => phase switch
    {
        ScanPhase.Idle => 0,
        ScanPhase.Discovering => 1,
        ScanPhase.Tagging => 2,
        ScanPhase.PostScan => 3,
        ScanPhase.Completed => 4,
        // Terminal states sit above the progression so a late interleaved
        // ProgressEvent can never clamp them away once PhaseChanged has synced
        // the latch to them.
        ScanPhase.Cancelled => 5,
        ScanPhase.Failed => 5,
        _ => 0,
    };
    private static readonly TimeSpan ProgressThrottle = TimeSpan.FromMilliseconds(100); // 10 Hz

    // throttled diagnostic counter for inbound progress events.
    // Lets `[IPC IN] ModelDownloadProgress #N` lines correlate with engine
    // activity without flooding app.log.
    private int _modelDownloadEventCount;

    // monotonic Apply-call counter. Used by the [APPLY:N] enter/exit
    // tracing to localize native fast-fails. The crash signature was an
    // app process death with NO managed exception and NO crash dump
    // (last-session.txt clean_exit=false). Without per-event tracing the
    // only visible signal was a 3-4 s log gap between the StartScan IPC
    // and process termination. The [APPLY:N] enter/exit pair makes the
    // last-processed event identifiable from app.log alone — when the app
    // dies, the highest-numbered `enter` without a matching `exit` is the
    // killer event.
    private int _applySeq;

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

    private bool _gpuDeviceRemoved;
    public bool GpuDeviceRemoved
    {
        get => _gpuDeviceRemoved;
        private set => Set(ref _gpuDeviceRemoved, value);
    }

    private EngineError? _lastWarning;
    /// Non-fatal events the engine still wants the user to see (skipped
    /// stages, partial discovery, stale-WAL warning). Kept in a separate
    /// slot so a later per-file error can't clobber the banner.
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
    private long _restructureMutationRevision;
    private long _pendingRestructurePlanRevision = -1;
    private long _restructurePlanDiscardedSignal;
    public RestructurePlan? LastRestructurePlan
    {
        get => _lastRestructurePlan;
        private set => Set(ref _lastRestructurePlan, value);
    }

    /// <summary>Discard the cached restructure plan. Called when the active
    /// library folder changes/clears/wipes: a plan is computed for one root and
    /// otherwise never invalidated, so a stale plan for the OLD folder would
    /// keep a live Apply that moves files in a library the user has moved on
    /// from (CRITICAL audit finding). Nulling it fires PropertyChanged →
    /// RestructureView clears the on-screen plan.</summary>
    public void InvalidateRestructurePlan() => LastRestructurePlan = null;

    public long RestructurePlanDiscardedSignal
    {
        get => _restructurePlanDiscardedSignal;
        private set => Set(ref _restructurePlanDiscardedSignal, value);
    }

    internal long CaptureRestructurePlanRevision()
    {
        var revision = Volatile.Read(ref _restructureMutationRevision);
        Volatile.Write(ref _pendingRestructurePlanRevision, revision);
        return revision;
    }

    internal void AbandonRestructurePlanRevision(long revision)
        => Interlocked.CompareExchange(
            ref _pendingRestructurePlanRevision,
            -1,
            revision);

    internal static bool ShouldAcceptRestructurePlan(
        long requestedRevision,
        long currentRevision)
        => requestedRevision >= 0 && requestedRevision == currentRevision;

    internal static bool MutationInvalidatesRestructurePlan(EventPayload? payload)
        => payload switch
        {
            PhaseChangedEvent phase when phase.Phase is
                ScanPhase.Discovering or ScanPhase.Tagging or ScanPhase.PostScan => true,
            ScanCompleteEvent => true,
            FaceClusteringCompleteEvent => true,
            DeepAnalyzeStartingEvent => true,
            DeepAnalyzeCompleteEvent => true,
            BulkActionResultEvent bulk => bulk.Result.Succeeded > 0,
            LibraryWipedEvent wiped => wiped.Result.Ok,
            _ => false,
        };

    internal static bool RestructureResultInvalidatesPlan(
        bool wasUndo,
        bool forwardRunWasUndoable,
        RestructureApplyResult result)
        => result.Applied > 0 && (wasUndo || forwardRunWasUndoable);

    private RestructureApplyResult? _lastRestructureApplyResult;
    public RestructureApplyResult? LastRestructureApplyResult
    {
        get => _lastRestructureApplyResult;
        private set => Set(ref _lastRestructureApplyResult, value);
    }

    /// <summary>M4: true when the current <see cref="LastRestructureApplyResult"/>
    /// is the reply to an Undo (not an Apply). Undo and Apply replies share the
    /// same slot; this is captured from <see cref="UndoRestructureInFlight"/> as
    /// the terminal arrives and paired with the result, so a view reading it on a
    /// later dispatcher turn (or an OnLoaded replay) still tells the two apart and
    /// can say "undone" instead of "applied".</summary>
    public bool LastRestructureApplyResultWasUndo { get; private set; }
    public bool LastRestructureApplyResultWasShortcutUndo { get; private set; }

    private bool _canUndoRestructure;
    /// <summary>True once an applyRestructure may have moved files and they haven't
    /// been fully undone yet — drives the "Undo last run" button. (R2)</summary>
    public bool CanUndoRestructure
    {
        get => _canUndoRestructure;
        private set => Set(ref _canUndoRestructure, value);
    }
    private string? _pendingRestructureApplyRoot;
    private bool _pendingRestructureApplyUndoable;
    internal string? UndoRestructureRoot { get; private set; }
    internal string? UndoRestructureShortcutToken { get; private set; }
    /// Set by UndoRestructureAsync so the next RestructureApplyResult is read as
    /// the undo's reply rather than a fresh apply.
    internal bool UndoRestructureInFlight { get; set; }
    internal bool UndoRestructureInFlightWasShortcut { get; set; }

    internal static bool? NextCanUndoRestructure(
        bool wasUndo,
        RestructureApplyResult result,
        bool forwardRunWasUndoable = true,
        bool wasShortcutUndo = false)
    {
        if (wasShortcutUndo) return null;
        if (wasUndo)
        {
            // A cancelled undo deliberately keeps its journal so the user can
            // retry it (restructure_apply.rs: "a cancelled undo must leave
            // the original intact"). Not consulting Cancelled here hid the
            // Undo button while the journal — and the still-relocated files —
            // remained, with no way back except relaunching the app.
            return result.Cancelled
                || result.Failed > 0
                || !string.IsNullOrWhiteSpace(result.PrivilegeError);
        }
        if (!forwardRunWasUndoable) return null;
        return result.Applied > 0 ? true : null;
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

    /// <summary>Most recent out-of-process video thumbnail rendered by the
    /// engine. ThumbnailService observes this (via PropertyChanged) to write
    /// the base64 JPEG under its (Path, ModifiedAt) cache key.</summary>
    private ThumbnailGenerated? _lastThumbnailGenerated;
    public ThumbnailGenerated? LastThumbnailGenerated
    {
        get => _lastThumbnailGenerated;
        private set => Set(ref _lastThumbnailGenerated, value);
    }

    /// <summary>latest CUDA/cuDNN re-probe result from the engine.
    /// Settings → Performance "Verify install" binds to this to flip the
    /// card to ✓ or surface a diagnostics string on failure.</summary>
    private HardwareReprobed? _lastHardwareReprobe;
    public HardwareReprobed? LastHardwareReprobe
    {
        get => _lastHardwareReprobe;
        private set => Set(ref _lastHardwareReprobe, value);
    }

    /// <summary>Result of the most recent engine-side wipeLibrary. The wipe
    /// flow (SidebarFolderHeader) waits on this via WipeLibraryAndWaitAsync.</summary>
    private LibraryWiped? _lastLibraryWiped;
    public LibraryWiped? LastLibraryWiped
    {
        get => _lastLibraryWiped;
        set => Set(ref _lastLibraryWiped, value);
    }

    private int _deepAnalyzePresentationGeneration = -1;
    private DeepAnalyzeStarting? _deepAnalyzeStarting;
    public DeepAnalyzeStarting? DeepAnalyzeStarting
    {
        get => _deepAnalyzeStarting;
        private set => Set(ref _deepAnalyzeStarting, value);
    }

    // AutoPilotStage enum + CurrentAutoPilotStage property removed.
    // The AutoPilot button is gone (macOS doesn't have one); auto-advance
    // from scan → face clustering is wired directly into Apply's
    // ScanCompleteEvent handler. There's no multi-stage tracker to feed.

    private ScanPhase? _phase;
    public ScanPhase? Phase
    {
        get => _phase;
        private set => Set(ref _phase, value);
    }

    /// <summary>Discovering/Tagging/PostScan — a scan actively holds the
    /// engine's mutation gate. A Deep Analyze command issued now queues behind
    /// it. (Fix A)</summary>
    internal static bool IsActiveScanPhase(ScanPhase? phase)
        => phase is ScanPhase.Discovering or ScanPhase.Tagging or ScanPhase.PostScan;

    /// <summary>True while a Deep Analyze command is waiting on the engine's
    /// mutation gate behind a running scan (or any other job). In that window
    /// the engine emits only QueueState — no DeepAnalyzeStarting/Progress — so
    /// the Deep Analyze view must show "queued" and must NOT arm its 45 s
    /// warm-up watchdog (which would false-fire "model took too long to load"
    /// on a healthy, merely-waiting job). Detected from the live scan phase
    /// and from a deepAnalyze job sitting in the QueueState pending list. (Fix A)</summary>
    public bool DeepAnalyzeQueuedBehindScan
    {
        get
        {
            if (IsActiveScanPhase(_phase)) return true;
            var qs = _queueState;
            if (qs?.Pending is { } pending)
            {
                for (int i = 0; i < pending.Count; i++)
                {
                    if (pending[i].Category == JobCategory.DeepAnalyze) return true;
                }
            }
            return false;
        }
    }

    /// <summary>Hot stream of every IPC event. Used by tests + the optional
    /// transcript log. Subscribe via System.Reactive.</summary>
    public IObservable<IpcEvent> Events => _events;

    public event PropertyChangedEventHandler? PropertyChanged;
    internal event Action<DeepAnalyzeFileDone>? DeepAnalyzeFileDoneReceived;

    private EngineClient()
    {
        // The singleton MUST be first-touched on the UI thread (App.OnLaunched
        // ensures this). If it's first touched from a thread-pool thread,
        // GetForCurrentThread returns null and there's no recovery — every
        // subsequent _ui.TryEnqueue would silently no-op. Throw early so
        // the misuse surfaces as a clean exception instead of silent UI
        // staleness across the lifetime of the app.
        _ui = DispatcherQueue.GetForCurrentThread()
              ?? throw new InvalidOperationException(
                  "EngineClient must be constructed on the UI thread. "
                  + "First-touch the singleton from App.OnLaunched, not from a Task.Run continuation.");
        RefreshPersistedRestructureUndo();
    }

    private const int MaxPersistedUndoLineBytes = 64 * 1024;
    private const int MaxPersistedUndoManifests = 1024;
    private const int MaxPersistedUndoStagingEntries = 1024;

    internal readonly record struct PersistedRealUndo(
        string LibraryRoot,
        DateTime UpdatedUtc);

    internal readonly record struct PersistedShortcutUndo(
        string LibraryRoot,
        string Token,
        DateTime UpdatedUtc);

    private void RefreshPersistedRestructureUndo(string? completedShortcutToken = null)
    {
        var realUndo = ReadPersistedRestructureUndo(
            Path.Combine(AppPaths.Root, "restructure_undo.ndjson"));
        var shortcutUndo = ReadPersistedShortcutUndo(
            Path.Combine(AppPaths.Root, "restructure_shortcut_undo"),
            completedShortcutToken);

        if (shortcutUndo is { } tiedShortcut
            && realUndo is { } tiedReal
            && tiedShortcut.UpdatedUtc == tiedReal.UpdatedUtc)
        {
            UndoRestructureRoot = null;
            UndoRestructureShortcutToken = null;
            CanUndoRestructure = false;
            return;
        }
        if (shortcutUndo is { } persistedShortcut
            && (realUndo is null || persistedShortcut.UpdatedUtc > realUndo.Value.UpdatedUtc))
        {
            UndoRestructureRoot = persistedShortcut.LibraryRoot;
            UndoRestructureShortcutToken = persistedShortcut.Token;
        }
        else
        {
            UndoRestructureRoot = realUndo?.LibraryRoot;
            UndoRestructureShortcutToken = null;
        }
        CanUndoRestructure = UndoRestructureRoot is not null;
    }

    internal static PersistedShortcutUndo? ReadPersistedShortcutUndo(
        string manifestDirectory,
        string? excludedToken = null)
    {
        try
        {
            if (!Directory.Exists(manifestDirectory)) return null;
            PersistedShortcutUndo? newest = null;
            var newestIsAmbiguous = false;
            var inspected = 0;
            foreach (var path in Directory.EnumerateFiles(
                manifestDirectory,
                "*.ndjson",
                SearchOption.TopDirectoryOnly))
            {
                if (++inspected > MaxPersistedUndoManifests) return null;
                try
                {
                    if (!TryGetRegularFileAttributes(path, out _)) continue;
                    var token = Path.GetFileNameWithoutExtension(path);
                    if (!Guid.TryParseExact(token, "D", out var parsed)
                        || !string.Equals(parsed.ToString("D"), token, StringComparison.Ordinal))
                    {
                        continue;
                    }
                    using var stream = new FileStream(
                        path,
                        FileMode.Open,
                        FileAccess.Read,
                        FileShare.ReadWrite | FileShare.Delete);
                    if (!TryReadBoundedUtf8Line(stream, out var headerLine)
                        || string.IsNullOrWhiteSpace(headerLine)
                        || !TryParseShortcutHeader(
                            headerLine,
                            token,
                            out var versionNumber,
                            out var libraryRoot,
                            out var stagingDirectory))
                    {
                        continue;
                    }

                    if (!TryGetLastWriteTimeUtc(path, out var updatedUtc))
                    {
                        continue;
                    }

                    var hasCommittedEntries = false;
                    var invalidCommittedEntry = false;
                    while (true)
                    {
                        if (!TryReadBoundedUtf8Line(stream, out var entryLine))
                        {
                            invalidCommittedEntry = true;
                            break;
                        }
                        if (entryLine is null) break;
                        if (!TryValidateShortcutEntry(
                            entryLine,
                            versionNumber,
                            libraryRoot,
                            stagingDirectory))
                        {
                            invalidCommittedEntry = true;
                            break;
                        }
                        hasCommittedEntries = true;
                    }
                    if (invalidCommittedEntry) continue;

                    if (!hasCommittedEntries
                        && (versionNumber != 3
                            || stagingDirectory is null
                            || !TryReadPendingShortcutIntents(
                                libraryRoot,
                                token,
                                stagingDirectory,
                                ref updatedUtc)))
                    {
                        continue;
                    }

                    if (string.Equals(token, excludedToken, StringComparison.Ordinal)) continue;
                    var candidate = new PersistedShortcutUndo(
                        libraryRoot,
                        token,
                        updatedUtc);
                    if (newest is null || candidate.UpdatedUtc > newest.Value.UpdatedUtc)
                    {
                        newest = candidate;
                        newestIsAmbiguous = false;
                    }
                    else if (candidate.UpdatedUtc == newest.Value.UpdatedUtc)
                    {
                        newestIsAmbiguous = true;
                    }
                }
                catch (Exception ex)
                {
                    DebugLog.Warn(
                        "Skipping unreadable Restructure shortcut undo manifest: " + ex.Message);
                }
            }
            return newestIsAmbiguous ? null : newest;
        }
        catch (Exception ex)
        {
            DebugLog.Warn("Could not read persisted Restructure shortcut undo metadata: " + ex.Message);
            return null;
        }
    }

    private static bool TryParseShortcutHeader(
        string headerLine,
        string token,
        out int versionNumber,
        out string libraryRoot,
        out string? stagingDirectory)
    {
        versionNumber = 0;
        libraryRoot = "";
        stagingDirectory = null;
        using var document = System.Text.Json.JsonDocument.Parse(headerLine);
        var header = document.RootElement;
        if (header.ValueKind != System.Text.Json.JsonValueKind.Object
            || !header.TryGetProperty("version", out var version)
            || !version.TryGetInt32(out versionNumber)
            || versionNumber is not (2 or 3)
            || !header.TryGetProperty("library_root", out var rootProperty)
            || rootProperty.ValueKind != System.Text.Json.JsonValueKind.String
            || !header.TryGetProperty("token", out var tokenProperty)
            || tokenProperty.ValueKind != System.Text.Json.JsonValueKind.String
            || !string.Equals(tokenProperty.GetString(), token, StringComparison.Ordinal)
            || !TryNormalizeAbsolutePath(rootProperty.GetString(), out libraryRoot))
        {
            return false;
        }

        if (versionNumber == 2) return true;
        if (!header.TryGetProperty("staging_dir", out var stagingProperty)
            || stagingProperty.ValueKind != System.Text.Json.JsonValueKind.String
            || !TryNormalizeAbsolutePath(stagingProperty.GetString(), out stagingDirectory)
            || !header.TryGetProperty("staging_dir_identity", out var stagingIdentity)
            || !IsFileIdentity(stagingIdentity))
        {
            return false;
        }

        var expectedStagingDirectory = Path.GetFullPath(Path.Combine(
            libraryRoot,
            ".fileid-restructure-shortcut-staging",
            token));
        return string.Equals(
            stagingDirectory,
            expectedStagingDirectory,
            StringComparison.OrdinalIgnoreCase);
    }

    private static bool TryValidateShortcutEntry(
        string entryLine,
        int versionNumber,
        string libraryRoot,
        string? stagingDirectory)
    {
        using var document = System.Text.Json.JsonDocument.Parse(entryLine);
        var entry = document.RootElement;
        if (entry.ValueKind != System.Text.Json.JsonValueKind.Object
            || !entry.TryGetProperty("file_id", out var fileId)
            || !fileId.TryGetInt64(out _)
            || !TryGetPathProperty(entry, "source", out var source)
            || !TryGetPathProperty(entry, "link", out var link)
            || !IsPathWithinRoot(source, libraryRoot)
            || !IsPathWithinRoot(link, libraryRoot)
            || !entry.TryGetProperty("source_identity", out var sourceIdentity)
            || !IsFileIdentity(sourceIdentity)
            || !entry.TryGetProperty("link_identity", out var linkIdentity)
            || !IsFileIdentity(linkIdentity))
        {
            return false;
        }

        if (!entry.TryGetProperty("staging_link", out var stagingLink)
            || stagingLink.ValueKind == System.Text.Json.JsonValueKind.Null)
        {
            return true;
        }
        if (versionNumber != 3
            || stagingDirectory is null
            || stagingLink.ValueKind != System.Text.Json.JsonValueKind.String
            || !TryNormalizeAbsolutePath(stagingLink.GetString(), out var stagedPath)
            || !string.Equals(
                Path.GetDirectoryName(stagedPath),
                stagingDirectory,
                StringComparison.OrdinalIgnoreCase))
        {
            return false;
        }
        var stagedName = Path.GetFileName(stagedPath);
        return stagedName.EndsWith(".link", StringComparison.Ordinal)
            && IsCanonicalGuid(stagedName[..^".link".Length]);
    }

    private static bool TryReadPendingShortcutIntents(
        string libraryRoot,
        string token,
        string stagingDirectory,
        ref DateTime updatedUtc)
    {
        if (!TryGetDirectoryAttributes(stagingDirectory, out _)) return false;
        var intentIds = new HashSet<string>(StringComparer.Ordinal);
        var stagedLinkIds = new HashSet<string>(StringComparer.Ordinal);
        var inspected = 0;
        foreach (var path in Directory.EnumerateFileSystemEntries(
            stagingDirectory,
            "*",
            SearchOption.TopDirectoryOnly))
        {
            if (++inspected > MaxPersistedUndoStagingEntries) return false;
            var name = Path.GetFileName(path);
            if (name.EndsWith(".intent.json", StringComparison.Ordinal))
            {
                if (!TryGetRegularFileAttributes(path, out _)) return false;
                var operationId = name[..^".intent.json".Length];
                if (!IsCanonicalGuid(operationId)
                    || !TryValidateShortcutIntent(
                        path,
                        operationId,
                        libraryRoot,
                        token,
                        stagingDirectory))
                {
                    return false;
                }
                if (!TryGetLastWriteTimeUtc(path, out var intentUpdatedUtc)) return false;
                if (intentUpdatedUtc > updatedUtc) updatedUtc = intentUpdatedUtc;
                intentIds.Add(operationId);
                continue;
            }

            if (name.EndsWith(".link", StringComparison.Ordinal))
            {
                var operationId = name[..^".link".Length];
                if (!IsCanonicalGuid(operationId)
                    || !TryGetReparseFileAttributes(path, out _))
                {
                    return false;
                }
                stagedLinkIds.Add(operationId);
                continue;
            }
            return false;
        }

        return intentIds.Count > 0 && stagedLinkIds.All(intentIds.Contains);
    }

    private static bool TryValidateShortcutIntent(
        string intentPath,
        string operationId,
        string libraryRoot,
        string token,
        string stagingDirectory)
    {
        using var stream = new FileStream(
            intentPath,
            FileMode.Open,
            FileAccess.Read,
            FileShare.ReadWrite | FileShare.Delete);
        if (!TryReadBoundedUtf8Line(stream, out var line)
            || string.IsNullOrWhiteSpace(line)
            || !TryReadBoundedUtf8Line(stream, out var trailing)
            || trailing is not null)
        {
            return false;
        }

        using var document = System.Text.Json.JsonDocument.Parse(line);
        var intent = document.RootElement;
        if (intent.ValueKind != System.Text.Json.JsonValueKind.Object
            || !intent.TryGetProperty("version", out var version)
            || !version.TryGetInt32(out var versionNumber)
            || versionNumber != 1
            || !intent.TryGetProperty("token", out var intentToken)
            || intentToken.ValueKind != System.Text.Json.JsonValueKind.String
            || !string.Equals(intentToken.GetString(), token, StringComparison.Ordinal)
            || !intent.TryGetProperty("operation_id", out var intentOperationId)
            || intentOperationId.ValueKind != System.Text.Json.JsonValueKind.String
            || !string.Equals(
                intentOperationId.GetString(),
                operationId,
                StringComparison.Ordinal)
            || !intent.TryGetProperty("file_id", out var fileId)
            || !fileId.TryGetInt64(out _)
            || !TryGetPathProperty(intent, "source", out var source)
            || !TryGetPathProperty(intent, "link", out var link)
            || !TryGetPathProperty(intent, "staging_link", out var stagingLink)
            || !IsPathWithinRoot(source, libraryRoot)
            || !IsPathWithinRoot(link, libraryRoot)
            || !string.Equals(
                stagingLink,
                Path.GetFullPath(Path.Combine(stagingDirectory, operationId + ".link")),
                StringComparison.OrdinalIgnoreCase)
            || !intent.TryGetProperty("source_identity", out var sourceIdentity)
            || !IsFileIdentity(sourceIdentity))
        {
            return false;
        }
        return true;
    }

    private static bool TryGetPathProperty(
        System.Text.Json.JsonElement element,
        string name,
        out string path)
    {
        path = "";
        return element.TryGetProperty(name, out var property)
            && property.ValueKind == System.Text.Json.JsonValueKind.String
            && TryNormalizeAbsolutePath(property.GetString(), out path);
    }

    private static bool TryNormalizeAbsolutePath(string? value, out string path)
    {
        path = "";
        if (string.IsNullOrWhiteSpace(value) || !Path.IsPathFullyQualified(value)) return false;
        path = Path.GetFullPath(value);
        return true;
    }

    private static bool IsPathWithinRoot(string path, string root)
    {
        var relative = Path.GetRelativePath(root, path);
        return !Path.IsPathRooted(relative)
            && !string.Equals(relative, ".", StringComparison.Ordinal)
            && !string.Equals(relative, "..", StringComparison.Ordinal)
            && !relative.StartsWith(
                ".." + Path.DirectorySeparatorChar,
                StringComparison.Ordinal)
            && !relative.StartsWith(
                ".." + Path.AltDirectorySeparatorChar,
                StringComparison.Ordinal);
    }

    private static bool IsFileIdentity(System.Text.Json.JsonElement element)
        => element.ValueKind == System.Text.Json.JsonValueKind.Object
            && element.TryGetProperty("volume", out var volume)
            && volume.TryGetUInt64(out _)
            && element.TryGetProperty("file", out var file)
            && file.TryGetUInt64(out _);

    private static bool IsCanonicalGuid(string value)
        => Guid.TryParseExact(value, "D", out var parsed)
            && string.Equals(parsed.ToString("D"), value, StringComparison.Ordinal);

    private static bool TryGetRegularFileAttributes(
        string path,
        out FileAttributes attributes)
    {
        attributes = File.GetAttributes(path);
        return IsRegularPersistedUndoFileAttributes(attributes);
    }

    internal static bool IsRegularPersistedUndoFileAttributes(FileAttributes attributes)
        => (attributes & (FileAttributes.Directory | FileAttributes.ReparsePoint)) == 0;

    private static bool TryGetReparseFileAttributes(
        string path,
        out FileAttributes attributes)
    {
        attributes = File.GetAttributes(path);
        return (attributes & FileAttributes.Directory) == 0
            && (attributes & FileAttributes.ReparsePoint) != 0;
    }

    private static bool TryGetDirectoryAttributes(
        string path,
        out FileAttributes attributes)
    {
        attributes = File.GetAttributes(path);
        return (attributes & FileAttributes.Directory) != 0
            && (attributes & FileAttributes.ReparsePoint) == 0;
    }

    private static bool TryGetLastWriteTimeUtc(string path, out DateTime updatedUtc)
    {
        try
        {
            updatedUtc = File.GetLastWriteTimeUtc(path);
            return true;
        }
        catch
        {
            updatedUtc = DateTime.MinValue;
            return false;
        }
    }

    private static bool TryReadBoundedUtf8Line(Stream stream, out string? line)
    {
        line = null;
        using var buffer = new MemoryStream(capacity: 256);
        while (true)
        {
            var next = stream.ReadByte();
            if (next < 0) return buffer.Length == 0;
            if (next == '\n')
            {
                try
                {
                    line = new UTF8Encoding(
                        encoderShouldEmitUTF8Identifier: false,
                        throwOnInvalidBytes: true)
                        .GetString(buffer.GetBuffer(), 0, checked((int)buffer.Length));
                    return true;
                }
                catch (DecoderFallbackException)
                {
                    return false;
                }
            }
            if (buffer.Length >= MaxPersistedUndoLineBytes) return false;
            buffer.WriteByte((byte)next);
        }
    }

    internal static string? ReadPersistedRestructureUndoRoot(string journalPath)
        => ReadPersistedRestructureUndo(journalPath)?.LibraryRoot;

    internal static PersistedRealUndo? ReadPersistedRestructureUndo(string journalPath)
    {
        try
        {
            var currentExists = File.Exists(journalPath);
            var currentValid = TryReadPersistedRestructureUndoJournal(
                journalPath,
                out var currentRoot,
                out var currentHasWork);
            if (currentValid && currentHasWork)
            {
                return TryGetLastWriteTimeUtc(journalPath, out var currentUpdatedUtc)
                    ? new PersistedRealUndo(currentRoot!, currentUpdatedUtc)
                    : null;
            }
            if (currentExists && !currentValid) return null;

            var directory = Path.GetDirectoryName(journalPath);
            var fileName = Path.GetFileName(journalPath);
            if (string.IsNullOrWhiteSpace(directory)
                || string.IsNullOrWhiteSpace(fileName)
                || !Directory.Exists(directory))
            {
                return null;
            }

            var prefix = $".{fileName}.prior-";
            var candidates = Directory
                .EnumerateFiles(directory, prefix + "*", SearchOption.TopDirectoryOnly)
                .Where(path =>
                {
                    var name = Path.GetFileName(path);
                    var suffix = name.Length > prefix.Length ? name[prefix.Length..] : "";
                    return Guid.TryParseExact(suffix, "D", out var parsed)
                        && string.Equals(parsed.ToString("D"), suffix, StringComparison.Ordinal);
                })
                .Take(2)
                .ToArray();
            if (candidates.Length != 1
                || !TryReadPersistedRestructureUndoJournal(
                    candidates[0],
                    out var priorRoot,
                    out var priorHasWork)
                || !priorHasWork)
            {
                return null;
            }
            if (currentValid
                && !string.Equals(currentRoot, priorRoot, StringComparison.OrdinalIgnoreCase))
            {
                return null;
            }
            return TryGetLastWriteTimeUtc(candidates[0], out var priorUpdatedUtc)
                ? new PersistedRealUndo(priorRoot!, priorUpdatedUtc)
                : null;
        }
        catch (Exception ex)
        {
            DebugLog.Warn("Could not read persisted Restructure undo metadata: " + ex.Message);
            return null;
        }
    }

    private static bool TryReadPersistedRestructureUndoJournal(
        string journalPath,
        out string? libraryRoot,
        out bool hasWork)
    {
        libraryRoot = null;
        hasWork = false;
        if (!File.Exists(journalPath)
            || (File.GetAttributes(journalPath) & FileAttributes.ReparsePoint) != 0)
        {
            return false;
        }
        using var stream = new FileStream(
            journalPath, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete);
        if (!TryReadBoundedUtf8Line(stream, out var firstLine)
            || string.IsNullOrWhiteSpace(firstLine))
        {
            return false;
        }
        using var document = System.Text.Json.JsonDocument.Parse(firstLine);
        var root = document.RootElement;
        if (!root.TryGetProperty("version", out var version)
            || !version.TryGetInt32(out var versionNumber)
            || versionNumber is not (2 or 3)
            || !root.TryGetProperty("library_root", out var rootProperty)
            || rootProperty.ValueKind != System.Text.Json.JsonValueKind.String
            || !TryNormalizeAbsolutePath(rootProperty.GetString(), out var normalizedRoot))
        {
            return false;
        }
        libraryRoot = normalizedRoot;
        if (!TryReadBoundedUtf8Line(stream, out var firstEntry)) return false;
        if (firstEntry is null) return true;
        using var entryDocument = System.Text.Json.JsonDocument.Parse(firstEntry);
        var entry = entryDocument.RootElement;
        hasWork = entry.TryGetProperty("file_id", out _)
            && entry.TryGetProperty("from", out _)
            && entry.TryGetProperty("to", out _)
            && (versionNumber == 2
                || (entry.TryGetProperty("source_identity", out var sourceIdentity)
                    && IsFileIdentity(sourceIdentity)));
        return hasWork;
    }

    // ─── Lifecycle ─────────────────────────────────────────────────────

    internal bool CanFinalizeApplicationClose(long terminalRevision)
    {
        var process = _process;
        var processAlive = process is not null;
        if (process is not null)
        {
            try
            {
                processAlive = !process.HasExited;
            }
            catch
            {
                processAlive = true;
            }
        }
        return IsSafeToFinalizeApplicationClose(
            _lifecycle.IsTerminalStopCurrent(terminalRevision),
            Volatile.Read(ref _isStarting) != 0,
            processAlive);
    }

    internal static bool IsSafeToFinalizeApplicationClose(
        bool terminalStopActive,
        bool startInFlight,
        bool processAlive)
        => terminalStopActive && !startInFlight && !processAlive;

    private async Task RunLifecycleIntentAsync(
        EngineLifecycleIntent intent,
        Func<Task> action)
    {
        await _lifecycleGate.WaitAsync(intent.Token).ConfigureAwait(false);
        try
        {
            ThrowIfLifecycleIntentSuperseded(intent);
            await action().ConfigureAwait(false);
            ThrowIfLifecycleIntentSuperseded(intent);
        }
        finally
        {
            _lifecycleGate.Release();
        }
    }

    private async Task<T> RunLifecycleIntentAsync<T>(
        EngineLifecycleIntent intent,
        Func<Task<T>> action)
    {
        await _lifecycleGate.WaitAsync(intent.Token).ConfigureAwait(false);
        try
        {
            ThrowIfLifecycleIntentSuperseded(intent);
            var result = await action().ConfigureAwait(false);
            ThrowIfLifecycleIntentSuperseded(intent);
            return result;
        }
        finally
        {
            _lifecycleGate.Release();
        }
    }

    private static void ThrowIfLifecycleIntentSuperseded(
        EngineLifecycleIntent intent)
    {
        intent.Token.ThrowIfCancellationRequested();
        if (!intent.IsCurrent)
        {
            throw new OperationCanceledException(
                "A newer engine lifecycle action superseded this action.",
                intent.Token);
        }
    }

    private void ThrowIfStartSuperseded(
        long lifecycleRevision,
        CancellationToken lifecycleToken)
    {
        lifecycleToken.ThrowIfCancellationRequested();
        if (!_lifecycle.IsCurrent(lifecycleRevision, shouldRun: true))
        {
            throw new OperationCanceledException(
                "A newer engine lifecycle action superseded this start.",
                lifecycleToken);
        }
    }

    /// <summary>
    /// Spawn the engine. Idempotent — calling this while already running
    /// is a no-op. On failure the state goes to Crashed; the caller can
    /// poll/observe State to react.
    /// </summary>
    public async Task StartAsync()
    {
        using var intent = _lifecycle.Begin(shouldRun: true);
        await RunLifecycleIntentAsync(
            intent,
            () => StartCoreAsync(intent.Revision, intent.Token))
            .ConfigureAwait(false);
    }

    private async Task StartCoreAsync(
        long lifecycleRevision,
        CancellationToken lifecycleToken)
    {
        ThrowIfStartSuperseded(lifecycleRevision, lifecycleToken);
        if (!_ui.HasThreadAccess)
        {
            var completion = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
            if (!_ui.TryEnqueue(async () =>
            {
                try
                {
                    await StartCoreAsync(lifecycleRevision, lifecycleToken);
                    completion.TrySetResult();
                }
                catch (Exception ex)
                {
                    completion.TrySetException(ex);
                }
            }))
            {
                throw new InvalidOperationException("The UI dispatcher rejected the engine start request.");
            }
            await completion.Task.ConfigureAwait(false);
            return;
        }

        ThrowIfStartSuperseded(lifecycleRevision, lifecycleToken);
        if (_process is { HasExited: false })
        {
            // Engine is already running. Don't touch _isStarting — another
            // caller may legitimately hold the gate while completing a
            // spawn we collided with.
            return;
        }
        // strict CAS gate. The earlier formulation (BUG-3 comment)
        // claimed the gate, then declined to bail when it lost the race,
        // letting a second StartAsync caller fall through to a parallel
        // spawn. That produced occasional double-spawn with a shared stdin/
        // stdout pair — the second engine crashed on bind or fed corrupt
        // IPC. Now: if we lose the CAS, return immediately.
        if (Interlocked.CompareExchange(ref _isStarting, 1, 0) != 0)
        {
            DebugLog.Info("EngineClient.StartAsync: spawn already in flight; skipping.");
            return;
        }

        // every code path below — including early-return on
        // signature verdicts, hash failures, and the spawn catch — must
        // release `_isStarting`, otherwise the gate latches at 1 forever
        // and OnProcessExited's respawn can't claim it. Wrap the whole
        // body in try/finally so the release is unconditional.
        Process? startedProcess = null;
        try
        {
            ThrowIfStartSuperseded(lifecycleRevision, lifecycleToken);
            if (_process is { HasExited: true })
            {
                Cleanup();
            }
            ResetProcessBoundScanState();

            // Notify singleton services that any cached engine state is now
            // stale and they should re-attach to PropertyChanged. Cheap +
            // idempotent.
            try { Services.ModelInstallerService.Instance.Reset(); } catch { /* swallow */ }

            State = LifecycleState.Starting;
            CrashReason = null;
            Interlocked.Exchange(ref _expectingExit, 0);
            _lastSpawnAttempt = DateTime.UtcNow;

            var enginePath = AppPaths.EngineExePath;
            DebugLog.Info($"EngineClient: spawning {PathRedactor.Redact(enginePath)}");

            var expectedThumb = Environment.GetEnvironmentVariable("FILEID_SIGN_THUMBPRINT")
                ?? Environment.GetEnvironmentVariable("FILEID_EV_THUMBPRINT");
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
                if (requireSignedEngine)
                {
                    spawnPin = await Task.Run(() =>
                        new FileStream(enginePath, FileMode.Open, FileAccess.Read, FileShare.Read));
                }
            }
            catch (Exception ex)
            {
                CrashReason = "Engine binary could not be pinned for signature verification: " + ex.Message;
                State = LifecycleState.Crashed;
                DebugLog.Error("EngineClient: engine pin failed — refusing to verify or spawn.");
                return;
            }
            using var spawnPinLease = spawnPin;

            // The deny-write/delete lease is acquired before path verification and
            // kept until Process.Start has opened the same image.
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
                        DebugLog.Error("EngineClient: unsigned engine refused by the embedded release policy.");
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
                    // Engine stdin is BOM-free NDJSON.
                    StandardInputEncoding = new System.Text.UTF8Encoding(encoderShouldEmitUTF8Identifier: false),
                    StandardOutputEncoding = new System.Text.UTF8Encoding(encoderShouldEmitUTF8Identifier: false),
                    StandardErrorEncoding = new System.Text.UTF8Encoding(encoderShouldEmitUTF8Identifier: false),
                    WorkingDirectory = AppPaths.Root,
                };
                // Pass the FILEID_LOG env to control engine tracing verbosity
                // (debug in dev profiles, info in release).
                psi.Environment["FILEID_LOG"] = Environment.GetEnvironmentVariable("FILEID_LOG") ?? "info";

                // Restructure folder-granularity (Settings ▸ Restructure). The engine
                // reads FILEID_RESTRUCTURE_GRANULARITY at plan time; forward the user's
                // saved choice at spawn so it applies on the next engine start.
                // "normal"/unset is the calibrated default, so only a validated
                // non-default value is forwarded. Mirrors macOS EngineClient.spawn.
                var granularity = AppSettings.Load().RestructureGranularity;
                if (granularity is "loose" or "tight")
                {
                    psi.Environment["FILEID_RESTRUCTURE_GRANULARITY"] = granularity;
                }

                ThrowIfStartSuperseded(lifecycleRevision, lifecycleToken);
                startedProcess = Process.Start(psi)
                    ?? throw new InvalidOperationException("Process.Start returned null");
                _process = startedProcess;
                _stdin = startedProcess.StandardInput;

                _readCts = new CancellationTokenSource();
                var ct = _readCts.Token;
                var generation = SpawnGeneration;
                _stdoutLoop = Task.Run(
                    () => StdoutLoopAsync(
                        startedProcess,
                        startedProcess.StandardOutput,
                        generation,
                        ct),
                    ct);
                _stderrLoop = Task.Run(
                    () => StderrLoopAsync(startedProcess.StandardError, ct),
                    ct);

                // Subscribe before enabling events so an immediate exit is observed.
                startedProcess.Exited += OnProcessExited;
                startedProcess.EnableRaisingEvents = true;
            }
            catch (OperationCanceledException)
                when (lifecycleToken.IsCancellationRequested
                    || !_lifecycle.IsCurrent(lifecycleRevision, shouldRun: true))
            {
                throw;
            }
            catch (Exception ex)
            {
                if (startedProcess is not null)
                {
                    try
                    {
                        if (!startedProcess.HasExited)
                        {
                            startedProcess.Kill(entireProcessTree: true);
                            startedProcess.WaitForExit(5_000);
                        }
                    }
                    catch { }
                    if (ReferenceEquals(_process, startedProcess))
                    {
                        Cleanup();
                    }
                    else
                    {
                        try { startedProcess.Dispose(); } catch { }
                    }
                }
                DebugLog.Error("EngineClient.StartAsync failed: " + ex.Message);
                CrashReason = ex.Message;
                State = LifecycleState.Crashed;
                return;
            }

            ThrowIfStartSuperseded(lifecycleRevision, lifecycleToken);
        }
        catch (OperationCanceledException)
            when (lifecycleToken.IsCancellationRequested
                || !_lifecycle.IsCurrent(lifecycleRevision, shouldRun: true))
        {
            TerminateSupersededStart(startedProcess);
            throw;
        }
        finally
        {
            // unconditional gate release. Every early-return path
            // above + the spawn-catch + the normal completion path all
            // converge here so OnProcessExited's respawn can always CAS
            // the gate back from 0 → 1.
            Interlocked.Exchange(ref _isStarting, 0);
        }
    }

    private void TerminateSupersededStart(Process? process)
    {
        if (process is not null && ReferenceEquals(_process, process))
        {
            var exited = false;
            try
            {
                if (!process.HasExited)
                {
                    process.Kill(entireProcessTree: true);
                }
                exited = process.WaitForExit(5_000);
            }
            catch (Exception ex)
            {
                DebugLog.Warn(
                    "EngineClient: superseded start termination failed: "
                    + ex.Message);
            }

            if (exited && ReferenceEquals(_process, process))
            {
                Cleanup();
            }
        }

        if (!_lifecycle.ShouldRun
            && (_process is null || _process.HasExited))
        {
            CrashReason = StoppedReason;
            State = LifecycleState.Crashed;
        }
    }

    /// <summary>S4: read one newline-delimited engine frame, bounded to
    /// <see cref="MaxFrameBytes"/> UTF-8 bytes. A frame that exceeds the cap before a
    /// newline arrives is discarded and we resync to the next newline, so a
    /// never-terminating line can't OOM the UI. Returns null at EOF. All framing
    /// state lives in the caller-owned <paramref name="st"/>, so each
    /// StdoutLoopAsync owns its own — no cross-loop sharing (#22).</summary>
    internal static async Task<string?> ReadBoundedFrameAsync(
        StreamReader reader,
        StdoutFraming st,
        CancellationToken ct,
        int maxFrameBytes = MaxFrameBytes)
    {
        while (true)
        {
            // Emit a completed frame if the buffer already holds a newline. Scan
            // only the not-yet-scanned tail ([Scanned..Length)): during a long
            // frame's accumulation Scanned == Length so this is a no-op, and after
            // a Remove it rescans only the small (<= one chunk) leftover — so the
            // multi-MB buffer is never re-indexed per chunk. (audit A0)
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
                st.Utf8Bytes = Math.Max(0, st.Utf8Bytes - Encoding.UTF8.GetByteCount(frame) - 1);
                if (st.Resyncing)
                {
                    // This frame is the tail of an oversize line — drop it and
                    // resume normal framing from the next one.
                    st.Resyncing = false;
                    continue;
                }
                if (frame.Length > 0 && frame[^1] == '\r') frame = frame[..^1];
                if (Encoding.UTF8.GetByteCount(frame) > maxFrameBytes)
                {
                    DebugLog.Warn($"Engine emitted an oversize IPC frame (> {maxFrameBytes} UTF-8 bytes); discarding.");
                    st.OversizeDropped = true;
                    continue;
                }
                return frame;
            }
            // Everything currently buffered is newline-free.
            st.Scanned = st.Buffer.Length;
            // No newline yet: if the buffer crossed the cap, the engine is
            // emitting an oversize/garbage frame. Drop it and resync.
            if (st.Utf8Bytes > maxFrameBytes)
            {
                DebugLog.Warn($"Engine emitted an oversize IPC frame (> {maxFrameBytes} UTF-8 bytes); discarding and resyncing.");
                st.Buffer.Clear();
                st.Scanned = 0;
                st.Utf8Bytes = 0;
                st.Resyncing = true;
                st.OversizeDropped = true;
            }
            int read = await reader.ReadAsync(st.Chunk.AsMemory(), ct).ConfigureAwait(false);
            if (read == 0)
            {
                // EOF. Surface a trailing partial frame, unless we were mid-resync.
                if (!st.Resyncing && st.Buffer.Length > 0)
                {
                    string tail = st.Buffer.ToString();
                    st.Buffer.Clear();
                    st.Scanned = 0;
                    st.Utf8Bytes = 0;
                    if (tail.Length > 0 && tail[^1] == '\r') tail = tail[..^1];
                    return tail;
                }
                return null;
            }
            // Detect a newline in the freshly-read chunk via a FLAT array scan
            // (O(read)) instead of re-indexing the StringBuilder. If this chunk
            // carries a newline, rewind Scanned to just before it so the loop's
            // indexed scan examines only this chunk's bytes — never the whole
            // accumulated buffer (the O(n^2) on a large frame). (audit A0)
            int bufLenBefore = st.Buffer.Length;
            st.Buffer.Append(st.Chunk, 0, read);
            st.Utf8Bytes += Encoding.UTF8.GetByteCount(st.Chunk, 0, read);
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

    private async Task StdoutLoopAsync(
        Process process,
        StreamReader reader,
        int generation,
        CancellationToken ct)
    {
        // Per-loop framing state — a respawn starts a fresh loop with its own
        // buffer, so stale bytes can never carry over or race (#22).
        var framing = new StdoutFraming();
        // No stdout idle watchdog: it killed healthy idle engines (e.g.
        // after auto-install of llama runtimes the engine sits quietly
        // waiting for the user — 5 min
        // of "idle" tripped the watchdog and forced a respawn that the
        // respawn-CAS double-bookkeeping then dropped). A watchdog can't
        // distinguish "engine hung" from "engine idle waiting for user".
        // Genuine engine hangs are caught by:
        //   - the engine's own parent-PID watchdog (which kills the
        //     engine if the C# app dies),
        //   - the engine's GPU TDR detection (sticky cancellation +
        //     EngineError), and
        //   - per-command timeouts on the C# side (WaitForReadyAsync,
        //     CudaAutoInstaller's 30-min cap, etc).
        // A global stdout idle timer is the wrong granularity.
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
                await HandleTransportFailureAsync(
                    "stdout read",
                    ex,
                    process,
                    generation).ConfigureAwait(false);
                return;
            }
            if (framing.OversizeDropped)
            {
                framing.OversizeDropped = false;
                // Surface the drop through the normal event-dispatch path: Apply
                // runs on the UI thread, so observable state is never written from
                // this loop thread (the off-UI-thread fast-fail class).
                var oversize = IpcEvent.Now(new ErrorEvent(new EngineError(
                    "ipc_frame_too_large",
                    "The engine sent a response too large for the app to read, so it was dropped. " +
                    "If this was a Restructure plan on a very large library, the plan may be incomplete — " +
                    "try restructuring a subfolder.",
                    null)));
                _ui.TryEnqueue(() => Apply(oversize, generation));
            }
            if (line is null)
            {
                if (!ct.IsCancellationRequested && IsTrackedLiveProcess(process, generation))
                {
                    await HandleTransportFailureAsync(
                        "stdout EOF",
                        new EndOfStreamException(
                            "The engine closed stdout while its process was still running."),
                        process,
                        generation).ConfigureAwait(false);
                }
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

            if (ev.Payload is HealthCheckResultEvent health)
            {
                if (!_healthWaiters.TryResolve(
                        health.Result.RequestId,
                        health.Result.Pid,
                        generation))
                {
                    DebugLog.Warn(
                        $"[IPC IN] ignored unmatched healthCheckResult " +
                        $"request={health.Result.RequestId}, pid={health.Result.Pid}, " +
                        $"generation={generation}.");
                }
                continue;
            }

            // Marshal to UI thread before touching observable state.
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
            // Engine writes structured tracing JSON to stderr. The engine
            // SHOULD redact paths via redact_path_for_log, but as a
            // belt-and-suspenders defense the C# bridge also passes any
            // detected path through PathRedactor. The detection is
            // best-effort: lines containing a Windows-shaped absolute
            // path (drive letter + colon + backslash) get reformatted
            // with the canonical home-tilde substitution.
            DebugLog.Debug("[engine] " + RedactWindowsPathsInLine(line));
        }
    }

    private static string RedactWindowsPathsInLine(string line)
    {
        // Cheap path detection: only run the regex if there's a `\` in
        // the line. Most engine tracing lines (event counters, model
        // names, performance numbers) won't match.
        if (line.IndexOf('\\') < 0) return line;
        return s_pathInLine.Replace(line, m => PathRedactor.Redact(m.Value));
    }

    // Match a drive path (C:\…) OR a UNC path (\\server\…), terminating only
    // on quotes / brackets / commas / line ends — NOT on spaces. Windows paths
    // routinely contain spaces ("C:\Users\Bob Smith\…"); stopping at the first
    // space truncated the match and leaked the remainder ("Smith\…") into the
    // log. Over-matching a little trailing text is fine — PathRedactor still
    // collapses the home prefix to ~ and nothing PII escapes.
    private static readonly System.Text.RegularExpressions.Regex s_pathInLine =
        new(@"(?:[A-Za-z]:\\|\\\\)[^"",)\]}>\r\n]*",
            System.Text.RegularExpressions.RegexOptions.Compiled);

    private bool IsTrackedLiveProcess(Process process, int generation)
    {
        if (generation != SpawnGeneration || !ReferenceEquals(process, _process))
        {
            return false;
        }
        try
        {
            return !process.HasExited;
        }
        catch
        {
            return false;
        }
    }

    private bool IsTransportFailureActive(int generation)
    {
        lock (_transportFailureLock)
        {
            return _transportFailureGeneration == generation
                && !_transportFailureTask.IsCompleted;
        }
    }

    private Task HandleTransportFailureAsync(
        string source,
        Exception error,
        Process process,
        int generation)
    {
        lock (_transportFailureLock)
        {
            if (generation != SpawnGeneration || !ReferenceEquals(process, _process))
            {
                return Task.CompletedTask;
            }
            if (_transportFailureGeneration == generation
                && !_transportFailureTask.IsCompleted)
            {
                return _transportFailureTask;
            }

            _transportFailureGeneration = generation;
            _transportFailureTask = ReplaceBrokenTransportAsync(
                source,
                error,
                process,
                generation);
            return _transportFailureTask;
        }
    }

    private async Task ReplaceBrokenTransportAsync(
        string source,
        Exception error,
        Process process,
        int generation)
    {
        int? pid = null;
        bool? hasExited = null;
        try
        {
            pid = process.Id;
            hasExited = process.HasExited;
        }
        catch { }

        DebugLog.Error(
            $"[ENGINE-TRANSPORT] {source} failed: {error.Message}; " +
            $"state={State}, generation={generation}, pid={pid?.ToString() ?? "?"}, " +
            $"hasExited={hasExited?.ToString() ?? "?"}, stdin={(_stdin is null ? "null" : "set")}, " +
            $"stdoutLoop={_stdoutLoop?.Status.ToString() ?? "null"}, " +
            $"processTracked={ReferenceEquals(process, _process)}.");

        _healthWaiters.FailGeneration(
            generation,
            new IOException("The engine command channel failed.", error));
        await TransitionFromReadyForTransportFailureAsync(generation).ConfigureAwait(false);

        if (!IsTrackedLiveProcess(process, generation))
        {
            return;
        }

        var lifecycleRevision = _lifecycle.CurrentRevision;
        if (_lifecycle.IsCurrent(lifecycleRevision, shouldRun: true))
        {
            ArmExpectedExitRestart(lifecycleRevision);
        }
        else
        {
            ClearExpectedExitRestart();
        }
        Interlocked.Exchange(ref _expectedExitProcess, process);
        Interlocked.Exchange(ref _expectingExitAtTicks, DateTime.UtcNow.Ticks);
        Interlocked.Exchange(ref _expectingExit, 1);

        StreamWriter? brokenStdin = null;
        lock (_writeLock)
        {
            if (generation == SpawnGeneration
                && ReferenceEquals(process, _process))
            {
                brokenStdin = _stdin;
                _stdin = null;
            }
        }
        try { brokenStdin?.Dispose(); }
        catch (Exception closeError)
        {
            DebugLog.Warn(
                "[ENGINE-TRANSPORT] closing broken stdin failed: " +
                closeError.Message);
        }

        if (await WaitForExactProcessExitAsync(
                process,
                TimeSpan.FromMilliseconds(750)).ConfigureAwait(false))
        {
            return;
        }

        try
        {
            process.Kill(entireProcessTree: true);
        }
        catch (Exception killError)
        {
            DebugLog.Error(
                "[ENGINE-TRANSPORT] could not terminate the broken engine process: " +
                killError.Message);
        }

        if (!await WaitForExactProcessExitAsync(
                process,
                TimeSpan.FromSeconds(10)).ConfigureAwait(false))
        {
            ClearExpectedExitRestart(lifecycleRevision);
            await TransitionToTerminalTransportCrashAsync(generation)
                .ConfigureAwait(false);
        }
    }

    private static async Task<bool> WaitForExactProcessExitAsync(
        Process process,
        TimeSpan timeout)
    {
        try
        {
            if (process.HasExited) return true;
            await process.WaitForExitAsync().WaitAsync(timeout).ConfigureAwait(false);
            return process.HasExited;
        }
        catch (TimeoutException)
        {
            return false;
        }
        catch
        {
            try { return process.HasExited; }
            catch { return false; }
        }
    }

    private Task TransitionFromReadyForTransportFailureAsync(int generation)
    {
        var completion = new TaskCompletionSource(
            TaskCreationOptions.RunContinuationsAsynchronously);
        void Transition()
        {
            try
            {
                if (generation == SpawnGeneration && State == LifecycleState.Ready)
                {
                    State = LifecycleState.Starting;
                    CrashReason = "The engine command channel stopped responding. Restarting it now.";
                }
                completion.TrySetResult();
            }
            catch (Exception ex)
            {
                completion.TrySetException(ex);
            }
        }

        if (_ui.HasThreadAccess)
        {
            Transition();
        }
        else if (!_ui.TryEnqueue(Transition))
        {
            completion.TrySetException(
                new InvalidOperationException(
                    "The UI dispatcher rejected the engine transport-failure transition."));
        }
        return completion.Task;
    }

    private Task TransitionToTerminalTransportCrashAsync(int generation)
    {
        var completion = new TaskCompletionSource(
            TaskCreationOptions.RunContinuationsAsynchronously);
        void Transition()
        {
            try
            {
                if (generation == SpawnGeneration)
                {
                    State = LifecycleState.Crashed;
                    CrashReason =
                        "FileID could not stop the engine after its command channel failed. " +
                        "Close FileID before trying again so the catalog writer is not orphaned.";
                }
                completion.TrySetResult();
            }
            catch (Exception ex)
            {
                completion.TrySetException(ex);
            }
        }

        if (_ui.HasThreadAccess)
        {
            Transition();
        }
        else if (!_ui.TryEnqueue(Transition))
        {
            completion.TrySetException(
                new InvalidOperationException(
                    "The UI dispatcher rejected the terminal engine transport transition."));
        }
        return completion.Task;
    }

    private void ArmExpectedExitRestart(long lifecycleRevision)
    {
        lock (_expectedExitRestartGate)
        {
            _restartAfterExpectedExitRevision = lifecycleRevision;
            _restartAfterExpectedExit = 1;
        }
    }

    private void ClearExpectedExitRestart(long? lifecycleRevision = null)
    {
        lock (_expectedExitRestartGate)
        {
            if (lifecycleRevision.HasValue
                && _restartAfterExpectedExitRevision
                    != lifecycleRevision.Value)
            {
                return;
            }
            _restartAfterExpectedExit = 0;
            _restartAfterExpectedExitRevision = -1;
        }
    }

    private long ConsumeExpectedExitRestartRevision()
    {
        lock (_expectedExitRestartGate)
        {
            if (_restartAfterExpectedExit != 1)
            {
                _restartAfterExpectedExitRevision = -1;
                return -1;
            }
            var revision = _restartAfterExpectedExitRevision;
            _restartAfterExpectedExit = 0;
            _restartAfterExpectedExitRevision = -1;
            return revision;
        }
    }

    private long ResolveCurrentExpectedExitRestartRevision()
    {
        var armedRevision = ConsumeExpectedExitRestartRevision();
        if (armedRevision >= 0
            && _lifecycle.IsCurrent(armedRevision, shouldRun: true))
        {
            return armedRevision;
        }

        var currentRevision = _lifecycle.CurrentRevision;
        return _lifecycle.IsCurrent(currentRevision, shouldRun: true)
            ? currentRevision
            : -1;
    }

    private bool ConsumeExpectedExit(Process? exited)
    {
        if (exited is null || !ReferenceEquals(exited, Volatile.Read(ref _expectedExitProcess)))
        {
            return false;
        }
        if (!ReferenceEquals(
                Interlocked.CompareExchange(ref _expectedExitProcess, null, exited),
                exited))
        {
            return false;
        }
        if (Interlocked.Exchange(ref _expectingExit, 0) != 1)
        {
            return false;
        }

        var setAt = new DateTime(Interlocked.Read(ref _expectingExitAtTicks), DateTimeKind.Utc);
        if (DateTime.UtcNow - setAt <= ExpectingExitWindow)
        {
            return true;
        }
        DebugLog.Warn("EngineClient: stale _expectingExit ignored; treating exit as a crash.");
        return false;
    }

    private void OnProcessExited(object? sender, EventArgs e)
    {
        // Capture the exit code from the process that ACTUALLY exited (sender),
        // not the mutable _process field — by the time this UI-thread callback
        // runs, _process may already point at a respawned process or be
        // disposed, and reading its ExitCode then throws and aborts cleanup.
        var exited = sender as System.Diagnostics.Process;
        int? exitCode = null;
        try { exitCode = exited?.ExitCode; } catch { /* not-exited / disposed */ }
        _ui.TryEnqueue(() =>
        {
            var expectedExit = ConsumeExpectedExit(exited);
            // Ignore a stale exit from a process we've already replaced. In the
            // RestartAsync path (StopAndWaitForExitAsync → StartAsync), the OLD
            // process's Exited callback is queued to the UI thread and can run
            // AFTER StartAsync installed the NEW _process/_stdin/_readCts. `sender`
            // is the exact Process that exited; if it is no longer the live
            // _process, running Cleanup() here would tear down the freshly-spawned
            // engine (cancel its read loops, null its stdin) and mis-count it as a
            // crash — wedging IPC with State stuck Starting. Dispose the dead sender
            // and bail, leaving the live engine untouched.
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

            // Notify install service immediately so any in-flight download
            // owned by the now-dead engine flips to Failed instead of
            // spinning forever. Runs on every exit (graceful shutdown,
            // crash + respawn, 3-strike terminal crash). Idempotent. A
            // deliberate (expected) exit — app close or an app-driven restart —
            // gets a neutral caption instead of the false "Engine restarted".
            try { Services.ModelInstallerService.Instance.Reset(cleanShutdown: expectedExit); }
            catch (Exception ex) { DebugLog.Warn("OnProcessExited: ModelInstallerService.Reset threw: " + ex.Message); }

            if (expectedExit)
            {
                ResetProcessBoundScanState();
                CrashReason = StoppedReason;
                State = LifecycleState.Crashed;
                var restartRevision =
                    ResolveCurrentExpectedExitRestartRevision();
                if (restartRevision >= 0)
                {
                    _ = StartAfterLateExpectedExitAsync(restartRevision);
                }
                return;
            }

            var respawnRevision = _lifecycle.CurrentRevision;
            if (!_lifecycle.IsCurrent(
                    respawnRevision,
                    shouldRun: true))
            {
                ResetProcessBoundScanState();
                CrashReason = StoppedReason;
                State = LifecycleState.Crashed;
                return;
            }

            // Auto-respawn with bounded backoff. The 3-strike window is
            // 60 s wide; failures beyond that reset the counter.
            var now = DateTime.UtcNow;
            // R5-07: a crash that follows a STABLE Ready (engine ran continuously
            // for >= StabilitySettle) is genuine recovery — clear the strike state.
            // A crash that recurs faster than that (Ready→crash flapping) is NOT
            // recovery and must keep ticking toward the 3-strike terminal cap.
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

            // 1s, 4s, 16s schedule.
            var delay = _consecutiveFailures switch
            {
                1 => TimeSpan.FromSeconds(1),
                2 => TimeSpan.FromSeconds(4),
                _ => TimeSpan.FromSeconds(16),
            };
            DebugLog.Info($"EngineClient: respawning in {delay.TotalSeconds}s (attempt {_consecutiveFailures}/3).");
            // No outer CAS — StartAsync has its own strict CAS at the top;
            // an outer one was actively harmful: setting _isStarting=1
            // here caused StartAsync's own CAS to see "already starting"
            // and bail, so every auto-respawn was silently dropped.
            _ = StartAfterCrashDelayAsync(delay, respawnRevision);
        });
    }

    private async Task StartAfterCrashDelayAsync(
        TimeSpan delay,
        long lifecycleRevision)
    {
        await Task.Delay(delay).ConfigureAwait(false);
        await StartIfCurrentAsync(
            lifecycleRevision,
            "EngineClient: respawn StartAsync threw: ")
            .ConfigureAwait(false);
    }

    private Task StartAfterLateExpectedExitAsync(long lifecycleRevision)
        => StartIfCurrentAsync(
            lifecycleRevision,
            "EngineClient: late expected-exit restart failed: ");

    private async Task StartIfCurrentAsync(
        long lifecycleRevision,
        string failurePrefix)
    {
        await _lifecycleGate.WaitAsync().ConfigureAwait(false);
        try
        {
            if (!_lifecycle.IsCurrent(
                    lifecycleRevision,
                    shouldRun: true))
            {
                return;
            }

            await StartCoreAsync(
                lifecycleRevision,
                CancellationToken.None).ConfigureAwait(false);
        }
        catch (OperationCanceledException)
            when (!_lifecycle.IsCurrent(
                lifecycleRevision,
                shouldRun: true))
        {
        }
        catch (Exception ex)
        {
            DebugLog.Error(failurePrefix + ex.Message);
        }
        finally
        {
            _lifecycleGate.Release();
        }
    }

    private void ResetProcessBoundScanState()
    {
        Phase = ScanPhase.Idle;
        LastProgress = null;
        LastBatch = null;
        LastError = null;
        IsPaused = false;
        _scanStartedAt = null;
        Interlocked.Increment(ref _scanControlRevision);
        _shownPhaseRank = -1;
        _lastProgressEmit = DateTime.MinValue;
        _lastProgressPhase = null;
    }

    private void Cleanup()
    {
        var retiringGeneration = SpawnGeneration;
        _healthWaiters.FailGeneration(
            retiringGeneration,
            new InvalidOperationException(
                "The engine stopped before confirming command-channel health."));
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

        // R5-06: release the auto-advance gates on every engine teardown. A crash
        // mid-clustering (or mid-deep-analyze) never emits the Complete/Failed
        // event that normally clears these, so without this a respawned engine
        // would see the gate still held and skip auto face-clustering / deep
        // analyze for the rest of the session. Cleanup() is the single teardown
        // chokepoint (crash, graceful shutdown, Dispose); the dead engine's
        // in-flight job is moot here. (Mirrors macOS handleEngineExit.)
        Interlocked.Exchange(ref _faceClusterAutoInFlight, 0);
        Interlocked.Exchange(ref _autoDeepAnalyzeInFlight, 0);
        RetireDeepAnalyzeGeneration(retiringGeneration);
        RetireScanStartGeneration(retiringGeneration);
        // Clear the observable mirror of _faceClusterAutoInFlight too: a crash
        // mid-clustering never emits the FaceClusteringComplete / face_clustering_failed
        // arm that normally flips it false, so without this the Library "finding
        // people…" banner stays up and the bool desyncs from its just-reset int gate.
        // Marshal through _ui (the property must be set on the UI thread); Cleanup()
        // can run from Dispose() off any thread. (audit R3-app)
        _ui.TryEnqueue(() => FaceClusteringInFlight = false);
        // An engine death mid-Deep-Analyze never emits the terminal
        // DeepAnalyzeComplete that clears the progress observables, so the
        // Library "Deep Analyze running…" banner and the Deep Analyze tab's
        // stream card (Cancel enabled, Analyze All disabled) stayed latched
        // for the rest of the session. Synthesize a cancelled terminal result
        // carrying the last known counts so every subscriber unlatches through
        // the same path a real completion takes; the next run's
        // DeepAnalyzeStarting clears it again. UI-marshaled like the flags above.
        _ui.TryEnqueue(() =>
        {
            if (Volatile.Read(ref _deepAnalyzePresentationGeneration) != retiringGeneration
                || DeepAnalyzeProgress is null && DeepAnalyzeStarting is null)
            {
                return;
            }
            var prog = DeepAnalyzeProgress;
            var modelKind = prog?.ModelKind ?? DeepAnalyzeStarting?.ModelKind ?? string.Empty;
            DebugLog.Warn("EngineClient: engine exited mid-Deep-Analyze; synthesizing a cancelled DeepAnalyzeComplete so the UI unlatches.");
            DeepAnalyzeComplete = new DeepAnalyzeComplete(
                prog?.Processed ?? 0, 0, 0, modelKind, Cancelled: true);
            DeepAnalyzeProgress = null;
            DeepAnalyzeStarting = null;
            Interlocked.CompareExchange(ref _deepAnalyzePresentationGeneration, -1, retiringGeneration);
        });
        // Same for the undo affordance: a crash mid-undo never emits the terminal
        // restructureApplyResult that clears this, so the next apply's result would
        // be mis-attributed as the dead undo's. (audit R2-app)
        UndoRestructureInFlight = false;
        UndoRestructureInFlightWasShortcut = false;
        // Bump the process generation LAST so an observer that reads it after a
        // State PropertyChanged sees the post-teardown value. Views holding a
        // static in-flight guard keyed to a command sent to THIS process
        // (RestructureView's apply single-flight) compare generations to detect
        // that the owning engine is gone and its result will never arrive.
        Interlocked.Increment(ref _spawnGeneration);
        _ui.TryEnqueue(() => PropertyChanged?.Invoke(
            this, new PropertyChangedEventArgs(nameof(SpawnGeneration))));
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
        => EnsureCommandChannelReadyAsync(timeout, ct);

    public async Task EnsureCommandChannelReadyAsync(
        TimeSpan timeout,
        CancellationToken ct = default)
    {
        var stopwatch = Stopwatch.StartNew();
        TimeSpan Remaining()
        {
            var remaining = timeout - stopwatch.Elapsed;
            if (remaining <= TimeSpan.Zero)
            {
                throw new TimeoutException(
                    $"Engine command channel did not become healthy within " +
                    $"{timeout.TotalSeconds:0}s.");
            }
            return remaining;
        }

        await WaitForLifecycleReadyAsync(Remaining(), ct).ConfigureAwait(false);
        var firstGeneration = SpawnGeneration;
        try
        {
            await ProbeCommandChannelAsync(
                MinTimeout(Remaining(), HealthProbeTimeout),
                ct).ConfigureAwait(false);
            return;
        }
        catch (OperationCanceledException) when (ct.IsCancellationRequested)
        {
            throw;
        }
        catch (Exception ex)
        {
            DebugLog.Warn(
                $"[ENGINE-HEALTH] generation {firstGeneration} failed its " +
                $"correlated probe: {ex.Message}");
        }

        await WaitForLifecycleReadyAsync(
            Remaining(),
            ct,
            generationAfter: firstGeneration).ConfigureAwait(false);
        await ProbeCommandChannelAsync(
            MinTimeout(Remaining(), HealthProbeTimeout),
            ct).ConfigureAwait(false);
    }

    private static TimeSpan MinTimeout(TimeSpan left, TimeSpan cap)
        => left <= cap ? left : cap;

    private async Task ProbeCommandChannelAsync(
        TimeSpan timeout,
        CancellationToken ct)
    {
        ct.ThrowIfCancellationRequested();
        if (State != LifecycleState.Ready)
        {
            throw new InvalidOperationException(
                $"Engine is not Ready for a health probe (state={State}).");
        }

        var generation = SpawnGeneration;
        var process = _process
            ?? throw new InvalidOperationException(
                "Engine reported Ready without a tracked process.");
        int pid;
        bool hasExited;
        try
        {
            hasExited = process.HasExited;
            if (hasExited)
            {
                throw new InvalidOperationException(
                    "Engine reported Ready after its process exited.");
            }
            pid = process.Id;
        }
        catch (InvalidOperationException)
        {
            throw;
        }
        catch (Exception ex)
        {
            throw new InvalidOperationException(
                "Could not identify the Ready engine process.",
                ex);
        }

        if (!IsHealthTargetCurrent(
                generation,
                SpawnGeneration,
                process,
                _process,
                hasExited))
        {
            throw new InvalidOperationException(
                "The engine changed while preparing its health probe.");
        }
        var requestId = Guid.NewGuid().ToString("N");
        var waiter = _healthWaiters.Register(requestId, generation, pid);
        if (!IsHealthTargetCurrent(
                generation,
                SpawnGeneration,
                process,
                _process,
                capturedHasExited: false))
        {
            var changed = new InvalidOperationException(
                "The engine changed before its health probe could be sent.");
            _healthWaiters.TryFail(requestId, changed);
            _ = waiter.Task.Exception;
            throw changed;
        }
        try
        {
            await SendCommandAsync(new HealthCheckCommand(requestId), ct)
                .ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            _healthWaiters.TryFail(requestId, ex);
            _ = waiter.Task.Exception;
            throw;
        }

        try
        {
            await waiter.Task.WaitAsync(timeout, ct).ConfigureAwait(false);
        }
        catch (TimeoutException ex)
        {
            var timeoutError = new TimeoutException(
                $"Engine generation {generation} (pid {pid}) did not answer " +
                $"health probe {requestId} within {timeout.TotalSeconds:0.###}s.",
                ex);
            _healthWaiters.TryFail(requestId, timeoutError);
            _ = waiter.Task.Exception;
            await HandleTransportFailureAsync(
                "health probe timeout",
                timeoutError,
                process,
                generation).ConfigureAwait(false);
            throw timeoutError;
        }
        catch (OperationCanceledException ex)
        {
            _healthWaiters.TryFail(requestId, ex);
            _ = waiter.Task.Exception;
            throw;
        }
    }

    internal static bool IsHealthTargetCurrent(
        int capturedGeneration,
        int currentGeneration,
        Process capturedProcess,
        Process? currentProcess,
        bool capturedHasExited)
        => !capturedHasExited
            && capturedGeneration == currentGeneration
            && ReferenceEquals(capturedProcess, currentProcess);

    private async Task WaitForLifecycleReadyAsync(
        TimeSpan timeout,
        CancellationToken ct,
        int? generationAfter = null)
    {
        ct.ThrowIfCancellationRequested();
        bool IsReady()
            => State == LifecycleState.Ready
                && (!generationAfter.HasValue
                    || SpawnGeneration != generationAfter.Value)
                && !IsTransportFailureActive(SpawnGeneration);

        if (IsReady()) return;
        if (State == LifecycleState.Crashed
            && !generationAfter.HasValue
            && !IsTransportFailureActive(SpawnGeneration))
        {
            throw new InvalidOperationException(
                "Engine has crashed: " + (CrashReason ?? "unknown reason"));
        }
        var tcs = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        PropertyChangedEventHandler? handler = null;
        handler = (_, e) =>
        {
            if (e.PropertyName is not nameof(State)
                and not nameof(SpawnGeneration))
            {
                return;
            }
            if (IsReady())
            {
                tcs.TrySetResult(true);
            }
            else if (State == LifecycleState.Crashed
                     && !generationAfter.HasValue
                     && !IsTransportFailureActive(SpawnGeneration))
            {
                tcs.TrySetException(new InvalidOperationException(
                    "Engine crashed while waiting for ready: " + (CrashReason ?? "unknown reason")));
            }
        };
        PropertyChanged += handler;
        // Re-check both terminal states after subscribing so a transition in
        // the precheck/attach window cannot strand the waiter until timeout.
        if (IsReady())
        {
            PropertyChanged -= handler;
            return;
        }
        if (State == LifecycleState.Crashed
            && !generationAfter.HasValue
            && !IsTransportFailureActive(SpawnGeneration))
        {
            PropertyChanged -= handler;
            throw new InvalidOperationException(
                "Engine crashed while waiting for ready: " + (CrashReason ?? "unknown reason"));
        }
        using var cts = CancellationTokenSource.CreateLinkedTokenSource(ct);
        cts.CancelAfter(timeout);
        try
        {
            await tcs.Task.WaitAsync(cts.Token).ConfigureAwait(false);
        }
        catch (OperationCanceledException) when (!ct.IsCancellationRequested)
        {
            throw new TimeoutException(
                $"Engine did not become Ready within {timeout.TotalSeconds:0}s " +
                $"(current state: {State}, generation: {SpawnGeneration}).");
        }
        finally
        {
            PropertyChanged -= handler;
        }
    }

    // ─── Event router ──────────────────────────────────────────────────

    private void Apply(IpcEvent ev, int generation)
    {
        if (generation != SpawnGeneration)
        {
            DebugLog.Debug($"[IPC IN] dropped stale event from generation {generation}; current={SpawnGeneration}");
            return;
        }

        // per-event diagnostic tracing. See _applySeq comment above
        // for why this exists. Only logs the event TYPE, never the payload
        // (payloads can contain user file paths — those route through
        // existing per-arm logging which already path-redacts).
        var applySeq = Interlocked.Increment(ref _applySeq);
        var applyEventName = ev.Payload?.GetType().Name ?? "<null>";
        DebugLog.Info($"[APPLY:{applySeq}] enter {applyEventName} tid={Environment.CurrentManagedThreadId}");

        // top-level try/wrap so a throw inside Set<T> →
        // PropertyChanged subscriber fanout cannot escape Apply into the
        // dispatcher loop. Two layers: inner catches per-arm routing
        // exceptions + writes a crash dump; outer catches anything else
        // (subject OnNext, sampling counter increment, the inner catch's
        // own logging). Worst case: log + carry on.
        try
        {
            // Always raise to subscribers first, even if the routing below
            // throws (defense-in-depth — never silently drop an event).
            // Scan FileDone events are sampled (every Nth) because a fast scan
            // can emit hundreds per second and subscribers (LibraryView) don't
            // need every one to feel responsive.
            bool publishToSubject = ev.Payload is not HealthCheckResultEvent;
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
                if (MutationInvalidatesRestructurePlan(ev.Payload))
                {
                    var revision = Interlocked.Increment(ref _restructureMutationRevision);
                    DebugLog.Info(
                        $"[RESTRUCTURE] inputs changed at revision {revision} after " +
                        $"{ev.Payload?.GetType().Name}; invalidating cached plan.");
                    if (LastRestructurePlan is not null)
                    {
                        InvalidateRestructurePlan();
                    }
                }

                switch (ev.Payload)
                {
                    case HealthCheckResultEvent:
                        break;
                    case ReadyEvent r:
                        Info = r.Info;
                        lock (_transportFailureLock)
                        {
                            if (_transportFailureGeneration != generation
                                || _transportFailureTask.IsCompleted)
                            {
                                _transportFailureGeneration = -1;
                                _transportFailureTask = Task.CompletedTask;
                            }
                        }
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
                        // C1: re-arm the background auto-installers BEFORE
                        // flipping State to Ready. Their one-shot attempt gate
                        // latches after the first fire; a crash that interrupted
                        // a mid-flight model download would otherwise abandon the
                        // model for the rest of the session (no VLM tags until a
                        // full app restart). Re-arming here lets the State=Ready
                        // PropertyChanged below re-trigger them; each re-checks
                        // its sentinel/weights and only re-downloads if still
                        // missing. Harmless on the first Ready (nothing attempted
                        // yet → the gate is already 0).
                        Services.LlamaRuntimeAutoInstaller.ResetAttempt();
                        Services.CudaAutoInstaller.ResetAttempt();
                        State = LifecycleState.Ready;
                        CrashReason = null;
                        // R5-07: record when the engine reached Ready, but do NOT
                        // reset the strike counter here. An engine that reaches
                        // Ready then deterministically crashes within seconds (a
                        // re-armed auto-installer re-firing a fatal model load, or
                        // the first command hitting a fatal native path) would
                        // otherwise zero the counter on every respawn and flap ~1s
                        // forever, never reaching terminal Crashed. OnProcessExited
                        // treats a crash AFTER >= StabilitySettle of continuous
                        // Ready as genuine recovery and only then clears the
                        // counter — preserving the corrupt-.gguf "user removed the
                        // bad file" recovery without the flap.
                        _lastReadyAt = DateTime.UtcNow;
                        break;
                    case ProgressEvent p:
                        ObserveAuthoritativeScanEvent(generation);
                        // Discovery + tagging emit ProgressEvents CONCURRENTLY
                        // during the pipeline overlap (discovery still walking
                        // while tagging workers consume). A late Discovering event
                        // carries processed=0, eta=None, fps=0 and its own memory
                        // reading; letting it replace LastProgress after Tagging
                        // started made the sidebar's Tagged / ETA / Memory flicker
                        // (N→0→N, real→"computing"→real, two RSS readings
                        // alternating). Gate the WHOLE event on a monotonic phase
                        // rank: drop any ProgressEvent whose phase is below the
                        // latch, so LastProgress only ever holds one phase's stats
                        // at a time. Tagging events carry the LIVE discovered count
                        // (scan_session.rs), so "Discovered" keeps climbing from
                        // them through the overlap. Equal-or-higher rank advances
                        // the latch; the authoritative PhaseChangedEvent below also
                        // syncs it.
                        var progRank = PhaseRank(p.Progress.Phase);
                        if (progRank < _shownPhaseRank)
                        {
                            // A late lower-rank (Discovering) event during the
                            // discovery/tagging overlap. Don't regress the
                            // displayed Phase or its stats — but don't discard its
                            // counts either. The engine holds Total at 0 until the
                            // discovery walk finishes, so the event that first
                            // carries the real Total (and the freshest Discovered
                            // count) can arrive tagged Discovering AFTER Tagging
                            // already latched. Merge those two forward; guard on an
                            // actual change so this stays off the hot path.
                            if (LastProgress is { } shown)
                            {
                                var mergedDiscovered = Math.Max(shown.Discovered, p.Progress.Discovered);
                                var mergedTotal = Math.Max(shown.Total, p.Progress.Total);
                                if (mergedDiscovered != shown.Discovered || mergedTotal != shown.Total)
                                {
                                    LastProgress = shown with { Discovered = mergedDiscovered, Total = mergedTotal };
                                }
                            }
                            break;
                        }
                        _shownPhaseRank = progRank;
                        // Throttle the heavy LastProgress-bound sidebar repaint to
                        // 10 Hz; a phase boundary bypasses the throttle so the
                        // Discovering→Tagging stat handoff is immediate (no blip).
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
                        if (pc.Phase != ScanPhase.Idle) ObserveAuthoritativeScanEvent(generation);
                        Phase = pc.Phase;
                        // Authoritative phase boundary — sync the monotonic latch
                        // so a late interleaved ProgressEvent can't pull the
                        // displayed phase back below it.
                        _shownPhaseRank = PhaseRank(pc.Phase);
                        // On cancel, also clear the in-flight tracking state.
                        // The sidebar's CompletedPanel binds to LastScanDuration
                        // + LastProgress; without this clear the prior-scan
                        // numbers linger after the user hits Cancel mid-scan.
                        if (pc.Phase == ScanPhase.Cancelled)
                        {
                            Interlocked.Increment(ref _scanControlRevision);
                            IsPaused = false;
                            _scanStartedAt = null;
                            LastProgress = null;
                            LastBatch = null;
                        }
                        // Faces persist incrementally during a scan (dbwriter
                        // commits per-batch), but auto-clustering otherwise fires
                        // ONLY on ScanComplete. A Failed scan would leave
                        // already-detected faces with no persons row, so fire the
                        // (idempotent, zero-face-safe) auto-cluster there too so
                        // persisted faces still surface. A user-Cancelled scan
                        // instead DEFERS clustering to a manual re-cluster — the
                        // user explicitly stopped, and auto-firing a clustering
                        // pass on cancel races the engine's own teardown.
                        if (pc.Phase == ScanPhase.Failed && !GpuDeviceRemoved)
                        {
                            FaceClusteringInFlight = true;
                            _ = AutoTriggerFaceClusteringAsync();
                        }
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
                    case ScanCompleteEvent sce:
                        ObserveAuthoritativeScanEvent(generation);
                        Interlocked.Increment(ref _scanControlRevision);
                        // Authoritative final count for the completed-scan summary
                        // (LastProgress.Processed can be throttle-stale by a batch).
                        LastScanProcessedFiles = sce.Result.ProcessedFiles;
                        IsPaused = false;
                        if (_scanStartedAt.HasValue)
                        {
                            LastScanDuration = DateTime.UtcNow - _scanStartedAt.Value;
                            _scanStartedAt = null;
                        }
                        // The engine now emits scanComplete for cancelled scans
                        // too (IPC parity with macOS — it carries the final
                        // counts). Cancelled is the terminal phase the user
                        // chose: don't overwrite it with Completed, and don't
                        // auto-advance to clustering (macOS gates its engine-side
                        // auto-cluster on !isCancelled the same way).
                        if (Phase == ScanPhase.Cancelled)
                        {
                            break;
                        }
                        Phase = ScanPhase.Completed;
                        _shownPhaseRank = PhaseRank(ScanPhase.Completed);
                        // auto-advance to face clustering, matching macOS.
                        // macOS engine itself auto-enqueues face clustering when the
                        // scan finishes (FileIDEngineMain.swift:535+ ::
                        // autoEnqueueFaceClusteringIfNeeded). On Windows the Rust
                        // engine doesn't have that hook yet, so the app fires the
                        // IPC after observing ScanComplete. The engine's
                        // RunFaceClustering handler is a no-op when there are
                        // zero face_prints (matches macOS's "no faces → skip"
                        // path), so this is safe even on a library with no images.
                        // Deep Analyze stays manual — matches macOS, which gates
                        // it on the user naming ≥1 person first.
                        FaceClusteringInFlight = true;
                        _ = AutoTriggerFaceClusteringAsync();
                        break;
                    case ErrorEvent e:
                        if (e.Error.Kind == "gpu_device_removed")
                        {
                            _gpuDeviceRemovedGeneration = generation;
                            GpuDeviceRemoved = true;
                        }
                        if (e.Error.Kind == "undo_restructure")
                        {
                            UndoRestructureInFlight = false;
                            UndoRestructureInFlightWasShortcut = false;
                        }
                        if (e.Error.Kind.StartsWith("plan_restructure", StringComparison.Ordinal))
                        {
                            Interlocked.Exchange(ref _pendingRestructurePlanRevision, -1);
                        }
                        if (IsNonFatalWarningKind(e.Error.Kind))
                        {
                            LastWarning = e.Error;
                            if (e.Error.Kind == "scan_already_running")
                            {
                                RejectScanStartCommand(generation);
                            }
                            else if (e.Error.Kind == "deep_analyze_already_running")
                            {
                                FenceRejectedDeepAnalyzeCommand(generation, e.Error.Message);
                            }
                            DebugLog.Info($"[IPC IN] engine warning: kind={e.Error.Kind} msg={e.Error.Message}");
                        }
                        else
                        {
                            LastError = e.Error;
                            DebugLog.Warn($"[IPC IN] engine error: kind={e.Error.Kind} msg={e.Error.Message} path={PathRedactor.Redact(e.Error.Path)}");
                        }
                        // PAR-111: a clustering FAILURE must release the auto gate,
                        // else auto-clustering stays suppressed for the rest of the
                        // session. Match the EXACT failure kind — not a broad
                        // Contains("cluster"), which also matched the newer
                        // "face_clustering_busy" bounce (a pass is still running — the
                        // opposite of a failure) and wrongly cleared the gate, letting
                        // a later rescan's auto-cluster be silently dropped. The
                        // in-flight pass emits its own FaceClusteringComplete /
                        // face_clustering_failed that legitimately releases the gate.
                        if (e.Error.Kind == "face_clustering_failed")
                        {
                            Interlocked.Exchange(ref _faceClusterAutoInFlight, 0);
                            FaceClusteringInFlight = false;
                        }
                        break;
                    case LogEvent:
                        // Engine LogLine events go to the transcript via Events.
                        // Local app log already captured stderr.
                        break;
                    case FaceClusteringCompleteEvent fc:
                        LastFaceClustering = fc.Result;
                        FaceClusteringInFlight = false;
                        Interlocked.Exchange(ref _faceClusterAutoInFlight, 0); // PAR-111: release the auto gate
                        break;
                    case DeepAnalyzeStartingEvent das:
                        MarkDeepAnalyzeCommandStarted(generation);
                        Volatile.Write(ref _deepAnalyzePresentationGeneration, generation);
                        DeepAnalyzeStarting = das.Starting;
                        // Clear the previous run's terminal result. DeepAnalyzeComplete
                        // is otherwise only cleared on the single-file path, so on a
                        // 2nd+ "Analyze All" run the stale Complete makes the view's
                        // SyncStream `complete` block clobber the live progress every
                        // tick — the buttons + status text visibly fight between live
                        // "{processed}/{total}" and the stale "Done — N captioned" at
                        // ~4 Hz, with Cancel wrongly greyed out for the whole run.
                        DeepAnalyzeComplete = null;
                        // Also clear the prior run's last-file result so a new run
                        // starts with no carried-over DeepAnalyzeLast — otherwise the
                        // view's run-start _proposedNameCount reset is immediately
                        // undone when SyncStream reprocesses the stale last in the
                        // same pass (over-counting smart-renames). (audit A13 re-audit)
                        DeepAnalyzeLast = null;
                        break;
                    case DeepAnalyzeProgressEvent dap:
                        MarkDeepAnalyzeCommandStarted(generation);
                        Volatile.Write(ref _deepAnalyzePresentationGeneration, generation);
                        DeepAnalyzeProgress = dap.Progress;
                        break;
                    case DeepAnalyzeFileDoneEvent dafd:
                        Volatile.Write(ref _deepAnalyzePresentationGeneration, generation);
                        // FileDone is terminal accounting, not droppable progress:
                        // concurrent wave members arrive back-to-back and every
                        // result must reach the view's proposed-name tally.
                        try { DeepAnalyzeFileDoneReceived?.Invoke(dafd.FileDone); }
                        catch (Exception ex) { DebugLog.Warn("Deep Analyze FileDone subscriber threw: " + ex.Message); }
                        DeepAnalyzeLast = dafd.FileDone;
                        break;
                    case DeepAnalyzeCompleteEvent dac:
                        if (!CompleteDeepAnalyzeCommand(generation, dac.Result, () =>
                            {
                                Volatile.Write(ref _deepAnalyzePresentationGeneration, generation);
                                DeepAnalyzeComplete = dac.Result;
                                DeepAnalyzeProgress = null;
                                DeepAnalyzeStarting = null;
                            }))
                        {
                            break;
                        }
                        // A1: a finished/cancelled auto-pass re-arms the gate so
                        // the next scan (or a later VLM-install) can trigger again.
                        Interlocked.Exchange(ref _autoDeepAnalyzeInFlight, 0);
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
                        var requestedRevision =
                            Interlocked.Exchange(ref _pendingRestructurePlanRevision, -1);
                        var currentRevision = Volatile.Read(ref _restructureMutationRevision);
                        if (!ShouldAcceptRestructurePlan(requestedRevision, currentRevision))
                        {
                            DebugLog.Warn(
                                $"[RESTRUCTURE] discarded stale plan for revision {requestedRevision}; " +
                                $"current revision is {currentRevision}.");
                            RestructurePlanDiscardedSignal++;
                            break;
                        }
                        LastRestructurePlan = rp.Plan;
                        break;
                    case RestructureApplyResultEvent rar:
                        // M4: capture whether this terminal is an Undo reply BEFORE
                        // UndoRestructureInFlight is cleared below, and set it before
                        // the result so it is already correct when the result's
                        // PropertyChanged enqueues the view's SyncApplyResult.
                        bool wasUndo = UndoRestructureInFlight;
                        bool wasShortcutUndo = wasUndo && UndoRestructureInFlightWasShortcut;
                        if (!wasShortcutUndo && RestructureResultInvalidatesPlan(
                            wasUndo,
                            _pendingRestructureApplyUndoable,
                            rar.Result))
                        {
                            InvalidateRestructurePlan();
                        }
                        LastRestructureApplyResultWasUndo = wasUndo;
                        LastRestructureApplyResultWasShortcutUndo = wasShortcutUndo;
                        _lastRestructureApplyResult = rar.Result;
                        PropertyChanged?.Invoke(
                            this,
                            new PropertyChangedEventArgs(nameof(LastRestructureApplyResult)));
                        var nextCanUndo = NextCanUndoRestructure(
                            wasUndo,
                            rar.Result,
                            _pendingRestructureApplyUndoable,
                            wasShortcutUndo);
                        if (wasUndo)
                        {
                            UndoRestructureInFlight = false;
                            UndoRestructureInFlightWasShortcut = false;
                            if (wasShortcutUndo
                                && IsSuccessfulRestructureUndoResult(rar.Result))
                            {
                                RefreshPersistedRestructureUndo(UndoRestructureShortcutToken);
                            }
                            else if (nextCanUndo == false)
                            {
                                UndoRestructureRoot = null;
                            }
                        }
                        else
                        {
                            if (!string.IsNullOrWhiteSpace(rar.Result.ShortcutUndoToken))
                            {
                                UndoRestructureShortcutToken = rar.Result.ShortcutUndoToken;
                                UndoRestructureRoot = _pendingRestructureApplyRoot;
                                CanUndoRestructure = UndoRestructureRoot is not null;
                            }
                            else if (nextCanUndo == true)
                            {
                                UndoRestructureShortcutToken = null;
                                UndoRestructureRoot = _pendingRestructureApplyRoot;
                            }
                            _pendingRestructureApplyRoot = null;
                            _pendingRestructureApplyUndoable = false;
                        }
                        if (nextCanUndo.HasValue)
                        {
                            CanUndoRestructure = nextCanUndo.Value;
                        }
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
                        // Also refresh the cached HardwareInfo in Info so Settings
                        // bindings to existing Info.Hardware fields update too.
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
                        // Unknown TAGS never get here (the strict decoder
                        // throws and the read loop logs the line). This
                        // catches a DTO added to the schema without a
                        // routing arm — log it so the gap is visible
                        // instead of silently dropping state updates.
                        DebugLog.Warn($"[IPC IN] event type with no Apply routing: {ev.Payload?.GetType().Name ?? "<null>"}");
                        break;
                }
            }
            catch (Exception ex)
            {
                // route through WriteCrashDump so a routing-side fault
                // leaves a forensic artifact (not just a log line). A null
                // deref or malformed payload in one switch arm must NOT tear
                // down the UI. Log + dump + carry on.
                DebugLog.Error($"EngineClient.Apply({ev.Payload?.GetType().Name ?? "<null>"}) threw: {ex}");
                try { DebugLog.WriteCrashDump($"EngineClient.Apply({ev.Payload?.GetType().Name ?? "<null>"})", ex, terminating: false); }
                catch { /* swallow */ }
            }

        }
        catch (Exception outerEx)
        {
            // outer-frame catch — last line of defense before the
            // dispatcher loop. Anything that escapes the inner switch
            // try/catch (e.g., the catch itself throwing while writing
            // a crash dump on a full disk, or a sampling counter
            // increment hitting a wedge) lands here. Log only — never
            // re-throw.
            try { DebugLog.Error($"EngineClient.Apply OUTER catch: {outerEx}"); }
            catch { /* truly nothing we can do */ }
        }

        // matching exit line for the [APPLY:N] enter above. After a
        // native fast-fail, the absence of this exit line for the highest
        // logged seq identifies the offending event. NOTE: the switch
        // fires PropertyChanged synchronously, which fans out to every
        // subscriber — those subscribers' [ENGINE-SUB:Class] entry lines
        // are logged BEFORE this exit, so the trailing ENGINE-SUB line
        // identifies the offending subscriber.
        DebugLog.Info($"[APPLY:{applySeq}] exit {applyEventName}");
    }

    /// <summary>scan-complete → face-clustering auto-advance.
    /// Fire-and-forget so the Apply switch returns quickly; any failure
    /// (engine not ready, IPC throw) is logged and swallowed because
    /// scan completion itself succeeded — we don't want a downstream
    /// clustering hiccup to surface as a scan failure.</summary>
    private async Task AutoTriggerFaceClusteringAsync()
    {
        // PAR-111: skip if an auto-clustering pass is already in flight (e.g. a
        // rescan completing while the prior pass still runs). Released on
        // FaceClusteringComplete or a clustering error.
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
            // The IPC send failed — release the gate so a later scan can retry.
            Interlocked.Exchange(ref _faceClusterAutoInFlight, 0);
            _ui.TryEnqueue(() => FaceClusteringInFlight = false);
            DebugLog.Warn("[AUTO-ADVANCE] face clustering trigger threw: " + ex.Message);
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

internal sealed class GenerationHealthWaiters
{
    internal sealed class Waiter
    {
        internal Waiter(string requestId, int generation, int pid)
        {
            RequestId = requestId;
            Generation = generation;
            Pid = pid;
        }

        internal string RequestId { get; }
        internal int Generation { get; }
        internal int Pid { get; }
        internal TaskCompletionSource Completion { get; } =
            new(TaskCreationOptions.RunContinuationsAsynchronously);
        internal Task Task => Completion.Task;
    }

    private readonly object _gate = new();
    private readonly Dictionary<string, Waiter> _waiters =
        new(StringComparer.Ordinal);

    internal int Count
    {
        get
        {
            lock (_gate) return _waiters.Count;
        }
    }

    internal Waiter Register(string requestId, int generation, int pid)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(requestId);
        var waiter = new Waiter(requestId, generation, pid);
        lock (_gate)
        {
            if (!_waiters.TryAdd(requestId, waiter))
            {
                throw new InvalidOperationException(
                    $"A health probe with request ID '{requestId}' is already registered.");
            }
        }
        return waiter;
    }

    internal bool TryResolve(string requestId, int pid, int generation)
    {
        Waiter? waiter;
        lock (_gate)
        {
            if (!_waiters.TryGetValue(requestId, out waiter)
                || waiter.Pid != pid
                || waiter.Generation != generation)
            {
                return false;
            }
            _waiters.Remove(requestId);
        }
        waiter.Completion.TrySetResult();
        return true;
    }

    internal bool TryFail(string requestId, Exception error)
    {
        Waiter? waiter;
        lock (_gate)
        {
            if (!_waiters.Remove(requestId, out waiter))
            {
                return false;
            }
        }
        waiter.Completion.TrySetException(error);
        return true;
    }

    internal int FailGeneration(int generation, Exception error)
    {
        List<Waiter> retired;
        lock (_gate)
        {
            retired = _waiters.Values
                .Where(waiter => waiter.Generation == generation)
                .ToList();
            foreach (var waiter in retired)
            {
                _waiters.Remove(waiter.RequestId);
            }
        }
        foreach (var waiter in retired)
        {
            waiter.Completion.TrySetException(error);
        }
        return retired.Count;
    }
}
