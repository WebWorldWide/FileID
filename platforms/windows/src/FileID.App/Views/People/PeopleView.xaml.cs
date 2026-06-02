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

    private bool _unloaded;
    public PeopleView()
    {
        ViewModel = new PeopleViewModel(AppPaths.DbPath, Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread());
        InitializeComponent();
        // Named handlers (not inline lambdas) so OnUnloaded can detach
        // them. Inline lambdas leak the view + VM graph (~hundreds of KB)
        // every time the tab is swapped + can fire after the view is
        // detached, touching disposed XAML — a known cause of the
        // "click sidebar mid-scan → app crash" symptom.
        ViewModel.PropertyChanged += OnViewModelPropertyChanged;
        ViewModel.Clusters.CollectionChanged += OnClustersCollectionChanged;
        // auto-refresh on FaceClusteringComplete. Without this,
        // a user who runs `runFaceClustering` (or AutoPilot's clustering
        // stage) while sitting on the People tab sees zero update until
        // they leave + re-enter the tab. Subscribe to the engine event
        // and call RefreshAsync inline; the _unloaded guard prevents a
        // late-firing dispatcher continuation from touching disposed state.
        FileID.ViewModels.EngineClient.Instance.PropertyChanged += OnEngineClientChanged;
        Loaded += OnLoadedAsync;
        Unloaded += OnUnloaded;
    }

    private void OnEngineClientChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("PeopleView.OnEngineClientChanged", () =>
        {
            if (_unloaded) return;
            if (e.PropertyName != nameof(FileID.ViewModels.EngineClient.LastFaceClustering)) return;
            DebugLog.Debug($"[ENGINE-SUB:PeopleView] {e.PropertyName}");
            DispatcherQueue.TryEnqueue(async () =>
            {
                if (_unloaded) return;
                try { await ViewModel.RefreshAsync(CancellationToken.None); }
                catch (Exception ex) { DebugLog.Warn("PeopleView post-clustering refresh threw: " + ex.Message); }
            });
        });

    private async void OnLoadedAsync(object sender, RoutedEventArgs e)
    {
        if (_unloaded) return;
        try { await ViewModel.RefreshAsync(CancellationToken.None); }
        catch (Exception ex) { DebugLog.Warn("PeopleView.OnLoaded refresh threw: " + ex.Message); }
        UpdateHiddenUnknownsFooter();
    }

    // ───── Hidden-unknowns footer ─────────────────────────────────────
    // Tracks how many is_unknown=1 clusters are currently filtered out
    // by the global HideUnknown setting; surfaces a one-tap reveal so
    // the user can flip the visibility without diving into Settings.
    // Matches macOS PeopleView's bottom-strip behavior.

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
                    var conn = new Microsoft.Data.Sqlite.SqliteConnection(
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
        try { hideUnknown = AppSettings.Load().PeopleHideUnknown; } catch { /* default false */ }
        // Defensive: view may have unloaded during the DB-read await.
        // Wrap UI mutations in try/catch so a disposed-XAML race doesn't
        // surface as a dispatcher fast-fail.
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

    // One-tap reveal: flips the global PeopleHideUnknown setting off and
    // refreshes. The Settings tab's toggle re-syncs to the new value next
    // time the user opens Settings. Mirrors macOS's "Show hidden" link in
    // the bottom strip.
    private async void OnToggleHiddenUnknowns(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync(nameof(OnToggleHiddenUnknowns), async () =>
        {
            try
            {
                var s = AppSettings.Load();
                s.PeopleHideUnknown = false;
                s.Save();
            }
            catch (Exception ex) { DebugLog.Warn("Toggle unknowns save threw: " + ex.Message); }
            try { await ViewModel.RefreshAsync(CancellationToken.None); }
            catch (Exception ex) { DebugLog.Warn("Toggle unknowns refresh threw: " + ex.Message); }
            UpdateHiddenUnknownsFooter();
        });

    private void OnViewModelPropertyChanged(object? sender, PropertyChangedEventArgs e)
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
    }

    private void OnUnloaded(object sender, RoutedEventArgs e)
    {
        _unloaded = true;
        Unloaded -= OnUnloaded;
        Loaded -= OnLoadedAsync;
        try { ViewModel.PropertyChanged -= OnViewModelPropertyChanged; } catch { /* swallow */ }
        try { ViewModel.Clusters.CollectionChanged -= OnClustersCollectionChanged; } catch { /* swallow */ }
        try { FileID.ViewModels.EngineClient.Instance.PropertyChanged -= OnEngineClientChanged; } catch { /* swallow */ }
        // Dispose the ViewModel — cancels its _disposalCts so any in-flight
        // RefreshAsync task running on a thread-pool thread unwinds with
        // OperationCanceledException instead of accessing detached state.
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
        try { await dialog.ShowAsync(); } catch { /* dialog already open */ }
        await ViewModel.RefreshAsync(System.Threading.CancellationToken.None);
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

    // ItemsRepeater + x:Bind does NOT populate the realized element's
    // DataContext (compiled bindings bypass it — same gotcha that broke
    // Library thumbnails). Resolve the cluster from the authoritative
    // repeater index and set DataContext so the drag / drop / double-tap
    // handlers that read el.DataContext resolve the right PersonCluster.
    // OnClusterDoubleTapped has no Tag fallback, so without this bridge a
    // double-tap silently returns and the person-detail sheet never opens.
    // Mirrors LibraryView.OnRepeaterElementPrepared.
    private void OnClusterElementPrepared(Microsoft.UI.Xaml.Controls.ItemsRepeater sender,
                                          Microsoft.UI.Xaml.Controls.ItemsRepeaterElementPreparedEventArgs args)
    {
        if (args.Element is not FrameworkElement el) return;
        var cluster = (args.Index >= 0 && args.Index < ViewModel.Clusters.Count)
            ? ViewModel.Clusters[args.Index]
            : el.DataContext as PersonCluster;
        if (cluster is null) return;
        el.DataContext = cluster;
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
            g.BorderBrush = FileID.Services.ThemeHelper.GetBrushSafe("CardStrokeColorDefaultBrush");
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
            XamlRoot = XamlRoot,
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
            // Await the engine's bulkActionResult instead of fire-and-forget:
            // a swallowed merge made the user think the merge happened, then
            // the refresh re-showed the old state. Surface any failure.
            var r = await ViewModels.EngineClient.Instance.WaitForBulkActionResultAsync(
                "mergeClusters",
                () => ViewModels.EngineClient.Instance.MergeClustersAsync(sourceId, destId),
                TimeSpan.FromSeconds(30));
            if (r.Failed > 0 || r.Succeeded == 0)
            {
                var detail = r.Messages.FirstOrDefault(m => m is not null && !m.Ok)?.Message
                             ?? (r.Messages.Count > 0 ? r.Messages[0] : null)?.Message
                             ?? "The engine did not confirm the merge.";
                await ShowAlertAsync("Merge failed",
                    $"Couldn't merge #{sourceId} into #{destId} — {detail}");
            }
        }
        catch (Exception ex)
        {
            DebugLog.Warn("MergeClusters drop IPC failed: " + ex.Message);
            await ShowAlertAsync("Merge failed",
                $"Couldn't merge #{sourceId} into #{destId} — {SqliteErrorTranslator.Humanize(ex)}");
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
        int merged = 0;
        int failed = 0;
        string? firstFailure = null;
        for (int i = 1; i < ids.Count; i++)
        {
            try
            {
                // Await each merge's bulkActionResult so a swallowed failure
                // can't masquerade as success (the refresh would then re-show
                // the unmerged clusters with no explanation).
                var r = await EngineClient.Instance.WaitForBulkActionResultAsync(
                    "mergeClusters",
                    () => EngineClient.Instance.MergeClustersAsync(ids[i], dest),
                    TimeSpan.FromSeconds(30));
                if (r.Failed > 0 || r.Succeeded == 0)
                {
                    failed++;
                    firstFailure ??= r.Messages.FirstOrDefault(m => m is not null && !m.Ok)?.Message
                                     ?? (r.Messages.Count > 0 ? r.Messages[0] : null)?.Message
                                     ?? $"#{ids[i]} could not be merged.";
                }
                else
                {
                    merged++;
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

    // Mirrors SidebarProcessingControl.ShowAlertAsync: a dismissible
    // ContentDialog for surfacing a failure. ShowAsync can throw on a
    // broken XamlRoot (mid-shutdown, tab re-host) so the call is wrapped
    // and logged — a failed alert must never escalate to UnhandledException.
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
