// SidebarProcessingControl code-behind. Subscribes to EngineClient state
// and re-paints the panel as scan phase advances.
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
    public SidebarProcessingControl()
    {
        InitializeComponent();
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
    {
        if (e.PropertyName is nameof(EngineClient.LastProgress)
                          or nameof(EngineClient.Phase)
                          or nameof(EngineClient.State)
                          or nameof(EngineClient.IsPaused)
                          or nameof(EngineClient.LastScanDuration)
                          or nameof(EngineClient.LastError))
        {
            DispatcherQueue.TryEnqueue(Sync);
        }
        if (e.PropertyName == nameof(EngineClient.LastWarning))
        {
            DispatcherQueue.TryEnqueue(SyncWarningBanner);
        }
        // V14.9-D8: per-batch completion ripple. Engine emits one
        // BatchSummary per ~100 files (or every 200 ms). Each new
        // BatchSummary object change triggers a gold ring pulse on
        // the "Tagged" stat — visual feedback that batches are landing.
        if (e.PropertyName == nameof(EngineClient.LastBatch))
        {
            DispatcherQueue.TryEnqueue(() =>
            {
                if (TaggedStatBorder != null && EngineClient.Instance.LastBatch is { } batch)
                {
                    FileID.Theme.Motion.CompletionRipple.SetTrigger(TaggedStatBorder, batch);
                }
            });
        }
    }


    private void OnAppChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName is nameof(AppViewModel.HasFolder))
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

    private async void OnStartScanClicked(object sender, RoutedEventArgs e)
    {
        // F2d (V14.8.3): wrap the whole async void body in a catch-all so a
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
                    "FileID needs a folder to scan. Use the picker at the top of the sidebar.");
                return;
            }

            // V14.9-F-A1: when the engine isn't Ready yet, instead of
            // silently no-op'ing (the prior "Start Scan does nothing"
            // symptom) WAIT for Ready up to 15 s with visible inline
            // feedback. A user-driven retry queue is friendlier than
            // a disabled button that gives no hint about why.
            if (EngineClient.Instance.State != EngineClient.LifecycleState.Ready)
            {
                IdleStatusText.Text = $"Waiting for engine ({EngineClient.Instance.State})…";
                StartScanButton.IsEnabled = false;
                try
                {
                    await EngineClient.Instance.WaitForReadyAsync(System.TimeSpan.FromSeconds(15));
                }
                catch (System.Exception ex)
                {
                    DebugLog.Warn("StartScan: WaitForReadyAsync threw: " + ex.Message);
                    await ShowAlertAsync("Engine isn't ready",
                        $"FileID's engine hasn't reported ready (status: {EngineClient.Instance.State}).\n\n" +
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
            // CPU. Performance Packs were removed in V14.8.2 — DirectML on
            // every D3D12-capable GPU is now the canonical path, so we no
            // longer prompt users to install a pack that doesn't exist.
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
                // Optimistic UI flip: switch into the scanning panel immediately
                // so the user gets visible feedback on click. The engine's
                // first PhaseChanged(Discovering) event echoes the same value
                // (no-op); any later transition (Tagging, Failed, Completed)
                // overwrites this. If StartScanAsync faults (engine not Ready),
                // the catch block surfaces an alert and the failure pill takes
                // over via the Sync() Failed branch.
                EngineClient.Instance.SetOptimisticScanningPhase();
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

        // CPU branch — the only state we still warn about. Performance Packs
        // were removed in V14.8.2; DirectML is the universal GPU path now,
        // so a non-CPU EP is always considered acceptable.
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
                XamlRoot = this.XamlRoot,
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
                ContentDialogResult.Primary   => PerformancePromptResult.OpenSettings,
                ContentDialogResult.Secondary => PerformancePromptResult.Continue,
                _                              => PerformancePromptResult.Cancel,
            };
        }
        // No "Open Settings" affordance: Primary == Continue, Secondary == Cancel.
        return result switch
        {
            ContentDialogResult.Primary   => PerformancePromptResult.Continue,
            _                              => PerformancePromptResult.Cancel,
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

    private void Sync()
    {
        var prog = EngineClient.Instance.LastProgress;
        var phase = EngineClient.Instance.Phase ?? prog?.Phase;

        bool isInFlight = phase is ScanPhase.Discovering or ScanPhase.Tagging or ScanPhase.PostScan;
        bool isCompleted = phase is ScanPhase.Completed;
        bool isFailed = phase is ScanPhase.Failed;

        // Failed: surface the engine error in the idle pill in red. Without
        // this branch a scan failure (e.g. missing model files) reported via
        // PhaseChanged(Failed) + Error(model_load_failed) falls through to
        // the default idle text and looks like the click did nothing.
        if (isFailed)
        {
            IdlePanel.Visibility = Visibility.Visible;
            ScanningPanel.Visibility = Visibility.Collapsed;
            CompletedPanel.Visibility = Visibility.Collapsed;
            var err = EngineClient.Instance.LastError;
            IdleStatusText.Text = err?.Message ?? "Scan failed.";
            IdleStatusText.Foreground = FailedTextBrush;
            // V14.9-F-A1: Start Scan is enabled on HasFolder alone; the
            // click handler waits for Ready with visible feedback.
            // Exception: a Crashed engine — there's nothing to wait for.
            StartScanButton.IsEnabled = AppViewModel.Instance.HasFolder
                                      && EngineClient.Instance.State != EngineClient.LifecycleState.Crashed;
            return;
        }

        IdlePanel.Visibility = (!isInFlight && !isCompleted) ? Visibility.Visible : Visibility.Collapsed;
        ScanningPanel.Visibility = isInFlight ? Visibility.Visible : Visibility.Collapsed;
        CompletedPanel.Visibility = isCompleted ? Visibility.Visible : Visibility.Collapsed;

        // Reset foreground so a successful follow-up scan doesn't keep the
        // red text from the previous failed attempt.
        IdleStatusText.Foreground = (Brush)Application.Current.Resources["TextFillColorSecondaryBrush"];

        // V14.9-F-A1: enable on HasFolder alone. The previous version also
        // required `EngineClient.State == Ready`, which made the button
        // silently grey for users whose engine took longer than usual to
        // spawn — they reported "I click Start scan, nothing happens."
        // OnStartScanClicked now awaits Ready up to 15 s with inline
        // feedback. Crashed is the one state the click can't recover from,
        // so we still disable for that.
        StartScanButton.IsEnabled = AppViewModel.Instance.HasFolder
                                  && EngineClient.Instance.State != EngineClient.LifecycleState.Crashed;

        // FEAT-1: Pause/Resume label always reflects engine truth.
        PauseResumeText.Text = EngineClient.Instance.IsPaused ? "Resume" : "Pause";

        if (isInFlight && prog is not null)
        {
            PhaseText.Text = phase switch
            {
                ScanPhase.Discovering => "Discovering files...",
                ScanPhase.Tagging     => "Tagging files...",
                ScanPhase.PostScan    => "Wrapping up...",
                _                      => "Working...",
            };
            // V14.7.6: glyphs were empty strings from a prior cp1252 round-trip
            // that ate the PUA chars. Use Unicode escapes (encoding-bulletproof):
            //   E721 = Search (Discovering)
            //   E8B7 = TagGroup / labels (Tagging)
            //   E895 = OEM (Wrapping up)
            //   E8FB = AcceptMedium (default Working)
            PhaseIcon.Glyph = phase switch
            {
                ScanPhase.Discovering => "",
                ScanPhase.Tagging     => "",
                ScanPhase.PostScan    => "",
                _                      => "",
            };

            if (prog.Total > 0)
            {
                ScanProgressBar.Maximum = prog.Total;
                ScanProgressBar.Value = prog.Processed;
                ScanProgressBar.IsIndeterminate = false;
            }
            else
            {
                ScanProgressBar.IsIndeterminate = true;
            }

            StatDiscovered.Text = prog.Discovered.ToString("N0");
            StatTagged.Text = prog.Processed.ToString("N0");
            StatMemory.Text = prog.ResidentMb + " MB";
            StatMemory.Foreground = prog.ResidentMb > 1200
                ? new SolidColorBrush(Color.FromArgb(0xFF, 0xFF, 0x99, 0x00))   // orange
                : new SolidColorBrush(Color.FromArgb(0xFF, 0xFF, 0xFF, 0xFF));   // default
            StatFailures.Text = prog.Failed.ToString("N0");
            StatFailures.Foreground = prog.Failed > 0
                ? new SolidColorBrush(Color.FromArgb(0xFF, 0xFF, 0x6B, 0x6B))    // red
                : new SolidColorBrush(Color.FromArgb(0xFF, 0xFF, 0xFF, 0xFF));

            EtaText.Text = prog.EtaSeconds is { } eta && eta > 0
                ? "ETA: " + FormatDuration(eta)
                : "ETA: computing...";
        }
        else if (isCompleted && prog is not null)
        {
            // FEAT-2: real duration from EngineClient.LastScanDuration
            // (tracked from StartScanAsync to ScanCompleteEvent). The
            // previous version showed "in 0s" because of a placeholder
            // typo `prog.Total > 0 ? 0 : 0`.
            var elapsed = EngineClient.Instance.LastScanDuration.TotalSeconds;
            CompletedSummary.Text = elapsed > 0
                ? $"Scan complete -- {prog.Processed:N0} files in {FormatDuration(elapsed)}."
                : $"Scan complete -- {prog.Processed:N0} files.";
        }
        else if (!AppViewModel.Instance.HasFolder)
        {
            IdleStatusText.Text = "Pick a folder above to begin.";
        }
        else
        {
            // V14.9-F-A1: surface engine state so the user knows why
            // Start Scan is/isn't immediately responsive.
            var state = EngineClient.Instance.State;
            IdleStatusText.Text = state switch
            {
                EngineClient.LifecycleState.Ready    => "Ready when you are.",
                EngineClient.LifecycleState.Starting => "Engine starting…",
                EngineClient.LifecycleState.Crashed  => EngineClient.Instance.CrashReason is string r && r.Length > 0
                    ? $"Engine crashed: {r}"
                    : "Engine crashed — try restarting the app.",
                _                                     => $"Engine state: {state}",
            };
            if (state == EngineClient.LifecycleState.Crashed)
            {
                IdleStatusText.Foreground = FailedTextBrush;
            }
        }
    }

    private static string FormatDuration(double seconds)
    {
        if (seconds < 60) return $"{seconds:F0}s";
        if (seconds < 3600) return $"{seconds / 60:F0}m";
        var hours = seconds / 3600;
        // V14.9-E6: cap pathological/garbage durations so the UI never
        // shows a four-digit hour count. Engine never legitimately
        // produces > 99h scans on supported hardware.
        if (hours > 99) return "99+ h";
        return $"{hours:F1}h";
    }

    // V14.9-C6: AutoPilot trigger. Mirrors the macOS chain of
    // scan → cluster → caption → plan, all on-device. RunAutoPilotAsync
    // advances EngineClient.CurrentAutoPilotStage, which drives the
    // tracker dots above this control.
    /// <summary>V14.9-H: open the engine's daily-rolled JSON log in the
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

    private async void OnAutoPilotClicked(object sender, RoutedEventArgs e)
    {
        try
        {
            var vm = AppViewModel.Instance;
            if (!vm.HasFolder)
            {
                await ShowAlertAsync("Pick a folder first",
                    "AutoPilot needs a folder to scan. Use the picker at the top of the sidebar.");
                return;
            }
            try
            {
                AutoPilotButton.IsEnabled = false;
                StartScanButton.IsEnabled = false;
                await EngineClient.Instance.RunAutoPilotAsync(vm.FolderPath!, vm.FolderDisplay);
            }
            catch (Exception ex)
            {
                DebugLog.Error("AutoPilot failed: " + ex.Message);
                await ShowAlertAsync("AutoPilot stopped",
                    "AutoPilot didn't complete: " + ex.Message);
            }
            finally
            {
                AutoPilotButton.IsEnabled = true;
                StartScanButton.IsEnabled = true;
            }
        }
        catch (Exception ex)
        {
            DebugLog.Error("OnAutoPilotClicked unexpected: " + ex);
        }
    }

    private async Task ShowAlertAsync(string title, string body)
    {
        // V14.9-A8: ContentDialog.ShowAsync can throw on a broken
        // XamlRoot (mid-shutdown, tab re-host). Catch + log so a failed
        // alert never escalates to App.UnhandledException.
        try
        {
            if (this.XamlRoot is null)
            {
                DebugLog.Warn($"ShowAlertAsync: XamlRoot is null ({title}); skipping dialog.");
                return;
            }
            var dialog = new ContentDialog
            {
                XamlRoot = this.XamlRoot,
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
