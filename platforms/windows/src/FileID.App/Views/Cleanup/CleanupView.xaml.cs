// CleanupView code-behind. Trash-non-keepers walks every group, gathers
// the file_ids for members where IsKeeper == false, confirms with the
// user, then sends one big trashFiles IPC.

using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Linq;
using System.Threading;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FileID.Views.Cleanup;

public sealed partial class CleanupView : UserControl, INotifyPropertyChanged
{
    internal CleanupViewModel ViewModel { get; }

    private bool _unloaded;
    public CleanupView()
    {
        ViewModel = new CleanupViewModel(AppPaths.DbPath, Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread());
        InitializeComponent();
        // Named handlers (not inline lambdas) so OnUnloaded can detach
        // them. Inline lambdas leak the view + VM graph every tab swap
        // and can fire after the view is detached, touching disposed
        // XAML — a known cause of the "click sidebar mid-scan → app crash"
        // symptom.
        ViewModel.PropertyChanged += OnViewModelPropertyChanged;
        ViewModel.Groups.CollectionChanged += OnGroupsCollectionChanged;
        ViewModels.EngineClient.Instance.PropertyChanged += OnEngineScanCompleted;
        Loaded += OnLoadedAsync;
        Unloaded += OnUnloaded;
    }

    private void OnEngineScanCompleted(object? sender, PropertyChangedEventArgs e)
    {
        if (_unloaded) return;
        if (e.PropertyName != nameof(ViewModels.EngineClient.Phase)) return;
        if (ViewModels.EngineClient.Instance.Phase != FileID.IpcSchema.ScanPhase.Completed) return;
        DispatcherQueue.TryEnqueue(async () =>
        {
            try { await ViewModel.RefreshAsync(CancellationToken.None); }
            catch (Exception ex) { DebugLog.Warn("Cleanup refresh on scan complete failed: " + ex.Message); }
        });
    }

    private async void OnLoadedAsync(object sender, RoutedEventArgs e)
    {
        if (_unloaded) return;
        try { await ViewModel.RefreshAsync(CancellationToken.None); }
        catch (Exception ex) { DebugLog.Warn("CleanupView.OnLoaded refresh threw: " + ex.Message); }
    }

    private void OnViewModelPropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (_unloaded) return;
        OnPropertyChanged(nameof(StatusText));
        OnPropertyChanged(nameof(FooterVisibility));
    }

    private void OnGroupsCollectionChanged(object? sender, System.Collections.Specialized.NotifyCollectionChangedEventArgs e)
    {
        if (_unloaded) return;
        OnPropertyChanged(nameof(StatusText));
        OnPropertyChanged(nameof(FooterVisibility));
    }

    private void OnUnloaded(object sender, RoutedEventArgs e)
    {
        _unloaded = true;
        Unloaded -= OnUnloaded;
        Loaded -= OnLoadedAsync;
        try { ViewModel.PropertyChanged -= OnViewModelPropertyChanged; } catch { /* swallow */ }
        try { ViewModel.Groups.CollectionChanged -= OnGroupsCollectionChanged; } catch { /* swallow */ }
        try { ViewModels.EngineClient.Instance.PropertyChanged -= OnEngineScanCompleted; } catch { /* swallow */ }
        try { ViewModel.Dispose(); } catch { /* swallow */ }
    }

    public string StatusText
    {
        get
        {
            if (!string.IsNullOrEmpty(ViewModel.ErrorMessage)) return ViewModel.ErrorMessage!;
            if (ViewModel.IsLoading) return "Scanning for duplicates…";
            if (ViewModel.Groups.Count == 0) return "No duplicates found yet — run a scan first.";
            return $"{ViewModel.Groups.Count} duplicate groups";
        }
    }

    public Visibility FooterVisibility =>
        ViewModel.IsLoading
        || !string.IsNullOrEmpty(ViewModel.ErrorMessage)
        || ViewModel.Groups.Count == 0
            ? Visibility.Visible : Visibility.Collapsed;

    private async void OnRefreshClicked(object sender, RoutedEventArgs e)
        => await ViewModel.RefreshAsync(CancellationToken.None);

    private async void OnTrashNonKeepersClicked(object sender, RoutedEventArgs e)
    {
        var ids = new List<long>();
        long bytes = 0;
        foreach (var grp in ViewModel.Groups)
        {
            // FEAT-CRIT-2: skipped groups are excluded from the global
            // "Trash non-keepers" run.
            if (grp.IsSkipped) continue;
            foreach (var m in grp.Members)
            {
                if (!m.IsKeeper)
                {
                    ids.Add(m.Id);
                    bytes += m.SizeBytes;
                }
            }
        }
        if (ids.Count == 0)
        {
            return;
        }
        var sizeDisplay = FormatSize(bytes);
        var confirm = new ContentDialog
        {
            XamlRoot = this.XamlRoot,
            Title = "Trash duplicates?",
            Content = $"{ids.Count} non-keeper file{(ids.Count == 1 ? "" : "s")} ({sizeDisplay}) will move to the Recycle Bin. They stay recoverable from there.",
            PrimaryButtonText = "Move to Recycle Bin",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Close,
        };
        var choice = await confirm.ShowAsync();
        if (choice != ContentDialogResult.Primary) return;

        try
        {
            Services.UndoStack.CaptureNextBulkResult(
                "trashFiles:",
                $"trash {ids.Count} duplicate{(ids.Count == 1 ? "" : "s")}",
                async batchId =>
                {
                    if (string.IsNullOrEmpty(batchId)) return false;
                    try
                    {
                        await ViewModels.EngineClient.Instance.RestoreFromTrashAsync(batchId);
                        return true;
                    }
                    catch { return false; }
                });
            await ViewModels.EngineClient.Instance.TrashFilesAsync(ids);
        }
        catch
        {
            // Result surfaces via BulkActionResultEvent.
        }

        await ViewModel.RefreshAsync(CancellationToken.None);
    }

    private static string FormatSize(long bytes)
    {
        if (bytes < 1024) return $"{bytes} B";
        if (bytes < 1024 * 1024) return $"{bytes / 1024.0:0.#} KB";
        if (bytes < 1024L * 1024 * 1024) return $"{bytes / (1024.0 * 1024):0.#} MB";
        return $"{bytes / (1024.0 * 1024 * 1024):0.##} GB";
    }

    // ─── FEAT-CRIT-2: Per-group action menu handlers ─────────────────

    // V14.7.6: WinUI 3 MenuFlyoutItem inside a Grid.ContextFlyout does NOT
    // inherit the parent Grid's DataContext, so the prior version's
    // `item.DataContext as DuplicateGroup` always returned null and every
    // per-group action silently no-op'd. Fix: cache the right-tapped group
    // here at the moment the context menu is invoked.
    private DuplicateGroup? _lastRightTappedGroup;

    private void OnGroupRightTapped(object sender, Microsoft.UI.Xaml.Input.RightTappedRoutedEventArgs e)
    {
        if (sender is FrameworkElement fe && fe.DataContext is DuplicateGroup g)
        {
            _lastRightTappedGroup = g;
        }
    }

    private DuplicateGroup? GroupFromFlyoutItem(object sender) => _lastRightTappedGroup;

    private void OnGroupKeepFirst(object sender, RoutedEventArgs e)
    {
        var grp = GroupFromFlyoutItem(sender);
        if (grp == null || grp.Members.Count == 0) return;
        for (int i = 0; i < grp.Members.Count; i++)
        {
            grp.Members[i].IsKeeper = (i == 0);
        }
    }

    private void OnGroupKeepLargest(object sender, RoutedEventArgs e)
    {
        var grp = GroupFromFlyoutItem(sender);
        if (grp == null || grp.Members.Count == 0) return;
        var largestIdx = 0;
        for (int i = 1; i < grp.Members.Count; i++)
        {
            if (grp.Members[i].SizeBytes > grp.Members[largestIdx].SizeBytes)
            {
                largestIdx = i;
            }
        }
        for (int i = 0; i < grp.Members.Count; i++)
        {
            grp.Members[i].IsKeeper = (i == largestIdx);
        }
    }

    private void OnGroupInvert(object sender, RoutedEventArgs e)
    {
        var grp = GroupFromFlyoutItem(sender);
        if (grp == null || grp.Members.Count == 0) return;
        var currentIdx = -1;
        for (int i = 0; i < grp.Members.Count; i++)
        {
            if (grp.Members[i].IsKeeper) { currentIdx = i; break; }
        }
        var nextIdx = (currentIdx + 1) % grp.Members.Count;
        for (int i = 0; i < grp.Members.Count; i++)
        {
            grp.Members[i].IsKeeper = (i == nextIdx);
        }
    }

    private void OnGroupSkip(object sender, RoutedEventArgs e)
    {
        var grp = GroupFromFlyoutItem(sender);
        if (grp != null) grp.IsSkipped = true;
    }

    private void OnGroupUnskip(object sender, RoutedEventArgs e)
    {
        var grp = GroupFromFlyoutItem(sender);
        if (grp != null) grp.IsSkipped = false;
    }

    private async void OnGroupTrashNow(object sender, RoutedEventArgs e)
    {
        var grp = GroupFromFlyoutItem(sender);
        if (grp == null) return;
        var ids = new List<long>();
        long bytes = 0;
        foreach (var m in grp.Members)
        {
            if (!m.IsKeeper) { ids.Add(m.Id); bytes += m.SizeBytes; }
        }
        if (ids.Count == 0) return;
        var confirm = new ContentDialog
        {
            XamlRoot = this.XamlRoot,
            Title = "Trash this group?",
            Content = $"{ids.Count} non-keeper file{(ids.Count == 1 ? "" : "s")} ({FormatSize(bytes)}) will move to the Recycle Bin.",
            PrimaryButtonText = "Move to Recycle Bin",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Close,
        };
        if (await confirm.ShowAsync() != ContentDialogResult.Primary) return;
        try
        {
            Services.UndoStack.CaptureNextBulkResult(
                "trashFiles:",
                $"trash {ids.Count} duplicate{(ids.Count == 1 ? "" : "s")}",
                async batchId =>
                {
                    if (string.IsNullOrEmpty(batchId)) return false;
                    try { await ViewModels.EngineClient.Instance.RestoreFromTrashAsync(batchId); return true; }
                    catch { return false; }
                });
            await ViewModels.EngineClient.Instance.TrashFilesAsync(ids);
        }
        catch { }
        await ViewModel.RefreshAsync(CancellationToken.None);
    }

    public event PropertyChangedEventHandler? PropertyChanged;
    private void OnPropertyChanged(string name)
        => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}
