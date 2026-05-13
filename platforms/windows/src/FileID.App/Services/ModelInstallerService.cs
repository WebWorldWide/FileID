// ModelInstallerService — per-model install state for the Welcome sheet.
//
// 1:1 port of the state shape used by macOS WelcomeSheet.swift +
// CLIPModelInstaller.swift + ArcFaceModelInstaller.swift. Each model
// tracks: status (NotInstalled / Downloading / Installed / Failed),
// fraction, bytes done / total, an EMA bytes-per-second, ETA seconds.
//
// Engine progress events are authoritative when a download is in flight.
// Sentinel files (`.fileid-installed`) are consulted at startup to seed
// Installed state for previously-completed models AND verified at the
// 100% transition so a buggy engine path can't lie to the user.
//
// PRIVACY: never makes a network call. Only sends IPC commands; the
// engine is the sole network surface.

using System.ComponentModel;
using System.IO;
using System.Runtime.CompilerServices;
using System.Threading;
using FileID.IpcSchema;
using FileID.ViewModels;

namespace FileID.Services;

internal enum ModelInstallStatus
{
    NotInstalled,
    Downloading,
    Installed,
    Failed,
}

/// <summary>
/// Per-model state. One of these for CLIP / ArcFace / VLM, observed by
/// the Welcome sheet via x:Bind.
/// </summary>
internal sealed class ModelSlot : INotifyPropertyChanged
{
    private string _displayLabel;
    public string DisplayLabel
    {
        get => _displayLabel;
        set => Set(ref _displayLabel, value);
    }

    private ulong _approxBytes;
    public ulong ApproxBytes
    {
        get => _approxBytes;
        set => Set(ref _approxBytes, value);
    }

    private readonly Func<Task> _installAction;

    public ModelSlot(string displayLabel, ulong approxBytes, Func<Task> installAction)
    {
        _displayLabel = displayLabel;
        _approxBytes = approxBytes;
        _installAction = installAction;
    }

    private ModelInstallStatus _status;
    public ModelInstallStatus Status
    {
        get => _status;
        set => Set(ref _status, value);
    }

    private double _fraction;
    public double Fraction
    {
        get => _fraction;
        set => Set(ref _fraction, value);
    }

    private ulong? _bytesDone;
    public ulong? BytesDone { get => _bytesDone; set => Set(ref _bytesDone, value); }

    private ulong? _totalBytes;
    public ulong? TotalBytes { get => _totalBytes; set => Set(ref _totalBytes, value); }

    private double _bytesPerSecond;
    public double BytesPerSecond { get => _bytesPerSecond; set => Set(ref _bytesPerSecond, value); }

    private double _etaSeconds;
    public double EtaSeconds { get => _etaSeconds; set => Set(ref _etaSeconds, value); }

    private string? _message;
    public string? Message { get => _message; set => Set(ref _message, value); }

    private string? _lastError;
    public string? LastError { get => _lastError; set => Set(ref _lastError, value); }

    /// <summary>The model_kind this slot is currently downloading, if any.
    /// Set by the service on PrewarmAsync entry, cleared on terminal state.
    /// Used by the engine-error router to decide which slot owns an
    /// EngineError that arrived without an explicit model_kind in payload.</summary>
    public string? CurrentModelKind { get; set; }

    /// <summary>Wall-clock UTC of the most recent progress event for this
    /// slot. Used by the no-progress watchdog to fail slots that go
    /// silent for 30+ s mid-download.</summary>
    public DateTime LastProgressAt { get; set; } = DateTime.MinValue;

    // EMA bandwidth tracking — mirrors macOS's `updateVLMRate` in
    // WelcomeSheet.swift. Sample every 500 ms; smooth with α=0.3.
    private DateTime _rateSampleAt;
    private double _rateSampleFrac;
    private double _lastFraction;

    public Task InstallAsync() => _installAction();

    /// <summary>
    /// Apply a fresh ModelDownloadProgress event. Status flips Downloading
    /// (or Installed if fraction ≥ 1.0 AND a sentinel file exists on
    /// disk). Updates EMA bandwidth.
    /// </summary>
    public void Apply(ModelDownloadProgress p, Func<bool> sentinelExists)
    {
        Fraction = p.Fraction;
        BytesDone = p.BytesDone;
        // V14.9-N1: don't let a per-file `total_bytes` from the engine
        // downgrade the slot's bundle-cumulative total. MobileCLIP-S2
        // ships 4 files; engine emits per-file `total_bytes` while the
        // app's `BytesDone` is the bundle-cumulative byte count. Without
        // this guard the Welcome row would render "578 MB of 201 MB"
        // mid-bundle (the user's reported regression).
        var newTotal = p.TotalBytes ?? (ApproxBytes > 0 ? ApproxBytes : null);
        if (newTotal is { } nt && TotalBytes is { } existing && nt < existing && nt < (BytesDone ?? 0))
        {
            // Per-file total < bundle progress → keep the existing total.
        }
        else
        {
            TotalBytes = newTotal;
        }
        Message = p.Message;
        LastProgressAt = DateTime.UtcNow;
        if (p.Fraction >= 1.0)
        {
            if (sentinelExists())
            {
                Status = ModelInstallStatus.Installed;
                BytesPerSecond = 0;
                EtaSeconds = 0;
                LastError = null;
                CurrentModelKind = null;
                return;
            }
            // 100 % event arrived but sentinel missing — engine wrote
            // the sentinel before emitting this event in the happy path,
            // so missing sentinel here means a write failure or a race.
            // Stay Downloading; a follow-up sentinel check will resolve.
            DebugLog.Warn($"[INSTALL] {DisplayLabel} reported 100% but sentinel not present yet — staying Downloading");
        }
        Status = ModelInstallStatus.Downloading;
        UpdateRate(p);
    }

    /// <summary>
    /// Mark this slot's install as failed. Surface the message so the row
    /// can render "Failed: …" + show a Retry button.
    /// </summary>
    public void Fail(string message)
    {
        Status = ModelInstallStatus.Failed;
        LastError = message;
        BytesPerSecond = 0;
        EtaSeconds = 0;
        CurrentModelKind = null;
    }

    /// <summary>Reset state (e.g. user clicked Retry).</summary>
    public void ResetForRetry()
    {
        Status = ModelInstallStatus.NotInstalled;
        Fraction = 0;
        BytesDone = null;
        BytesPerSecond = 0;
        EtaSeconds = 0;
        Message = null;
        LastError = null;
        CurrentModelKind = null;
        _rateSampleAt = default;
        _rateSampleFrac = 0;
        _lastFraction = 0;
    }

    /// <summary>V14.9-N1: number of consecutive stalled (sub-100 B/s) samples
    /// observed. After 5 in a row (≈2.5 s of stall) the Message field flips
    /// to "Stalled — check connection" so the user sees feedback well before
    /// the 30 s no-progress watchdog declares failure.</summary>
    private int _stallSampleCount;
    private const double StallThresholdBytesPerSecond = 100.0;
    private const double MaxEtaSeconds = 99.0 * 3600.0;

    private void UpdateRate(ModelDownloadProgress p)
    {
        var now = DateTime.UtcNow;
        var total = (double)(TotalBytes ?? ApproxBytes);
        if (total <= 0) return;

        var bytesNow = total * p.Fraction;
        if (_rateSampleAt == default || p.Fraction < _lastFraction)
        {
            // First sample, or fraction reset (new file in a multi-file
            // bundle). Restart EMA.
            _rateSampleAt = now;
            _rateSampleFrac = p.Fraction;
            BytesPerSecond = 0;
            EtaSeconds = 0;
            _stallSampleCount = 0;
            _lastFraction = p.Fraction;
            return;
        }
        var dt = (now - _rateSampleAt).TotalSeconds;
        if (dt < 0.5)
        {
            _lastFraction = p.Fraction;
            return; // Sample at most every 500 ms — TCP slow-start would skew earlier samples.
        }
        var bytesPrev = total * _rateSampleFrac;
        var instant = (bytesNow - bytesPrev) / dt;

        // V14.9-N1: clean stall detection. The previous EMA decayed
        // asymptotically toward zero when `instant == 0` (BytesPerSecond
        // *= 0.7 each sample) but never actually hit zero. After enough
        // stall samples BytesPerSecond became a tiny positive ε; the
        // bytesLeft/ε ETA exploded into values like 7e18 hours.
        // Treat any sub-100 B/s sample as "stalled": zero the rate +
        // ETA cleanly, and after 5 consecutive stall samples surface a
        // user-readable Message.
        if (instant < StallThresholdBytesPerSecond)
        {
            BytesPerSecond = 0;
            EtaSeconds = 0;
            _stallSampleCount++;
            if (_stallSampleCount >= 5 && Status == ModelInstallStatus.Downloading)
            {
                Message = "Stalled — check your internet connection.";
            }
        }
        else
        {
            _stallSampleCount = 0;
            if (BytesPerSecond <= 0)
            {
                BytesPerSecond = instant;
            }
            else
            {
                BytesPerSecond = 0.7 * BytesPerSecond + 0.3 * instant;
            }
            if (BytesPerSecond > 0)
            {
                var bytesLeft = total - bytesNow;
                EtaSeconds = bytesLeft > 0
                    ? Math.Min(bytesLeft / BytesPerSecond, MaxEtaSeconds)
                    : 0;
            }
        }
        _rateSampleAt = now;
        _rateSampleFrac = p.Fraction;
        _lastFraction = p.Fraction;
    }

    public event PropertyChangedEventHandler? PropertyChanged;

    private void Set<T>(ref T field, T value, [CallerMemberName] string? propertyName = null)
    {
        if (EqualityComparer<T>.Default.Equals(field, value)) return;
        field = value;
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(propertyName));
    }
}

internal sealed class ModelInstallerService : INotifyPropertyChanged
{
    // Sentinel-dir constants. Static field init runs in source order, so
    // these MUST be declared before Instance — its ctor calls
    // SeedFromSentinels which reads them.
    private static readonly string[] ClipSentinelDirs = { "MobileCLIP" };
    private static readonly string[] ArcfaceSentinelDirs = { "arcfaceMobileFace", "arcfaceIResNet50" };
    private static readonly string[] VlmSentinelDirs = { "Qwen2.5-VL-3B", "Qwen2.5-VL-7B", "SmolVLM", "Gemma-3-4B" };

    /// <summary>Time the engine has to reach Ready before an Install
    /// click gives up and surfaces "Engine not ready" to the user.</summary>
    private static readonly TimeSpan WaitForReadyTimeout = TimeSpan.FromSeconds(30);

    /// <summary>Time after which a Downloading slot with no progress
    /// events gets flipped to Failed. Mirrors macOS WelcomeSheet's
    /// "stuck install" guard.</summary>
    private static readonly TimeSpan NoProgressTimeout = TimeSpan.FromSeconds(30);

    public static ModelInstallerService Instance { get; } = new();

    public ModelSlot Clip { get; }
    public ModelSlot Arcface { get; }
    public ModelSlot Vlm { get; }

    private int _installAllInFlight; // 0 = idle, 1 = in flight

    /// <summary>VLM choice can update once we learn the user's RAM
    /// (from EngineClient.Info). The slot's installAction reads this
    /// field at click time, so a re-recommendation between app launch
    /// and click takes effect. Default matches macOS's 8 GB threshold:
    /// Qwen 2.5-VL 3B on 8 GB+ machines, SmolVLM below.</summary>
    private string _vlmModelKind = "qwen2_5_vl_3b";

    private ModelInstallerService()
    {
        Clip = new ModelSlot(
            displayLabel: "MobileCLIP-S2",
            approxBytes: 220UL * 1024 * 1024,
            installAction: () => PrewarmAsync("mobileclip_s2"));
        Arcface = new ModelSlot(
            displayLabel: "ArcFace MobileFace",
            approxBytes: 14UL * 1024 * 1024,
            installAction: () => PrewarmAsync("arcface_default"));
        Vlm = new ModelSlot(
            displayLabel: "Qwen 2.5-VL 3B",
            approxBytes: 1_650UL * 1024 * 1024,
            installAction: () => PrewarmAsync(_vlmModelKind));

        Clip.PropertyChanged += OnSlotPropertyChanged;
        Arcface.PropertyChanged += OnSlotPropertyChanged;
        Vlm.PropertyChanged += OnSlotPropertyChanged;

        SeedFromSentinels();
        EngineClient.Instance.PropertyChanged += OnEngineClientChanged;
    }

    /// <summary>
    /// Re-attach the EngineClient.PropertyChanged handler. Called by
    /// EngineClient at the start of each spawn so a stale subscription
    /// against an orphaned EngineClient doesn't keep firing — and so a
    /// download that was in flight when the engine crashed is correctly
    /// flipped to Failed (otherwise the row would spin forever).
    /// </summary>
    public void Reset()
    {
        EngineClient.Instance.PropertyChanged -= OnEngineClientChanged;
        EngineClient.Instance.PropertyChanged += OnEngineClientChanged;

        // Any in-flight download owned by the now-dead engine is
        // unreachable — flip to Failed so the user sees a Retry button
        // instead of a permanent spinner.
        FailIfDownloading(Clip, "Engine restarted — please retry.");
        FailIfDownloading(Arcface, "Engine restarted — please retry.");
        FailIfDownloading(Vlm, "Engine restarted — please retry.");

        SeedFromSentinels();
    }

    private static void FailIfDownloading(ModelSlot slot, string reason)
    {
        if (slot.Status == ModelInstallStatus.Downloading)
        {
            DebugLog.Info($"[INSTALL] Reset(): flipping {slot.DisplayLabel} from Downloading to Failed ({reason})");
            slot.Fail(reason);
        }
    }

    /// <summary>
    /// Install every not-yet-installed model in parallel — matches macOS
    /// WelcomeSheet.swift:146-151 which fires all three install actions
    /// from a single Install-all click. Engine handles the three downloads
    /// concurrently via tokio::spawn (main.rs:266-278). Per-slot try/catch
    /// keeps each error scoped so a CLIP failure can't abort ArcFace + VLM.
    /// Double-click is a no-op (Interlocked gate).
    /// </summary>
    public async Task InstallAllAsync()
    {
        if (Interlocked.CompareExchange(ref _installAllInFlight, 1, 0) != 0)
        {
            DebugLog.Info("[INSTALL] InstallAllAsync already in flight; ignoring duplicate request");
            return;
        }
        try
        {
            // V14.8.4 Bug 1: pre-stamp every not-yet-installed slot to
            // Downloading + "Queued — starting download…" BEFORE awaiting.
            // The three TryInstallAsync calls race for EngineClient._writeLock
            // when their IPC commands serialize; whichever loses both races
            // looked frozen to the user until its engine "Queued" event finally
            // landed. Pre-stamping makes the UI flip identical for all three
            // rows the instant the user clicks Install all, regardless of
            // which IPC write wins. The engine's F1 Queued event then arrives
            // and overwrites with the same caption — no visible flicker.
            // LastProgressAt also resets so the no-progress watchdog (30 s)
            // doesn't false-fire while the slowest row waits for its IPC turn.
            var now = DateTime.UtcNow;
            foreach (var slot in new[] { Clip, Arcface, Vlm })
            {
                if (slot.Status == ModelInstallStatus.Installed) continue;
                slot.ResetForRetry();
                slot.Status = ModelInstallStatus.Downloading;
                slot.Message = "Queued — starting download…";
                slot.LastProgressAt = now;
            }

            // The pack slot is included only when it exists AND isn't
            // Install the three AI models in parallel.
            var tasks = new List<Task>(3)
            {
                TryInstallAsync(Clip),
                TryInstallAsync(Arcface),
                TryInstallAsync(Vlm),
            };
            await Task.WhenAll(tasks).ConfigureAwait(false);
        }
        finally
        {
            Interlocked.Exchange(ref _installAllInFlight, 0);
        }
    }

    private static async Task TryInstallAsync(ModelSlot slot)
    {
        if (slot.Status == ModelInstallStatus.Installed) return;
        try
        {
            await slot.InstallAsync().ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"[INSTALL] {slot.DisplayLabel} install threw inside InstallAllAsync: {ex.Message}");
            slot.Fail(ex.Message);
        }
    }

    public Task CancelAllAsync() => EngineClient.Instance.CancelPrewarmAsync();

    /// <summary>Re-pick the VLM model based on detected RAM. Mirrors
    /// macOS <c>AIModelKind.safeDefaultFor(ramGB:)</c>: Qwen 2.5-VL 3B
    /// on 8 GB+ (≈1.5 GB), SmolVLM on smaller machines (≈700 MB) so
    /// the user doesn't OOM their box on Welcome's auto-install.
    /// No-op once the VLM is mid-flight or already installed — never
    /// change a slot's identity while it's working.</summary>
    public void UpdateVlmRecommendation(double physicalMemoryGB)
    {
        if (Vlm.Status == ModelInstallStatus.Downloading
            || Vlm.Status == ModelInstallStatus.Installed)
        {
            return;
        }
        string kind;
        string label;
        ulong bytes;
        if (physicalMemoryGB >= 8.0)
        {
            kind = "qwen2_5_vl_3b";
            label = "Qwen 2.5-VL 3B";
            bytes = 1_650UL * 1024 * 1024;
        }
        else
        {
            kind = "smolvlm";
            label = "SmolVLM 256M";
            bytes = 700UL * 1024 * 1024;
        }
        if (_vlmModelKind == kind) return;
        DebugLog.Info($"[INSTALL] VLM recommendation: {label} ({physicalMemoryGB:F1} GB RAM)");
        _vlmModelKind = kind;
        Vlm.DisplayLabel = label;
        Vlm.ApproxBytes = bytes;
    }

    private bool _allInstalled;
    public bool AllInstalled
    {
        get => _allInstalled;
        private set => Set(ref _allInstalled, value);
    }

    private bool _isBusy;
    public bool IsBusy
    {
        get => _isBusy;
        private set => Set(ref _isBusy, value);
    }

    private void RecomputeAggregates()
    {
        AllInstalled =
            Clip.Status == ModelInstallStatus.Installed
            && Arcface.Status == ModelInstallStatus.Installed
            && Vlm.Status == ModelInstallStatus.Installed;
        IsBusy =
            Clip.Status == ModelInstallStatus.Downloading
            || Arcface.Status == ModelInstallStatus.Downloading
            || Vlm.Status == ModelInstallStatus.Downloading;
    }

    private void OnSlotPropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName != nameof(ModelSlot.Status)) return;
        RecomputeAggregates();
    }

    /// <summary>
    /// Seed initial state from on-disk sentinels. Only sets Installed for
    /// slots that already have a `.fileid-installed` marker; never
    /// overrides Downloading or Failed (downloads in flight are owned by
    /// the engine event stream).
    /// </summary>
    public void SeedFromSentinels()
    {
        SeedSlot(Clip, ClipSentinelDirs);
        SeedSlot(Arcface, ArcfaceSentinelDirs);
        SeedSlot(Vlm, VlmSentinelDirs);
        RecomputeAggregates();
    }

    /// <summary>Alias for callers that just want a sentinel re-check
    /// without caring about the engine-event side of the state machine
    /// (MainWindow startup, DeepAnalyzeView model-install panel).</summary>
    public void Refresh() => SeedFromSentinels();

    private static void SeedSlot(ModelSlot slot, string[] candidateDirs)
    {
        if (slot.Status == ModelInstallStatus.Downloading
            || slot.Status == ModelInstallStatus.Failed)
        {
            return;
        }
        foreach (var name in candidateDirs)
        {
            if (HasSentinel(Path.Combine(AppPaths.ModelsDir, name)))
            {
                slot.Status = ModelInstallStatus.Installed;
                return;
            }
        }
        slot.Status = ModelInstallStatus.NotInstalled;
    }

    /// <summary>
    /// Drive an install for a single model. Gates on engine readiness
    /// BEFORE flipping Status to Downloading, so a click that loses the
    /// startup race surfaces a clean "Engine not ready" error rather
    /// than a Downloading flicker followed by a swallowed exception.
    /// </summary>
    private async Task PrewarmAsync(string modelKind)
    {
        var slot = SlotFor(modelKind);
        if (slot is null)
        {
            DebugLog.Warn($"[INSTALL] PrewarmAsync('{modelKind}') — no slot routes for this id");
            return;
        }
        DebugLog.Info($"[INSTALL] PrewarmAsync('{modelKind}') called. priorStatus={slot.Status}");

        // Wait for engine Ready before touching slot.Status. If the engine
        // never reaches Ready, the slot stays in its prior state and the
        // user sees a clear error message instead of a misleading spinner.
        try
        {
            await EngineClient.Instance.WaitForReadyAsync(WaitForReadyTimeout).ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"[INSTALL] WaitForReadyAsync threw for '{modelKind}': {ex.Message}");
            slot.Fail("Engine not ready: " + ex.Message);
            return;
        }

        // V14.8.4 Bug 1: only reset state if this slot wasn't already
        // pre-stamped to Downloading by InstallAllAsync. Re-running
        // ResetForRetry after the pre-stamp would blank Fraction/Message
        // mid-flight if the engine's first progress event happens to
        // arrive between pre-stamp and PrewarmAsync entry.
        if (slot.Status != ModelInstallStatus.Downloading)
        {
            slot.ResetForRetry();
            slot.Status = ModelInstallStatus.Downloading;
            slot.Message = "Starting…";
            slot.LastProgressAt = DateTime.UtcNow;
        }
        slot.CurrentModelKind = modelKind;
        DebugLog.Info($"[INSTALL] {modelKind} status set to Downloading; sending IPC...");

        try
        {
            await EngineClient.Instance.PrewarmModelAsync(modelKind).ConfigureAwait(false);
            DebugLog.Info($"[INSTALL] {modelKind} prewarmModel IPC sent; awaiting progress events.");
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"[INSTALL] PrewarmModelAsync('{modelKind}') threw: {ex.Message}");
            slot.Fail(ex.Message);
            return;
        }

        // No-progress watchdog: 30 s after the IPC send, if Status is still
        // Downloading and no progress event has landed, fail with a clear
        // message. Mirrors macOS WelcomeSheet's "stuck install" handling.
        ScheduleNoProgressWatchdog(slot, modelKind);
    }

    private static void ScheduleNoProgressWatchdog(ModelSlot slot, string modelKind, CancellationToken ct = default)
    {
        var sentAt = DateTime.UtcNow;
        // Capture the UI dispatcher at schedule time. The watchdog runs on
        // a thread-pool thread (Task.Run) but slot.Fail mutates state that
        // x:Bind UI elements observe — those updates have to land on the
        // UI thread or downstream PropertyChanged handlers may touch
        // FrameworkElements off-thread.
        var ui = Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread();
        _ = Task.Run(async () =>
        {
            try
            {
                await Task.Delay(NoProgressTimeout, ct).ConfigureAwait(false);
                // V14.9-A6: cancellation check after the delay — if the
                // user cancelled the install during the watchdog window,
                // don't surface a "no response" error on top of a clean
                // cancellation flow.
                if (ct.IsCancellationRequested) return;
                // Read-only check off-thread is fine (status/timestamp are
                // primitives + DateTime; no torn-read risk on x64/ARM64).
                if (slot.Status != ModelInstallStatus.Downloading) return;
                if (slot.LastProgressAt > sentAt) return; // got progress, all good
                DebugLog.Warn($"[INSTALL] {modelKind} no-progress watchdog firing (no events in {NoProgressTimeout.TotalSeconds:0}s)");
                if (ui is not null)
                {
                    ui.TryEnqueue(() => slot.Fail("No response from engine — try again."));
                }
                else
                {
                    // V14.9-A5: previously fell through to calling slot.Fail()
                    // directly on the thread-pool thread, which raises
                    // PropertyChanged off the UI thread → x:Bind UI hit
                    // off-thread → potential FrameworkElement violation.
                    // Refuse to fail the slot when we can't marshal; log
                    // and let the engine's own error event (if any) drive
                    // the eventual transition.
                    DebugLog.Warn($"[INSTALL] {modelKind} watchdog: no UI dispatcher; skipping slot.Fail to avoid off-thread PropertyChanged.");
                }
            }
            catch (OperationCanceledException)
            {
                // Cancellation is a normal terminating condition.
            }
            catch (Exception ex)
            {
                DebugLog.Warn($"[INSTALL] no-progress watchdog threw: {ex.Message}");
            }
        }, ct);
    }

    private ModelSlot? SlotFor(string? modelKind)
    {
        switch (modelKind)
        {
            case "mobileclip_s2":
            case "clip_image":
            case "clip_text":
                return Clip;
            case "arcface_default":
            case "arcface_iresnet50":
            case "arcface_mobileface":
                return Arcface;
            case "qwen2_5_vl_3b":
            case "qwen2_5_vl_7b":
            case "gemma_3_4b":
            case "smolvlm":
                return Vlm;
            default:
                return null;
        }
    }

    /// <summary>Fallback slot lookup by error path. Only used when the
    /// engine's error event carries no model_kind (legacy emitters, or
    /// non-model errors that still have a path). Path-substring matching
    /// is intentionally narrow — we DON'T match on substrings like "cuda"
    /// because pack paths and model paths can collide. The "in-flight
    /// fallback" that used to live here was the root cause of D-track
    /// cross-wiring (CUDA pack 404 + MobileCLIP in flight → MobileCLIP
    /// row showed cuda.zip error) and has been removed.</summary>
    private ModelSlot? SlotForErrorPath(string? path)
    {
        if (string.IsNullOrEmpty(path)) return null;
        if (path.Contains("MobileCLIP", StringComparison.OrdinalIgnoreCase)) return Clip;
        if (path.Contains("arcface", StringComparison.OrdinalIgnoreCase)) return Arcface;
        if (path.Contains("Qwen", StringComparison.OrdinalIgnoreCase)
            || path.Contains("SmolVLM", StringComparison.OrdinalIgnoreCase)
            || path.Contains("Gemma", StringComparison.OrdinalIgnoreCase))
        {
            return Vlm;
        }
        return null;
    }

    private int _progressEventCount;

    private void OnEngineClientChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName == nameof(EngineClient.ModelDownloadProgress))
        {
            HandleProgress(EngineClient.Instance.ModelDownloadProgress);
            return;
        }
        if (e.PropertyName == nameof(EngineClient.LastError))
        {
            HandleEngineError(EngineClient.Instance.LastError);
            return;
        }
        if (e.PropertyName == nameof(EngineClient.Info))
        {
            var info = EngineClient.Instance.Info;
            if (info is not null) UpdateVlmRecommendation(info.PhysicalMemoryGB);
            return;
        }
    }

    private void HandleProgress(ModelDownloadProgress? p)
    {
        if (p is null) return;
        var n = Interlocked.Increment(ref _progressEventCount);
        if (n <= 5 || n % 50 == 0 || p.Fraction >= 0.999)
        {
            DebugLog.Info($"[INSTALL] OnEngineClientChanged #{n}: {p.ModelKind} {p.Fraction:P0} bytes={p.BytesDone}/{p.TotalBytes}");
        }
        var slot = SlotFor(p.ModelKind);
        if (slot is null)
        {
            DebugLog.Warn($"[INSTALL] no slot for model_kind '{p.ModelKind}' — progress event dropped.");
            return;
        }
        var sentinelDirs = SentinelDirsFor(slot);
        slot.Apply(p, () => SentinelExistsForAnyOf(sentinelDirs));
    }

    private void HandleEngineError(EngineError? error)
    {
        if (error is null) return;
        // Only route install-related errors. Other engine errors (e.g.
        // scan_failed, ipc_decode_failed) belong to other surfaces.
        var kind = error.Kind ?? string.Empty;
        var isInstallError =
            kind == "model_download_failed"
            || kind == "zip_extract_failed"
            || kind == "pack_not_available"
            || kind.StartsWith("prewarm_", StringComparison.OrdinalIgnoreCase);
        if (!isInstallError) return;

        // prewarm_cancelled is user-initiated; don't surface as a failure.
        if (kind == "prewarm_cancelled") return;

        // D-track fix: route by error.ModelKind first. The engine now stamps
        // every install-failure event with the originating model id, so we
        // don't need to infer it from the path string. SlotForErrorPath is
        // kept as a fallback for legacy emitters / non-model errors that
        // still carry a path.
        var slot = !string.IsNullOrEmpty(error.ModelKind)
            ? SlotFor(error.ModelKind)
            : SlotForErrorPath(error.Path);
        if (slot is null)
        {
            DebugLog.Warn($"[INSTALL] engine error '{kind}' has no routable slot (modelKind={error.ModelKind ?? "<null>"}, path={error.Path ?? "<null>"})");
            return;
        }
        DebugLog.Info($"[INSTALL] engine error → {slot.DisplayLabel}.Fail(): {error.Message}");
        slot.Fail(error.Message);
    }

    private static string[] SentinelDirsFor(ModelSlot slot)
    {
        if (ReferenceEquals(slot, Instance.Clip)) return ClipSentinelDirs;
        if (ReferenceEquals(slot, Instance.Arcface)) return ArcfaceSentinelDirs;
        if (ReferenceEquals(slot, Instance.Vlm)) return VlmSentinelDirs;
        return Array.Empty<string>();
    }

    private static bool SentinelExistsForAnyOf(string[] candidateDirs)
    {
        foreach (var name in candidateDirs)
        {
            if (HasSentinel(Path.Combine(AppPaths.ModelsDir, name))) return true;
        }
        return false;
    }

    private static bool HasSentinel(string dir)
    {
        try
        {
            if (!Directory.Exists(dir)) return false;
            if (!File.Exists(Path.Combine(dir, ".fileid-installed"))) return false;
            // Defensive: a stray sentinel file in an otherwise-empty
            // directory shouldn't be trusted (could be left over from a
            // botched install + manual cleanup, or a malicious drop).
            // Require at least one non-sentinel file in the dir.
            foreach (var f in Directory.EnumerateFiles(dir))
            {
                var name = Path.GetFileName(f);
                if (!string.Equals(name, ".fileid-installed", StringComparison.OrdinalIgnoreCase))
                {
                    return true;
                }
            }
            // Recurse one level for compound-dir layouts (packs/cuda/...)
            foreach (var sub in Directory.EnumerateDirectories(dir))
            {
                if (Directory.EnumerateFiles(sub).Any()) return true;
            }
            return false;
        }
        catch { return false; }
    }

    public event PropertyChangedEventHandler? PropertyChanged;

    private void Set<T>(ref T field, T value, [CallerMemberName] string? propertyName = null)
    {
        if (EqualityComparer<T>.Default.Equals(field, value)) return;
        field = value;
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(propertyName));
    }
}
