// PeopleView code-behind. Cluster cards are draggable and drop targets for merging.

using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.IO;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.Data.Sqlite;
using Windows.ApplicationModel.DataTransfer;
using Windows.System;

namespace FileID.Views.People;

public sealed partial class PeopleView : UserControl, INotifyPropertyChanged
{
    internal PeopleViewModel ViewModel { get; }
    private const string MergeFormatId = "fileid/person-cluster-id";

    // A SolidColorBrush is a DispatcherObject; building one per drag-over event is a native fast-fail, so cache one.
    private readonly SolidColorBrush _goldBrush = new(Microsoft.UI.Colors.Gold);

    private readonly System.Collections.Generic.HashSet<int> _selectedClusterIds = new();

    // Concurrent bulk ops would share the single LastBulkAction correlation slot, crediting one op with the other's reply, and a re-fired merge would hit already-deleted source rows.
    private int _bulkOpInFlight; // 0 = idle, 1 = a bulk op running

    private bool _selectMaintenancePending;

    private bool _unloaded;
    public PeopleView()
    {
        ViewModel = new PeopleViewModel(AppPaths.DbPath, Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread());
        InitializeComponent();
        ViewModel.PropertyChanged += OnViewModelPropertyChanged;
        ViewModel.Clusters.CollectionChanged += OnClustersCollectionChanged;
        // The _unloaded guard stops a late dispatcher continuation from touching disposed state.
        FileID.ViewModels.EngineClient.Instance.PropertyChanged += OnEngineClientChanged;
        _undonePeopleChanges = UndonePeopleChanges();
        ChangeLog.Instance.Changed += OnChangeLogChanged;
        Loaded += OnLoadedAsync;
        Unloaded += OnUnloaded;
    }

    private int _undonePeopleChanges;

    private static int UndonePeopleChanges()
    {
        var undone = 0;
        foreach (var entry in ChangeLog.Instance.Snapshot())
        {
            if (entry.Page == ChangePage.People && entry.Status == ChangeStatus.Undone) undone++;
        }
        return undone;
    }

    private void OnChangeLogChanged(object? sender, EventArgs e)
        => DebugLog.SafeRun("PeopleView.OnChangeLogChanged", () =>
        {
            if (_unloaded) return;
            var undone = UndonePeopleChanges();
            if (Interlocked.Exchange(ref _undonePeopleChanges, undone) == undone) return;
            DispatcherQueue.TryEnqueue(async () =>
            {
                if (_unloaded) return;
                try { await ViewModel.RefreshAsync(CancellationToken.None); }
                catch (Exception ex) { DebugLog.Warn("PeopleView post-undo refresh threw: " + ex.Message); }
                await RefreshReviewCountAsync();
            });
        });

    private void OnEngineClientChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("PeopleView.OnEngineClientChanged", () =>
        {
            if (_unloaded) return;
            if (e.PropertyName == nameof(FileID.ViewModels.EngineClient.LastCatalogResponse))
            {
                OnFaceRefreshEngineChanged(e);
                return;
            }
            if (e.PropertyName != nameof(FileID.ViewModels.EngineClient.LastFaceClustering)) return;
            DebugLog.Info($"[ENGINE-SUB:PeopleView] {e.PropertyName}");
            DispatcherQueue.TryEnqueue(async () =>
            {
                if (_unloaded) return;
                try { await ViewModel.RefreshAsync(CancellationToken.None); }
                catch (Exception ex) { DebugLog.Warn("PeopleView post-clustering refresh threw: " + ex.Message); }
                if (!_unloaded) OnPropertyChanged(nameof(StatusText));
                await RefreshReviewCountAsync();
                await RefreshFaceRefreshStatusAsync();
            });
        });

    private async void OnLoadedAsync(object sender, RoutedEventArgs e)
    {
        if (_unloaded) return;
        try { await ViewModel.RefreshAsync(CancellationToken.None); }
        catch (Exception ex) { DebugLog.Warn("PeopleView.OnLoaded refresh threw: " + ex.Message); }
        UpdateHiddenUnknownsFooter();
        RefreshContinueToDeepAnalyzeBanner();
        await RefreshReviewCountAsync();
        await RefreshFaceRefreshStatusAsync();
    }

    private static readonly string[] ReviewQueues = { FaceQueuePolicy.Merges, FaceQueuePolicy.Splits };

    private async Task RefreshReviewCountAsync()
    {
        if (_unloaded) return;
        try
        {
            var totals = new FaceQueueTotals();
            var page = await FileID.ViewModels.EngineClient.Instance.ListFaceReviewAsync(null, 0, 1);
            if (_unloaded || !FaceReviewPolicy.IsOk(page)) return;
            totals.Set(FaceQueuePolicy.Faces, page.Total);
            foreach (var queue in ReviewQueues)
            {
                var counted = await FileID.ViewModels.EngineClient.Instance.ListFaceReviewAsync(null, 0, 1, queue);
                if (_unloaded) return;
                if (FaceReviewPolicy.IsOk(counted)) totals.Set(queue, counted.Total);
            }
            RenderReviewCount(totals.Sum);
        }
        catch (Exception ex)
        {
            DebugLog.Warn("PeopleView review count refresh failed: " + ex.Message);
        }
    }

    private void RenderReviewCount(long total)
    {
        ReviewSuggestionsButton.Visibility = total > 0 ? Visibility.Visible : Visibility.Collapsed;
        ReviewSuggestionsText.Text = FaceReviewPolicy.ButtonText(total);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(ReviewSuggestionsButton, FaceReviewPolicy.Header(total));
    }

    private async void OnReviewSuggestionsClicked(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync("PeopleView.OnReviewSuggestionsClicked", async () =>
        {
            var panel = new FaceReviewPanel { MinWidth = 640, MinHeight = 440 };
            panel.SetPerson(null);
            panel.TotalChanged += (_, total) => RenderReviewCount(total);
            var dialog = new ContentDialog
            {
                XamlRoot = XamlRoot,
                Title = "Review suggestions",
                Content = panel,
                CloseButtonText = "Done",
                DefaultButton = ContentDialogButton.Close,
            };
            _ = panel.ReloadAsync();
            try { await dialog.ShowAsync(); } catch { /* dialog already open */ }
            await ViewModel.RefreshAsync(CancellationToken.None);
            await RefreshReviewCountAsync();
        });

    // Counts the is_unknown clusters hidden by the HideUnknown setting and offers a one-tap reveal.

    private async void UpdateHiddenUnknownsFooter()
    {
        if (_unloaded) return;
        int hiddenCount = 0;
        try
        {
            hiddenCount = await Task.Run(() =>
            {
                try
                {
                    if (!System.IO.File.Exists(AppPaths.DbPath)) return 0;
                    using var conn = new Microsoft.Data.Sqlite.SqliteConnection(
                        new Microsoft.Data.Sqlite.SqliteConnectionStringBuilder
                        {
                            DataSource = AppPaths.DbPath,
                            Mode = Microsoft.Data.Sqlite.SqliteOpenMode.ReadOnly,
                        }.ToString());
                    conn.Open();
                    using var cmd = conn.CreateCommand();
                    cmd.CommandText = "SELECT COUNT(*) FROM persons WHERE is_unknown = 1";
                    var v = cmd.ExecuteScalar();
                    return v is null ? 0 : Convert.ToInt32(v);
                }
                catch { return 0; }
            }).ConfigureAwait(true);
        }
        catch { hiddenCount = 0; }

        if (_unloaded) return;
        bool hideUnknown = false;
        try { hideUnknown = AppViewModel.Instance.Settings.PeopleHideUnknown; } catch { /* default false */ }
        // A disposed-XAML race here would otherwise fast-fail the dispatcher.
        try
        {
            if (hiddenCount == 0 || !hideUnknown)
            {
                HiddenUnknownsFooter.Visibility = Visibility.Collapsed;
                return;
            }
            HiddenUnknownsFooter.Visibility = Visibility.Visible;
            HiddenUnknownsText.Text = hiddenCount == 1
                ? "1 unknown person is hidden"
                : $"{hiddenCount} unknown people are hidden";
            HiddenUnknownsButtonText.Text = "Show";
        }
        catch (Exception ex)
        {
            DebugLog.Warn("UpdateHiddenUnknownsFooter UI update threw (view unloaded?): " + ex.Message);
        }
    }

    // Shows once at least one cluster is named (and not unknown), nudging the user to Deep Analyze, which uses the names in captions and filenames.

    private async void RefreshContinueToDeepAnalyzeBanner()
    {
        if (_unloaded) return;
        int named = 0;
        try
        {
            named = await Task.Run(() =>
            {
                try
                {
                    if (!System.IO.File.Exists(AppPaths.DbPath)) return 0;
                    using var conn = new Microsoft.Data.Sqlite.SqliteConnection(
                        new Microsoft.Data.Sqlite.SqliteConnectionStringBuilder
                        {
                            DataSource = AppPaths.DbPath,
                            Mode = Microsoft.Data.Sqlite.SqliteOpenMode.ReadOnly,
                        }.ToString());
                    conn.Open();
                    using var cmd = conn.CreateCommand();
                    cmd.CommandText =
                        "SELECT COUNT(*) FROM persons WHERE (name IS NOT NULL OR first_name IS NOT NULL) AND IFNULL(is_unknown, 0) = 0";
                    var v = cmd.ExecuteScalar();
                    return v is null ? 0 : Convert.ToInt32(v);
                }
                catch { return 0; }
            }).ConfigureAwait(true);
        }
        catch { named = 0; }

        if (_unloaded) return;
        try
        {
            ContinueToDeepAnalyzeBanner.Visibility =
                named > 0 ? Visibility.Visible : Visibility.Collapsed;
        }
        catch (Exception ex)
        {
            DebugLog.Warn("RefreshContinueToDeepAnalyzeBanner UI update threw (view unloaded?): " + ex.Message);
        }
    }

    private void OnContinueToDeepAnalyzeClicked(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun("PeopleView.OnContinueToDeepAnalyzeClicked", () =>
        {
            string model = "qwen2_5_vl_7b";
            try { model = AppViewModel.Instance.Settings.SelectedVlmModelKind; }
            catch { /* fall back to default model */ }
            _ = EngineClient.Instance.DeepAnalyzeAllAsync(model, skipExisting: true);
            AppViewModel.Instance.ActiveTab = SidebarTab.DeepAnalyze;
        });

    private async void OnToggleHiddenUnknowns(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync(nameof(OnToggleHiddenUnknowns), async () =>
        {
            try
            {
                // Use the shared singleton: a fresh instance's Save() would cancel the singleton's pending debounced write.
                var s = AppViewModel.Instance.Settings;
                s.PeopleHideUnknown = false;
                s.Save();
            }
            catch (Exception ex) { DebugLog.Warn("Toggle unknowns save threw: " + ex.Message); }
            try { await ViewModel.RefreshAsync(CancellationToken.None); }
            catch (Exception ex) { DebugLog.Warn("Toggle unknowns refresh threw: " + ex.Message); }
            UpdateHiddenUnknownsFooter();
        });

    private void OnViewModelPropertyChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun(nameof(OnViewModelPropertyChanged), () => OnViewModelPropertyChangedCore(sender, e));

    private void OnViewModelPropertyChangedCore(object? sender, PropertyChangedEventArgs e)
    {
        if (_unloaded) return;
        OnPropertyChanged(nameof(StatusText));
        OnPropertyChanged(nameof(FooterVisibility));
    }

    private void OnClustersCollectionChanged(object? sender, System.Collections.Specialized.NotifyCollectionChangedEventArgs e)
    {
        if (_unloaded) return;
        OnPropertyChanged(nameof(StatusText));
        OnPropertyChanged(nameof(FooterVisibility));
        RefreshContinueToDeepAnalyzeBanner();

        // A re-cluster mid-selection must re-wire checkboxes and the count.
        if (!ViewModel.IsSelectMode) return;

        if (e.OldItems != null)
        {
            foreach (var removed in e.OldItems)
            {
                if (removed is PersonCluster oc) oc.PropertyChanged -= OnClusterIsSelectedChanged;
            }
        }

        // Only the final collection matters, so coalesce into one deferred pass; per-delta work is O(N^2) when a re-cluster replaces most instances.
        if (_selectMaintenancePending) return;
        _selectMaintenancePending = true;
        DispatcherQueue.TryEnqueue(() =>
        {
            _selectMaintenancePending = false;
            if (_unloaded || !ViewModel.IsSelectMode) return;
            // Detaching before re-attaching avoids re-entrancy and double subscription; selection is restored by stable ClusterId.
            foreach (var c in ViewModel.Clusters) c.PropertyChanged -= OnClusterIsSelectedChanged;
            ReprojectSelection(ViewModel.Clusters, _selectedClusterIds);
            foreach (var c in ViewModel.Clusters) c.PropertyChanged += OnClusterIsSelectedChanged;
            PublishCheckedPeople();
            UpdateCheckboxVisibility();
            UpdateSelectionCountText();
        });
    }

    private void OnUnloaded(object sender, RoutedEventArgs e)
    {
        _unloaded = true;
        PersonSelectionRegistry.Instance.Clear();
        Unloaded -= OnUnloaded;
        Loaded -= OnLoadedAsync;
        try { ViewModel.PropertyChanged -= OnViewModelPropertyChanged; } catch { /* swallow */ }
        try { ViewModel.Clusters.CollectionChanged -= OnClustersCollectionChanged; } catch { /* swallow */ }
        try { foreach (var c in ViewModel.Clusters) c.PropertyChanged -= OnClusterIsSelectedChanged; } catch { /* swallow */ }
        try { FileID.ViewModels.EngineClient.Instance.PropertyChanged -= OnEngineClientChanged; } catch { /* swallow */ }
        try { ChangeLog.Instance.Changed -= OnChangeLogChanged; } catch { /* swallow */ }
        // Disposing cancels in-flight RefreshAsync tasks so they unwind instead of touching detached state.
        try { ViewModel.Dispose(); } catch { /* swallow */ }
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
                return "Loading people…";
            }
            string coverage = ViewModels.PeopleViewModel.DeferredFaceStatus(ViewModels.EngineClient.Instance.LastFaceClustering?.DeferredFaces);
            if (ViewModel.Clusters.Count == 0)
            {
                return string.IsNullOrEmpty(coverage) ? "No people yet. Scan a folder with photos; faces are grouped into one card per person." : coverage;
            }
            return $"{ViewModel.Clusters.Count:N0} {(ViewModel.Clusters.Count == 1 ? "person" : "people")}" + (string.IsNullOrEmpty(coverage) ? string.Empty : $" · {coverage}");
        }
    }

    public Visibility FooterVisibility =>
        ViewModel.IsLoading
        || !string.IsNullOrEmpty(ViewModel.ErrorMessage)
        || ViewModel.Clusters.Count == 0
            ? Visibility.Visible : Visibility.Collapsed;

    private async void OnContextOpenDetails(object sender, RoutedEventArgs e)
    {
        if (ViewModel.IsSelectMode) return;
        if (sender is not MenuFlyoutItem item || item.Tag is not int cid) return;
        var cluster = ViewModel.Clusters.FirstOrDefault(c => c.ClusterId == cid);
        if (cluster is null) return;
        await OpenDetailSheetAsync(cluster);
    }

    private void OnContextSuggestedMerges(object sender, RoutedEventArgs e)
    {
        if (ViewModel.IsSelectMode) return;
        OnSuggestedMergesClicked(sender, e);
    }

    private async void OnClusterEditNameClicked(object sender, RoutedEventArgs e)
    {
        if (ViewModel.IsSelectMode) return;
        if (sender is not FrameworkElement element || element.DataContext is not PersonCluster cluster) return;
        await OpenDetailSheetAsync(cluster);
    }

    private void OnClusterKeyDown(object sender, KeyRoutedEventArgs e)
    {
        var shift = Microsoft.UI.Input.InputKeyboardSource
            .GetKeyStateForCurrentThread(VirtualKey.Shift)
            .HasFlag(Windows.UI.Core.CoreVirtualKeyStates.Down);
        if (e.Key != VirtualKey.Application && !(e.Key == VirtualKey.F10 && shift)) return;
        if (sender is not FrameworkElement target) return;

        for (var parent = target.Parent as FrameworkElement; parent is not null; parent = parent.Parent as FrameworkElement)
        {
            if (parent.ContextFlyout is not { } flyout) continue;
            flyout.ShowAt(target);
            e.Handled = true;
            return;
        }
    }

    private async Task OpenDetailSheetAsync(PersonCluster pc)
    {
        if (ViewModel.IsSelectMode) return;
        var sheet = new PersonDetailSheet();
        sheet.SetPerson(pc.ClusterId, pc.DisplayName);
        PersonSelectionRegistry.Instance.OpenPerson(pc.ClusterId);
        var dialog = new ContentDialog
        {
            XamlRoot = XamlRoot,
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
        try { await dialog.ShowAsync(); }
        catch { /* dialog already open */ }
        finally { PersonSelectionRegistry.Instance.ClosePerson(pc.ClusterId); }
        await ViewModel.RefreshAsync(System.Threading.CancellationToken.None);
        await RefreshReviewCountAsync();
    }

    private async void OnSuggestedMergesClicked(object sender, RoutedEventArgs e)
    {
        var sheet = new SuggestedMergesSheet();
        var dialog = new ContentDialog
        {
            XamlRoot = XamlRoot,
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
        // The engine emits faceClusteringComplete when done; refreshing only then avoids briefly showing old data.
        try
        {
            await ViewModels.EngineClient.Instance.WaitForReadyAsync(TimeSpan.FromSeconds(15));
            await ViewModels.EngineClient.Instance.RunFaceClusteringAsync();
        }
        catch (Exception ex)
        {
            Services.DebugLog.Warn($"People Re-cluster: clustering not run — {ex.Message}");
        }
        await ViewModel.RefreshAsync(CancellationToken.None);
    }

    // OnClusterDoubleTapped has no Tag fallback, so the DataContext must be set here or a double-tap never opens the detail sheet.
    private void OnClusterElementPrepared(Microsoft.UI.Xaml.Controls.ItemsRepeater sender,
                                          Microsoft.UI.Xaml.Controls.ItemsRepeaterElementPreparedEventArgs args)
    {
        if (args.Element is not FrameworkElement el) return;
        var cluster = (args.Index >= 0 && args.Index < ViewModel.Clusters.Count)
            ? ViewModel.Clusters[args.Index]
            : el.DataContext as PersonCluster;
        if (cluster is null) return;
        el.DataContext = cluster;

        // ItemsRepeater recycles cards, so the one-shot UpdateCheckboxVisibility() walk misses cards realized later; their checkbox would never appear or stay stale.
        el.DispatcherQueue?.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low, () =>
        {
            if (_unloaded) return;
            var selectVisible = ViewModel.IsSelectMode ? Visibility.Visible : Visibility.Collapsed;
            foreach (var d in EnumerateDescendants(el))
            {
                if (FindCheckBoxInTree(d) is { } cb) cb.Visibility = selectVisible;
            }
        });
    }

    private static System.Collections.Generic.IEnumerable<DependencyObject> EnumerateDescendants(DependencyObject root)
    {
        var stack = new System.Collections.Generic.Stack<DependencyObject>();
        stack.Push(root);
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

    private void OnClusterDragStarting(UIElement sender, DragStartingEventArgs args)
    {
        if (ViewModel.IsSelectMode) { args.Cancel = true; return; }
        if (sender is FrameworkElement el && el.DataContext is PersonCluster pc)
        {
            args.Data.Properties.Add(MergeFormatId, (long)pc.ClusterId);
            args.Data.RequestedOperation = DataPackageOperation.Move;
        }
        else if (sender is FrameworkElement el2 && el2.Tag is int pid)
        {
            args.Data.Properties.Add(MergeFormatId, (long)pid);
            args.Data.RequestedOperation = DataPackageOperation.Move;
        }
    }

    private void OnClusterDragOver(object sender, DragEventArgs args)
    {
        if (ViewModel.IsSelectMode)
        {
            args.AcceptedOperation = DataPackageOperation.None;
            return;
        }
        if (args.DataView.Properties.ContainsKey(MergeFormatId))
        {
            args.AcceptedOperation = DataPackageOperation.Move;
            if (sender is Grid g)
            {
                g.BorderBrush = _goldBrush;
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
            g.BorderBrush = FileID.Services.ThemeHelper.GetBrushSafe("CardStrokeColorDefaultBrush");
            g.BorderThickness = new Thickness(1);
        }
    }

    private void OnClusterTapped(object sender, Microsoft.UI.Xaml.Input.TappedRoutedEventArgs e)
    {
        if (ViewModel.IsSelectMode)
        {
            if (e.OriginalSource is CheckBox) { e.Handled = true; return; }
            if (sender is FrameworkElement el && el.DataContext is PersonCluster pc)
            {
                pc.IsSelected = !pc.IsSelected;
                e.Handled = true;
            }
        }
    }

    private async void OnClusterDoubleTapped(object sender, Microsoft.UI.Xaml.Input.DoubleTappedRoutedEventArgs e)
    {
        if (ViewModel.IsSelectMode)
        {
            e.Handled = true;
            return;
        }
        if (sender is not FrameworkElement el || el.DataContext is not PersonCluster pc) return;

        var sheet = new PersonDetailSheet();
        sheet.SetPerson(pc.ClusterId, pc.DisplayName);
        PersonSelectionRegistry.Instance.OpenPerson(pc.ClusterId);
        var dialog = new ContentDialog
        {
            XamlRoot = XamlRoot,
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
        try { await dialog.ShowAsync(); }
        catch { /* dialog already open */ }
        finally { PersonSelectionRegistry.Instance.ClosePerson(pc.ClusterId); }
        await ViewModel.RefreshAsync(System.Threading.CancellationToken.None);
        await RefreshReviewCountAsync();
    }

    private async void OnClusterDrop(object sender, DragEventArgs args)
    {
        if (ViewModel.IsSelectMode) return;
        if (sender is not Grid g) return;
        // Restore styling first so a failure mid-drop doesn't leave the gold ring.
        g.BorderBrush = FileID.Services.ThemeHelper.GetBrushSafe("CardStrokeColorDefaultBrush");
        g.BorderThickness = new Thickness(1);

        if (!args.DataView.Properties.TryGetValue(MergeFormatId, out var raw)) return;
        if (raw is not long sourceId) return;

        long destId;
        if (g.Tag is int t) destId = t;
        else if (g.DataContext is PersonCluster pc) destId = pc.ClusterId;
        else return;

        if (sourceId == destId) return; // no-op self-drop

        if (System.Threading.Interlocked.CompareExchange(ref _bulkOpInFlight, 1, 0) != 0) return;
        try
        {
            var srcCluster = ViewModel.Clusters.FirstOrDefault(c => c.ClusterId == sourceId);
            var dstCluster = ViewModel.Clusters.FirstOrDefault(c => c.ClusterId == destId);
            var srcName = srcCluster?.DisplayName ?? $"Person #{sourceId}";
            var dstName = dstCluster?.DisplayName ?? $"Person #{destId}";

            var confirm = new ContentDialog
            {
                XamlRoot = XamlRoot,
                Title = "Combine these cards?",
                Content = $"Merge \"{srcName}\" ({srcCluster?.MemberCount ?? 0} faces) into \"{dstName}\" ({dstCluster?.MemberCount ?? 0} faces)?\n\nAll faces from {srcName} move to {dstName}. {PersonMergePolicy.UndoHint}",
                PrimaryButtonText = "Merge",
                CloseButtonText = "Cancel",
                DefaultButton = ContentDialogButton.Primary,
            };
            var choice = await confirm.ShowAsync();
            if (choice != ContentDialogResult.Primary) return;

            long[] sourceFaceIds;
            try
            {
                sourceFaceIds = await ReadPersonFaceIdsAsync(sourceId);
                if (sourceFaceIds.Length == 0)
                {
                    await ShowAlertAsync("Merge unavailable", "The source person has no faces to move. Refresh People and try again.");
                    return;
                }
            }
            catch (Exception ex)
            {
                DebugLog.Warn("Could not prepare merge undo: " + ex.Message);
                await ShowAlertAsync("Merge unavailable", "FileID couldn't prepare a reliable undo record. Refresh People and try again.");
                return;
            }

            var r = await ViewModels.EngineClient.Instance.WaitForBulkActionResultAsync(
                "mergeClusters",
                () => ViewModels.EngineClient.Instance.MergeClustersAsync(sourceId, destId),
                TimeSpan.FromSeconds(30));
            if (r.Failed > 0 || r.Succeeded == 0)
            {
                var detail = r.Messages.FirstOrDefault(m => m is not null && !m.Ok)?.Message
                             ?? (r.Messages.Count > 0 ? r.Messages[0] : null)?.Message
                             ?? "The engine did not confirm the merge.";
                await ShowAlertAsync("Merge failed", detail);
            }
            else
            {
                OfferMergeUndo(r, sourceId, destId, sourceFaceIds);
            }
            await ViewModel.RefreshAsync(System.Threading.CancellationToken.None);
        }
        catch (Exception ex)
        {
            DebugLog.Warn("MergeClusters drop IPC failed: " + ex.Message);
            await ShowAlertAsync("Merge failed",
                $"Couldn't merge #{sourceId} into #{destId} — {SqliteErrorTranslator.Humanize(ex)}");
        }
        finally
        {
            System.Threading.Interlocked.Exchange(ref _bulkOpInFlight, 0);
        }
    }

    internal static void OfferMergeUndo(
        BulkActionResult merged,
        long sourcePersonId,
        long destinationPersonId,
        IReadOnlyList<long> faceIdsToRevert)
    {
        if (PersonMergePolicy.OperationId(merged) is not { } operationId)
        {
            if (faceIdsToRevert.Count > 0) PushMergeUndo(sourcePersonId, destinationPersonId, faceIdsToRevert);
            return;
        }
        UndoStack.Instance.Push(PersonMergePolicy.UndoLabel, ChangePage.People, async () =>
        {
            try
            {
                await EngineClient.Instance.WaitForReadyAsync(TimeSpan.FromSeconds(30));
                var page = await EngineClient.Instance.UndoFaceReviewAsync(operationId);
                return FaceReviewPolicy.IsOk(page);
            }
            catch (Exception ex)
            {
                DebugLog.Warn("People merge undo failed: " + ex.Message);
                return false;
            }
        });
    }

    internal static void PushMergeUndo(long sourcePersonId, long destinationPersonId, IReadOnlyList<long> faceIdsToRevert)
    {
        var faceIds = faceIdsToRevert.ToArray();
        UndoStack.Instance.Push(PersonMergePolicy.UndoLabel, ChangePage.People, async () =>
        {
            var engine = EngineClient.Instance;
            try
            {
                await engine.WaitForReadyAsync(TimeSpan.FromSeconds(30));
                var r = await engine.WaitForBulkActionResultAsync(
                    "revertMerge",
                    () => engine.RevertMergeAsync(sourcePersonId, destinationPersonId, faceIds),
                    TimeSpan.FromSeconds(30));
                var restored = r.Failed == 0 && r.Succeeded > 0;
                if (restored)
                {
                    await engine.RunFaceClusteringAsync();
                }
                return restored;
            }
            catch (Exception ex)
            {
                DebugLog.Warn("People merge undo failed: " + ex.Message);
                return false;
            }
        });
    }

    internal static Task<long[]> ReadPersonFaceIdsAsync(long personId) => Task.Run(() =>
    {
        var connectionString = new SqliteConnectionStringBuilder
        {
            DataSource = AppPaths.DbPath,
            Mode = SqliteOpenMode.ReadOnly,
        }.ToString();
        using var connection = new SqliteConnection(connectionString);
        connection.Open();
        using var command = connection.CreateCommand();
        command.CommandText = "SELECT id FROM face_prints WHERE person_id = @personId ORDER BY id";
        command.Parameters.AddWithValue("@personId", personId);
        using var reader = command.ExecuteReader();
        var faceIds = new List<long>();
        while (reader.Read()) faceIds.Add(reader.GetInt64(0));
        return faceIds.ToArray();
    });

    private void OnToggleSelectMode(object sender, RoutedEventArgs e)
    {
        ViewModel.IsSelectMode = !ViewModel.IsSelectMode;
        SelectButtonText.Text = ViewModel.IsSelectMode ? "Done" : "Select";
        BulkActionBar.Visibility = ViewModel.IsSelectMode ? Visibility.Visible : Visibility.Collapsed;
        _selectedClusterIds.Clear();
        PublishCheckedPeople();
        // ItemsRepeater has no ItemContainerStyle, so walk the realized children.
        UpdateCheckboxVisibility();
        UpdateSelectionCountText();
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
        => DebugLog.SafeRun(nameof(OnClusterIsSelectedChanged), () => OnClusterIsSelectedChangedCore(sender, e));

    private void OnClusterIsSelectedChangedCore(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName != nameof(PersonCluster.IsSelected)) return;
        // The id-keyed set lets a later instance-replacing refresh re-project the selection.
        if (sender is PersonCluster pc)
        {
            if (pc.IsSelected) _selectedClusterIds.Add(pc.ClusterId);
            else _selectedClusterIds.Remove(pc.ClusterId);
            PublishCheckedPeople();
        }
        UpdateSelectionCountText();
    }

    private void PublishCheckedPeople() =>
        PersonSelectionRegistry.Instance.SetChecked(ViewModel.Clusters.Where(c => c.IsSelected).Select(c => (long)c.ClusterId));

    // Static so the selection survives an instance-replacing refresh and is testable without the UI runtime.
    internal static void ReprojectSelection(
        System.Collections.Generic.IEnumerable<PersonCluster> clusters,
        System.Collections.Generic.ISet<int> selectedIds)
    {
        foreach (var c in clusters)
        {
            bool want = selectedIds.Contains(c.ClusterId);
            if (c.IsSelected != want) c.IsSelected = want;
        }
    }

    private void UpdateSelectionCountText()
    {
        var n = ViewModel.SelectedCount;
        BulkSelectionText.Text = n switch
        {
            0 => "Choose cards to combine or hide",
            1 => "1 selected",
            _ => $"{n} selected",
        };
        BulkMergeButton.IsEnabled = n >= 2;
        BulkUnknownButton.IsEnabled = n >= 1;
    }

    private void UpdateCheckboxVisibility()
    {
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
        if (System.Threading.Interlocked.CompareExchange(ref _bulkOpInFlight, 1, 0) != 0) return;
        try
        {
            var ids = ViewModel.SelectedClusterIds;
            if (ids.Count < 2) return;

            var selectedClusters = ViewModel.Clusters.Where(c => ids.Contains(c.ClusterId)).ToList();
            if (selectedClusters.Count < 2) return;

            var combo = new ComboBox
            {
                HorizontalAlignment = HorizontalAlignment.Stretch,
                Margin = new Thickness(0, 12, 0, 0),
                DisplayMemberPath = nameof(PersonCluster.DisplayNameWithCount),
                ItemsSource = selectedClusters,
                SelectedIndex = 0,
            };

            var stack = new StackPanel { Spacing = 6 };
            stack.Children.Add(new TextBlock
            {
                Text = "Which card should stay?",
                Style = Application.Current.Resources["BodyStrongTextBlockStyle"] as Style,
            });
            stack.Children.Add(new TextBlock
            {
                Text = $"Combine {selectedClusters.Count} cards into one person. Choose the card to keep; photos from the other cards move into it, and those cards go away. {PersonMergePolicy.UndoHint}",
                Style = Application.Current.Resources["CaptionTextBlockStyle"] as Style,
                Foreground = FileID.Services.ThemeHelper.GetBrushSafe("TextFillColorSecondaryBrush"),
                TextWrapping = TextWrapping.Wrap,
            });
            stack.Children.Add(combo);

            var dialog = new ContentDialog
            {
                XamlRoot = XamlRoot,
                Title = $"Combine {selectedClusters.Count} cards into one person",
                Content = stack,
                PrimaryButtonText = "Merge",
                CloseButtonText = "Cancel",
                DefaultButton = ContentDialogButton.Primary,
            };

            var choice = await dialog.ShowAsync();
            if (choice != ContentDialogResult.Primary || combo.SelectedItem is not PersonCluster targetCluster) return;

            int dest = targetCluster.ClusterId;
            var sourcesToMerge = ids.Where(id => id != dest).ToList();

            int merged = 0;
            int failed = 0;
            string? firstFailure = null;
            for (int i = 0; i < sourcesToMerge.Count; i++)
            {
                var srcId = sourcesToMerge[i];
                try
                {
                    var sourceFaceIds = await ReadPersonFaceIdsAsync(srcId);
                    if (sourceFaceIds.Length == 0)
                    {
                        failed++;
                        firstFailure ??= $"Person #{srcId} has no faces to move.";
                        continue;
                    }
                    var r = await EngineClient.Instance.WaitForBulkActionResultAsync(
                        "mergeClusters",
                        () => EngineClient.Instance.MergeClustersAsync(srcId, dest),
                        TimeSpan.FromSeconds(30));
                    if (r.Failed > 0 || r.Succeeded == 0)
                    {
                        failed++;
                        firstFailure ??= r.Messages.FirstOrDefault(m => m is not null && !m.Ok)?.Message
                                         ?? (r.Messages.Count > 0 ? r.Messages[0] : null)?.Message
                                         ?? $"#{srcId} could not be merged.";
                    }
                    else
                    {
                        merged++;
                        OfferMergeUndo(r, srcId, dest, sourceFaceIds);
                    }
                }
                catch (Exception ex)
                {
                    DebugLog.Warn("BulkMerge IPC failed: " + ex.Message);
                    failed++;
                    firstFailure ??= SqliteErrorTranslator.Humanize(ex);
                }
            }
            DebugLog.Info($"Bulk-merged {merged} clusters into {dest}; {failed} failed");
            if (failed > 0)
            {
                await ShowAlertAsync("Some merges failed",
                    $"Merged {merged} into #{dest}; {failed} failed — {firstFailure}");
            }
            ViewModel.IsSelectMode = false;
            _selectedClusterIds.Clear();
            PublishCheckedPeople();
            BulkActionBar.Visibility = Visibility.Collapsed;
            SelectButtonText.Text = "Select";
            UpdateCheckboxVisibility();
            await ViewModel.RefreshAsync(CancellationToken.None);
        }
        finally
        {
            System.Threading.Interlocked.Exchange(ref _bulkOpInFlight, 0);
        }
    }

    private async void OnBulkMarkUnknownClicked(object sender, RoutedEventArgs e)
    {
        if (System.Threading.Interlocked.CompareExchange(ref _bulkOpInFlight, 1, 0) != 0) return;
        try
        {
            var ids = ViewModel.SelectedClusterIds;
            if (ids.Count == 0) return;
            var longIds = new System.Collections.Generic.List<long>(ids.Count);
            foreach (var id in ids) longIds.Add(id);
            try
            {
                // On failure stay in select mode so the user can retry.
                var r = await EngineClient.Instance.WaitForBulkActionResultAsync(
                    "markPersonsAsUnknown",
                    () => EngineClient.Instance.MarkPersonsAsUnknownAsync(longIds),
                    TimeSpan.FromSeconds(30));
                if (r.Failed > 0 || r.Succeeded == 0)
                {
                    var detail = r.Messages.FirstOrDefault(m => m is not null && !m.Ok)?.Message
                                 ?? (r.Messages.Count > 0 ? r.Messages[0] : null)?.Message
                                 ?? "The engine did not confirm the change.";
                    await ShowAlertAsync("Mark as unknown failed",
                        $"Couldn't mark {ids.Count} cluster{(ids.Count == 1 ? "" : "s")} as unknown — {detail}");
                    return;
                }
                DebugLog.Info($"Marked {ids.Count} clusters as unknown");
            }
            catch (Exception ex)
            {
                DebugLog.Warn("BulkMarkUnknown IPC failed: " + ex.Message);
                await ShowAlertAsync("Mark as unknown failed",
                    $"Couldn't mark {ids.Count} cluster{(ids.Count == 1 ? "" : "s")} as unknown — {SqliteErrorTranslator.Humanize(ex)}");
                return;
            }
            ViewModel.IsSelectMode = false;
            _selectedClusterIds.Clear();
            PublishCheckedPeople();
            BulkActionBar.Visibility = Visibility.Collapsed;
            SelectButtonText.Text = "Select";
            UpdateCheckboxVisibility();
            await ViewModel.RefreshAsync(CancellationToken.None);
        }
        finally
        {
            System.Threading.Interlocked.Exchange(ref _bulkOpInFlight, 0);
        }
    }

    // ShowAsync throws on a broken XamlRoot (mid-shutdown, tab re-host); a failed alert must not reach UnhandledException.
    private async Task ShowAlertAsync(string title, string body)
    {
        try
        {
            if (_unloaded || XamlRoot is null)
            {
                DebugLog.Warn($"PeopleView.ShowAlertAsync: XamlRoot null/unloaded ({title}); skipping dialog.");
                return;
            }
            var dialog = new ContentDialog
            {
                XamlRoot = XamlRoot,
                Title = title,
                Content = body,
                CloseButtonText = "OK",
                DefaultButton = ContentDialogButton.Close,
            };
            await dialog.ShowAsync();
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"PeopleView.ShowAlertAsync threw ({title}): " + ex.Message);
        }
    }

    public event PropertyChangedEventHandler? PropertyChanged;
    private void OnPropertyChanged(string name)
        => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}
