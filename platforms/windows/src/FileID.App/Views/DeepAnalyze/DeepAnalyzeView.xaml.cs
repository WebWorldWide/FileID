// DeepAnalyzeView code-behind. Subscribes to EngineClient observables
// + ModelInstallerService for the per-model install state. Drives the
// llama.cpp runtime install, model install, full-library/per-file
// analyze, cancel, and renders the live caption stream as tokens
// arrive from the engine.

using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.IO;
using System.Linq;
using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;

namespace FileID.Views.DeepAnalyze;

public sealed partial class DeepAnalyzeView : UserControl
{
    private string _activeModel = "qwen2_5_vl_3b";
    private string _captionAccumulator = string.Empty;

    public DeepAnalyzeView()
    {
        InitializeComponent();
        Loaded += OnLoadedHandler;
        Unloaded += (_, _) =>
        {
            ModelInstallerService.Instance.PropertyChanged -= OnInstallerChanged;
            EngineClient.Instance.PropertyChanged -= OnEngineChanged;
        };
    }

    private void OnLoadedHandler(object sender, RoutedEventArgs e)
    {
        ModelInstallerService.Instance.PropertyChanged += OnInstallerChanged;
        EngineClient.Instance.PropertyChanged += OnEngineChanged;
        SyncCards();
        SyncRuntimeBanner();
        UpdateActiveModelLabel();
    }

    private void OnInstallerChanged(object? sender, PropertyChangedEventArgs e)
        => DispatcherQueue.TryEnqueue(SyncCards);

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
    {
        switch (e.PropertyName)
        {
            case nameof(EngineClient.DeepAnalyzeStarting):
            case nameof(EngineClient.DeepAnalyzeProgress):
            case nameof(EngineClient.DeepAnalyzeLast):
            case nameof(EngineClient.DeepAnalyzeComplete):
                DispatcherQueue.TryEnqueue(SyncStream);
                break;
        }
    }

    private void SyncCards()
    {
        var svc = ModelInstallerService.Instance;
        ApplyCard(QwenSmallStatus, QwenSmallProgress, QwenSmallInstallButton, svc.VlmStatus, svc.VlmProgress);
        ApplyCard(QwenLargeStatus, QwenLargeProgress, QwenLargeInstallButton, svc.VlmStatus, svc.VlmProgress);
        ApplyCard(SmolVlmStatus, SmolVlmProgress, SmolVlmInstallButton, svc.VlmStatus, svc.VlmProgress);
        HighlightActiveCard();
    }

    private static void ApplyCard(TextBlock status, ProgressBar bar, Button installButton, ModelInstallStatus s, double progress)
    {
        switch (s)
        {
            case ModelInstallStatus.Installed:
                status.Text = "Installed";
                bar.Visibility = Visibility.Collapsed;
                installButton.Content = "Reinstall";
                installButton.IsEnabled = true;
                break;
            case ModelInstallStatus.Downloading:
                status.Text = $"Downloading… {Math.Round(progress * 100)}%";
                bar.Visibility = Visibility.Visible;
                bar.Value = progress;
                installButton.IsEnabled = false;
                break;
            case ModelInstallStatus.Failed:
                status.Text = "Install failed — retry?";
                bar.Visibility = Visibility.Collapsed;
                installButton.IsEnabled = true;
                break;
            default:
                status.Text = string.Empty;
                bar.Visibility = Visibility.Collapsed;
                installButton.IsEnabled = true;
                break;
        }
    }

    private void HighlightActiveCard()
    {
        var idle = (Brush)Application.Current.Resources["CardStrokeColorDefaultBrush"];
        var gold = (Brush)Application.Current.Resources["GoldBrush"];
        QwenSmallCard.BorderBrush = _activeModel == "qwen2_5_vl_3b" ? gold : idle;
        QwenLargeCard.BorderBrush = _activeModel == "qwen2_5_vl_7b" ? gold : idle;
        SmolVlmCard.BorderBrush   = _activeModel == "smolvlm"       ? gold : idle;
        QwenSmallCard.BorderThickness = _activeModel == "qwen2_5_vl_3b" ? new Thickness(2) : new Thickness(1);
        QwenLargeCard.BorderThickness = _activeModel == "qwen2_5_vl_7b" ? new Thickness(2) : new Thickness(1);
        SmolVlmCard.BorderThickness   = _activeModel == "smolvlm"       ? new Thickness(2) : new Thickness(1);
    }

    private void SyncRuntimeBanner()
    {
        // Probe %LOCALAPPDATA%\FileID\Models\llama.cpp for the runtime.
        var runtimeRoot = Path.Combine(AppPaths.Root, "Models", "llama.cpp");
        var cli1 = Path.Combine(runtimeRoot, "llama-mtmd-cli.exe");
        var cli2 = Path.Combine(runtimeRoot, "llama-mtmd-cli", "llama-mtmd-cli.exe");
        var present = File.Exists(cli1) || File.Exists(cli2);
        RuntimeBanner.Visibility = present ? Visibility.Collapsed : Visibility.Visible;
        if (!present)
        {
            RuntimeBannerText.Text = "Deep Analyze needs the llama.cpp runtime (Vulkan x64). " +
                                     "It's a one-time ~80 MB download from the official llama.cpp release. " +
                                     "Click Install runtime — files land under %LOCALAPPDATA%\\FileID\\Models\\llama.cpp\\.";
        }
    }

    private void UpdateActiveModelLabel()
    {
        ActiveModelText.Text = _activeModel switch
        {
            "qwen2_5_vl_7b" => "Active model: Qwen 2.5-VL 7B (best quality)",
            "smolvlm"       => "Active model: SmolVLM 256M (fastest)",
            _               => "Active model: Qwen 2.5-VL 3B (balanced)",
        };
    }

    private void SyncStream()
    {
        var ec = EngineClient.Instance;
        var prog = ec.DeepAnalyzeProgress;
        var last = ec.DeepAnalyzeLast;
        var complete = ec.DeepAnalyzeComplete;

        if (prog is null && last is null && complete is null) return;

        if (prog is not null)
        {
            StreamCard.Visibility = Visibility.Visible;
            CancelButton.IsEnabled = true;
            AnalyzeAllButton.IsEnabled = false;

            var pct = prog.Total == 0 ? 0 : (double)prog.Processed / prog.Total;
            OverallProgress.Value = pct;
            OverallProgressText.Text = $"{prog.Processed} / {prog.Total} files";

            if (!string.IsNullOrEmpty(prog.CurrentPath))
            {
                StreamFileNameText.Text = Path.GetFileName(prog.CurrentPath);
                _ = LoadStreamThumbAsync(prog.CurrentPath);
                _captionAccumulator = string.Empty;
                StreamCaptionText.Text = string.Empty;
            }
        }

        if (last is not null)
        {
            StreamCaptionText.Text = last.Description ?? string.Empty;
            StreamProposedNameText.Text = string.IsNullOrEmpty(last.ProposedName)
                ? string.Empty
                : $"Proposed name: {last.ProposedName}";
        }

        if (complete is not null)
        {
            CancelButton.IsEnabled = false;
            AnalyzeAllButton.IsEnabled = true;
            OverallProgressText.Text = complete.Cancelled
                ? $"Cancelled ({complete.Processed} done, {complete.Failed} failed)"
                : $"Done — {complete.Processed} captioned in {complete.TotalSeconds:0.#}s ({complete.Failed} failed)";
        }
    }

    private async System.Threading.Tasks.Task LoadStreamThumbAsync(string path)
    {
        try
        {
            var file = await Windows.Storage.StorageFile.GetFileFromPathAsync(path);
            using var thumb = await file.GetThumbnailAsync(
                Windows.Storage.FileProperties.ThumbnailMode.SingleItem, 320,
                Windows.Storage.FileProperties.ThumbnailOptions.UseCurrentScale);
            if (thumb != null && thumb.Size > 0)
            {
                var bmp = new BitmapImage();
                await bmp.SetSourceAsync(thumb);
                StreamImage.Source = bmp;
            }
        }
        catch { /* fall back to placeholder */ }
    }

    private void OnModelCardTapped(object sender, Microsoft.UI.Xaml.Input.TappedRoutedEventArgs e)
    {
        if (sender is FrameworkElement el && el.Tag is string id)
        {
            _activeModel = id;
            HighlightActiveCard();
            UpdateActiveModelLabel();
        }
    }

    private async void OnInstallModelClicked(object sender, RoutedEventArgs e)
    {
        if (sender is not Button b || b.Tag is not string modelId) return;
        try
        {
            await ModelInstallerService.Instance.InstallRecommendedVlmAsync();
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"VLM install '{modelId}' failed: {ex}");
        }
    }

    private async void OnInstallRuntimeClicked(object sender, RoutedEventArgs e)
    {
        InstallRuntimeButton.IsEnabled = false;
        try
        {
            await EngineClient.Instance.PrewarmModelAsync("llama_runtime_x64");
            // Engine emits modelDownloadProgress events; we re-check the
            // banner each time the installer service refreshes.
            ModelInstallerService.Instance.Refresh();
            SyncRuntimeBanner();
        }
        catch (Exception ex)
        {
            RuntimeBannerText.Text = $"Couldn't fetch runtime: {ex.Message}";
        }
        finally
        {
            InstallRuntimeButton.IsEnabled = true;
        }
    }

    private async void OnAnalyzeAllClicked(object sender, RoutedEventArgs e)
    {
        try
        {
            await EngineClient.Instance.DeepAnalyzeAllAsync(_activeModel, SkipExistingToggle.IsOn);
            StreamCard.Visibility = Visibility.Visible;
            CancelButton.IsEnabled = true;
        }
        catch (Exception ex)
        {
            DebugLog.Warn("DeepAnalyzeAll failed: " + ex);
        }
    }

    private async void OnCancelClicked(object sender, RoutedEventArgs e)
    {
        try { await EngineClient.Instance.DeepAnalyzeCancelAsync(); } catch { /* swallow */ }
    }
}
