// EngineClient — owns the FileIDEngine.exe child process lifecycle.
//
// Responsibilities:
//   1. Spawn FileIDEngine.exe with stdin/stdout/stderr redirected.
//   2. Verify the engine binary's Authenticode signature before each spawn
//      (warns on Unsigned, refuses on Untrusted; will tighten to require
//      Trusted with a pinned EV thumbprint).
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

    // S4: bounded engine-stdout framing. Mirrors the engine's 1 MiB
    // MAX_FRAME_BYTES guard so a wedged/compromised engine that never emits a
    // newline can't grow an unbounded read buffer and OOM the UI process.
    private const int MaxFrameChars = 1024 * 1024;

    /// <summary>Per-loop stdout framing state (#22). Owned by a single
    /// StdoutLoopAsync invocation — never shared across loops, so an overlapping
    /// loop from a respawn can't race another's buffer/resync flag.</summary>
    private sealed class StdoutFraming
    {
        public readonly StringBuilder Buffer = new();
        public readonly char[] Chunk = new char[16 * 1024];
        public bool Resyncing;
    }

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
    // ShutdownAsync; OnProcessExited consumes it. Uses int + Interlocked
    // (instead of bool) so reads/writes are atomic across threads on
    // ARM64 — bool reads can theoretically tear on weakly-ordered
    // architectures, and OnProcessExited fires on whichever thread
    // detects process exit (not always the UI thread).
    private int _expectingExit; // 0 = false, 1 = true

    private DateTime _lastDeepAnalyzeFileDone = DateTime.MinValue;
    private static readonly TimeSpan DeepAnalyzeFileDoneThrottle = TimeSpan.FromMilliseconds(500); // 2 Hz

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

    /// <summary>Hot stream of every IPC event. Used by tests + the optional
    /// transcript log. Subscribe via System.Reactive.</summary>
    public IObservable<IpcEvent> Events => _events;

    public event PropertyChangedEventHandler? PropertyChanged;

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
        try
        {
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

            // SEC: TOCTOU mitigation. Hash the binary AFTER WinVerifyTrust
            // returned its verdict, then re-hash + compare immediately
            // before Process.Start. If a privileged adversary swaps the
            // engine binary between Verify and spawn, the post-spawn hash
            // diverges and we abort. Skipped in dev (no thumbprint pinned)
            // because Visual Studio rebuilds change the hash legitimately.
            byte[]? preSpawnHash = null;
            if (!string.IsNullOrEmpty(expectedThumb))
            {
                try
                {
                    using var sha = System.Security.Cryptography.SHA256.Create();
                    using var fs = System.IO.File.OpenRead(enginePath);
                    preSpawnHash = sha.ComputeHash(fs);
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
                // Re-hash + compare immediately before Process.Start.
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
                    // `System.Text.Encoding.UTF8` is the
                    // BOM-prefixing variant. On first write its
                    // StreamWriter pushes three bytes (`EF BB BF`) into
                    // the engine's stdin, which trips serde_json with
                    // "expected value at line 1 column 1" and used to
                    // surface as a red toast on every cold launch. The
                    // explicit `new UTF8Encoding(false)` is identical
                    // UTF-8 minus the preamble.
                    StandardInputEncoding = new System.Text.UTF8Encoding(encoderShouldEmitUTF8Identifier: false),
                    StandardOutputEncoding = new System.Text.UTF8Encoding(encoderShouldEmitUTF8Identifier: false),
                    StandardErrorEncoding = new System.Text.UTF8Encoding(encoderShouldEmitUTF8Identifier: false),
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
        finally
        {
            // unconditional gate release. Every early-return path
            // above + the spawn-catch + the normal completion path all
            // converge here so OnProcessExited's respawn can always CAS
            // the gate back from 0 → 1.
            Interlocked.Exchange(ref _isStarting, 0);
        }
    }

    /// <summary>S4: read one newline-delimited engine frame, bounded to
    /// <see cref="MaxFrameChars"/>. A frame that exceeds the cap before a
    /// newline arrives is discarded and we resync to the next newline, so a
    /// never-terminating line can't OOM the UI. Returns null at EOF. All framing
    /// state lives in the caller-owned <paramref name="st"/>, so each
    /// StdoutLoopAsync owns its own — no cross-loop sharing (#22).</summary>
    private static async Task<string?> ReadBoundedFrameAsync(StreamReader reader, StdoutFraming st, CancellationToken ct)
    {
        while (true)
        {
            // Emit a completed frame if the buffer already holds a newline.
            int nl = -1;
            for (int i = 0; i < st.Buffer.Length; i++)
            {
                if (st.Buffer[i] == '\n') { nl = i; break; }
            }
            if (nl >= 0)
            {
                string frame = st.Buffer.ToString(0, nl);
                st.Buffer.Remove(0, nl + 1);
                if (st.Resyncing)
                {
                    // This frame is the tail of an oversize line — drop it and
                    // resume normal framing from the next one.
                    st.Resyncing = false;
                    continue;
                }
                if (frame.Length > 0 && frame[^1] == '\r') frame = frame[..^1];
                return frame;
            }
            // No newline yet: if the buffer crossed the cap, the engine is
            // emitting an oversize/garbage frame. Drop it and resync.
            if (st.Buffer.Length > MaxFrameChars)
            {
                DebugLog.Warn($"Engine emitted an oversize IPC frame (> {MaxFrameChars} chars); discarding and resyncing.");
                st.Buffer.Clear();
                st.Resyncing = true;
            }
            int read = await reader.ReadAsync(st.Chunk.AsMemory(), ct).ConfigureAwait(false);
            if (read == 0)
            {
                // EOF. Surface a trailing partial frame, unless we were mid-resync.
                if (!st.Resyncing && st.Buffer.Length > 0)
                {
                    string tail = st.Buffer.ToString();
                    st.Buffer.Clear();
                    if (tail.Length > 0 && tail[^1] == '\r') tail = tail[..^1];
                    return tail;
                }
                return null;
            }
            st.Buffer.Append(st.Chunk, 0, read);
        }
    }

    private async Task StdoutLoopAsync(StreamReader reader, CancellationToken ct)
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

    // Conservative match: drive letter, colon, backslash, then any
    // non-whitespace / non-quote / non-bracket. Catches `C:\Users\…`
    // anywhere in the line while leaving Unicode logging strings alone.
    private static readonly System.Text.RegularExpressions.Regex s_pathInLine =
        new(@"[A-Za-z]:\\[^\s""\)\}\>]+",
            System.Text.RegularExpressions.RegexOptions.Compiled);

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
            // Interlocked.Exchange both reads + clears in one atomic op.
            if (Interlocked.Exchange(ref _expectingExit, 0) == 1)
            {
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
            // No outer CAS — StartAsync has its own strict CAS at the top;
            // an outer one was actively harmful: setting _isStarting=1
            // here caused StartAsync's own CAS to see "already starting"
            // and bail, so every auto-respawn was silently dropped.
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

    // ─── Event router ──────────────────────────────────────────────────

    private void Apply(IpcEvent ev)
    {
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
                switch (ev.Payload)
                {
                    case ReadyEvent r:
                        Info = r.Info;
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
                        // A successful Ready is the canonical signal that the
                        // engine has fully recovered from any prior crash.
                        // Reset both the failure counter AND the failure-window
                        // timestamp so a subsequent crash doesn't tick toward
                        // the 3-strike limit using stale state. Without this
                        // reset, a deterministic-crash file (corrupt .gguf)
                        // could permanently lock the engine in Crashed even
                        // after the user removes the bad file.
                        _consecutiveFailures = 0;
                        _failureWindowStart = DateTime.MinValue;
                        break;
                    case ProgressEvent p:
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
                        if (progRank < _shownPhaseRank) break;
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
                        if (pc.Phase == ScanPhase.Failed)
                        {
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
                        // Authoritative final count for the completed-scan summary
                        // (LastProgress.Processed can be throttle-stale by a batch).
                        LastScanProcessedFiles = sce.Result.ProcessedFiles;
                        Phase = ScanPhase.Completed;
                        _shownPhaseRank = PhaseRank(ScanPhase.Completed);
                        IsPaused = false;
                        if (_scanStartedAt.HasValue)
                        {
                            LastScanDuration = DateTime.UtcNow - _scanStartedAt.Value;
                            _scanStartedAt = null;
                        }
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
                        _ = AutoTriggerFaceClusteringAsync();
                        break;
                    case ErrorEvent e:
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
                        // PAR-111: a clustering failure must release the auto gate,
                        // else auto-clustering stays suppressed for the rest of the session.
                        if (e.Error.Kind is { } ck && ck.Contains("cluster", StringComparison.OrdinalIgnoreCase))
                        {
                            Interlocked.Exchange(ref _faceClusterAutoInFlight, 0);
                        }
                        break;
                    case LogEvent:
                        // Engine LogLine events go to the transcript via Events.
                        // Local app log already captured stderr.
                        break;
                    case FaceClusteringCompleteEvent fc:
                        LastFaceClustering = fc.Result;
                        Interlocked.Exchange(ref _faceClusterAutoInFlight, 0); // PAR-111: release the auto gate
                        break;
                    case DeepAnalyzeStartingEvent das:
                        DeepAnalyzeStarting = das.Starting;
                        // Clear the previous run's terminal result. DeepAnalyzeComplete
                        // is otherwise only cleared on the single-file path, so on a
                        // 2nd+ "Analyze All" run the stale Complete makes the view's
                        // SyncStream `complete` block clobber the live progress every
                        // tick — the buttons + status text visibly fight between live
                        // "{processed}/{total}" and the stale "Done — N captioned" at
                        // ~4 Hz, with Cancel wrongly greyed out for the whole run.
                        DeepAnalyzeComplete = null;
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
