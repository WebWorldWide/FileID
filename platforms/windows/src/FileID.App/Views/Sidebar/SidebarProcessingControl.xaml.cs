// SidebarProcessingControl code-behind: the v3 scan card. Subscribes to
// EngineClient state and re-paints title, bar, sub line, counters and
// controls as the pipeline advances (macOS PipelineBlock).
//
// Stat color thresholds (matches macOS):
//   memory  > 1200 MB -> orange
//   failures > 0      -> red

using System.ComponentModel;
using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.UI;

namespace FileID.Views.Sidebar;

public sealed partial class SidebarProcessingControl : UserControl
{
    // Brushes are DispatcherObjects. The previous version allocated
    // four fresh SolidColorBrushes per progress event (10 Hz during scan =
    // 40 DispatcherObject allocations/sec). Per CLAUDE.md guidance, cache
    // UI-thread-affined resources at ctor time. SidebarPipelineProgress
    // uses the same pattern.
    private readonly SolidColorBrush _memoryWarnBrush;
    private readonly SolidColorBrush _statDefaultBrush;
    // Idle-status default foreground. Resolved once at ctor instead of via a
    // Resources traversal on every Sync() (~10 Hz during a scan, even with the
    // IdlePanel collapsed).
    private readonly Brush _idleStatusDefaultBrush;
    private readonly Brush _gold;
    private readonly Brush _ai;
    private readonly Brush _delight;
    private readonly Brush _text1;
    private readonly Brush _text2;
    private readonly Brush _goldBar;
    private readonly Brush _aiBar;
    private readonly Brush _doneBar;
    private readonly Brush _pausedBar;

    public SidebarProcessingControl()
    {
        InitializeComponent();
        _memoryWarnBrush = new SolidColorBrush(Color.FromArgb(0xFF, 0xFF, 0x99, 0x00));
        _statDefaultBrush = new SolidColorBrush(Color.FromArgb(0xFF, 0xF0, 0xF1, 0xF2));
        _idleStatusDefaultBrush = FileID.Services.ThemeHelper.GetBrushSafe("Text3Brush");
        _gold = FileID.Services.ThemeHelper.GetBrushSafe("GoldBrush");
        _ai = FileID.Services.ThemeHelper.GetBrushSafe("AiBrush");
        _delight = FileID.Services.ThemeHelper.GetBrushSafe("DelightBrush");
        _text1 = FileID.Services.ThemeHelper.GetBrushSafe("Text1Brush");
        _text2 = FileID.Services.ThemeHelper.GetBrushSafe("Text2Brush");
        _goldBar = FileID.Services.ThemeHelper.GetBrushSafe("GoldProgressBrush");
        _aiBar = FileID.Services.ThemeHelper.GetBrushSafe("AiProgressBrush");
        _doneBar = FileID.Services.ThemeHelper.GetBrushSafe("DoneProgressBrush");
        _pausedBar = FileID.Services.ThemeHelper.GetBrushSafe("PausedBrush");
        Loaded += (_, _) => { Sync(); SyncWarningBanner(); };
        EngineClient.Instance.PropertyChanged += OnEngineChanged;
        AppViewModel.Instance.PropertyChanged += OnAppChanged;
        Unloaded += (_, _) =>
        {
            EngineClient.Instance.PropertyChanged -= OnEngineChanged;
            AppViewModel.Instance.PropertyChanged -= OnAppChanged;
        };
    }

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
        // SafeRun wrap + per-property tracing. A handler that throws
        // would otherwise escape into the dispatcher loop. The trace line
        // (logged only when we actually act on the property) names the
        // last subscriber called before a native fast-fail.
        => DebugLog.SafeRun("SidebarProcessingControl.OnEngineChanged", () =>
        {
            if (e.PropertyName is nameof(EngineClient.LastProgress)
                              or nameof(EngineClient.Phase)
                              or nameof(EngineClient.State)
                              or nameof(EngineClient.IsPaused)
                              or nameof(EngineClient.LastScanDuration)
                              or nameof(EngineClient.LastError)
                              or nameof(EngineClient.GpuDeviceRemoved)
                              or nameof(EngineClient.DeepAnalyzeProgress)
                              or nameof(EngineClient.DeepAnalyzeStarting)
                              or nameof(EngineClient.DeepAnalyzeComplete)
                              or nameof(EngineClient.QueueState))
            {
                DebugLog.Debug($"[ENGINE-SUB:SidebarProcessingControl] {e.PropertyName}");
                DispatcherQueue.TryEnqueue(Sync);
            }
            if (e.PropertyName == nameof(EngineClient.LastWarning))
            {
                DebugLog.Debug($"[ENGINE-SUB:SidebarProcessingControl] {e.PropertyName}");
                DispatcherQueue.TryEnqueue(SyncWarningBanner);
            }
            // No per-batch CompletionRipple: at ~5 Hz its 0.9 s rings stacked
            // 4-5 deep and destabilized the sidebar; macOS has no such affordance.
        });


    private void OnAppChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun(nameof(OnAppChanged), () => OnAppChangedCore(sender, e));

    private void OnAppChangedCore(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName is nameof(AppViewModel.HasFolder) or nameof(AppViewModel.FolderDisplay))
        {
            DispatcherQueue.TryEnqueue(Sync);
        }
    }

    /// <summary>Cached per-launch (NOT persisted) so the pre-scan
    /// performance warning isn't shown twice in a single session. Reset
    /// every app relaunch — gives the user a fresh chance to install
    /// the recommended Performance Pack.</summary>
    private bool _userAcceptedSuboptimalScan;

    /// <summary>Re-entrancy guard for the pre-scan ContentDialog.
    /// WinUI 3's ContentDialog throws if a second is shown while the
    /// first is open — spam-clicking Start scan would crash. Set on
    /// dialog open, cleared in finally.</summary>
    private bool _prescanDialogShowing;

    /// <summary>Click-to-engine-ack gate. Without this, spam-clicking
    /// Start Scan was issuing N IPC `startScan` commands in parallel and
    /// N optimistic `Phase = Discovering` flips, racing each other through
    /// the state machine. Set true at the top of `OnStartScanClicked`,
    /// cleared in the outermost `finally`. Sync() reads this to keep the
    /// button disabled for the entire click-to-engine-ack window.</summary>
    private bool _startInFlight;

    private async void OnStartScanClicked(object sender, RoutedEventArgs e)
    {
        // short-circuit spam-clicks. The button's IsEnabled
        // binding (Sync()) already prevents this in the normal case,
        // but a touch-screen double-tap can squeeze a second click in
        // before Sync() runs.
        if (_startInFlight) return;
        _startInFlight = true;
        Sync(); // pre-flip button + pill so visual feedback is immediate

        // wrap the whole async void body in a catch-all so a
        // disposed XamlRoot / broken dialog / engine death between the prompt
        // and the IPC send can't escape into App.UnhandledException and take
        // down the process. The user reported "click Start scan, nothing
        // happens, then the app crashes" — guarding here ensures the worst
        // we ever do is log the failure and show a clean error sheet.
        try
        {
            var vm = AppViewModel.Instance;
            if (!vm.HasFolder)
            {
                await ShowAlertAsync("Pick a folder first",
                    "FileID needs a folder to scan. Use the folder switcher in the title bar.");
                return;
            }

            // Engine not Ready yet: wait (up to 30 s) with inline feedback
            // instead of silently no-op'ing ("Start Scan does nothing"). 30 s
            // covers a cold DirectML probe + slow HDD, where the first Ready
            // can land at 20-25 s, while still failing fast on a dead engine.
            if (EngineClient.Instance.State != EngineClient.LifecycleState.Ready)
            {
                IdleStatusText.Text = $"Waiting for engine ({EngineClient.Instance.State})…";
                StartScanButton.IsEnabled = false;
                try
                {
                    await EngineClient.Instance.WaitForReadyAsync(System.TimeSpan.FromSeconds(30));
                }
                catch (System.Exception ex)
                {
                    DebugLog.Warn("StartScan: WaitForReadyAsync threw: " + ex.Message);
                    await ShowAlertAsync("Engine isn't ready",
                        $"FileID's engine hasn't reported ready after 30 seconds (status: {EngineClient.Instance.State}).\n\n" +
                        "Try restarting the app. If this keeps happening, check the engine log at " +
                        "%LOCALAPPDATA%\\FileID\\logs\\engine.jsonl.");
                    DispatcherQueue.TryEnqueue(Sync);
                    return;
                }
                finally
                {
                    DispatcherQueue.TryEnqueue(Sync);
                }
            }

            // Pre-scan EP gate: warn only when the engine will fall back to
            // CPU. DirectML on every D3D12-capable GPU is now the canonical
            // path; no Performance Pack prompts.
            if (!_userAcceptedSuboptimalScan)
            {
                var hw = EngineClient.Instance.Info?.Hardware;
                if (hw is not null)
                {
                    var prompt = BuildPerformancePrompt(hw);
                    if (prompt is not null)
                    {
                        var result = await ShowPerformancePromptAsync(prompt);
                        switch (result)
                        {
                            case PerformancePromptResult.OpenSettings:
                                AppViewModel.Instance.ActiveTab = SidebarTab.Settings;
                                return;
                            case PerformancePromptResult.Cancel:
                                return;
                            case PerformancePromptResult.Continue:
                                _userAcceptedSuboptimalScan = true;
                                break;
                        }
                    }
                }
            }

            try
            {
                await EngineClient.Instance.StartScanAsync(vm.FolderPath!, vm.FolderDisplay);
                DebugLog.Info($"Sent startScan: {PathRedactor.Redact(vm.FolderPath!)}");
            }
            catch (Exception ex)
            {
                DebugLog.Error("StartScan IPC failed: " + ex.Message);
                await ShowAlertAsync("Scan didn't start",
                    "FileID couldn't tell the engine to start. Engine status: "
                    + EngineClient.Instance.State);
            }
        }
        catch (Exception ex)
        {
            DebugLog.Error("OnStartScanClicked unexpected exception: " + ex);
        }
        finally
        {
            // clear the in-flight gate AFTER the IPC send returns
            // (success or failure). The engine's PhaseChanged events will
            // then drive Sync() to the right state (Discovering / Failed /
            // etc.). Re-run Sync() once so the button reflects the new
            // state without waiting for the next PropertyChanged.
            _startInFlight = false;
            try { Sync(); } catch { /* swallow */ }
        }
    }

    private enum PerformancePromptResult { Continue, OpenSettings, Cancel }

    private sealed record PerformancePrompt(string Title, string Body, bool ShowOpenSettings);

    /// <summary>Decide whether to warn the user before scanning. Returns
    /// null when the active EP is already optimal for this hardware.</summary>
    private static PerformancePrompt? BuildPerformancePrompt(HardwareInfo hw)
    {
        var ep = (hw.ExecutionProvider ?? string.Empty).ToLowerInvariant();
        var vendor = (hw.GpuVendor ?? string.Empty).ToLowerInvariant();

        // Optimal — best EP already active, scan freely.
        if (ep is "cuda" or "qnn" or "openvino" or "directml") return null;

        // CPU branch — the only state we still warn about. DirectML is the
        // universal GPU path; non-CPU EPs are always acceptable.
        if (ep == "cpu")
        {
            if (vendor is "" or "none")
            {
                return new PerformancePrompt(
                    "Scan will run on CPU",
                    "No GPU was detected on this PC. Scanning on CPU is roughly 10× slower than on GPU.\n\nContinue anyway?",
                    ShowOpenSettings: false);
            }
            return new PerformancePrompt(
                "GPU detected but inactive",
                $"FileID detected a {hw.AdapterName ?? hw.GpuVendor} GPU but is currently running on CPU. Check Settings → Performance for the GPU execution-provider override.\n\nScanning now will use CPU (roughly 10× slower).",
                ShowOpenSettings: true);
        }

        return null;
    }

    private async Task<PerformancePromptResult> ShowPerformancePromptAsync(PerformancePrompt p)
    {
        if (_prescanDialogShowing) return PerformancePromptResult.Cancel;
        _prescanDialogShowing = true;
        ContentDialogResult result;
        try
        {
            var dialog = new ContentDialog
            {
                XamlRoot = XamlRoot,
                Title = p.Title,
                Content = p.Body,
                PrimaryButtonText = p.ShowOpenSettings ? "Open Settings" : "Continue scan",
                SecondaryButtonText = p.ShowOpenSettings ? "Continue scan" : "Cancel",
                CloseButtonText = p.ShowOpenSettings ? "Cancel" : null,
                DefaultButton = ContentDialogButton.Primary,
            };
            result = await dialog.ShowAsync();
        }
        finally
        {
            _prescanDialogShowing = false;
        }
        if (p.ShowOpenSettings)
        {
            return result switch
            {
                ContentDialogResult.Primary => PerformancePromptResult.OpenSettings,
                ContentDialogResult.Secondary => PerformancePromptResult.Continue,
                _ => PerformancePromptResult.Cancel,
            };
        }
        // No "Open Settings" affordance: Primary == Continue, Secondary == Cancel.
        return result switch
        {
            ContentDialogResult.Primary => PerformancePromptResult.Continue,
            _ => PerformancePromptResult.Cancel,
        };
    }


    private async void OnPauseResumeClicked(object sender, RoutedEventArgs e)
    {
        try
        {
            // FEAT-1: drive the toggle off EngineClient.IsPaused (set by
            // PauseScanAsync/ResumeScanAsync optimistically) instead of
            // reading the visible Text -- which desyncs if the engine
            // emits an unrelated phase update between click + IPC reply.
            if (EngineClient.Instance.IsPaused)
            {
                await EngineClient.Instance.ResumeScanAsync();
            }
            else
            {
                await EngineClient.Instance.PauseScanAsync();
            }
        }
        catch (Exception ex)
        {
            DebugLog.Warn("Pause/Resume IPC failed: " + ex.Message);
        }
    }

    private async void OnCancelClicked(object sender, RoutedEventArgs e)
    {
        try
        {
            await EngineClient.Instance.CancelScanAsync();
        }
        catch (Exception ex)
        {
            DebugLog.Warn("Cancel IPC failed: " + ex.Message);
        }
    }

    private static readonly SolidColorBrush FailedTextBrush =
        new(Color.FromArgb(0xFF, 0xFF, 0x6B, 0x6B));

    private void SyncWarningBanner()
    {
        if (WarningBanner == null || WarningBannerText == null) return;
        var warning = EngineClient.Instance.LastWarning;
        if (warning is null)
        {
            WarningBanner.Visibility = Visibility.Collapsed;
            return;
        }
        WarningBannerText.Text = warning.Message;
        WarningBanner.Visibility = Visibility.Visible;
    }

    private void OnDismissWarningClicked(object sender, RoutedEventArgs e)
    {
        EngineClient.Instance.LastWarning = null;
    }

    /// <summary>Latched true once a scan's file Total is known, reset when no
    /// scan is in flight. Keeps a momentary Total==0 mid-scan from bouncing
    /// ScanProgressBar back to the indeterminate sweep (scan-bar flicker).</summary>
    private bool _scanTotalKnown;
    private double _lastScanFraction;
    private bool _detailsOpen;

    private enum CardControls { Pick, Start, Scanning, Rescan }

    private void OnDetailsToggleClicked(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnDetailsToggleClicked), () =>
        {
            _detailsOpen = !_detailsOpen;
            Sync();
        });

    private void Sync()
    {
        var engine = EngineClient.Instance;
        var vm = AppViewModel.Instance;
        var prog = engine.LastProgress;
        var phase = engine.Phase ?? prog?.Phase;

        bool isInFlight = phase is ScanPhase.Discovering or ScanPhase.Tagging or ScanPhase.PostScan;
        bool isCompleted = phase is ScanPhase.Completed;
        bool isFailed = phase is ScanPhase.Failed;
        bool isCancelled = phase is ScanPhase.Cancelled;
        bool captionsRunning = engine.DeepAnalyzeProgress is not null || engine.DeepAnalyzeStarting is not null;
        bool facesRunning = engine.QueueState?.Running?.Category == JobCategory.FaceCluster;
        var captionsDone = engine.DeepAnalyzeComplete;

        // Reset the latch between scans so the next scan starts indeterminate
        // until its file count lands.
        if (!isInFlight) _scanTotalKnown = false;

        // Start Scan is enabled on HasFolder alone; the click handler waits
        // for Ready with visible feedback. A Crashed engine or a removed GPU
        // can't recover from a click, and `!_startInFlight` stops a
        // touch-double-tap from issuing a second startScan (macOS parity).
        StartScanButton.IsEnabled = AppViewModel.Instance.HasFolder
                                  && EngineClient.Instance.State != EngineClient.LifecycleState.Crashed
                                  && !EngineClient.Instance.GpuDeviceRemoved
                                  && !_startInFlight;
        RestartEngineButton.Visibility = EngineClient.Instance.GpuDeviceRemoved
            ? Visibility.Visible
            : Visibility.Collapsed;

        string title;
        string sub;
        double? fraction = 0;
        Brush bar = _goldBar;
        Brush accent = _text2;
        bool danger = false;
        bool showLog = false;
        var controls = CardControls.Start;

        if (!vm.HasFolder)
        {
            title = "Pick a folder to start";
            sub = "Or drop a folder on the window.";
            controls = CardControls.Pick;
        }
        else if (EngineClient.Instance.GpuDeviceRemoved)
        {
            title = "Engine needs a restart";
            sub = EngineClient.GpuRestartRequiredMessage + " Use Restart Engine here in the sidebar.";
            bar = _pausedBar;
            danger = true;
        }
        else if (isInFlight)
        {
            var processed = prog?.Processed ?? 0;
            var total = prog is null ? 0UL : Math.Max(prog.Total, prog.Discovered);
            if (prog is { Total: > 0UL })
            {
                _lastScanFraction = Math.Clamp((double)prog.Processed / prog.Total, 0, 1);
                _scanTotalKnown = true;
                fraction = _lastScanFraction;
            }
            else
            {
                // Total not yet known (still discovering) — sweep; a transient
                // Total==0 after the count was known keeps the determinate bar.
                fraction = _scanTotalKnown ? _lastScanFraction : null;
            }

            var counted = $"{processed:N0} of {total:N0} files";
            if (engine.IsPaused)
            {
                title = "Scan paused";
                sub = counted;
                bar = _pausedBar;
                accent = _idleStatusDefaultBrush;
            }
            else
            {
                title = phase switch
                {
                    ScanPhase.Discovering => "Counting files",
                    ScanPhase.PostScan => "Finishing up",
                    _ => "Scanning",
                };
                sub = phase == ScanPhase.Discovering
                    ? $"{(prog?.Discovered ?? 0):N0} found"
                    : prog is { FilesPerSecond: > 0 } rate ? $"{counted} · {rate.FilesPerSecond:F0}/s" : counted;
                accent = _gold;
            }
            controls = CardControls.Scanning;
        }
        else if (captionsRunning)
        {
            var d = engine.DeepAnalyzeProgress;
            title = "Writing captions";
            fraction = d is { Total: > 0UL } ? Math.Clamp((double)d.Processed / d.Total, 0, 1) : null;
            sub = d is not null
                ? $"{d.Processed:N0} of {d.Total:N0} captioned"
                : engine.DeepAnalyzeStarting?.Message ?? "Loading the model";
            bar = _aiBar;
            accent = _ai;
            controls = CardControls.Rescan;
        }
        else if (facesRunning)
        {
            title = "Grouping faces";
            sub = "Comparing face prints";
            fraction = null;
            bar = _aiBar;
            accent = _ai;
            controls = CardControls.Rescan;
        }
        else if (isFailed)
        {
            title = "Scan failed";
            sub = engine.LastError?.Message ?? "Scan failed.";
            bar = _pausedBar;
            danger = true;
            showLog = true;
        }
        else if (captionsDone is { Processed: > 0UL, Cancelled: false })
        {
            title = "Everything analyzed";
            sub = $"{captionsDone.Processed:N0} files captioned";
            fraction = 1;
            bar = _doneBar;
            accent = _delight;
            controls = CardControls.Rescan;
        }
        else if (isCancelled && prog is not null)
        {
            title = "Scan cancelled";
            sub = $"{prog.Processed:N0} of {prog.Total:N0} tagged";
            fraction = prog.Total > 0 ? Math.Clamp((double)prog.Processed / prog.Total, 0, 1) : 0;
            bar = _pausedBar;
            accent = _idleStatusDefaultBrush;
        }
        else if (isCompleted)
        {
            // FEAT-2: real duration from EngineClient.LastScanDuration and the
            // engine's authoritative ScanComplete count (LastProgress can be
            // throttle-stale by up to one batch on a fast scan).
            var elapsed = engine.LastScanDuration.TotalSeconds;
            var done = Math.Max(engine.LastScanProcessedFiles, prog?.Processed ?? 0);
            title = "Scan complete";
            sub = elapsed > 0 ? $"{done:N0} files in {FormatDuration(elapsed)}" : $"{done:N0} files";
            fraction = 1;
            accent = _gold;
            controls = CardControls.Rescan;
        }
        else
        {
            var state = engine.State;
            title = _startInFlight ? "Starting…" : "Not scanned yet";
            sub = state switch
            {
                EngineClient.LifecycleState.Ready => $"{vm.FolderDisplay} is ready to scan.",
                EngineClient.LifecycleState.Starting => "Engine starting…",
                EngineClient.LifecycleState.Crashed => engine.CrashReason is string r && r.Length > 0
                    ? $"Engine crashed: {r}"
                    : "Engine crashed — try restarting the app.",
                _ => $"Engine state: {state}",
            };
            if (state == EngineClient.LifecycleState.Crashed)
            {
                danger = true;
                showLog = true;
            }
        }

        CardTitleText.Text = title;
        CardTitleText.Foreground = danger ? FailedTextBrush : _text1;
        IdleStatusText.Text = sub;
        IdleStatusText.Foreground = danger ? FailedTextBrush : _idleStatusDefaultBrush;

        var showProgress = vm.HasFolder ? Visibility.Visible : Visibility.Collapsed;
        ScanProgressBar.Visibility = showProgress;
        StepsStrip.Visibility = showProgress;
        CardPercentText.Visibility = showProgress;
        ScanProgressBar.Foreground = bar;
        if (fraction is { } f)
        {
            ScanProgressBar.IsIndeterminate = false;
            ScanProgressBar.Maximum = 1000;
            ScanProgressBar.Value = f * 1000;
            CardPercentText.Text = $"{(int)Math.Round(f * 100)}%";
        }
        else
        {
            ScanProgressBar.IsIndeterminate = true;
            CardPercentText.Text = "…";
        }
        CardPercentText.Foreground = danger ? FailedTextBrush : accent;

        IdlePanel.Visibility = controls is CardControls.Pick or CardControls.Start ? Visibility.Visible : Visibility.Collapsed;
        StartScanButton.Visibility = controls == CardControls.Start ? Visibility.Visible : Visibility.Collapsed;
        PickFolderButton.Visibility = controls == CardControls.Pick ? Visibility.Visible : Visibility.Collapsed;
        ScanningPanel.Visibility = controls == CardControls.Scanning ? Visibility.Visible : Visibility.Collapsed;
        CompletedPanel.Visibility = controls == CardControls.Rescan ? Visibility.Visible : Visibility.Collapsed;
        RescanButton.IsEnabled = !captionsRunning && !facesRunning && !_startInFlight && !EngineClient.Instance.GpuDeviceRemoved;
        OpenLogButton.Visibility = showLog ? Visibility.Visible : Visibility.Collapsed;
        StartScanText.Text = string.IsNullOrEmpty(vm.FolderDisplay) ? "Start Scan" : $"Scan {vm.FolderDisplay}";

        // FEAT-1: Pause/Resume label always reflects engine truth.
        PauseResumeText.Text = engine.IsPaused ? "Resume" : "Pause";
        PauseResumeGlyph.Glyph = engine.IsPaused ? "\uE768" : "\uE769";
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(
            PauseResumeButton, engine.IsPaused ? "Resume scan" : "Pause scan");

        SyncDetails(prog, phase, isInFlight);
    }

    private void SyncDetails(ScanProgress? prog, ScanPhase? phase, bool isInFlight)
    {
        bool hasDetails = prog is not null && phase is not null and not ScanPhase.Idle;
        DetailsToggleButton.Visibility = hasDetails ? Visibility.Visible : Visibility.Collapsed;
        DetailsPanel.Visibility = hasDetails && _detailsOpen ? Visibility.Visible : Visibility.Collapsed;
        DetailsToggleGlyph.Glyph = _detailsOpen ? "\uE70E" : "\uE70D";
        var toggleLabel = _detailsOpen ? "Hide scan details" : "Show scan details";
        ToolTipService.SetToolTip(DetailsToggleButton, toggleLabel);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(DetailsToggleButton, toggleLabel);
        if (prog is null || !hasDetails || !_detailsOpen) return;

        StatDiscovered.Text = prog.Discovered.ToString("N0");
        StatTagged.Text = prog.Processed.ToString("N0");
        StatRate.Text = prog.FilesPerSecond > 0 ? $"{prog.FilesPerSecond:F1}/s" : string.Empty;
        StatMemory.Text = prog.ResidentMb + " MB";
        StatMemory.Foreground = prog.ResidentMb > 1200 ? _memoryWarnBrush : _statDefaultBrush;
        StatAvailable.Text = prog.AvailableMb > 0 ? $"{prog.AvailableMb:N0} MB free" : string.Empty;
        StatFailures.Text = prog.Failed.ToString("N0");
        StatFailures.Foreground = prog.Failed > 0 ? FailedTextBrush : _statDefaultBrush;
        FailuresStatBorder.Visibility = prog.Failed > 0 ? Visibility.Visible : Visibility.Collapsed;

        // During discovery the total is unknowable (counting the files IS the
        // work), so never fabricate an ETA there.
        EtaText.Text = isInFlight && phase != ScanPhase.Discovering && prog.EtaSeconds is { } eta && eta > 0
            ? $"{FormatDuration(eta)} left"
            : string.Empty;

        // Screen-reader names combine label + value so each stat row
        // announces e.g. "Discovered: 1,240" rather than a bare label.
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(
            DiscoveredStatBorder, $"Discovered: {StatDiscovered.Text}");
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(
            TaggedStatBorder, $"Tagged: {StatTagged.Text}");
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(
            MemoryStatBorder, $"Memory: {StatMemory.Text}");
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(
            FailuresStatBorder, $"Failures: {StatFailures.Text}");
    }

    private int _pickInFlight;

    private async void OnPickFolderClicked(object sender, RoutedEventArgs e)
    {
        if (System.Threading.Interlocked.CompareExchange(ref _pickInFlight, 1, 0) != 0) return;
        try
        {
            var hwnd = App.HostWindow is { } window
                ? WinRT.Interop.WindowNative.GetWindowHandle(window)
                : IntPtr.Zero;
            var result = await FolderPickerService.PickFolderAsync(hwnd);
            if (result.FailureReason is not null)
            {
                await ShowAlertAsync("Couldn't open folder", result.FailureReason);
                return;
            }
            if (result.Path is not null)
            {
                AppViewModel.Instance.FolderPath = result.Path;
                DebugLog.Info($"FolderPicker: chose {PathRedactor.Redact(result.Path)}");
            }
        }
        catch (Exception ex)
        {
            DebugLog.Warn("OnPickFolderClicked threw: " + ex.Message);
        }
        finally
        {
            System.Threading.Interlocked.Exchange(ref _pickInFlight, 0);
        }
    }

    private async void OnRestartEngineClicked(object sender, RoutedEventArgs e)
    {
        RestartEngineButton.IsEnabled = false;
        try
        {
            await EngineClient.Instance.RestartAsync();
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"GPU recovery restart failed: {ex.Message}");
            await ShowAlertAsync("Engine restart failed", "Restart FileID and try again.");
        }
        finally
        {
            RestartEngineButton.IsEnabled = true;
            Sync();
        }
    }

    private static string FormatDuration(double seconds)
    {
        if (seconds < 60) return $"{seconds:F0}s";
        if (seconds < 3600) return $"{seconds / 60:F0}m";
        var hours = seconds / 3600;
        // cap pathological/garbage durations so the UI never
        // shows a four-digit hour count. Engine never legitimately
        // produces > 99h scans on supported hardware.
        if (hours > 99) return "99+ h";
        return $"{hours:F1}h";
    }

    /// <summary>open the engine's daily-rolled JSON log in the
    /// user's default .jsonl handler. Lets the user diagnose a stuck scan
    /// or surface a tagging failure without having to navigate AppData
    /// by hand.</summary>
    private void OnOpenLogClicked(object sender, RoutedEventArgs e)
    {
        try
        {
            // engine.jsonl uses tracing_appender's daily rolling pattern —
            // the actual file is named "engine.jsonl.YYYY-MM-DD". Pick the
            // newest one in the logs dir as a heuristic; fall back to the
            // logs folder itself when no logs exist.
            var logsDir = System.IO.Path.Combine(
                System.Environment.GetFolderPath(System.Environment.SpecialFolder.LocalApplicationData),
                "FileID", "logs");
            string target = logsDir;
            try
            {
                if (System.IO.Directory.Exists(logsDir))
                {
                    var newest = new System.IO.DirectoryInfo(logsDir)
                        .EnumerateFiles("engine.jsonl*")
                        .OrderByDescending(f => f.LastWriteTimeUtc)
                        .FirstOrDefault();
                    if (newest is not null) target = newest.FullName;
                }
            }
            catch { /* swallow — fall through to opening the directory */ }
            var psi = new System.Diagnostics.ProcessStartInfo
            {
                FileName = target,
                UseShellExecute = true,
            };
            System.Diagnostics.Process.Start(psi);
        }
        catch (Exception ex)
        {
            DebugLog.Warn("OnOpenLogClicked threw: " + ex.Message);
        }
    }

    // OnAutoPilotClicked removed along with the AutoPilot button —
    // see XAML comment near the deleted button. Auto-advance is now built
    // into Start Scan via EngineClient.Apply's ScanCompleteEvent handler,
    // matching the macOS "engine auto-enqueues face clustering after scan"
    // pattern.

    private async Task ShowAlertAsync(string title, string body)
    {
        // ContentDialog.ShowAsync can throw on a broken
        // XamlRoot (mid-shutdown, tab re-host). Catch + log so a failed
        // alert never escalates to App.UnhandledException.
        try
        {
            if (XamlRoot is null)
            {
                DebugLog.Warn($"ShowAlertAsync: XamlRoot is null ({title}); skipping dialog.");
                return;
            }
            var dialog = new ContentDialog
            {
                XamlRoot = XamlRoot,
                Title = title,
                Content = body,
                CloseButtonText = "OK",
                DefaultButton = ContentDialogButton.Close,
            };
            await dialog.ShowAsync();
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"ShowAlertAsync({title}) threw: " + ex.Message);
        }
    }
}
