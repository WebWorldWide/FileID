// ModelSlot — per-model install state row used by ModelInstallerService.
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

    /// <summary>Without this, a failure path that wrote to LastError fired PropertyChanged from a worker thread, the welcome sheet's x:Bind propagated it to ErrorLabel.Text, and the TextBlock setter threw RPC_E_WRONG_THREAD (COMException 0x8001010E).</summary>
    private readonly DispatcherQueue? _ui;

    public ModelSlot(
        string displayLabel,
        ulong approxBytes,
        Func<Task> installAction,
        DispatcherQueue? uiDispatcher = null)
    {
        _displayLabel = displayLabel;
        _approxBytes = approxBytes;
        _installAction = installAction;
        _ui = uiDispatcher;
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

    private bool _hasStarted;
    public bool HasStarted
    {
        get => _hasStarted;
        private set => Set(ref _hasStarted, value);
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

    /// <summary>Downloading rows also use it to target cancellation.</summary>
    public string? CurrentModelKind { get; set; }

    public DateTime LastProgressAt { get; set; } = DateTime.MinValue;

    private DateTime _rateSampleAt;
    private double _rateSampleFrac;
    private double _lastFraction;

    public Task InstallAsync() => _installAction();

    /// <summary>Status flips Downloading (or Installed if fraction ≥ 1.0 AND a sentinel file exists on disk).</summary>
    public void Apply(ModelDownloadProgress p, Func<bool> sentinelExists)
    {
        // ResetForRetry (fresh user-initiated start) is the only place Fraction is allowed back to 0.
        Fraction = Status == ModelInstallStatus.Downloading
            ? Math.Max(Fraction, p.Fraction)
            : p.Fraction;
        if (Fraction > 0) HasStarted = true;
        BytesDone = p.BytesDone;
        var newTotal = p.TotalBytes ?? (ApproxBytes > 0 ? ApproxBytes : null);
        if (newTotal is { } nt && TotalBytes is { } existing && nt < existing && nt < (BytesDone ?? 0))
        {
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
            // 100 % event arrived but sentinel missing — engine wrote the sentinel before emitting this event in the happy path, so missing sentinel here means a write failure or a race.
            DebugLog.Warn($"[INSTALL] {DisplayLabel} reported 100% but sentinel not present yet — staying Downloading");
        }
        Status = ModelInstallStatus.Downloading;
        UpdateRate(p);
    }

    public void Fail(string message)
    {
        Status = ModelInstallStatus.Failed;
        LastError = message;
        BytesPerSecond = 0;
        EtaSeconds = 0;
    }

    public void ResetForRetry()
    {
        Status = ModelInstallStatus.NotInstalled;
        Fraction = 0;
        HasStarted = false;
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

    /// <summary>After 5 in a row (≈2.5 s of stall) the Message field flips to "Stalled — check connection" so the user sees feedback well before the 30 s no-progress watchdog declares failure.</summary>
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

        // The previous EMA decayed asymptotically toward zero when `instant == 0` (BytesPerSecond *= 0.7 each sample) but never actually hit zero.
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
        // marshal PropertyChanged to UI thread. x:Bind forwards property writes into XAML DispatcherObjects (TextBlock.Text, VisibilityProperty, etc), which throw RPC_E_WRONG_THREAD if invoked from a worker thread.
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

