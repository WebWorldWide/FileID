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
        Loaded += (_, _) => Sync();
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
                          or nameof(EngineClient.LastScanDuration))
        {
            DispatcherQueue.TryEnqueue(Sync);
        }
    }


    private void OnAppChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName is nameof(AppViewModel.HasFolder))
        {
            DispatcherQueue.TryEnqueue(Sync);
        }
    }

    private async void OnStartScanClicked(object sender, RoutedEventArgs e)
    {
        var vm = AppViewModel.Instance;
        if (!vm.HasFolder)
        {
            await ShowAlertAsync("Pick a folder first",
                "FileID needs a folder to scan. Use the picker at the top of the sidebar.");
            return;
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

    private void Sync()
    {
        var prog = EngineClient.Instance.LastProgress;
        var phase = EngineClient.Instance.Phase ?? prog?.Phase;

        bool isInFlight = phase is ScanPhase.Discovering or ScanPhase.Tagging or ScanPhase.PostScan;
        bool isCompleted = phase is ScanPhase.Completed;

        IdlePanel.Visibility = (!isInFlight && !isCompleted) ? Visibility.Visible : Visibility.Collapsed;
        ScanningPanel.Visibility = isInFlight ? Visibility.Visible : Visibility.Collapsed;
        CompletedPanel.Visibility = isCompleted ? Visibility.Visible : Visibility.Collapsed;

        StartScanButton.IsEnabled = AppViewModel.Instance.HasFolder
                                  && EngineClient.Instance.State == EngineClient.LifecycleState.Ready;

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
            IdleStatusText.Text = "Ready when you are.";
        }
    }

    private static string FormatDuration(double seconds)
    {
        if (seconds < 60) return $"{seconds:F0}s";
        if (seconds < 3600) return $"{seconds / 60:F0}m";
        return $"{seconds / 3600:F1}h";
    }

    private async Task ShowAlertAsync(string title, string body)
    {
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
}
