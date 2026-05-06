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
    public string DisplayLabel { get; }
    public ulong ApproxBytes { get; }
    private readonly Func<Task> _installAction;

    public ModelSlot(string displayLabel, ulong approxBytes, Func<Task> installAction)
    {
        DisplayLabel = displayLabel;
        ApproxBytes = approxBytes;
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
        TotalBytes = p.TotalBytes ?? (ApproxBytes > 0 ? ApproxBytes : null);
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
            EtaSeconds = bytesLeft > 0 ? bytesLeft / BytesPerSecond : 0;
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
            installAction: () => PrewarmAsync("qwen2_5_vl_3b"));

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
            await Task.WhenAll(
                TryInstallAsync(Clip),
                TryInstallAsync(Arcface),
                TryInstallAsync(Vlm)
            ).ConfigureAwait(false);
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
        if (e.PropertyName == nameof(ModelSlot.Status))
        {
            RecomputeAggregates();
        }
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

        slot.ResetForRetry();
        slot.Status = ModelInstallStatus.Downloading;
        slot.Message = "Starting…";
        slot.CurrentModelKind = modelKind;
        slot.LastProgressAt = DateTime.UtcNow;
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

    private static void ScheduleNoProgressWatchdog(ModelSlot slot, string modelKind)
    {
        var sentAt = DateTime.UtcNow;
        _ = Task.Run(async () =>
        {
            try
            {
                await Task.Delay(NoProgressTimeout).ConfigureAwait(false);
                if (slot.Status != ModelInstallStatus.Downloading) return;
                if (slot.LastProgressAt > sentAt) return; // got progress, all good
                DebugLog.Warn($"[INSTALL] {modelKind} no-progress watchdog firing (no events in {NoProgressTimeout.TotalSeconds:0}s)");
                slot.Fail("No response from engine — try again.");
            }
            catch (Exception ex)
            {
                DebugLog.Warn($"[INSTALL] no-progress watchdog threw: {ex.Message}");
            }
        });
    }

    private ModelSlot? SlotFor(string? modelKind) => modelKind switch
    {
        "mobileclip_s2" or "clip_image" or "clip_text"               => Clip,
        "arcface_default" or "arcface_iresnet50" or "arcface_mobileface" => Arcface,
        "qwen2_5_vl_3b" or "qwen2_5_vl_7b" or "gemma_3_4b" or "smolvlm" => Vlm,
        _ => null,
    };

    /// <summary>Map an engine error path (e.g. ".../MobileCLIP/...") to
    /// the slot that owns it. Falls back to whichever slot currently
    /// has CurrentModelKind set (one engine-initiated download is in
    /// flight at a time per the engine's IN_FLIGHT dedup).</summary>
    private ModelSlot? SlotForErrorPath(string? path)
    {
        if (!string.IsNullOrEmpty(path))
        {
            if (path.Contains("MobileCLIP", StringComparison.OrdinalIgnoreCase)) return Clip;
            if (path.Contains("arcface", StringComparison.OrdinalIgnoreCase)) return Arcface;
            if (path.Contains("Qwen", StringComparison.OrdinalIgnoreCase)
                || path.Contains("SmolVLM", StringComparison.OrdinalIgnoreCase)
                || path.Contains("Gemma", StringComparison.OrdinalIgnoreCase))
            {
                return Vlm;
            }
        }
        // Last-resort: any slot currently flagged as in-flight.
        if (Clip.CurrentModelKind is not null) return Clip;
        if (Arcface.CurrentModelKind is not null) return Arcface;
        if (Vlm.CurrentModelKind is not null) return Vlm;
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
            || kind.StartsWith("prewarm_", StringComparison.OrdinalIgnoreCase);
        if (!isInstallError) return;

        // prewarm_cancelled is user-initiated; don't surface as a failure.
        if (kind == "prewarm_cancelled") return;

        var slot = SlotForErrorPath(error.Path);
        if (slot is null)
        {
            DebugLog.Warn($"[INSTALL] engine error '{kind}' has no routable slot (path={error.Path ?? "<null>"})");
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
            return Directory.Exists(dir) && File.Exists(Path.Combine(dir, ".fileid-installed"));
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
