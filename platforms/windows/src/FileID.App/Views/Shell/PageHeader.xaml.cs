using System.ComponentModel;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FileID.Views.Shell;

public sealed partial class PageHeader : UserControl
{
    public PageHeader()
    {
        InitializeComponent();
        Loaded += OnLoaded;
        Unloaded += OnUnloaded;
    }

    private void OnLoaded(object sender, RoutedEventArgs e)
    {
        AppViewModel.Instance.PropertyChanged -= OnAppChanged;
        AppViewModel.Instance.PropertyChanged += OnAppChanged;
        EngineClient.Instance.PropertyChanged -= OnEngineChanged;
        EngineClient.Instance.PropertyChanged += OnEngineChanged;
        Sync();
    }

    private void OnUnloaded(object sender, RoutedEventArgs e)
    {
        AppViewModel.Instance.PropertyChanged -= OnAppChanged;
        EngineClient.Instance.PropertyChanged -= OnEngineChanged;
    }

    private void OnAppChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("PageHeader.OnAppChanged", () =>
        {
            if (e.PropertyName is nameof(AppViewModel.ActiveTab)
                              or nameof(AppViewModel.HasFolder)
                              or nameof(AppViewModel.FolderDisplay))
            {
                DispatcherQueue.TryEnqueue(Sync);
            }
        });

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("PageHeader.OnEngineChanged", () =>
        {
            if (e.PropertyName is nameof(EngineClient.DeepAnalyzeProgress)
                              or nameof(EngineClient.Phase))
            {
                DebugLog.Debug($"[ENGINE-SUB:PageHeader] {e.PropertyName}");
                DispatcherQueue.TryEnqueue(SyncMeta);
            }
        });

    private void Sync()
    {
        var vm = AppViewModel.Instance;
        var tab = vm.ActiveTab;
        Visibility = vm.HasFolder || !tab.RequiresFolder ? Visibility.Visible : Visibility.Collapsed;
        CrumbText.Text = tab.Crumb.ToUpperInvariant();
        PageTitleText.Text = tab.Label;
        SyncMeta();
    }

    private void SyncMeta()
    {
        var vm = AppViewModel.Instance;
        var engine = EngineClient.Instance;
        var meta = vm.ActiveTab.Id switch
        {
            "library" when engine.LastScanProcessedFiles > 0 => $"{engine.LastScanProcessedFiles:N0} files",
            "deepanalyze" when engine.DeepAnalyzeProgress is { } progress => $"{progress.Processed:N0} / {progress.Total:N0}",
            "restructure" => vm.FolderDisplay ?? string.Empty,
            _ => string.Empty,
        };
        if (PageMetaText.Text != meta)
        {
            PageMetaText.Text = meta;
        }
    }
}
