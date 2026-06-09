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
using FileID.ViewModels;
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

    /// <summary>Cancels the auto-dismiss task + any in-flight restart
    /// prompt if the sheet unloads before they complete. Without this
    /// the Task.Run + Task.Delay continues firing TryEnqueue / ShowAsync
    /// on a control that's already been removed from the visual tree.</summary>
    private CancellationTokenSource? _lifetimeCts = new();

    public WelcomeSheet()
    {
        InitializeComponent();
        // Subscribe FIRST, then seed. Reverse order would race: a
        // sentinel that flips Status synchronously during Seed fires
        // PropertyChanged with no handler attached, the AllInstalled
        // signal is lost, and the auto-dismiss never fires.
        Svc.PropertyChanged += OnServicePropertyChanged;
        Unloaded += (_, _) =>
        {
            Svc.PropertyChanged -= OnServicePropertyChanged;
            // Cancel in-flight auto-dismiss + restart-prompt tasks so
            // they don't fire TryEnqueue / ShowAsync on a detached control.
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

        // If the user opens the sheet with everything already installed
        // (e.g. they re-opened it from Settings), the auto-dismiss should
        // still fire so they're not staring at three green checkmarks.
        // Belt-and-braces with the PropertyChanged path above.
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
        var ct = _lifetimeCts?.Token ?? CancellationToken.None;
        var dq = DispatcherQueue; // capture now while still attached
        // Match macOS WelcomeSheet.swift:103-109: 800 ms before dismissing
        // so the user sees the green checkmark transition land.
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
        // NotInstalled + Downloading both use gold — matches macOS line 174
        // which uses Theme.gold for every non-installed state.
        _ => GoldBrushResolved,
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

    // A single ProgressBar per row (no ProgressBar↔ProgressRing swap). It's
    // visible for the whole Downloading phase (VisibleIfDownloading) and just
    // flips indeterminate → determinate the moment the first byte lands.
    // Toggling one property once is flicker-free; swapping two controls'
    // Visibility every time Fraction crossed 0 was the old flicker source.
    //
    // Gate indeterminate on NOT-yet-started (HasStarted is sticky for the
    // session), not on an instantaneous frac<=0. The GPU pack installs two
    // sub-packs into one slot, so Fraction rewinds 1.0→~0 at the boundary; a
    // frac<=0 gate would re-flap the bar back to its marquee mid-download.
    internal bool IsStarting(ModelInstallStatus s, bool hasStarted) =>
        s == ModelInstallStatus.Downloading && !hasStarted;

    internal Visibility ShowActionButton(ModelInstallStatus s) =>
        s != ModelInstallStatus.Installed ? Visibility.Visible : Visibility.Collapsed;

    internal string ButtonLabel(ModelInstallStatus s) => s switch
    {
        ModelInstallStatus.Downloading => "Cancel",
        ModelInstallStatus.Failed => "Retry",
        _ => "Install",
    };

    internal string SkipLabel(bool allInstalled) => allInstalled ? "Done" : "Skip for now";

    internal bool InstallAllEnabled(bool allInstalled, bool isBusy) => !allInstalled && !isBusy;

    // x:Bind function-call bindings only re-evaluate when their argument
    // EXPRESSIONS change. Passing `Svc.Clip` (the singleton slot) means
    // the binding never updates because Svc.Clip is never reassigned —
    // we have to pass the individual properties so x:Bind subscribes to
    // each one's PropertyChanged. Hence the verbose argument lists.

    // Sticky: once the rate row has shown during a Downloading session, keep
    // it Visible rather than collapsing on a single transient sub-100 B/s
    // stall sample (which would flap the row in/out). RateEtaLabel still
    // renders empty text when BytesPerSecond==0, so a stalled row shows a
    // blank line instead of vanishing. The 5-sample "Stalled…" Message in
    // ModelSlot still surfaces a real stall.
    internal Visibility ShowRateEta(ModelInstallStatus status, bool hasStarted) =>
        status == ModelInstallStatus.Downloading && hasStarted
            ? Visibility.Visible : Visibility.Collapsed;

    internal string ProgressLabel(string? message, double fraction, ulong? bytesDone, ulong? totalBytes)
    {
        // Prefer the engine's caption (e.g. "Queued — starting download…") while
        // we're still at 0% — otherwise the row reads "Starting…" forever even
        // after the engine has acknowledged the prewarm. Once real progress
        // lands (fraction > 0), the percentage is more useful than the caption.
        string pct;
        if (fraction > 0) pct = $"{fraction * 100:0}%";
        else if (!string.IsNullOrEmpty(message)) pct = message;
        else pct = "Starting…";
        var bytes = string.Empty;
        if (bytesDone is { } done && totalBytes is { } total && total > 0)
        {
            // defensive `Max` so the user never sees "578 MB of 201 MB"
            // when the engine's per-file total falls behind the bundle-cumulative
            // BytesDone. ModelSlot.Apply already guards against TotalBytes
            // downgrading; this is belt-and-suspenders for any race window.
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

    /// <summary>Deep Analyze (Qwen) row title — e.g. "Deep Analyze (Qwen2.5-VL
    /// 3B)". Reads DisplayLabel via x:Bind so the hardware-tiered recommendation
    /// (3B ↔ 7B) updates the row text without a page reload.</summary>
    internal string VlmTitle(string displayLabel) => $"Deep Analyze ({displayLabel})";

    internal string VlmSize(ulong approxBytes)
    {
        const double GB = 1024.0 * 1024.0 * 1024.0;
        const double MB = 1024.0 * 1024.0;
        if (approxBytes >= GB) return $"~{approxBytes / GB:0.0} GB";
        return $"~{approxBytes / MB:0} MB";
    }

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
        // clamp pathological values (NaN, infinity, EMA-asymptote
        // overflow) before any arithmetic so the user never sees
        // "7726735523606260000000000h" again. The slot-side fix in
        // UpdateRate already caps at 99h via MaxEtaSeconds, but a stale
        // value plumbed through other code paths still reaches here.
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

    // GPU Acceleration Pack row. On NVIDIA this kicks off the
    // cuDNN download via PrewarmModelAsync("cudnn_runtime_x64"). On other
    // vendors the button isn't shown (ShowAcceleratorButton returns
    // Collapsed) so this handler can't fire.
    private void OnAcceleratorActionClicked(object sender, RoutedEventArgs e)
    {
        DebugLog.Info("[INSTALL] GPU Acceleration Pack per-row button clicked.");
        HandleAction(Svc.Accelerator);
    }

    // XAML binding helpers for the Accelerator row.
    internal Visibility ShowAcceleratorButton(ModelInstallStatus status, bool isRealInstall)
    {
        // Show the button only when there's something the user can do:
        // NVIDIA + cuDNN not yet installed (NotInstalled / Failed) OR
        // a download in flight (so they can Cancel). On non-NVIDIA we
        // pre-set Status=Installed AND AcceleratorIsRealInstall=false;
        // hide the button there.
        if (status == ModelInstallStatus.Installed && !isRealInstall) return Visibility.Collapsed;
        return status != ModelInstallStatus.Installed ? Visibility.Visible : Visibility.Collapsed;
    }

    internal Visibility ShowAcceleratorInstalledBadge(ModelInstallStatus status, bool isRealInstall)
    {
        // "Installed" badge is shown only after a real cuDNN install
        // (NVIDIA only). For non-NVIDIA, no badge — the Message text
        // already explains "DirectML is already optimal".
        return (status == ModelInstallStatus.Installed && isRealInstall)
            ? Visibility.Visible : Visibility.Collapsed;
    }

    internal string AcceleratorGlyph(ModelInstallStatus status, bool isRealInstall)
    {
        // Reuse the same glyph palette: green check for "really installed",
        // info chip for "not applicable on this vendor", spinner-ish for
        // downloading, hint for not-yet-installed.
        if (status == ModelInstallStatus.Installed && !isRealInstall) return ""; // Info — DirectML optimal
        return GlyphFor(status);
    }

    internal Brush AcceleratorIconBrush(ModelInstallStatus status, bool isRealInstall)
    {
        // For "DirectML optimal" rows (non-NVIDIA Installed-pseudo) tint
        // muted instead of green so the row reads as informational, not
        // success.
        if (status == ModelInstallStatus.Installed && !isRealInstall)
        {
            return FileID.Services.ThemeHelper.GetBrushSafe("TextFillColorSecondaryBrush");
        }
        return IconBrushFor(status);
    }

    internal string AcceleratorSize(ulong approxBytes, ModelInstallStatus status, bool isRealInstall)
    {
        // Hide the "~430 MB" badge when the slot is pseudo-installed for
        // a non-NVIDIA vendor (Status=Installed, but no real install
        // happened — DirectML is optimal so the byte count is misleading).
        if (status == ModelInstallStatus.Installed && !isRealInstall) return string.Empty;
        if (approxBytes <= 0) return string.Empty;
        if (approxBytes >= 1024UL * 1024 * 1024)
            return $"~{approxBytes / (1024.0 * 1024 * 1024):0.#} GB";
        return $"~{approxBytes / (1024 * 1024)} MB";
    }

    // Per-row install re-entry guard. PrewarmAsync only flips Status to
    // Downloading AFTER awaiting WaitForReadyAsync, so a rapid double-click
    // on Install/Retry can fire two slot.InstallAsync() (→ duplicate prewarm
    // IPC) while Status is still NotInstalled/Failed. Tracked by slot
    // reference; cleared when the install task settles.
    private readonly HashSet<ModelSlot> _installInFlight = new();

    private void HandleAction(ModelSlot slot)
    {
        DebugLog.Info($"[INSTALL] HandleAction({slot.DisplayLabel}) — Status={slot.Status}");
        switch (slot.Status)
        {
            case ModelInstallStatus.Downloading:
                // Pre-flip caption to "Cancelling…" so the user gets instant
                // feedback. The engine takes 1-5 s to confirm cancellation
                // (downloads need to abort their in-flight chunks), and
                // during that window the stale progress text would otherwise
                // keep the row reading like nothing happened. Matches
                // macOS WelcomeSheet.swift:74-82's pre-emptive reset.
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
    /// { welcomeSheetSeen = true }
    /// (FileIDApp.swift:39). Idempotent — safe to invoke from both the
    /// auto-dismiss path and the manual Skip/Done paths.</summary>
    private void RaiseDismissed()
    {
        try
        {
            // Use the ONE canonical in-memory instance, not a throwaway
            // Load(): the long-lived AppViewModel instance would otherwise
            // serialize its stale snapshot on its next Save() and revert this
            // write (the Welcome sheet then re-appears every launch).
            var settings = AppViewModel.Instance.Settings;
            if (!settings.WelcomeSheetSeen)
            {
                settings.WelcomeSheetSeen = true;
                // Synchronous flush, not the debounced Save(): dismissing the
                // sheet then closing the app within the ~200 ms debounce window
                // would otherwise drop the write and re-show the sheet next
                // launch. Mirrors MainWindow.OnClosed's SaveImmediately().
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
