// PeopleView code-behind. Cluster cards are draggable + drop targets;
// dropping cluster A onto cluster B emits engine `mergeClusters` IPC
// (A's face_prints reassigned to B's person_id, A's person row deleted).

using System;
using System.ComponentModel;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
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

    private async void OnContextOpenDetails(object sender, RoutedEventArgs e)
    {
        if (sender is not MenuFlyoutItem item || item.Tag is not int cid) return;
        var cluster = ViewModel.Clusters.FirstOrDefault(c => c.ClusterId == cid);
        if (cluster is null) return;
        await OpenDetailSheetAsync(cluster);
    }

    private void OnContextSuggestedMerges(object sender, RoutedEventArgs e)
        => OnSuggestedMergesClicked(sender, e);

    private async Task OpenDetailSheetAsync(PersonCluster pc)
    {
        var sheet = new PersonDetailSheet();
        sheet.SetPerson(pc.ClusterId, pc.DisplayName);
        var dialog = new ContentDialog
        {
            XamlRoot = this.XamlRoot,
            Title = "Person details",
            Content = sheet,
            PrimaryButtonText = "Save",
            CloseButtonText = "Close",
            DefaultButton = ContentDialogButton.Primary,
        };
        dialog.PrimaryButtonClick += async (_, args) =>
        {
            var deferral = args.GetDeferral();
            var ok = await sheet.CommitAsync();
            if (!ok) args.Cancel = true;
            deferral.Complete();
        };
        try { await dialog.ShowAsync(); } catch { /* dialog already open */ }
        await ViewModel.RefreshAsync(System.Threading.CancellationToken.None);
    }

    private async void OnSuggestedMergesClicked(object sender, RoutedEventArgs e)
    {
        var sheet = new SuggestedMergesSheet();
        var dialog = new ContentDialog
        {
            XamlRoot = this.XamlRoot,
            Title = "Suggested merges",
            Content = sheet,
            CloseButtonText = "Done",
            DefaultButton = ContentDialogButton.Close,
        };
        try { await dialog.ShowAsync(); } catch { /* dialog already open */ }
        await ViewModel.RefreshAsync(System.Threading.CancellationToken.None);
    }

    private async void OnRefreshClicked(object sender, RoutedEventArgs e)
    {
        // Fire the engine's runFaceClustering pass first so the People tab
        // reflects the latest face_print → person_id assignments. The
        // engine emits a faceClusteringComplete IPC event when done; we
        // refresh after our local IPC fire-and-forget to avoid a confusing
        // "old data shown briefly" flicker.
        try
        {
            await ViewModels.EngineClient.Instance.RunFaceClusteringAsync();
        }
        catch
        {
            // engine offline — fall through to a plain reload
        }
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

    private async void OnClusterDoubleTapped(object sender, Microsoft.UI.Xaml.Input.DoubleTappedRoutedEventArgs e)
    {
        if (sender is not FrameworkElement el || el.DataContext is not PersonCluster pc) return;

        var sheet = new PersonDetailSheet();
        sheet.SetPerson(pc.ClusterId, pc.DisplayName);
        var dialog = new ContentDialog
        {
            XamlRoot = this.XamlRoot,
            Title = "Person details",
            Content = sheet,
            PrimaryButtonText = "Save",
            CloseButtonText = "Close",
            DefaultButton = ContentDialogButton.Primary,
        };
        dialog.PrimaryButtonClick += async (_, args2) =>
        {
            var deferral = args2.GetDeferral();
            var ok = await sheet.CommitAsync();
            if (!ok) args2.Cancel = true;
            deferral.Complete();
        };
        try { await dialog.ShowAsync(); } catch { /* dialog already open */ }
        await ViewModel.RefreshAsync(System.Threading.CancellationToken.None);
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

    // ─── FEAT-CRIT-1: People multi-select bulk merge / mark-as-unknown ──

    private void OnToggleSelectMode(object sender, RoutedEventArgs e)
    {
        ViewModel.IsSelectMode = !ViewModel.IsSelectMode;
        SelectButtonText.Text = ViewModel.IsSelectMode ? "Done" : "Select";
        BulkActionBar.Visibility = ViewModel.IsSelectMode ? Visibility.Visible : Visibility.Collapsed;
        // Show/hide every per-card checkbox via tag-walk. ItemsRepeater
        // doesn't ItemContainerStyle, so we walk realized children. The
        // initial state of newly-realized cards is Collapsed (XAML default);
        // when we enter select mode this loop reveals them.
        UpdateCheckboxVisibility();
        UpdateSelectionCountText();
        // Wire each cluster's IsSelected change so SelectedCount stays
        // current. Cheap; PersonCluster instances are stable across
        // refreshes within select-mode.
        foreach (var c in ViewModel.Clusters)
        {
            c.PropertyChanged -= OnClusterIsSelectedChanged;
            if (ViewModel.IsSelectMode)
            {
                c.PropertyChanged += OnClusterIsSelectedChanged;
            }
            else
            {
                c.IsSelected = false;
            }
        }
    }

    private void OnClusterIsSelectedChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName == nameof(PersonCluster.IsSelected))
        {
            UpdateSelectionCountText();
        }
    }

    private void UpdateSelectionCountText()
    {
        var n = ViewModel.SelectedCount;
        BulkSelectionText.Text = n switch
        {
            0 => "Pick clusters to merge or mark as unknown",
            1 => "1 selected",
            _ => $"{n} selected",
        };
        BulkMergeButton.IsEnabled = n >= 2;
        BulkUnknownButton.IsEnabled = n >= 1;
    }

    private void UpdateCheckboxVisibility()
    {
        // Walk realized cards, find the CheckBox tagged "select-cb",
        // toggle its visibility based on IsSelectMode.
        foreach (var element in EnumerateRepeaterChildren())
        {
            if (FindCheckBoxInTree(element) is { } cb)
            {
                cb.Visibility = ViewModel.IsSelectMode ? Visibility.Visible : Visibility.Collapsed;
            }
        }
    }

    private System.Collections.Generic.IEnumerable<DependencyObject> EnumerateRepeaterChildren()
    {
        // Walk the visual tree of every cluster card. Use VisualTreeHelper.
        var stack = new System.Collections.Generic.Stack<DependencyObject>();
        stack.Push(this);
        while (stack.Count > 0)
        {
            var d = stack.Pop();
            int n = Microsoft.UI.Xaml.Media.VisualTreeHelper.GetChildrenCount(d);
            for (int i = 0; i < n; i++)
            {
                var c = Microsoft.UI.Xaml.Media.VisualTreeHelper.GetChild(d, i);
                yield return c;
                stack.Push(c);
            }
        }
    }

    private CheckBox? FindCheckBoxInTree(DependencyObject root)
    {
        if (root is CheckBox cb && cb.Tag is string tag && tag == "select-cb") return cb;
        return null;
    }

    private async void OnBulkMergeClicked(object sender, RoutedEventArgs e)
    {
        var ids = ViewModel.SelectedClusterIds;
        if (ids.Count < 2) return;
        // Merge cluster ids[1..N] into ids[0] (the first selected).
        // Engine `mergeClusters` is 1:1; loop the call N-1 times.
        var dest = ids[0];
        try
        {
            for (int i = 1; i < ids.Count; i++)
            {
                await EngineClient.Instance.MergeClustersAsync(ids[i], dest);
            }
            DebugLog.Info($"Bulk-merged {ids.Count - 1} clusters into {dest}");
        }
        catch (Exception ex)
        {
            DebugLog.Warn("BulkMerge IPC failed: " + ex.Message);
        }
        // Exit select mode + refresh.
        ViewModel.IsSelectMode = false;
        BulkActionBar.Visibility = Visibility.Collapsed;
        SelectButtonText.Text = "Select";
        UpdateCheckboxVisibility();
        await ViewModel.RefreshAsync(CancellationToken.None);
    }

    private async void OnBulkMarkUnknownClicked(object sender, RoutedEventArgs e)
    {
        var ids = ViewModel.SelectedClusterIds;
        if (ids.Count == 0) return;
        try
        {
            // PersonCluster.ClusterId is int; engine wants long.
            var longIds = new System.Collections.Generic.List<long>(ids.Count);
            foreach (var id in ids) longIds.Add(id);
            await EngineClient.Instance.MarkPersonsAsUnknownAsync(longIds);
            DebugLog.Info($"Marked {ids.Count} clusters as unknown");
        }
        catch (Exception ex)
        {
            DebugLog.Warn("BulkMarkUnknown IPC failed: " + ex.Message);
        }
        ViewModel.IsSelectMode = false;
        BulkActionBar.Visibility = Visibility.Collapsed;
        SelectButtonText.Text = "Select";
        UpdateCheckboxVisibility();
        await ViewModel.RefreshAsync(CancellationToken.None);
    }

    public event PropertyChangedEventHandler? PropertyChanged;
    private void OnPropertyChanged(string name)
        => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}
