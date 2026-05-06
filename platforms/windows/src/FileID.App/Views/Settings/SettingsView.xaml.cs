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
        EngineClient.Instance.PropertyChanged += OnEngineChanged;
        Unloaded += (_, _) => EngineClient.Instance.PropertyChanged -= OnEngineChanged;
        Loaded += (_, _) => HydrateToggles();
    }

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
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
        // The engine reads this value at startup via runtime.rs's
        // read_user_ep_override(); to apply a change live, the user
        // must restart the engine (the Restart button in this view, or
        // the prompt shown after installing a Performance Pack).
    }

    public string EngineVersionText
    {
        get
        {
            // V14.7.6: Info can be null even when State==Ready in the brief
            // window between spawn + first ready event. Surface State so
            // the Settings card stops claiming "Engine starting..." for
            // a Ready engine.
            var ec = EngineClient.Instance;
            var info = ec.Info;
            if (info is not null) return $"Engine v{info.Version} - PID {info.Pid}";
            return ec.State switch
            {
                EngineClient.LifecycleState.Ready    => "Engine ready",
                EngineClient.LifecycleState.Starting => "Engine starting...",
                EngineClient.LifecycleState.Crashed  => "Engine stopped (manual restart required)",
                _                                     => "Engine state unknown",
            };
        }
    }

    public string WorkerCapText
    {
        get
        {
            var info = EngineClient.Instance.Info;
            if (info is null) return "Worker pool: pending";
            var cores = info.Hardware?.PhysicalCpuCores;
            var coreText = cores.HasValue ? $" - {cores.Value} physical cores" : string.Empty;
            return $"Worker cap: {info.WorkerCap}{coreText} - {info.PhysicalMemoryGB:0.#} GB RAM";
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
        var svc = Services.ModelInstallerService.Instance;
        var (statusText, progressBar, slot) = modelKind switch
        {
            "arcface_buffalo" => (ArcFaceStatusText, ArcFaceProgress, svc.Arcface),
            "mobileclip_s2"   => (ClipStatusText,    ClipProgress,    svc.Clip),
            _ => (null!, null!, null!),
        };
        if (statusText is null || slot is null) return;
        var originalContent = button.Content;
        button.IsEnabled = false;
        button.Content = "Installing…";
        progressBar.Visibility = Visibility.Visible;

        // Subscribe to slot.Fraction for live updates.
        void OnProgress(object? _, System.ComponentModel.PropertyChangedEventArgs ev)
        {
            if (ev.PropertyName != nameof(Services.ModelSlot.Fraction)) return;
            var pct = slot.Fraction;
            DispatcherQueue.TryEnqueue(() =>
            {
                progressBar.Value = pct;
                statusText.Text = pct > 0 && pct < 1
                    ? $"Downloading… {pct * 100:0}%"
                    : (pct >= 1 ? "Installed" : statusText.Text);
            });
        }
        slot.PropertyChanged += OnProgress;
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
            slot.PropertyChanged -= OnProgress;
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
                    Content = "FileID needs to restart its engine to activate the new execution provider. Restart now?",
                    PrimaryButtonText = "Restart now",
                    SecondaryButtonText = "Later",
                    DefaultButton = ContentDialogButton.Primary,
                };
                var choice = await dialog.ShowAsync();
                if (choice == ContentDialogResult.Primary)
                {
                    button.IsEnabled = false;
                    button.Content = "Restarting…";
                    try
                    {
                        await EngineClient.Instance.RestartAsync();
                        button.Content = "Active";
                    }
                    catch (Exception rex)
                    {
                        DebugLog.Warn($"Engine restart after pack install failed: {rex.Message}");
                        button.Content = "Installed (restart manually)";
                    }
                    finally
                    {
                        button.IsEnabled = true;
                    }
                }
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
