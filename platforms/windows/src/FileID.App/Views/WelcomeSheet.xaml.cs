// WelcomeSheet code-behind. 1:1 port of platforms/apple/.../WelcomeSheet.swift.
//
// State authority is ModelInstallerService.Instance — every visual element
// binds to it via x:Bind OneWay. There is NO polling timer; row updates
// land the moment a slot's PropertyChanged fires (which happens on every
// engine progress event). The code-behind is just:
//   1. Helper functions referenced by x:Bind expressions in the XAML.
//   2. Click handlers that delegate to ModelSlot.InstallAsync /
//      ModelInstallerService.CancelAllAsync / dismiss.
//   3. A small subscription to AllInstalled that auto-dismisses 800 ms
//      after every model finishes (matches macOS).
//
// The "LIVE INSTALL STATE" diagnostic panel from earlier iterations is
// gone — x:Bind makes it redundant (UI reflects model state in real time).

using FileID.IpcSchema;
using FileID.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using System.ComponentModel;

namespace FileID.Views;

public sealed partial class WelcomeSheet : UserControl
{
    /// <summary>Raised when the user clicks Skip OR all models finish installing.</summary>
    public event EventHandler? Dismissed;

    /// <summary>Singleton service exposed for x:Bind. WinUI 3 x:Bind binds
    /// against the page's own properties/fields, not DataContext, so we
    /// surface the service through this property.</summary>
    internal ModelInstallerService Svc => ModelInstallerService.Instance;

    private bool _autoDismissScheduled;

    public WelcomeSheet()
    {
        InitializeComponent();
        try
        {
            Svc.SeedFromSentinels();
        }
        catch (Exception ex)
        {
            DebugLog.Warn("WelcomeSheet ctor SeedFromSentinels threw: " + ex.Message);
        }

        Svc.PropertyChanged += OnServicePropertyChanged;
        Unloaded += (_, _) => Svc.PropertyChanged -= OnServicePropertyChanged;

        // If the user opens the sheet with everything already installed
        // (e.g. they re-opened it from Settings), the auto-dismiss should
        // still fire so they're not staring at three green checkmarks.
        if (Svc.AllInstalled) ScheduleAutoDismiss();
    }

    private void OnServicePropertyChanged(object? sender, PropertyChangedEventArgs e)
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
        // Match macOS WelcomeSheet.swift:103-109: 800 ms before dismissing
        // so the user sees the green checkmark transition land.
        _ = Task.Run(async () =>
        {
            try
            {
                await Task.Delay(800).ConfigureAwait(false);
                if (DispatcherQueue is not null)
                {
                    DispatcherQueue.TryEnqueue(() =>
                    {
                        if (Svc.AllInstalled) RaiseDismissed();
                    });
                }
            }
            catch (Exception ex) { DebugLog.Warn("WelcomeSheet auto-dismiss threw: " + ex.Message); }
        });
    }

    // ─── x:Bind helper functions ────────────────────────────────────────
    //
    // WinUI 3 supports x:Bind functions: the XAML can reference any
    // public/internal method on the page and re-evaluate it whenever any
    // bound argument changes. This lets the view stay declarative without
    // needing IValueConverter classes.

    // Segoe Fluent Icons code points. Numeric escapes survive any source
    // round-trip; raw chars don't. Glyphs match macOS SF Symbols 1:1:
    //   Installed  → checkmark (matches "checkmark.seal.fill")
    //   Downloading→ down-arrow circle (matches "arrow.down.circle.fill")
    //   NotInstalled → cloud download (matches "square.and.arrow.down.on.square")
    //   Failed     → warning triangle
    private const string GlyphCheck   = "\uE73E"; // CheckMark
    private const string GlyphArrow   = "\uE896"; // Download (tray + arrow)
    private const string GlyphCloud   = "\uE896"; // same — cloud-download metaphor
    private const string GlyphWarning = "\uEA39"; // Warning

    internal string GlyphFor(ModelInstallStatus s) => s switch
    {
        ModelInstallStatus.Installed    => GlyphCheck,
        ModelInstallStatus.Downloading  => GlyphArrow,
        ModelInstallStatus.Failed       => GlyphWarning,
        _                               => GlyphCloud,
    };

    private SolidColorBrush GoldBrushResolved =>
        (SolidColorBrush)Application.Current.Resources["GoldBrush"];

    private static readonly SolidColorBrush GreenBrush =
        new(Windows.UI.Color.FromArgb(0xFF, 0x6B, 0xE0, 0x82));

    private static readonly SolidColorBrush RedBrush =
        new(Windows.UI.Color.FromArgb(0xFF, 0xE5, 0x55, 0x55));

    internal Brush IconBrushFor(ModelInstallStatus s) => s switch
    {
        ModelInstallStatus.Installed   => GreenBrush,
        ModelInstallStatus.Failed      => RedBrush,
        // NotInstalled + Downloading both use gold — matches macOS line 174
        // which uses Theme.gold for every non-installed state.
        _                              => GoldBrushResolved,
    };

    // x:Bind functions that drive Visibility return Visibility directly:
    // x:Bind's implicit bool→Visibility convert path isn't emitted by the
    // codegen for function-call sources (only property-path sources), so
    // returning Visibility avoids the broken cast.

    internal Visibility VisibleIfDownloading(ModelInstallStatus s) =>
        s == ModelInstallStatus.Downloading ? Visibility.Visible : Visibility.Collapsed;

    internal Visibility VisibleIfInstalled(ModelInstallStatus s) =>
        s == ModelInstallStatus.Installed ? Visibility.Visible : Visibility.Collapsed;

    internal Visibility VisibleIfFailed(ModelInstallStatus s) =>
        s == ModelInstallStatus.Failed ? Visibility.Visible : Visibility.Collapsed;

    internal Visibility ShowDeterminate(ModelInstallStatus s, double frac) =>
        s == ModelInstallStatus.Downloading && frac > 0
            ? Visibility.Visible : Visibility.Collapsed;

    internal Visibility ShowSpinner(ModelInstallStatus s, double frac) =>
        s == ModelInstallStatus.Downloading && frac <= 0
            ? Visibility.Visible : Visibility.Collapsed;

    internal bool SpinnerActive(ModelInstallStatus s, double frac) =>
        s == ModelInstallStatus.Downloading && frac <= 0;

    internal Visibility ShowActionButton(ModelInstallStatus s) =>
        s != ModelInstallStatus.Installed ? Visibility.Visible : Visibility.Collapsed;

    internal string ButtonLabel(ModelInstallStatus s) => s switch
    {
        ModelInstallStatus.Downloading => "Cancel",
        ModelInstallStatus.Failed      => "Retry",
        _                              => "Install",
    };

    internal string SkipLabel(bool allInstalled) => allInstalled ? "Done" : "Skip for now";

    internal bool InstallAllEnabled(bool allInstalled, bool isBusy) => !allInstalled && !isBusy;

    // x:Bind function-call bindings only re-evaluate when their argument
    // EXPRESSIONS change. Passing `Svc.Clip` (the singleton slot) means
    // the binding never updates because Svc.Clip is never reassigned —
    // we have to pass the individual properties so x:Bind subscribes to
    // each one's PropertyChanged. Hence the verbose argument lists.

    internal Visibility ShowRateEta(ModelInstallStatus status, double bytesPerSecond) =>
        status == ModelInstallStatus.Downloading && bytesPerSecond > 0
            ? Visibility.Visible : Visibility.Collapsed;

    internal string ProgressLabel(double fraction, ulong? bytesDone, ulong? totalBytes)
    {
        var pct = fraction > 0 ? $"{fraction * 100:0}%" : "Starting…";
        var bytes = string.Empty;
        if (bytesDone is { } done && totalBytes is { } total && total > 0)
        {
            bytes = $" · {FormatBytes(done)} of {FormatBytes(total)}";
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

    private static string FormatBytes(ulong b)
    {
        const double KB = 1024.0;
        const double MB = 1024.0 * 1024.0;
        const double GB = 1024.0 * 1024.0 * 1024.0;
        if (b >= GB) return $"{b / GB:0.00} GB";
        if (b >= MB) return $"{b / MB:0.0} MB";
        if (b >= KB) return $"{b / KB:0} KB";
        return $"{b} B";
    }

    private static string FormatEta(double seconds)
    {
        if (seconds < 60) return $"{seconds:0}s";
        if (seconds < 3600) return $"{seconds / 60:0}m {seconds % 60:00}s";
        return $"{seconds / 3600:0}h {(seconds % 3600) / 60:00}m";
    }

    // ─── Per-row action handlers ────────────────────────────────────────

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

    private void OnVlmActionClicked(object sender, RoutedEventArgs e)
    {
        DebugLog.Info("[INSTALL] VLM per-row button clicked.");
        HandleAction(Svc.Vlm);
    }

    private void HandleAction(ModelSlot slot)
    {
        DebugLog.Info($"[INSTALL] HandleAction({slot.DisplayLabel}) — Status={slot.Status}");
        switch (slot.Status)
        {
            case ModelInstallStatus.Downloading:
                _ = SafeRunAsync(() => Svc.CancelAllAsync(), "Cancel " + slot.DisplayLabel);
                break;
            case ModelInstallStatus.NotInstalled:
            case ModelInstallStatus.Failed:
                _ = SafeRunAsync(() => slot.InstallAsync(), "Install " + slot.DisplayLabel);
                break;
            case ModelInstallStatus.Installed:
                // No-op — UI shouldn't show a button in Installed state, but
                // belt-and-braces if a click slips through during a state
                // transition.
                break;
        }
    }

    private void OnInstallAllClicked(object sender, RoutedEventArgs e)
    {
        DebugLog.Info("[INSTALL] 'Install all' button clicked.");
        _ = SafeRunAsync(() => Svc.InstallAllAsync(), "Install all");
    }

    private void OnSkipClicked(object sender, RoutedEventArgs e)
    {
        RaiseDismissed();
    }

    /// <summary>Persist welcomeSheetSeen and raise the Dismissed event.
    /// Mirror of macOS .onDisappear { welcomeSheetSeen = true }
    /// (FileIDApp.swift:39). Idempotent — safe to invoke from both the
    /// auto-dismiss path and the manual Skip/Done paths.</summary>
    private void RaiseDismissed()
    {
        try
        {
            var settings = AppSettings.Load();
            if (!settings.WelcomeSheetSeen)
            {
                settings.WelcomeSheetSeen = true;
                settings.Save();
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
