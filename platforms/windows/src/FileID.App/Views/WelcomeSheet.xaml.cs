// WelcomeSheet code-behind: click handlers delegate to ModelSlot.InstallAsync, ModelInstallerService.CancelAllAsync and dismiss.

using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using System.ComponentModel;

namespace FileID.Views;

public sealed partial class WelcomeSheet : UserControl
{
    public event EventHandler? Dismissed;

    /// <summary>WinUI 3 x:Bind binds against the page's own properties/fields, not DataContext, so we surface the service through this property.</summary>
    internal ModelInstallerService Svc => ModelInstallerService.Instance;

    private bool _autoDismissScheduled;

    /// <summary>Cancels the auto-dismiss task + any in-flight restart prompt if the sheet unloads before they complete.</summary>
    private CancellationTokenSource? _lifetimeCts = new();

    public WelcomeSheet()
    {
        InitializeComponent();
        // Subscribe before SeedFromSentinels: a sentinel that flips Status synchronously would otherwise raise PropertyChanged with no handler and the auto-dismiss would never fire.
        Svc.PropertyChanged += OnServicePropertyChanged;
        Unloaded += (_, _) =>
        {
            Svc.PropertyChanged -= OnServicePropertyChanged;
            // Keeps in-flight auto-dismiss and restart-prompt tasks from touching a detached control.
            try { _lifetimeCts?.Cancel(); _lifetimeCts?.Dispose(); }
            catch { /* swallow — Cts may already be disposed */ }
            _lifetimeCts = null;
        };

        try
        {
            Svc.SeedFromSentinels();
        }
        catch (Exception ex)
        {
            DebugLog.Warn("WelcomeSheet ctor SeedFromSentinels threw: " + ex.Message);
        }

        // Reopened from Settings with everything installed: still auto-dismiss.
        if (Svc.AllInstalled) ScheduleAutoDismiss();
    }

    private void OnServicePropertyChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun(nameof(OnServicePropertyChanged), () => OnServicePropertyChangedCore(sender, e));

    private void OnServicePropertyChangedCore(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName == nameof(ModelInstallerService.AllInstalled) && Svc.AllInstalled)
        {
            ScheduleAutoDismiss();
        }
    }

    private void ScheduleAutoDismiss()
    {
        if (_autoDismissScheduled) return;
        _autoDismissScheduled = true;
        var ct = _lifetimeCts?.Token ?? CancellationToken.None;
        var dq = DispatcherQueue; // captured while still attached
        // 800 ms lets the green checkmark transition land before dismissing.
        _ = Task.Run(async () =>
        {
            try
            {
                await Task.Delay(800, ct).ConfigureAwait(false);
                if (ct.IsCancellationRequested) return;
                dq?.TryEnqueue(() =>
                {
                    if (ct.IsCancellationRequested) return;
                    if (Svc.AllInstalled) RaiseDismissed();
                });
            }
            catch (OperationCanceledException) { /* sheet dismissed before 800 ms — fine */ }
            catch (Exception ex) { DebugLog.Warn("WelcomeSheet auto-dismiss threw: " + ex.Message); }
        }, ct);
    }

    private const string GlyphCheck = "\uE73E"; // CheckMark
    private const string GlyphArrow = "\uE896"; // Download (tray + arrow)
    private const string GlyphCloud = "\uE896"; // same — cloud-download metaphor
    private const string GlyphWarning = "\uEA39"; // Warning

    internal string GlyphFor(ModelInstallStatus s) => s switch
    {
        ModelInstallStatus.Installed => GlyphCheck,
        ModelInstallStatus.Downloading => GlyphArrow,
        ModelInstallStatus.Failed => GlyphWarning,
        _ => GlyphCloud,
    };

    private static readonly SolidColorBrush s_goldFallback =
        new(Windows.UI.Color.FromArgb(0xFF, 0xFF, 0xCC, 0x00));

    private Brush GoldBrushResolved =>
        FileID.Services.ThemeHelper.GetBrushSafe("GoldBrush", s_goldFallback);

    private static readonly SolidColorBrush GreenBrush =
        new(Windows.UI.Color.FromArgb(0xFF, 0x6B, 0xE0, 0x82));

    private static readonly SolidColorBrush RedBrush =
        new(Windows.UI.Color.FromArgb(0xFF, 0xE5, 0x55, 0x55));

    internal Brush IconBrushFor(ModelInstallStatus s) => s switch
    {
        ModelInstallStatus.Installed => GreenBrush,
        ModelInstallStatus.Failed => RedBrush,
        _ => GoldBrushResolved,
    };

    internal Visibility VisibleIfDownloading(ModelInstallStatus s) =>
        s == ModelInstallStatus.Downloading ? Visibility.Visible : Visibility.Collapsed;

    internal Visibility VisibleIfInstalled(ModelInstallStatus s) =>
        s == ModelInstallStatus.Installed ? Visibility.Visible : Visibility.Collapsed;

    internal Visibility VisibleIfFailed(ModelInstallStatus s) =>
        s == ModelInstallStatus.Failed ? Visibility.Visible : Visibility.Collapsed;

    internal bool IsStarting(ModelInstallStatus s, bool hasStarted) =>
        s == ModelInstallStatus.Downloading && !hasStarted;

    internal Visibility ShowActionButton(ModelInstallStatus s) =>
        s != ModelInstallStatus.Installed ? Visibility.Visible : Visibility.Collapsed;

    internal string ButtonLabel(ModelInstallStatus s) => s switch
    {
        ModelInstallStatus.Downloading => "Cancel",
        ModelInstallStatus.Failed => "Retry",
        _ => "Download",
    };

    internal string SkipLabel(bool allInstalled) => allInstalled ? "Continue" : "Later";

    internal string FooterHint(bool allInstalled) =>
        allInstalled ? "Next: choose a folder to scan." : "You can also download them later in Settings.";

    internal string InstallAllLabel(
        bool allInstalled,
        bool isBusy,
        ModelInstallStatus clip,
        ModelInstallStatus ramPlus,
        ModelInstallStatus faces,
        ModelInstallStatus vlm,
        ulong vlmBytes)
    {
        if (allInstalled) return "All downloaded";
        if (isBusy) return "Downloading…";
        ulong bytes = 0;
        if (clip != ModelInstallStatus.Installed) bytes += Svc.Clip.ApproxBytes;
        if (ramPlus != ModelInstallStatus.Installed) bytes += Svc.RamPlus.ApproxBytes;
        if (faces != ModelInstallStatus.Installed) bytes += Svc.Arcface.ApproxBytes;
        if (vlm != ModelInstallStatus.Installed) bytes += vlmBytes;
        return bytes > 0 ? $"Download all · ~{Services.ByteFormat.File(bytes)}" : "Download all";
    }

    internal bool InstallAllEnabled(bool allInstalled, bool isBusy) => !allInstalled && !isBusy;

    // x:Bind callers pass each slot property individually: Svc.Clip is never reassigned, so binding the slot itself would never update.

    // RateEtaLabel still renders empty text when BytesPerSecond==0, so a stalled row shows a blank line instead of vanishing.
    internal Visibility ShowRateEta(ModelInstallStatus status, bool hasStarted) =>
        status == ModelInstallStatus.Downloading && hasStarted
            ? Visibility.Visible : Visibility.Collapsed;

    internal string ProgressLabel(string? message, double fraction, ulong? bytesDone, ulong? totalBytes)
    {
        // Show the engine's message (e.g. "Queued — starting download…") while still at 0%, so the row does not read "Starting…" after the engine has acknowledged the prewarm.
        string pct;
        if (fraction > 0) pct = $"{fraction * 100:0}%";
        else if (!string.IsNullOrEmpty(message)) pct = message;
        else pct = "Starting…";
        var bytes = string.Empty;
        if (bytesDone is { } done && totalBytes is { } total && total > 0)
        {
            // Max: the engine's per-file total can fall behind the bundle-cumulative BytesDone ("578 MB of 201 MB").
            var shownTotal = Math.Max(done, total);
            bytes = $" · {FormatBytes(done)} of {FormatBytes(shownTotal)}";
        }
        else if (totalBytes is { } total2)
        {
            bytes = $" · of {FormatBytes(total2)}";
        }
        return pct + bytes;
    }

    internal string RateEtaLabel(double bytesPerSecond, double etaSeconds)
    {
        if (bytesPerSecond <= 0) return string.Empty;
        var rate = $"{FormatBytes((ulong)bytesPerSecond)}/s";
        var eta = etaSeconds > 0 ? " · " + FormatEta(etaSeconds) + " remaining" : string.Empty;
        return rate + eta;
    }

    internal string ErrorLabel(string? lastError) =>
        "Failed: " + (lastError ?? "unknown error");

    /// <summary>The welcome slot is satisfied by any installed VLM; avoid naming a recommended model that might not be the one on disk.</summary>
    internal string VlmTitle(string displayLabel, ModelInstallStatus status) =>
        status == ModelInstallStatus.Installed
            ? "Description model downloaded"
            : displayLabel;

    internal string VlmInstallSize(ulong approxBytes, ModelInstallStatus status) =>
        status == ModelInstallStatus.Installed ? string.Empty : VlmSize(approxBytes);

    internal string VlmSize(ulong approxBytes) => "~" + Services.ByteFormat.File(approxBytes);

    private static string FormatBytes(ulong b) => Services.ByteFormat.File(b);

    private static string FormatEta(double seconds)
    {
        // Clamp NaN, infinity and EMA-asymptote overflow before any arithmetic.
        if (double.IsNaN(seconds) || double.IsInfinity(seconds) || seconds < 0)
        {
            return "—";
        }
        if (seconds < 60) return $"{seconds:0}s";
        if (seconds < 3600) return $"{seconds / 60:0}m {seconds % 60:00}s";
        var hours = seconds / 3600;
        if (hours > 99) return "99+ h";
        return $"{hours:0}h {(seconds % 3600) / 60:00}m";
    }

    private void OnClipActionClicked(object sender, RoutedEventArgs e)
    {
        DebugLog.Info("[INSTALL] CLIP per-row button clicked.");
        HandleAction(Svc.Clip);
    }

    private void OnArcfaceActionClicked(object sender, RoutedEventArgs e)
    {
        DebugLog.Info("[INSTALL] ArcFace per-row button clicked.");
        HandleAction(Svc.Arcface);
    }

    private void OnRamPlusActionClicked(object sender, RoutedEventArgs e)
    {
        DebugLog.Info("[INSTALL] RAM++ per-row button clicked.");
        HandleAction(Svc.RamPlus);
    }

    private void OnDeepVlmActionClicked(object sender, RoutedEventArgs e)
    {
        DebugLog.Info("[INSTALL] Deep Analyze (Qwen) per-row button clicked.");
        HandleAction(Svc.DeepVlm);
    }

    // Only reachable where ShowAcceleratorButton is visible.
    private void OnAcceleratorActionClicked(object sender, RoutedEventArgs e)
    {
        DebugLog.Info("[INSTALL] GPU Acceleration Pack per-row button clicked.");
        HandleAction(Svc.Accelerator);
    }

    internal Visibility ShowAcceleratorButton(ModelInstallStatus status, bool isRealInstall)
    {
        if (status == ModelInstallStatus.Installed && !isRealInstall) return Visibility.Collapsed;
        return status != ModelInstallStatus.Installed ? Visibility.Visible : Visibility.Collapsed;
    }

    internal Visibility ShowAcceleratorInstalledBadge(ModelInstallStatus status, bool isRealInstall)
    {
        return (status == ModelInstallStatus.Installed && isRealInstall)
            ? Visibility.Visible : Visibility.Collapsed;
    }

    internal string AcceleratorGlyph(ModelInstallStatus status, bool isRealInstall)
    {
        if (status == ModelInstallStatus.Installed && !isRealInstall) return ""; // Info — DirectML optimal
        return GlyphFor(status);
    }

    internal Brush AcceleratorIconBrush(ModelInstallStatus status, bool isRealInstall)
    {
        if (status == ModelInstallStatus.Installed && !isRealInstall)
        {
            return FileID.Services.ThemeHelper.GetBrushSafe("TextFillColorSecondaryBrush");
        }
        return IconBrushFor(status);
    }

    internal string AcceleratorSize(ulong approxBytes, ModelInstallStatus status, bool isRealInstall)
    {
        if (status == ModelInstallStatus.Installed && !isRealInstall) return string.Empty;
        if (approxBytes <= 0) return string.Empty;
        return "~" + Services.ByteFormat.File(approxBytes);
    }

    private readonly HashSet<ModelSlot> _installInFlight = new();

    private void HandleAction(ModelSlot slot)
    {
        DebugLog.Info($"[INSTALL] HandleAction({slot.DisplayLabel}) — Status={slot.Status}");
        switch (slot.Status)
        {
            case ModelInstallStatus.Downloading:
                slot.Message = "Cancelling…";
                slot.BytesPerSecond = 0;
                slot.EtaSeconds = 0;
                _ = SafeRunAsync(() => Svc.CancelModelAsync(slot.CurrentModelKind), "Cancel " + slot.DisplayLabel);
                break;
            case ModelInstallStatus.NotInstalled:
            case ModelInstallStatus.Failed:
                if (!_installInFlight.Add(slot))
                {
                    DebugLog.Info($"[INSTALL] {slot.DisplayLabel} install already in flight; ignoring duplicate click.");
                    break;
                }
                _ = SafeRunAsync(async () =>
                {
                    try { await slot.InstallAsync().ConfigureAwait(true); }
                    finally { _installInFlight.Remove(slot); }
                }, "Install " + slot.DisplayLabel);
                break;
            case ModelInstallStatus.Installed:
                break;
        }
    }

    private void OnInstallAllClicked(object sender, RoutedEventArgs e)
    {
        DebugLog.Info("[INSTALL] 'Download all' button clicked.");
        _ = SafeRunAsync(() => Svc.InstallAllAsync(), "Install all");
    }

    private void OnSkipClicked(object sender, RoutedEventArgs e)
    {
        RaiseDismissed();
    }

    private void RaiseDismissed()
    {
        try
        {
            // Write through the canonical in-memory instance: a throwaway Load() would be overwritten by AppViewModel's stale snapshot on its next Save(), re-showing the sheet every launch.
            var settings = AppViewModel.Instance.Settings;
            if (!settings.WelcomeSheetSeen)
            {
                settings.WelcomeSheetSeen = true;
                // Flush synchronously: closing the app inside the ~200 ms debounce window would drop the write.
                settings.SaveImmediately();
                DebugLog.Info("[INSTALL] welcomeSheetSeen=true persisted to app-settings.json");
            }
        }
        catch (Exception ex) { DebugLog.Warn("RaiseDismissed: settings.Save threw: " + ex.Message); }
        Dismissed?.Invoke(this, EventArgs.Empty);
    }

    private static async Task SafeRunAsync(Func<Task> action, string label)
    {
        try
        {
            await action().ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"[INSTALL] {label} threw: {ex}");
        }
    }
}
