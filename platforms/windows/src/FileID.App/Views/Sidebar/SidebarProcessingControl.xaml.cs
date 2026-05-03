// SidebarProcessingControl code-behind. Subscribes to EngineClient state
// and re-paints the panel as scan phase advances.
//
// Stat color thresholds (matches macOS):
//   memory  > 1200 MB → orange
//   failures > 0      → red

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
    }

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName is nameof(EngineClient.LastProgress)
                          or nameof(EngineClient.Phase)
                          or nameof(EngineClient.State))
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
        var phase = EngineClient.Instance.LastProgress?.Phase;
        try
        {
            // Pause/Resume detection: macOS uses a separate IsPaused flag in
            // ScanProgress; for our schema, we just toggle and let the engine
            // ignore irrelevant state. Phase 2 wires a proper IsPaused.
            if (PauseResumeText.Text == "Pause")
            {
                await EngineClient.Instance.PauseScanAsync();
                PauseResumeText.Text = "Resume";
            }
            else
            {
                await EngineClient.Instance.ResumeScanAsync();
                PauseResumeText.Text = "Pause";
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
            PauseResumeText.Text = "Pause";
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

        if (isInFlight && prog is not null)
        {
            PhaseText.Text = phase switch
            {
                ScanPhase.Discovering => "Discovering files…",
                ScanPhase.Tagging     => "Tagging files…",
                ScanPhase.PostScan    => "Wrapping up…",
                _                      => "Working…",
            };
            PhaseIcon.Glyph = phase switch
            {
                ScanPhase.Discovering => "",
                ScanPhase.Tagging     => "",
                ScanPhase.PostScan    => "",
                _                      => "",
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
                : "ETA: computing…";
        }
        else if (isCompleted && prog is not null)
        {
            CompletedSummary.Text = $"Scan complete — {prog.Processed:N0} files in {FormatDuration(prog.Total > 0 ? 0 : 0)}.";
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
