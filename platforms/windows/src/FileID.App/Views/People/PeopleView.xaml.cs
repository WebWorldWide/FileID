// PeopleView code-behind. Cluster cards are draggable + drop targets;
// dropping cluster A onto cluster B emits engine `mergeClusters` IPC
// (A's face_prints reassigned to B's person_id, A's person row deleted).

using System;
using System.ComponentModel;
using System.Threading;
using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.ApplicationModel.DataTransfer;

namespace FileID.Views.People;

public sealed partial class PeopleView : UserControl, INotifyPropertyChanged
{
    internal PeopleViewModel ViewModel { get; }
    private const string MergeFormatId = "fileid/person-cluster-id";

    public PeopleView()
    {
        ViewModel = new PeopleViewModel(AppPaths.DbPath, Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread());
        InitializeComponent();
        ViewModel.PropertyChanged += (_, _) =>
        {
            OnPropertyChanged(nameof(StatusText));
            OnPropertyChanged(nameof(FooterVisibility));
        };
        ViewModel.Clusters.CollectionChanged += (_, _) =>
        {
            OnPropertyChanged(nameof(StatusText));
            OnPropertyChanged(nameof(FooterVisibility));
        };
        Loaded += async (_, _) => await ViewModel.RefreshAsync(CancellationToken.None);
    }

    public string StatusText
    {
        get
        {
            if (!string.IsNullOrEmpty(ViewModel.ErrorMessage))
            {
                return ViewModel.ErrorMessage!;
            }
            if (ViewModel.IsLoading)
            {
                return "Loading clusters…";
            }
            if (ViewModel.Clusters.Count == 0)
            {
                return "No people yet — run face clustering after a scan.";
            }
            return $"{ViewModel.Clusters.Count} clusters";
        }
    }

    public Visibility FooterVisibility =>
        ViewModel.IsLoading
        || !string.IsNullOrEmpty(ViewModel.ErrorMessage)
        || ViewModel.Clusters.Count == 0
            ? Visibility.Visible : Visibility.Collapsed;

    private async void OnRefreshClicked(object sender, RoutedEventArgs e)
    {
        await ViewModel.RefreshAsync(CancellationToken.None);
    }

    private void OnClusterDragStarting(UIElement sender, DragStartingEventArgs args)
    {
        if (sender is FrameworkElement el && el.DataContext is PersonCluster pc)
        {
            args.Data.Properties.Add(MergeFormatId, (long)pc.ClusterId);
            args.Data.RequestedOperation = DataPackageOperation.Move;
        }
        else if (sender is FrameworkElement el2 && el2.Tag is long pid)
        {
            args.Data.Properties.Add(MergeFormatId, pid);
            args.Data.RequestedOperation = DataPackageOperation.Move;
        }
    }

    private void OnClusterDragOver(object sender, DragEventArgs args)
    {
        if (args.DataView.Properties.ContainsKey(MergeFormatId))
        {
            args.AcceptedOperation = DataPackageOperation.Move;
            // Highlight the drop target with a gold outer ring (BorderBrush
            // animation would be nicer; brush swap is cheaper + lands now).
            if (sender is Grid g)
            {
                g.BorderBrush = new SolidColorBrush(Microsoft.UI.Colors.Gold);
                g.BorderThickness = new Thickness(2);
            }
        }
        else
        {
            args.AcceptedOperation = DataPackageOperation.None;
        }
    }

    private void OnClusterDragLeave(object sender, DragEventArgs args)
    {
        if (sender is Grid g)
        {
            g.BorderBrush = (SolidColorBrush)Application.Current.Resources["CardStrokeColorDefaultBrush"];
            g.BorderThickness = new Thickness(1);
        }
    }

    private async void OnClusterDrop(object sender, DragEventArgs args)
    {
        if (sender is not Grid g) return;
        // Restore styling first so a failure mid-drop doesn't leave the gold ring.
        g.BorderBrush = (SolidColorBrush)Application.Current.Resources["CardStrokeColorDefaultBrush"];
        g.BorderThickness = new Thickness(1);

        if (!args.DataView.Properties.TryGetValue(MergeFormatId, out var raw)) return;
        if (raw is not long sourceId) return;

        long destId;
        if (g.Tag is long t) destId = t;
        else if (g.DataContext is PersonCluster pc) destId = pc.ClusterId;
        else return;

        if (sourceId == destId) return; // no-op self-drop

        var confirm = new ContentDialog
        {
            XamlRoot = this.XamlRoot,
            Title = "Merge clusters?",
            Content = $"Move all faces from #{sourceId} into #{destId}? This can't be auto-undone.",
            PrimaryButtonText = "Merge",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
        };
        var choice = await confirm.ShowAsync();
        if (choice != ContentDialogResult.Primary) return;

        try
        {
            await ViewModels.EngineClient.Instance.MergeClustersAsync(sourceId, destId);
        }
        catch (Exception)
        {
            // Failure surfaces through the BulkActionResult event.
        }

        await ViewModel.RefreshAsync(CancellationToken.None);
    }

    public event PropertyChangedEventHandler? PropertyChanged;
    private void OnPropertyChanged(string name)
        => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}
