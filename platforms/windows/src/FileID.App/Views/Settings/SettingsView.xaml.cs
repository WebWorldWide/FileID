// SettingsView code-behind.

using System;
using System.ComponentModel;
using System.Diagnostics;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FileID.Views.Settings;

public sealed partial class SettingsView : UserControl, INotifyPropertyChanged
{
    /// <summary>
    /// Set true while we hydrate the toggles from AppSettings on Loaded.
    /// Prevents the Toggled handlers from re-saving the value we just read.
    /// </summary>
    private bool _initializingToggles;

    public SettingsView()
    {
        InitializeComponent();
        EngineClient.Instance.PropertyChanged += (_, e) =>
        {
            if (e.PropertyName is nameof(EngineClient.Info)
                or nameof(EngineClient.State))
            {
                OnPropertyChanged(nameof(EngineVersionText));
                OnPropertyChanged(nameof(WorkerCapText));
                OnPropertyChanged(nameof(GpuSummaryText));
                OnPropertyChanged(nameof(ExecutionProviderText));
                OnPropertyChanged(nameof(RecommendationText));
                OnPropertyChanged(nameof(RecommendationVisibility));
            }
        };
        Loaded += (_, _) => HydrateToggles();
    }

    private void HydrateToggles()
    {
        _initializingToggles = true;
        try
        {
            var s = AppViewModel.Instance.Settings;
            HideUnknownToggle.IsOn = s.PeopleHideUnknown;
            CleanupAutoTagToggle.IsOn = s.CleanupAutoTagKept;
            RestructureTreeModeToggle.IsOn = s.RestructureTreeMode;

            // Hydrate the EP override picker too.
            string current = s.GpuExecutionProviderOverride ?? "auto";
            for (int i = 0; i < ProviderCombo.Items.Count; i++)
            {
                if (ProviderCombo.Items[i] is ComboBoxItem item && item.Tag is string tag && tag == current)
                {
                    ProviderCombo.SelectedIndex = i;
                    break;
                }
            }
        }
        finally
        {
            _initializingToggles = false;
        }
    }

    private void OnHideUnknownToggled(object sender, RoutedEventArgs e)
    {
        if (_initializingToggles) return;
        var s = AppViewModel.Instance.Settings;
        s.PeopleHideUnknown = HideUnknownToggle.IsOn;
        s.Save();
    }

    private void OnCleanupAutoTagToggled(object sender, RoutedEventArgs e)
    {
        if (_initializingToggles) return;
        var s = AppViewModel.Instance.Settings;
        s.CleanupAutoTagKept = CleanupAutoTagToggle.IsOn;
        s.Save();
    }

    private void OnRestructureTreeModeToggled(object sender, RoutedEventArgs e)
    {
        if (_initializingToggles) return;
        var s = AppViewModel.Instance.Settings;
        s.RestructureTreeMode = RestructureTreeModeToggle.IsOn;
        s.Save();
    }

    private void OnProviderOverrideChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_initializingToggles) return;
        if (ProviderCombo.SelectedItem is not ComboBoxItem item || item.Tag is not string tag) return;
        var s = AppViewModel.Instance.Settings;
        s.GpuExecutionProviderOverride = (tag == "auto") ? null : tag;
        s.Save();
        // V14.x note: send a setExecutionProvider IPC here once the
        // engine has a real consumer. For now the override sits in
        // settings.json and the engine ignores it.
    }

    public string EngineVersionText
    {
        get
        {
            var info = EngineClient.Instance.Info;
            return info is null
                ? "Engine starting…"
                : $"Engine v{info.Version} · PID {info.Pid}";
        }
    }

    public string WorkerCapText
    {
        get
        {
            var info = EngineClient.Instance.Info;
            if (info is null) return "Worker pool — pending";
            var cores = info.Hardware?.PhysicalCpuCores;
            var coreText = cores.HasValue ? $" · {cores.Value} physical cores" : string.Empty;
            return $"Worker cap: {info.WorkerCap}{coreText} · {info.PhysicalMemoryGB:0.#} GB RAM";
        }
    }

    public string GpuSummaryText
    {
        get
        {
            var hw = EngineClient.Instance.Info?.Hardware;
            if (hw is null) return "GPU detection pending…";
            var vendor = hw.GpuVendor switch
            {
                "nvidia"   => "NVIDIA",
                "amd"      => "AMD",
                "intel"    => "Intel",
                "qualcomm" => "Qualcomm Snapdragon",
                "none"     => "No discrete GPU detected",
                _          => "Other / generic GPU",
            };
            return string.IsNullOrEmpty(hw.AdapterName)
                ? vendor
                : $"{vendor} · {hw.AdapterName}";
        }
    }

    public string ExecutionProviderText
    {
        get
        {
            var hw = EngineClient.Instance.Info?.Hardware;
            if (hw is null) return string.Empty;
            return hw.ExecutionProvider switch
            {
                "cuda"     => "CUDA — NVIDIA-tuned (highest perf on RTX class)",
                "tensorrt" => "TensorRT — NVIDIA optimized graph compilation",
                "directml" => "DirectML — works on every Windows GPU vendor",
                "openvino" => "OpenVINO — Intel-tuned",
                "qnn"      => "QNN — Snapdragon Hexagon NPU (most power-efficient on WoA)",
                "cpu"      => "CPU — AVX2 / NEON SIMD",
                _          => hw.ExecutionProvider,
            };
        }
    }

    public string RecommendationText =>
        EngineClient.Instance.Info?.Hardware?.Recommendation ?? string.Empty;

    public Visibility RecommendationVisibility =>
        string.IsNullOrEmpty(RecommendationText) ? Visibility.Collapsed : Visibility.Visible;

    public string AppVersionText
    {
        get
        {
            var version = typeof(SettingsView).Assembly.GetName().Version?.ToString(3) ?? "0.0.0";
            return $"FileID for Windows · v{version}";
        }
    }

    private void OnOpenLogsClicked(object sender, RoutedEventArgs e)
    {
        try
        {
            Process.Start(new ProcessStartInfo
            {
                FileName = "explorer.exe",
                Arguments = $"\"{AppPaths.LogsDir}\"",
                UseShellExecute = true,
            });
        }
        catch
        {
            // Ignore — Settings tab is a low-stakes UI; failure to open
            // Explorer doesn't warrant an error toast.
        }
    }

    private async void OnRecentScansClicked(object sender, RoutedEventArgs e)
    {
        try
        {
            var sheet = new RecentScansSheet();
            var dialog = new ContentDialog
            {
                XamlRoot = this.XamlRoot,
                Title = "Recent scans",
                Content = sheet,
                CloseButtonText = "Done",
                DefaultButton = ContentDialogButton.Close,
            };
            await dialog.ShowAsync();
        }
        catch (Exception ex)
        {
            DebugLog.Warn("RecentScansSheet open failed: " + ex);
        }
    }

    private async void OnVerifyPrivacyClicked(object sender, RoutedEventArgs e)
    {
        try
        {
            var hits = await PrivacyGrep.RunAsync();
            var dialog = new ContentDialog
            {
                XamlRoot = this.XamlRoot,
                Title = hits.Count == 0 ? "Privacy verified" : "Suspicious strings found",
                Content = hits.Count == 0
                    ? "Scanned the engine binary for telemetry markers (Sentry, AppInsights, Firebase, Segment, Mixpanel, Google Analytics, Amplitude, AppCenter). Zero hits — your engine is telemetry-clean."
                    : "Found these markers in the engine binary:\n\n" + string.Join("\n", hits),
                CloseButtonText = "OK",
                DefaultButton = ContentDialogButton.Close,
            };
            await dialog.ShowAsync();
        }
        catch (Exception ex)
        {
            DebugLog.Warn("PrivacyGrep failed: " + ex);
        }
    }

    /// <summary>FEAT-CRIT-4: install one of the local-AI models from the
    /// new Settings → Local AI cards. Shares the same engine prewarm path
    /// as the welcome sheet — the engine downloads + verifies SHA, the
    /// app surfaces a progress bar via ModelInstallerService.</summary>
    private async void OnInstallModelClicked(object sender, RoutedEventArgs e)
    {
        if (sender is not Button button || button.Tag is not string modelKind || string.IsNullOrWhiteSpace(modelKind))
            return;
        var (statusText, progressBar, progressProp) = modelKind switch
        {
            "arcface_buffalo" => (ArcFaceStatusText, ArcFaceProgress, nameof(Services.ModelInstallerService.ArcfaceProgress)),
            "mobileclip_s2"   => (ClipStatusText,    ClipProgress,    nameof(Services.ModelInstallerService.ClipProgress)),
            _ => (null!, null!, string.Empty),
        };
        if (statusText is null) return;
        var originalContent = button.Content;
        button.IsEnabled = false;
        button.Content = "Installing…";
        progressBar.Visibility = Visibility.Visible;

        // Subscribe to ModelInstallerService.<*Progress> for live updates.
        void OnProgress(object? _, System.ComponentModel.PropertyChangedEventArgs ev)
        {
            if (ev.PropertyName != progressProp) return;
            var pct = modelKind switch
            {
                "arcface_buffalo" => Services.ModelInstallerService.Instance.ArcfaceProgress,
                "mobileclip_s2"   => Services.ModelInstallerService.Instance.ClipProgress,
                _ => 0.0,
            };
            DispatcherQueue.TryEnqueue(() =>
            {
                progressBar.Value = pct;
                statusText.Text = pct > 0 && pct < 1
                    ? $"Downloading… {pct * 100:0}%"
                    : (pct >= 1 ? "Installed" : statusText.Text);
            });
        }
        Services.ModelInstallerService.Instance.PropertyChanged += OnProgress;
        try
        {
            await EngineClient.Instance.PrewarmModelAsync(modelKind);
            button.Content = "Installed";
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"Model install '{modelKind}' failed: {ex}");
            button.Content = originalContent;
            button.IsEnabled = true;
            statusText.Text = $"Failed: {ex.Message}";
        }
        finally
        {
            Services.ModelInstallerService.Instance.PropertyChanged -= OnProgress;
            progressBar.Visibility = Visibility.Collapsed;
        }
    }

    private async void OnInstallPackClicked(object sender, RoutedEventArgs e)
    {
        if (sender is not Button button || button.Tag is not string packId || string.IsNullOrWhiteSpace(packId))
            return;
        var originalContent = button.Content;
        try
        {
            button.IsEnabled = false;
            button.Content = "Installing…";
            await EngineClient.Instance.PrewarmModelAsync(packId);
            button.Content = "Installed";
            try
            {
                var dialog = new ContentDialog
                {
                    XamlRoot = this.XamlRoot,
                    Title = "Performance Pack installed",
                    Content = "Restart the engine to start using the new execution provider. Sidebar → Engine → Restart, or just relaunch FileID.",
                    CloseButtonText = "OK",
                    DefaultButton = ContentDialogButton.Close,
                };
                await dialog.ShowAsync();
            }
            catch { /* dialog show is best-effort */ }
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"Pack install '{packId}' failed: {ex}");
            button.Content = originalContent;
            button.IsEnabled = true;
            try
            {
                var dialog = new ContentDialog
                {
                    XamlRoot = this.XamlRoot,
                    Title = "Pack install failed",
                    Content = ex.Message,
                    CloseButtonText = "OK",
                    DefaultButton = ContentDialogButton.Close,
                };
                await dialog.ShowAsync();
            }
            catch { }
        }
    }

    public event PropertyChangedEventHandler? PropertyChanged;
    private void OnPropertyChanged(string name)
        => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}
