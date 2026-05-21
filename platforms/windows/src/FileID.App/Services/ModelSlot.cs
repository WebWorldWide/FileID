// ModelSlot — per-model install state row used by ModelInstallerService.
// One of these for CLIP / ArcFace / VLM, observed by the Welcome sheet via
// x:Bind. Split from ModelInstallerService.cs so the orchestrator file
// focuses on cross-slot logic.
//
// PRIVACY: never makes a network call.

using System.ComponentModel;
using System.Runtime.CompilerServices;
using FileID.IpcSchema;
using Microsoft.UI.Dispatching;

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

    /// <summary> UI dispatcher captured at construction time on
    /// the UI thread so PropertyChanged notifications can be safely
    /// marshalled even when Apply/Fail/ResetForRetry run on a worker
    /// thread (TryInstallAsync's continuations resume off the UI
    /// SynchronizationContext after Task.WhenAll). Without this, a
    /// failure path that wrote to LastError fired PropertyChanged from
    /// a worker thread, the welcome sheet's x:Bind propagated it to
    /// ErrorLabel.Text, and the TextBlock setter threw
    /// RPC_E_WRONG_THREAD (COMException 0x8001010E). The instance is
    /// constructed on the UI thread by ModelInstallerService.Instance's
    /// static initializer (touched first from App.OnLaunched).</summary>
    private readonly DispatcherQueue? _ui;

    public ModelSlot(string displayLabel, ulong approxBytes, Func<Task> installAction)
    {
        _displayLabel = displayLabel;
        _approxBytes = approxBytes;
        _installAction = installAction;
        _ui = DispatcherQueue.GetForCurrentThread();
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
        // don't let a per-file `total_bytes` from the engine
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

    /// <summary>number of consecutive stalled (sub-100 B/s) samples
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
            // First sample, or a multi-file-bundle fraction reset (file 2 starts
            // at fraction ~0). Re-baseline the EMA window but CARRY the prior
            // rate/ETA — zeroing them here made the rate blink to 0 / "Stalled"
            // at every file boundary (the "freaking out at first" jitter). On the
            // genuine first sample BytesPerSecond is already 0 from init.
            _rateSampleAt = now;
            _rateSampleFrac = p.Fraction;
            _stallSampleCount = 0;
            _lastFraction = p.Fraction;
            return;
        }
        var dt = (now - _rateSampleAt).TotalSeconds;
        if (dt < 0.25)
        {
            _lastFraction = p.Fraction;
            return; // Sample at most every 250 ms so the EMA tracks the TCP ramp.
        }
        var bytesPrev = total * _rateSampleFrac;
        var instant = (bytesNow - bytesPrev) / dt;

        // clean stall detection. The previous EMA decayed
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
        // marshal PropertyChanged to UI thread. x:Bind forwards
        // property writes into XAML DispatcherObjects (TextBlock.Text,
        // VisibilityProperty, etc), which throw RPC_E_WRONG_THREAD if
        // invoked from a worker thread. Without this guard,
        // ModelSlot.Fail() called from a TryInstallAsync continuation
        // crashed the welcome sheet with COMException 0x8001010E.
        var handler = PropertyChanged;
        if (handler is null) return;
        var args = new PropertyChangedEventArgs(propertyName);
        if (_ui is null || _ui.HasThreadAccess)
        {
            handler(this, args);
        }
        else
        {
            _ui.TryEnqueue(() => handler(this, args));
        }
    }
}

