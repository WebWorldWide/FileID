// Scan card: re-paints title, bar, sub line, counters and controls from
// EngineClient state as the pipeline advances.
//
// Stat colors: memory > 1200 MB -> orange, failures > 0 -> red.

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
    // Brushes are DispatcherObjects: cached at construction so progress events
    // (~10 Hz during a scan) do not allocate them.
    private readonly SolidColorBrush _memoryWarnBrush;
    private readonly SolidColorBrush _statDefaultBrush;
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
        // The trace line names the last subscriber called before a native fast-fail.
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
                DebugLog.Info($"[ENGINE-SUB:SidebarProcessingControl] {e.PropertyName}");
                DispatcherQueue.TryEnqueue(Sync);
            }
            if (e.PropertyName == nameof(EngineClient.LastWarning))
            {
                DebugLog.Info($"[ENGINE-SUB:SidebarProcessingControl] {e.PropertyName}");
                DispatcherQueue.TryEnqueue(SyncWarningBanner);
            }
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

    /// <summary>True from a Start Scan click until the IPC send returns, so repeated clicks cannot issue parallel startScan commands.</summary>
    private bool _startInFlight;

    private async void OnStartScanClicked(object sender, RoutedEventArgs e)
    {
        // A touch double-tap can land a second click before Sync() disables the button.
        if (_startInFlight) return;
        _startInFlight = true;
        Sync();

        // Catch-all: a disposed XamlRoot, broken dialog or engine death must not escape this async void into App.UnhandledException.
        try
        {
            var vm = AppViewModel.Instance;
            if (!vm.HasFolder)
            {
                await ShowAlertAsync("Choose a folder first",
                    "Choose a folder to scan from the folder button in the title bar.");
                return;
            }

            // 30 s covers a slow HDD, where the first Ready can land at 20-25 s, while still failing fast on a dead engine.
            if (EngineClient.Instance.State != EngineClient.LifecycleState.Ready)
            {
                IdleStatusText.Text = "Getting ready…";
                StartScanButton.IsEnabled = false;
                try
                {
                    await EngineClient.Instance.WaitForReadyAsync(System.TimeSpan.FromSeconds(30));
                }
                catch (System.Exception ex)
                {
                    DebugLog.Warn("StartScan: WaitForReadyAsync threw: " + ex.Message);
                    await ShowAlertAsync("FileID isn't ready yet",
                        "FileID is still getting ready. Try again in a moment.\n\n" +
                        "If this keeps happening, quit and reopen FileID. Details are in the engine log at " +
                        "%LOCALAPPDATA%\\FileID\\logs\\engine.jsonl.");
                    DispatcherQueue.TryEnqueue(Sync);
                    return;
                }
                finally
                {
                    DispatcherQueue.TryEnqueue(Sync);
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
                    "FileID is still getting ready. Try again in a moment.");
            }
        }
        catch (Exception ex)
        {
            DebugLog.Error("OnStartScanClicked unexpected exception: " + ex);
        }
        finally
        {
            // Cleared only after the IPC send returns; Sync() then reflects the new state without waiting for the next PropertyChanged.
            _startInFlight = false;
            try { Sync(); } catch { /* swallow */ }
        }
    }

    private async void OnPauseResumeClicked(object sender, RoutedEventArgs e)
    {
        try
        {
            // IsPaused, not the visible text, which desyncs if the engine emits an unrelated phase update before the IPC reply.
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
        WarningBannerText.Text = EngineErrorText.Text(warning.Kind, warning.Message);
        WarningBanner.Visibility = Visibility.Visible;
    }

    private void OnNextStepClicked(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnNextStepClicked), () => AppViewModel.Instance.ActiveTab = SidebarTab.DeepAnalyze);

    private void OnDismissWarningClicked(object sender, RoutedEventArgs e)
    {
        EngineClient.Instance.LastWarning = null;
    }

    /// <summary>Latched once a scan's file Total is known so a momentary Total==0 does not flip the bar back to indeterminate.</summary>
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

        if (!isInFlight) _scanTotalKnown = false;

        // Enabled on HasFolder alone; the click handler waits for Ready with visible feedback.
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
        bool retry = false;
        bool nextStep = false;
        bool celebrate = false;
        var controls = CardControls.Start;

        if (!vm.HasFolder)
        {
            title = "Start with a folder";
            sub = "Or drag a folder onto this window.";
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
                // Still discovering: sweep. A transient Total==0 keeps the determinate bar.
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
                    ScanPhase.PostScan => "Grouping faces",
                    _ => "Scanning",
                };
                var left = prog is { EtaSeconds: { } eta } && eta > 0 ? $" · about {FormatDuration(eta)} left" : string.Empty;
                sub = phase switch
                {
                    ScanPhase.Discovering => $"{(prog?.Discovered ?? 0):N0} files found so far",
                    ScanPhase.PostScan => "Finding the same person across photos",
                    _ => counted + left,
                };
                accent = _gold;
            }
            controls = CardControls.Scanning;
        }
        else if (captionsRunning)
        {
            var d = engine.DeepAnalyzeProgress;
            title = "Describing files";
            fraction = d is { Total: > 0UL } ? Math.Clamp((double)d.Processed / d.Total, 0, 1) : null;
            sub = d is not null
                ? $"{d.Processed:N0} of {d.Total:N0} files described"
                : engine.DeepAnalyzeStarting?.Message ?? "Loading the model";
            bar = _aiBar;
            accent = _ai;
            controls = CardControls.Rescan;
        }
        else if (facesRunning)
        {
            title = "Grouping faces";
            sub = "Finding the same person across photos";
            fraction = null;
            bar = _aiBar;
            accent = _ai;
            controls = CardControls.Rescan;
        }
        else if (isFailed)
        {
            title = "Scan failed";
            sub = "Try Scan again. Details are in the engine log.";
            bar = _pausedBar;
            danger = true;
            showLog = true;
            retry = true;
        }
        else if (captionsDone is { Processed: > 0UL, Cancelled: false })
        {
            title = "All done";
            sub = $"{captionsDone.Processed:N0} files described";
            fraction = 1;
            bar = _doneBar;
            accent = _delight;
            controls = CardControls.Rescan;
        }
        else if (isCancelled && prog is not null)
        {
            title = "Scan stopped";
            sub = $"{prog.Processed:N0} of {prog.Total:N0} files read. Scan again to finish; finished files are skipped.";
            fraction = prog.Total > 0 ? Math.Clamp((double)prog.Processed / prog.Total, 0, 1) : 0;
            bar = _pausedBar;
            accent = _idleStatusDefaultBrush;
            controls = CardControls.Rescan;
            retry = true;
        }
        else if (isCompleted)
        {
            // LastProgress can be throttle-stale by up to one batch, so prefer the engine's ScanComplete count.
            var done = Math.Max(engine.LastScanProcessedFiles, prog?.Processed ?? 0);
            title = "Scan complete";
            sub = $"{done:N0} files ready to browse";
            fraction = 1;
            bar = _doneBar;
            accent = _delight;
            celebrate = true;
            controls = CardControls.Rescan;
            nextStep = !captionsRunning && !facesRunning && (captionsDone?.Processed ?? 0) == 0 && done > 0;
        }
        else
        {
            var state = engine.State;
            title = _startInFlight ? "Starting…" : "Ready to scan";
            sub = state switch
            {
                EngineClient.LifecycleState.Ready => $"FileID reads every file in {vm.FolderDisplay}. Nothing is moved or changed.",
                EngineClient.LifecycleState.Starting => "Getting ready…",
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
        CardTitleText.Foreground = danger ? FailedTextBrush : celebrate ? _delight : _text1;
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
            if (f <= 0) CardPercentText.Visibility = Visibility.Collapsed;
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
        NextStepButton.Visibility = nextStep ? Visibility.Visible : Visibility.Collapsed;
        StartScanText.Text = retry ? "Scan again" : string.IsNullOrEmpty(vm.FolderDisplay) ? "Scan" : $"Scan {vm.FolderDisplay}";
        RescanText.Text = retry ? "Scan again" : "Rescan";
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(RescanButton, RescanText.Text);
        ToolTipService.SetToolTip(RescanButton, captionsRunning || facesRunning
            ? "Wait for the current work to finish, then rescan."
            : $"Look for new or changed files in {vm.FolderDisplay}. Files already read are skipped. (Ctrl+R)");

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
        StatRate.Text = prog.FilesPerSecond > 0 ? $"{prog.FilesPerSecond:F0} files a second" : string.Empty;
        StatMemory.Text = prog.ResidentMb + " MB";
        StatMemory.Foreground = prog.ResidentMb > 1200 ? _memoryWarnBrush : _statDefaultBrush;
        StatAvailable.Text = prog.AvailableMb > 0 ? $"{prog.AvailableMb:N0} MB free" : string.Empty;
        StatFailures.Text = prog.Failed.ToString("N0");
        StatFailures.Foreground = prog.Failed > 0 ? FailedTextBrush : _statDefaultBrush;
        FailuresStatBorder.Visibility = prog.Failed > 0 ? Visibility.Visible : Visibility.Collapsed;

        // No ETA during discovery: counting the files is the work.
        EtaText.Text = isInFlight && phase != ScanPhase.Discovering && prog.EtaSeconds is { } eta && eta > 0
            ? $"{FormatDuration(eta)} left"
            : string.Empty;

        // Screen-reader names combine label and value.
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(
            DiscoveredStatBorder, $"Discovered: {StatDiscovered.Text}");
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(
            TaggedStatBorder, $"Read: {StatTagged.Text}");
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
            var result = await FolderPickerService.PickFolderAsync(hwnd, FolderPickerService.ScanCommitText);
            if (result.FailureReason is not null)
            {
                await ShowAlertAsync("Couldn't open folder", result.FailureReason);
                return;
            }
            if (result.Path is not null)
            {
                DebugLog.Info($"FolderPicker: chose {PathRedactor.Redact(result.Path)}");
                await AppViewModel.Instance.ChooseFolderAsync(result.Path);
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
        // Cap garbage durations so the UI never shows a four-digit hour count.
        if (hours > 99) return "99+ h";
        return $"{hours:F1}h";
    }

    private void OnOpenLogClicked(object sender, RoutedEventArgs e)
    {
        try
        {
            // The daily log "engine.jsonl.YYYY-MM-DD" has no registered handler, so reveal the newest one in Explorer instead of launching it.
            var logsDir = AppPaths.LogsDir;
            string? newest = null;
            try
            {
                if (System.IO.Directory.Exists(logsDir))
                {
                    newest = new System.IO.DirectoryInfo(logsDir)
                        .EnumerateFiles("engine.jsonl*")
                        .OrderByDescending(f => f.LastWriteTimeUtc)
                        .FirstOrDefault()?.FullName;
                }
            }
            catch { /* fall through to opening the folder */ }
            if (newest is not null)
            {
                SafeOpen.Reveal(newest);
                return;
            }
            System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo
            {
                FileName = logsDir,
                UseShellExecute = true,
            });
        }
        catch (Exception ex)
        {
            DebugLog.Warn("OnOpenLogClicked threw: " + ex.Message);
        }
    }

    private async Task ShowAlertAsync(string title, string body)
    {
        // ContentDialog.ShowAsync can throw on a broken XamlRoot (mid-shutdown, tab re-host).
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
