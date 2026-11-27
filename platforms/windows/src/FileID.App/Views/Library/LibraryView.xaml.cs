using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Windows.System;
using Windows.UI.Core;

namespace FileID.Views.Library;

public sealed partial class LibraryView : UserControl, INotifyPropertyChanged
{
    internal LibraryViewModel ViewModel { get; }
    private FileTile? _lastClickedTile;
    private int _trashInFlight;
    private readonly ThumbnailService _thumbnails = ThumbnailService.Shared;
    private long _lastSeenBatchIndex = -1;
    private DateTime _lastReloadAt = DateTime.MinValue;
    private static readonly TimeSpan LibraryReloadThrottle = TimeSpan.FromSeconds(1);
    private bool _unloaded;
    // ElementPrepared/ElementClearing fire on the UI thread, but LoadThumbAsync's finally-block .Remove can resume on a worker thread after ConfigureAwait(true) without a SyncContext.
    private readonly System.Collections.Concurrent.ConcurrentDictionary<FileTile, CancellationTokenSource> _inflight = new();
    private readonly ClipSearchService _clip;
    // Neither ClipSearchService.Dispose nor LibraryViewModel.Dispose releases it, so without this it leaked one connection per tab navigation.
    private readonly ReadStore _store;
    // Keyed on the element (not the FileTile, which is recreated every Reset) so the mark survives rebinds.
    private static readonly System.Runtime.CompilerServices.ConditionalWeakTable<UIElement, object> _enteredTiles = new();
    private static readonly object _enteredMarker = new();

    private int _focusedIndex = -1;
    private Microsoft.UI.Xaml.Input.KeyEventHandler? _gridKeyHandler;

    public LibraryView()
    {
        var paths = AppPaths.DbPath;
        _store = new ReadStore(paths) { Scope = AppViewModel.Instance.Scope };
        _clip = new ClipSearchService(_store);
        ViewModel = new LibraryViewModel(_store, _clip, Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread());

        InitializeComponent();
        Unloaded += OnUnloaded;
        // Inline lambdas leak the view + VM graph (~500 KB) every time the tab is navigated away from and back to.
        ViewModel.PropertyChanged += OnViewModelPropertyChanged;
        ViewModel.Items.CollectionChanged += OnItemsCollectionChanged;
        UndoStack.Instance.PropertyChanged += OnUndoStackChanged;
        EngineClient.Instance.PropertyChanged += OnEngineChanged;
        AppViewModel.Instance.LibraryQueryRequested += OnLibraryQueryRequested;

        // ItemsRepeater has NO built-in keyboard nav (unlike GridView), so we drive it ourselves.
        _gridKeyHandler = new Microsoft.UI.Xaml.Input.KeyEventHandler(OnGridPreviewKeyDown);
        GridScroller.AddHandler(UIElement.PreviewKeyDownEvent, _gridKeyHandler, handledEventsToo: true);
        GridScroller.ViewChanged += OnGridScrolled;
        SelectedPane.Store = _store;
        SelectedPane.CloseRequested += (_, _) => ClosePane();
        SelectedPane.OpenPreviewRequested += OnPaneOpenPreviewRequested;
        SelectedPane.FindSimilarRequested += OnPaneFindSimilarRequested;

        Loaded += async (_, _) =>
        {
            try
            {
                await _store.OpenAsync(CancellationToken.None);
                await ViewModel.RefreshAsync(CancellationToken.None);
            }
            catch
            {
                // ReadStore.OpenAsync sets LastOpenError on a real open failure, which LibraryViewModel surfaces into ErrorMessage (StatusText) via its service-PropertyChanged subscription — so the error shows even though this throw skips RefreshAsync.
            }
            SyncUndoPill();
            SyncBanners();
            ApplyLibraryQueryHandoff();
            StartScope();
            StartOfflineRoots();
        };
    }

    private void OnLibraryQueryRequested(object? sender, EventArgs e)
        => DebugLog.SafeRun(nameof(OnLibraryQueryRequested), () => DispatcherQueue.TryEnqueue(ApplyLibraryQueryHandoff));

    private void ApplyLibraryQueryHandoff()
    {
        if (_unloaded) return;
        if (AppViewModel.Instance.TakeLibraryQuery() is { } query)
        {
            SearchBox.Text = query;
        }
    }

    private void OnUndoStackChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName is nameof(UndoStack.CanUndo) or nameof(UndoStack.TopLabel))
        {
            DispatcherQueue.TryEnqueue(SyncUndoPill);
        }
    }

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
        => Services.DebugLog.SafeRun("LibraryView.OnEngineChanged", () =>
        {
            if (_unloaded) return;
            switch (e.PropertyName)
            {
                case nameof(EngineClient.Phase):
                    if (EngineClient.Instance.Phase == FileID.IpcSchema.ScanPhase.Completed)
                    {
                        Services.DebugLog.Info($"[ENGINE-SUB:LibraryView] {e.PropertyName}=Completed");
                        RequestLibraryRefresh(force: true);
                        DispatcherQueue.TryEnqueue(RefreshOfflineRoots);
                    }
                    break;
                case nameof(EngineClient.LastBatch):
                    var summary = EngineClient.Instance.LastBatch;
                    if (summary is null) return;
                    long batchIndex = summary.BatchIndex;
                    if (batchIndex == _lastSeenBatchIndex) return;
                    _lastSeenBatchIndex = batchIndex;
                    if (DateTime.UtcNow - _lastReloadAt < LibraryReloadThrottle) return;
                    Services.DebugLog.Info($"[ENGINE-SUB:LibraryView] {e.PropertyName} batch={batchIndex}");
                    RequestLibraryRefresh(force: false);
                    break;
                case nameof(EngineClient.DeepAnalyzeProgress):
                case nameof(EngineClient.DeepAnalyzeStarting):
                    DispatcherQueue.TryEnqueue(SyncBanners);
                    break;
                case nameof(EngineClient.LastFaceClustering):
                    DispatcherQueue.TryEnqueue(SyncBanners);
                    break;
            }
        });

    private void SyncBanners()
    {
        if (_unloaded) return;
        bool clipMissingShow = false;
        try
        {
            var q = SearchBox?.Text ?? string.Empty;
            if (q.Trim().Length >= 3 &&
                ModelInstallerService.Instance.Clip.Status != ModelInstallStatus.Installed)
            {
                clipMissingShow = true;
            }
        }
        catch { /* defensive */ }
        ClipMissingBanner.Visibility = clipMissingShow ? Visibility.Visible : Visibility.Collapsed;

        var dap = EngineClient.Instance.DeepAnalyzeProgress;
        if (dap is not null && dap.Processed < dap.Total)
        {
            DeepAnalyzeBanner.Visibility = Visibility.Visible;
            var current = string.IsNullOrEmpty(dap.CurrentPath)
                ? string.Empty
                : System.IO.Path.GetFileName(dap.CurrentPath!);
            DeepAnalyzeBannerText.Text = string.IsNullOrEmpty(current)
                ? $"Describing files… {dap.Processed:N0} of {dap.Total:N0} files"
                : $"Describing files… {current} · {dap.Processed:N0} of {dap.Total:N0} files";
        }
        else
        {
            DeepAnalyzeBanner.Visibility = Visibility.Collapsed;
        }

        // The auto-trigger after a scan completes runs in <2s on typical libraries so a short banner is enough.
        FaceClusteringBanner.Visibility = Visibility.Collapsed;
    }

    private async void OnInstallClipFromBannerClicked(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync(nameof(OnInstallClipFromBannerClicked), async () =>
        {
            try
            {
                await ModelInstallerService.Instance.Clip.InstallAsync().ConfigureAwait(true);
            }
            catch (Exception ex)
            {
                DebugLog.Warn("CLIP install from banner failed: " + ex.Message);
            }
            SyncBanners();
        });

    private void RequestLibraryRefresh(bool force)
    {
        if (_unloaded) return;
        _lastReloadAt = DateTime.UtcNow;
        DispatcherQueue.TryEnqueue(async () =>
        {
            if (_unloaded) return;
            try { await ViewModel.RefreshAsync(liveScan: !force, CancellationToken.None); }
            catch (Exception ex) { Services.DebugLog.Warn($"Library refresh failed (force={force}): {ex.Message}"); }
        });
    }

    private void SyncUndoPill()
    {
        if (UndoButton == null) return;
        var stack = UndoStack.Instance;
        var label = stack.TopLabelOn(ChangePage.Library);
        if (stack.CanUndoOn(ChangePage.Library))
        {
            UndoButton.Visibility = Visibility.Visible;
            UndoButtonText.Text = string.IsNullOrEmpty(label)
                ? "Undo"
                : $"Undo {label}";
        }
        else
        {
            UndoButton.Visibility = Visibility.Collapsed;
        }
    }

    /// <summary>If anything is currently selected, clear; otherwise select every visible tile.</summary>
    private void OnSelectAllClicked(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnSelectAllClicked), () =>
        {
            if (ViewModel.SelectedCount > 0)
            {
                ViewModel.ClearSelection();
                SelectAllText.Text = "Select";
            }
            else
            {
                using (ViewModel.BulkSelectionScope())
                {
                    foreach (var t in ViewModel.Items) t.IsSelected = true;
                }
                SelectAllText.Text = "Clear";
            }
            UpdateSelectionBar();
        });

    private async void OnUndoLastClicked(object sender, RoutedEventArgs e)
    {
        try
        {
            UndoButton.IsEnabled = false;
            var label = await UndoStack.Instance.UndoAsync(ChangePage.Library);
            DebugLog.Info(string.IsNullOrEmpty(label) ? "Undo: nothing to undo" : $"Undo applied: {label}");
            await ViewModel.RefreshAsync(CancellationToken.None);
        }
        catch (Exception ex)
        {
            DebugLog.Warn("OnUndoLastClicked: " + ex.Message);
        }
        finally
        {
            UndoButton.IsEnabled = true;
        }
    }

    /// <summary>Teardown order is load-bearing: the ViewModel is disposed first, then the services its in-flight tasks use, and the ReadStore last.</summary>
    private void OnUnloaded(object sender, RoutedEventArgs e)
    {
        _unloaded = true;
        Unloaded -= OnUnloaded;
        try { ViewModel.PropertyChanged -= OnViewModelPropertyChanged; } catch { /* swallow */ }
        try { ViewModel.Items.CollectionChanged -= OnItemsCollectionChanged; } catch { /* swallow */ }
        try { UndoStack.Instance.PropertyChanged -= OnUndoStackChanged; } catch { /* swallow */ }
        try { EngineClient.Instance.PropertyChanged -= OnEngineChanged; } catch { /* swallow */ }
        try { AppViewModel.Instance.LibraryQueryRequested -= OnLibraryQueryRequested; } catch { /* swallow */ }
        try { StopOfflineRoots(); } catch (Exception ex) { DebugLog.Warn("LibraryView: stop offline roots failed: " + ex.Message); }
        try { AppViewModel.Instance.PropertyChanged -= OnAppScopeChanged; } catch { /* swallow */ }
        // Cancel every in-flight thumb load so a closed tab holds no BitmapImage refs.
        foreach (var (_, cts) in _inflight)
        {
            try { cts.Cancel(); } catch { /* swallow */ }
            cts.Dispose();
        }
        _inflight.Clear();
        if (_gridKeyHandler is not null)
        {
            try { GridScroller.RemoveHandler(UIElement.PreviewKeyDownEvent, _gridKeyHandler); } catch { /* swallow */ }
            _gridKeyHandler = null;
        }
        try { GridScroller.ViewChanged -= OnGridScrolled; } catch { /* swallow */ }
        try { ViewModel.Dispose(); } catch { /* swallow */ }
        // Only after the ViewModel has cancelled its disposal CTS may the services its in-flight tasks touch be torn down.
        try { _clip.Dispose(); } catch { /* swallow */ }
        // The ReadStore goes last: ViewModel and _clip reads still use its connection.
        try { _store.Dispose(); } catch { /* swallow */ }
    }

    private void OnViewModelPropertyChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e)
        => DebugLog.SafeRun(nameof(OnViewModelPropertyChanged), () => OnViewModelPropertyChangedCore(e));

    private void OnViewModelPropertyChangedCore(System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (_unloaded) return;
        if (e.PropertyName is nameof(LibraryViewModel.IsLoading)
            or nameof(LibraryViewModel.ErrorMessage))
        {
            OnPropertyChanged(nameof(StatusText));
            OnPropertyChanged(nameof(FooterVisibility));
        }
        // A background refresh can prune selected tiles; the toolbar only re-reads the count on user input, so refresh it here to avoid a stale number.
        if (e.PropertyName is nameof(LibraryViewModel.SelectedCount))
        {
            DispatcherQueue.TryEnqueue(UpdateSelectionBar);
        }
        if (e.PropertyName is nameof(LibraryViewModel.HiddenWeaker) or nameof(LibraryViewModel.SearchSummary))
        {
            OnPropertyChanged(nameof(StatusText));
            OnPropertyChanged(nameof(FooterVisibility));
        }
        if (e.PropertyName is nameof(LibraryViewModel.TotalCount) or nameof(LibraryViewModel.SearchSummary) or nameof(LibraryViewModel.HiddenWeaker))
        {
            DispatcherQueue.TryEnqueue(SyncShownCount);
        }
    }

    private void OnItemsCollectionChanged(object? sender, System.Collections.Specialized.NotifyCollectionChangedEventArgs e)
    {
        if (_unloaded) return;
        OnPropertyChanged(nameof(StatusText));
        OnPropertyChanged(nameof(FooterVisibility));
        SyncShownCount();
        if (SelectedPane.Tile is { } paneTile
            && (e.Action == System.Collections.Specialized.NotifyCollectionChangedAction.Reset
                || e.OldItems?.Contains(paneTile) == true))
        {
            DispatcherQueue.TryEnqueue(() =>
            {
                if (!_unloaded && IsPaneOpen && !ViewModel.Items.Contains(paneTile)) ClosePane();
            });
        }
    }

    internal static string ShownCount(int shown, int? total) =>
        total is { } all
            ? $"{shown:N0} of {all:N0} shown"
            : $"{shown:N0} result{(shown == 1 ? "" : "s")}";

    private void SyncShownCount()
    {
        if (_unloaded) return;
        int shown = ViewModel.Items.Count;
        var plain = shown == 0 && ViewModel.TotalCount is null or 0 ? string.Empty : ShownCount(shown, ViewModel.TotalCount);
        var summary = shown == 0 ? null : ViewModel.SearchSummary;
        var text = SearchStrengthPolicy.CountText(plain, summary);
        ShownCountText.Text = text;
        ShowWeakerButton.Visibility = ViewModel.HiddenWeaker > 0 ? Visibility.Visible : Visibility.Collapsed;
        ShowWeakerButton.Content = SearchStrengthPolicy.WeakerButtonText(ViewModel.HiddenWeaker);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(ShownCountText, SearchStrengthPolicy.AccessibleName(text, summary));
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
                return "Searching…";
            }
            if (ViewModel.Items.Count == 0)
            {
                if (!string.IsNullOrWhiteSpace(ViewModel.Query)
                    && (ViewModel.HiddenWeaker > 0 || ViewModel.SearchSummary == "No matches"))
                {
                    return SearchStrengthPolicy.NoMatchesText(ViewModel.Query.Trim(), ViewModel.HiddenWeaker);
                }
                return string.IsNullOrWhiteSpace(ViewModel.Query)
                    ? "No files yet. Files appear here as the scan reads them. If nothing is happening, press Scan in the sidebar."
                    : "Nothing matches this filter. Try other words, or choose All kinds to see every file.";
            }
            return $"{ViewModel.Items.Count} files";
        }
    }

    public Visibility FooterVisibility =>
        ViewModel.IsLoading
        || !string.IsNullOrEmpty(ViewModel.ErrorMessage)
        || ViewModel.HiddenWeaker > 0
        || ViewModel.Items.Count == 0
            ? Visibility.Visible : Visibility.Collapsed;

    private void OnShowWeakerClicked(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnShowWeakerClicked), () => ViewModel.ShowWeakerMatches());

    private void OnSearchChanged(object sender, TextChangedEventArgs e)
        => DebugLog.SafeRun(nameof(OnSearchChanged), () =>
        {
            ViewModel.Query = SearchBox.Text;
            SyncBanners();
        });

    private void OnFocusSearchAccelerator(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
        => DebugLog.SafeRun(nameof(OnFocusSearchAccelerator), () =>
        {
            SearchBox.Focus(FocusState.Keyboard);
            SearchBox.SelectAll();
            args.Handled = true;
        });

    private void OnEscapeAccelerator(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
        => DebugLog.SafeRun(nameof(OnEscapeAccelerator), () =>
        {
            if (IsPaneOpen)
            {
                ClosePane();
                args.Handled = true;
                return;
            }
            if (ViewModel.SelectedCount > 0)
            {
                ViewModel.ClearSelection();
                UpdateSelectionBar();
                args.Handled = true;
                return;
            }
            if (!string.IsNullOrEmpty(SearchBox.Text))
            {
                SearchBox.Text = string.Empty;
                args.Handled = true;
            }
        });

    private void OnSelectAllAccelerator(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
        => DebugLog.SafeRun(nameof(OnSelectAllAccelerator), () =>
        {
            if (KeyboardFocusGuard.IsTextEditing(XamlRoot)) return;
            OnSelectAllClicked(this, new RoutedEventArgs());
            args.Handled = true;
        });

    private void OnUndoAccelerator(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
        => DebugLog.SafeRun(nameof(OnUndoAccelerator), () =>
        {
            if (KeyboardFocusGuard.IsTextEditing(XamlRoot)) return;
            if (!UndoStack.Instance.CanUndoOn(ChangePage.Library)) return;
            OnUndoLastClicked(this, new RoutedEventArgs());
            args.Handled = true;
        });

    private void OnKindChanged(object sender, SelectionChangedEventArgs e)
        => DebugLog.SafeRun(nameof(OnKindChanged), () =>
        {
            if (KindFilter.SelectedItem is ComboBoxItem item && item.Tag is string tag)
            {
                ViewModel.KindFilter = tag;
            }
        });

    private void OnTileRightTapped(object sender, Microsoft.UI.Xaml.Input.RightTappedRoutedEventArgs e)
    {
        e.Handled = false;
    }

    // A thumbnail load starts on prepare; on clear it is cancelled and nulled so off-screen tiles do not hold BitmapImage refs that block GC.
    private void OnRepeaterElementPrepared(Microsoft.UI.Xaml.Controls.ItemsRepeater sender,
                                            Microsoft.UI.Xaml.Controls.ItemsRepeaterElementPreparedEventArgs args)
    {
        if (args.Element is not FrameworkElement el) return;
        // Resolve the tile from the repeater index and set DataContext so the other handlers that read el.DataContext see the same tile.
        var tile = (args.Index >= 0 && args.Index < ViewModel.Items.Count)
            ? ViewModel.Items[args.Index]
            : el.DataContext as FileTile;
        Services.DebugLog.Debug(
            $"[THUMB] PREPARE idx={args.Index} dcWasNull={el.DataContext is null} resolved={tile is not null}");
        if (tile is null) return;
        el.DataContext = tile;

        // The tile is back in the virtualization window: undo the ElementClearing detach so a fresh load can bind.
        tile.IsDetached = false;
        tile.ThumbnailFailed = false;

        AnimateTileEntry(el);

        if (tile.Thumbnail != null)
        {
            return;
        }

        var cts = new CancellationTokenSource();
        // TryAdd tolerates a re-entrant prepare.
        if (!_inflight.TryAdd(tile, cts))
        {
            cts.Dispose();
            return;
        }
        _ = LoadThumbAsync(tile, cts);
    }

    /// <summary>A scale-only entrance never hides content; on failure the tile simply snaps in without animation.</summary>
    private static void AnimateTileEntry(FrameworkElement el)
    {
        try
        {
            var visual = Microsoft.UI.Xaml.Hosting.ElementCompositionPreview.GetElementVisual(el);
            visual.StopAnimation("Opacity");
            visual.Opacity = 1f;

            bool firstEntry = !_enteredTiles.TryGetValue(el, out _);

            if (FileID.Theme.Motion.ReducedMotion.Instance.IsReduced)
            {
                visual.StopAnimation("Scale.X");
                visual.StopAnimation("Scale.Y");
                visual.Scale = new System.Numerics.Vector3(1f, 1f, 1f);
                _enteredTiles.AddOrUpdate(el, _enteredMarker);
                return;
            }

            var size = visual.Size;
            if (size.X <= 0 || size.Y <= 0)
            {
                // Ensure the tile is visible at full scale and let a later prepare (post-arrange) carry the pop.
                visual.StopAnimation("Scale.X");
                visual.StopAnimation("Scale.Y");
                visual.Scale = new System.Numerics.Vector3(1f, 1f, 1f);
                return;
            }

            if (!firstEntry)
            {
                // Already played its entrance (recycled or re-realized on a Reset): snap to rest rather than pulsing the grid on every refresh.
                visual.StopAnimation("Scale.X");
                visual.StopAnimation("Scale.Y");
                visual.Scale = new System.Numerics.Vector3(1f, 1f, 1f);
                return;
            }

            visual.CenterPoint = new System.Numerics.Vector3(size.X / 2, size.Y / 2, 0);
            visual.Scale = new System.Numerics.Vector3(0.96f, 0.96f, 1f);
            var t = FileID.Theme.Motion.SpringEasing.Tokens.Tight;
            FileID.Theme.Motion.SpringEasing.AnimateScale(el, 1f, t.Response, t.DampingFraction);
            _enteredTiles.AddOrUpdate(el, _enteredMarker);
        }
        catch (Exception ex)
        {
            Services.DebugLog.Warn("AnimateTileEntry threw: " + ex.Message);
            // Snap to final state so the tile is at least visible.
            try { el.Opacity = 1; } catch { /* swallow */ }
        }
    }

    private void OnRepeaterElementClearing(Microsoft.UI.Xaml.Controls.ItemsRepeater sender,
                                           Microsoft.UI.Xaml.Controls.ItemsRepeaterElementClearingEventArgs args)
    {
        if (args.Element is not FrameworkElement el || el.DataContext is not FileTile tile) return;
        // Source is bound to Thumbnail, so a recycled element would otherwise flash the previous file's bitmap on the next reveal.
        tile.ClearThumbnailForRecycle();
        Services.DebugLog.Debug($"[THUMB] RECYCLE_NULLED file={Services.PathRedactor.Redact(tile.Path)}");
        // Mark detached so a late thumbnail render does not bind into a stale tile.
        tile.IsDetached = true;
        if (_inflight.TryRemove(tile, out var cts))
        {
            try { cts.Cancel(); } catch { /* swallow */ }
            cts.Dispose();
        }
        // ClearThumbnailForRecycle nulled Source, so a recycled tile shows the shimmer until its own thumbnail loads.
    }

    private async Task LoadThumbAsync(FileTile tile, CancellationTokenSource cts)
    {
        // Read the token synchronously: this runs on the UI thread before the first await, so ElementClearing and OnUnloaded cannot have disposed cts yet.
        var ct = cts.Token;
        try
        {
            // ConfigureAwait(false) leaves this on a worker thread; the bind is posted to the UI dispatcher below.
            var bmp = await _thumbnails.RequestAsync(tile.Path, tile.ModifiedAt, ct).ConfigureAwait(false);
            if (bmp == null)
            {
                Services.DebugLog.Debug($"[THUMB] LOAD_NULL file={Services.PathRedactor.Redact(tile.Path)}");
                // Hand off from shimmer to a broken-image placeholder so the tile doesn't shimmer forever.
                if (!tile.IsDetached)
                {
                    DispatcherQueue.TryEnqueue(() =>
                    {
                        if (_unloaded || tile.IsDetached) return;
                        tile.ThumbnailFailed = true;
                    });
                }
                return;
            }
            if (ct.IsCancellationRequested || tile.IsDetached)
            {
                Services.DebugLog.Debug($"[THUMB] LOAD_DROPPED file={Services.PathRedactor.Redact(tile.Path)} cancelled={ct.IsCancellationRequested} detached={tile.IsDetached}");
                return;
            }
            var enqueued = DispatcherQueue.TryEnqueue(() =>
            {
                // _unloaded guard: this continuation is queued from a worker thread (ConfigureAwait(false) above) and can run AFTER a tab switch tore down the view.
                if (_unloaded || tile.IsDetached) return;
                tile.Thumbnail = bmp;
                Services.DebugLog.Debug($"[THUMB] TILE_THUMBNAIL_ASSIGNED file={Services.PathRedactor.Redact(tile.Path)} px={bmp.PixelWidth}x{bmp.PixelHeight}");
            });
            if (!enqueued)
            {
                Services.DebugLog.Debug($"[THUMB] ASSIGN_ENQUEUE_FAILED file={Services.PathRedactor.Redact(tile.Path)}");
            }
        }
        catch (Exception ex)
        {
            Services.DebugLog.Debug($"[THUMB] LOAD_EX file={Services.PathRedactor.Redact(tile.Path)} ex={ex.GetType().Name}");
        }
        finally
        {
            // Compare-and-remove: a re-prepare can replace our entry with a fresh CTS for the same tile while this finally is queued.
            if (_inflight.TryRemove(new KeyValuePair<FileTile, CancellationTokenSource>(tile, cts)))
            {
                cts.Dispose();
            }
        }
    }

    private void OnTileSizeChanged(object sender, SizeChangedEventArgs e)
        => DebugLog.SafeRun(nameof(OnTileSizeChanged), () =>
        {
            if (sender is not FrameworkElement el) return;
            double w = e.NewSize.Width;
            if (w <= 0) return;
            double target = w + 68;
            if (Math.Abs(el.Height - target) > 0.5)
            {
                el.Height = target;
                Services.DebugLog.Debug($"[THUMB] TILE_SIZED w={w:F0} h={target:F0}");
            }
        });

    private void OnTilePointerEntered(object sender, PointerRoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnTilePointerEntered), () =>
        {
            if (sender is not FrameworkElement el) return;
            ApplyTileScale(el, 1.012f);
        });

    private void OnTilePointerExited(object sender, PointerRoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnTilePointerExited), () =>
        {
            if (sender is not FrameworkElement el) return;
            ApplyTileScale(el, 1.0f);
        });

    private static void ApplyTileScale(FrameworkElement el, float scale)
    {
        FileID.Theme.Motion.SpringEasing.SpringScale(el, scale, FileID.Theme.Motion.SpringEasing.Tokens.Tight);
        ApplyTileStrokeOpacity(el, hovering: scale > 1.0f);
    }

    // A brush colour is not a Composition visual property, so the hover stroke keeps the Mac's quick ease-out (Theme.quick) rather than a spring.
    private static void ApplyTileStrokeOpacity(FrameworkElement el, bool hovering)
    {
        if (el is not Grid grid) return;
        if (grid.BorderBrush is not Microsoft.UI.Xaml.Media.SolidColorBrush brush) return;
        var sb = new Microsoft.UI.Xaml.Media.Animation.Storyboard();
        var anim = new Microsoft.UI.Xaml.Media.Animation.DoubleAnimation
        {
            To = hovering ? 0.18 : 0.08,
            Duration = new Microsoft.UI.Xaml.Duration(TimeSpan.FromSeconds(0.18)),
            EasingFunction = new Microsoft.UI.Xaml.Media.Animation.CubicEase
            {
                EasingMode = Microsoft.UI.Xaml.Media.Animation.EasingMode.EaseOut,
            },
        };
        Microsoft.UI.Xaml.Media.Animation.Storyboard.SetTarget(anim, brush);
        Microsoft.UI.Xaml.Media.Animation.Storyboard.SetTargetProperty(anim, "Opacity");
        sb.Children.Add(anim);
        sb.Begin();
    }

    private void OnTileTapped(object sender, TappedRoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnTileTapped), () =>
        {
            if (sender is not FrameworkElement el || el.DataContext is not FileTile tile) return;

            _focusedIndex = ViewModel.Items.IndexOf(tile);
            try { GridScroller.Focus(FocusState.Programmatic); } catch { /* swallow */ }

            var ctrl = Microsoft.UI.Input.InputKeyboardSource
                .GetKeyStateForCurrentThread(VirtualKey.Control)
                .HasFlag(CoreVirtualKeyStates.Down);
            var shift = Microsoft.UI.Input.InputKeyboardSource
                .GetKeyStateForCurrentThread(VirtualKey.Shift)
                .HasFlag(CoreVirtualKeyStates.Down);

            if (shift && _lastClickedTile is not null)
            {
                int a = ViewModel.Items.IndexOf(_lastClickedTile);
                int b = ViewModel.Items.IndexOf(tile);
                if (a >= 0 && b >= 0)
                {
                    int lo = Math.Min(a, b);
                    int hi = Math.Max(a, b);
                    using (ViewModel.BulkSelectionScope())
                    {
                        if (!ctrl) foreach (var t in ViewModel.Items) t.IsSelected = false;
                        for (int i = lo; i <= hi; i++) ViewModel.Items[i].IsSelected = true;
                    }
                }
            }
            else if (ctrl)
            {
                tile.IsSelected = !tile.IsSelected;
                _lastClickedTile = tile;
            }
            else
            {
                using (ViewModel.BulkSelectionScope())
                {
                    foreach (var t in ViewModel.Items) t.IsSelected = false;
                    tile.IsSelected = true;
                }
                _lastClickedTile = tile;
                ShowPane(tile);
            }

            UpdateSelectionBar();
        });

    private const double PaneWidth = 312;

    private void ShowPane(FileTile tile)
    {
        bool wasHidden = PaneHost.Visibility != Visibility.Visible;
        SelectedPane.Show(tile);
        PaneColumn.Width = new GridLength(PaneWidth);
        PaneHost.Visibility = Visibility.Visible;
        if (wasHidden && !FileID.Theme.Motion.ReducedMotion.Instance.IsReduced)
        {
            var visual = Microsoft.UI.Xaml.Hosting.ElementCompositionPreview.GetElementVisual(PaneHost);
            Microsoft.UI.Xaml.Hosting.ElementCompositionPreview.SetIsTranslationEnabled(PaneHost, true);
            visual.Opacity = 0f;
            visual.Properties.InsertVector3("Translation", new System.Numerics.Vector3(24, 0, 0));
            FileID.Theme.Motion.SpringEasing.AnimateOpacity(PaneHost, 1f, 0.40, 0.80);
            FileID.Theme.Motion.SpringEasing.AnimateScalar(PaneHost, "Translation.X", 0f, 0.40, 0.80);
        }
    }

    private void ClosePane()
    {
        if (_unloaded) return;
        PaneHost.Visibility = Visibility.Collapsed;
        PaneColumn.Width = new GridLength(0);
    }

    private bool IsPaneOpen => PaneHost.Visibility == Visibility.Visible;

    private async void OnPaneOpenPreviewRequested(object? sender, EventArgs e)
        => await DebugLog.SafeRunAsync(nameof(OnPaneOpenPreviewRequested), async () =>
        {
            if (SelectedPane.Tile is not { } tile) return;
            int index = ViewModel.Items.IndexOf(tile);
            if (index < 0) return;
            await OpenPreview(tile, index);
        });

    private async void OnPaneFindSimilarRequested(object? sender, EventArgs e)
        => await DebugLog.SafeRunAsync(nameof(OnPaneFindSimilarRequested), async () =>
        {
            if (SelectedPane.Tile is not { } tile) return;
            await ViewModel.FindSimilarAsync(tile.Id, System.Threading.CancellationToken.None);
        });

    private void UpdateSelectionBar()
    {
        int count = ViewModel.SelectedCount;
        SelectionCountText.Text = count switch
        {
            0 => string.Empty,
            1 => "1 file selected",
            _ => $"{count} files selected",
        };
        SelectionBar.Visibility = count > 0 ? Visibility.Visible : Visibility.Collapsed;
        SyncAskChat(count);
    }

    private void OnGridScrolled(object? sender, ScrollViewerViewChangedEventArgs e)
    {
        if (GridScroller.VerticalOffset + GridScroller.ViewportHeight * 3 >= GridScroller.ExtentHeight)
        {
            _ = ViewModel.LoadMoreAsync();
        }
    }

    // Wired on GridScroller's tunneling PreviewKeyDown (handledEventsToo) so it preempts the ScrollViewer's built-in arrow-scroll.
    private void OnGridPreviewKeyDown(object sender, Microsoft.UI.Xaml.Input.KeyRoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnGridPreviewKeyDown), () =>
        {
            int count = ViewModel.Items.Count;
            if (count == 0) return;

            var shift = Microsoft.UI.Input.InputKeyboardSource
                .GetKeyStateForCurrentThread(VirtualKey.Shift)
                .HasFlag(CoreVirtualKeyStates.Down);
            if (e.Key == VirtualKey.Application || (e.Key == VirtualKey.F10 && shift))
            {
                int targetIndex = _focusedIndex;
                if (targetIndex < 0 && _lastClickedTile is not null)
                {
                    targetIndex = ViewModel.Items.IndexOf(_lastClickedTile);
                }

                if (targetIndex >= 0 && targetIndex < count
                    && Repeater.TryGetElement(targetIndex) is FrameworkElement target
                    && target.ContextFlyout is { } flyout)
                {
                    flyout.ShowAt(target);
                    e.Handled = true;
                }
                return;
            }

            int cur = _focusedIndex >= 0 ? Math.Min(_focusedIndex, count - 1) : 0;
            int last = count - 1;

            switch (e.Key)
            {
                case VirtualKey.Enter:
                    OpenPreviewAt(cur);
                    e.Handled = true;
                    return;
                case VirtualKey.Space:
                    ToggleSelectAt(cur);
                    e.Handled = true;
                    return;
            }

            if (_focusedIndex < 0 &&
                e.Key is VirtualKey.Left or VirtualKey.Right or VirtualKey.Up
                    or VirtualKey.Down or VirtualKey.Home or VirtualKey.End
                    or VirtualKey.PageUp or VirtualKey.PageDown)
            {
                MoveFocusTo(0, extend: false);
                e.Handled = true;
                return;
            }

            int cols = ColumnsPerRow();
            int page = cols * Math.Max(1, VisibleRows());
            int focusTarget;
            switch (e.Key)
            {
                case VirtualKey.Left: focusTarget = cur - 1; break;
                case VirtualKey.Right: focusTarget = cur + 1; break;
                case VirtualKey.Up: focusTarget = cur - cols; break;
                case VirtualKey.Down: focusTarget = cur + cols; break;
                case VirtualKey.Home: focusTarget = 0; break;
                case VirtualKey.End: focusTarget = last; break;
                case VirtualKey.PageUp: focusTarget = cur - page; break;
                case VirtualKey.PageDown: focusTarget = cur + page; break;
                default: return;
            }

            MoveFocusTo(focusTarget, extend: shift);
            e.Handled = true;
        });

    private int ColumnsPerRow()
    {
        double avail = GridScroller.ViewportWidth;
        if (avail <= 0) avail = Repeater.ActualWidth;
        const double minItemWidth = 180, colSpacing = 12;
        int cols = (int)Math.Floor((avail + colSpacing) / (minItemWidth + colSpacing));
        return Math.Max(1, cols);
    }

    private int VisibleRows()
    {
        const double rowHeight = 248 + 12;
        int rows = (int)Math.Floor(GridScroller.ViewportHeight / rowHeight);
        return Math.Max(1, rows);
    }

    private void MoveFocusTo(int target, bool extend)
    {
        int count = ViewModel.Items.Count;
        if (count == 0) return;
        target = Math.Clamp(target, 0, count - 1);

        if (extend && _lastClickedTile is not null)
        {
            int anchor = ViewModel.Items.IndexOf(_lastClickedTile);
            if (anchor >= 0)
            {
                int lo = Math.Min(anchor, target);
                int hi = Math.Max(anchor, target);
                using (ViewModel.BulkSelectionScope())
                {
                    foreach (var t in ViewModel.Items) t.IsSelected = false;
                    for (int i = lo; i <= hi; i++) ViewModel.Items[i].IsSelected = true;
                }
            }
        }
        else
        {
            var tile = ViewModel.Items[target];
            using (ViewModel.BulkSelectionScope())
            {
                foreach (var t in ViewModel.Items) t.IsSelected = false;
                tile.IsSelected = true;
            }
            _lastClickedTile = tile;
            ShowPane(tile);
        }

        _focusedIndex = target;
        BringIndexIntoView(target);
        UpdateSelectionBar();
    }

    // Realization can race a mid-scan Reset.
    private void BringIndexIntoView(int index)
    {
        try
        {
            if (Repeater.TryGetElement(index) is FrameworkElement existing)
            {
                existing.StartBringIntoView();
                return;
            }
            if (Repeater.GetOrCreateElement(index) is FrameworkElement created)
            {
                created.UpdateLayout();
                created.StartBringIntoView();
            }
        }
        catch { /* realization raced a refresh — non-fatal */ }
    }

    private async void OpenPreviewAt(int index)
        => await DebugLog.SafeRunAsync(nameof(OpenPreviewAt), async () =>
        {
            if (index < 0 || index >= ViewModel.Items.Count) return;
            await OpenPreview(ViewModel.Items[index], index);
        });

    private void ToggleSelectAt(int index)
    {
        if (index < 0 || index >= ViewModel.Items.Count) return;
        var tile = ViewModel.Items[index];
        tile.IsSelected = !tile.IsSelected;
        _focusedIndex = index;
        _lastClickedTile = tile;
        UpdateSelectionBar();
    }

    private async void OnTagSelectedClicked(object sender, RoutedEventArgs e)
    {
        var ids = ViewModel.SelectedItems.Select(t => t.Id).ToArray();
        if (ids.Length == 0) return;

        var sheet = new BulkTagSheet();
        sheet.SetSelection(ids);
        var dialog = new ContentDialog
        {
            XamlRoot = XamlRoot,
            Title = "Tag the chosen files",
            Content = sheet,
            PrimaryButtonText = "Apply",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
        };
        bool committed = false;
        dialog.PrimaryButtonClick += async (d, args) =>
        {
            var deferral = args.GetDeferral();
            var ok = await sheet.CommitAsync();
            if (!ok) args.Cancel = true; else committed = true;
            deferral.Complete();
        };
        try { await dialog.ShowAsync(); } catch { /* dialog already open */ }
        if (committed) RequestLibraryRefresh(force: true);
    }

    private async void OnRenameSelectedClicked(object sender, RoutedEventArgs e)
    {
        var selected = ViewModel.SelectedItems.ToArray();
        if (selected.Length == 0) return;

        var plan = selected.Select(t => new RenamePlan
        {
            FileId = t.Id,
            CurrentPath = t.Path,
            ProposedName = t.FileName,
            Include = true,
        }).ToArray();

        var sheet = new BulkRenameSheet();
        sheet.SetPlan(plan);
        var dialog = new ContentDialog
        {
            XamlRoot = XamlRoot,
            Title = "Rename the chosen files",
            Content = sheet,
            PrimaryButtonText = "Rename",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
        };
        bool committed = false;
        dialog.PrimaryButtonClick += async (d, args) =>
        {
            var deferral = args.GetDeferral();
            var ok = await sheet.CommitAsync();
            if (!ok) args.Cancel = true; else committed = true;
            deferral.Complete();
        };
        try { await dialog.ShowAsync(); } catch { /* dialog already open */ }
        if (committed)
        {
            // FileTile.Path and FileName are init-only and the refresh merge keeps the old instance for a surviving id, so it would keep the old name.
            foreach (var t in selected) ViewModel.Items.Remove(t);
            RequestLibraryRefresh(force: true);
        }
    }

    private async void OnTrashSelectedClicked(object sender, RoutedEventArgs e)
    {
        if (Interlocked.CompareExchange(ref _trashInFlight, 1, 0) != 0) return;
        try
        {
            var ids = ViewModel.SelectedItems.Select(t => t.Id).ToArray();
            if (ids.Length == 0) return;

            long totalBytes = ViewModel.SelectedItems.Sum(t => t.SizeBytes);
            string sizeDisplay = FormatSize(totalBytes);
            string countDisplay = ids.Length == 1 ? "1 file" : $"{ids.Length} files";

            var confirm = new ContentDialog
            {
                XamlRoot = XamlRoot,
                Title = "Move to Recycle Bin?",
                Content = $"{countDisplay} ({sizeDisplay}) will be moved to the Recycle Bin. You can recover them from there.",
                PrimaryButtonText = "Move to Recycle Bin",
                CloseButtonText = "Cancel",
                DefaultButton = ContentDialogButton.Close,
            };
            var choice = await confirm.ShowAsync();
            if (choice != ContentDialogResult.Primary) return;

            // The engine tags the BulkActionResult with "trashFiles:<batch_id>", which undo needs.
            Func<IDisposable?> beforeSend = () => Services.UndoStack.CaptureNextBulkResult(
                "trashFiles:",
                $"trash {ids.Length} file{(ids.Length == 1 ? "" : "s")}",
                async batchId =>
                {
                    if (string.IsNullOrEmpty(batchId)) return false;
                    try
                    {
                        return await EngineClient.Instance.RestoreFromTrashAsync(batchId);
                    }
                    catch { return false; }
                },
                ChangePage.Library);

            FileID.IpcSchema.BulkActionResult? result = null;
            try
            {
                result = await EngineClient.Instance.WaitForBulkActionResultAsync(
                    "trashFiles",
                    () => EngineClient.Instance.TrashFilesAsync(ids),
                    BulkActionTimeout.ForTrash(ids.Length),
                    beforeSend: beforeSend);
            }
            catch (TimeoutException ex)
            {
                Services.DebugLog.Warn("Trash timed out: " + ex.Message);
                await ShowAlertAsync(
                    "Couldn't confirm the move",
                    "FileID couldn't tell whether the files reached the Recycle Bin. Look in the Recycle Bin, then scan again before retrying.");
                return;
            }
            catch (Exception ex)
            {
                Services.DebugLog.Warn("Trash failed: " + ex.Message);
                await ShowAlertAsync("Couldn't move the files", $"The selected files weren't moved to the Recycle Bin. {ex.Message}");
                return;
            }

            // Remove only tiles the engine actually trashed (a per-file Ok in the result).
            var trashedIds = new HashSet<long>(
                result.Messages?.Where(m => m.Ok && m.FileId is not null).Select(m => m.FileId!.Value)
                    ?? Enumerable.Empty<long>());
            foreach (var id in trashedIds)
            {
                var match = ViewModel.Items.FirstOrDefault(t => t.Id == id);
                if (match is not null) ViewModel.Items.Remove(match);
            }
            UpdateSelectionBar();

            // Guarding on Failed>0 alone would stay silent on a total failure; ids is non-empty here, so Succeeded==0 is a real total failure.
            if (result.Failed > 0 || result.Succeeded == 0)
            {
                var why = Services.TrashFailureText.Summary(result);
                var body = result.Succeeded == 0 && result.Failed == 0
                    ? (why.Length > 0 ? $"Nothing was moved. {why}" : "Nothing was moved. Your files are unchanged. Try again in a moment.")
                    : $"{result.Succeeded:N0} moved to the Recycle Bin. {why}";
                await ShowAlertAsync("Some files weren't moved", body);
                RequestLibraryRefresh(force: true);
            }
        }
        finally
        {
            Interlocked.Exchange(ref _trashInFlight, 0);
        }
    }

    // Dismissible alert so a partial or failed bulk trash never looks successful.
    private async Task ShowAlertAsync(string title, string body)
    {
        try
        {
            if (_unloaded || XamlRoot is null)
            {
                DebugLog.Warn($"LibraryView.ShowAlertAsync: XamlRoot null/unloaded ({title}); skipping dialog.");
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
            DebugLog.Warn($"LibraryView.ShowAlertAsync({title}) threw: " + ex.Message);
        }
    }

    private void OnClearSelectionClicked(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnClearSelectionClicked), () =>
        {
            ViewModel.ClearSelection();
            UpdateSelectionBar();
        });

    private async void OnTileDragStarting(UIElement sender, DragStartingEventArgs args)
    {
        var deferral = args.GetDeferral();
        try
        {
            await DebugLog.SafeRunAsync(nameof(OnTileDragStarting), async () =>
            {
                var paths = ViewModel.SelectedCount > 0
                    ? ViewModel.SelectedItems.Select(t => t.Path).ToList()
                    : (sender is FrameworkElement el && el.DataContext is FileTile tile
                        ? new List<string> { tile.Path }
                        : new List<string>());
                var items = new List<Windows.Storage.IStorageItem>(paths.Count);
                foreach (var p in paths)
                {
                    // No synchronous File.Exists: on a slow or disconnected SMB share or USB stick it can stall the UI thread for seconds.
                    try
                    {
                        items.Add(await Windows.Storage.StorageFile.GetFileFromPathAsync(p));
                    }
                    catch { /* missing / inaccessible — skip */ }
                }
                if (items.Count > 0)
                {
                    args.Data.SetStorageItems(items);
                    args.Data.RequestedOperation = Windows.ApplicationModel.DataTransfer.DataPackageOperation.Copy;
                }
            });
        }
        finally
        {
            deferral.Complete();
        }
    }

    private static string FormatSize(long bytes) => FileID.Services.ByteFormat.File(bytes);

    // The tile Tag carries the absolute path; the FileTile lookup in ViewModel.Items supplies kind, size and modified. The sibling list is frozen at open time so a refresh mid-preview cannot shift indices.
    private async void OnTileDoubleTapped(object sender, Microsoft.UI.Xaml.Input.DoubleTappedRoutedEventArgs e)
    {
        if (sender is not FrameworkElement el || el.Tag is not string path) return;
        FileTile? tile = null;
        int tileIndex = -1;
        for (int i = 0; i < ViewModel.Items.Count; i++)
        {
            if (ViewModel.Items[i].Path == path) { tile = ViewModel.Items[i]; tileIndex = i; break; }
        }
        if (tile is null) return;
        await OpenPreview(tile, tileIndex);
    }

    // Opens the FilePreviewSheet modal for the given tile, freezing the sibling list at open time.
    private async System.Threading.Tasks.Task OpenPreview(FileTile tile, int tileIndex)
    {
        // Snapshot siblings so a live BatchSummary refresh mid-preview cannot reorder them.
        var siblings = ViewModel.Items.ToList();

        var sheet = new FilePreviewSheet();
        sheet.SetSiblings(siblings, tileIndex);

        var dialog = new Microsoft.UI.Xaml.Controls.ContentDialog
        {
            XamlRoot = XamlRoot,
            Content = sheet,
        };
        // Override the default ContentDialog max bounds so the sheet gets its declared 1080x720 minimum.
        dialog.Resources["ContentDialogMaxWidth"] = 1600.0;
        dialog.Resources["ContentDialogMaxHeight"] = 1100.0;
        dialog.Opened += (_, _) => sheet.SetFile(
            tile.Path, tile.Kind, tile.SizeBytes, tile.ModifiedAt, tile.Id, tile.HasText);
        sheet.RequestClose += (_, _) => { try { dialog.Hide(); } catch { /* swallow */ } };

        // Intercept on the dialog's tunneling pass with handledEventsToo: the XY-focus engine cannot swallow the arrows first, and a focused Button cannot consume Space first.
        dialog.AddHandler(
            Microsoft.UI.Xaml.UIElement.PreviewKeyDownEvent,
            new Microsoft.UI.Xaml.Input.KeyEventHandler((_, ev) => sheet.HandleKeyDown(ev)),
            handledEventsToo: true);

        try { await dialog.ShowAsync(); }
        catch { /* dialog already open */ }
        finally { sheet.CloseFromHost(); }
        FollowPreviewedFile(sheet.FileId);
    }

    private void FollowPreviewedFile(long fileId)
    {
        if (_unloaded || !IsPaneOpen || fileId == 0 || SelectedPane.Tile?.Id == fileId) return;
        int index = -1;
        for (int i = 0; i < ViewModel.Items.Count; i++)
        {
            if (ViewModel.Items[i].Id == fileId) { index = i; break; }
        }
        if (index < 0) return;
        var tile = ViewModel.Items[index];
        using (ViewModel.BulkSelectionScope())
        {
            foreach (var t in ViewModel.Items) t.IsSelected = false;
            tile.IsSelected = true;
        }
        _lastClickedTile = tile;
        _focusedIndex = index;
        ShowPane(tile);
        BringIndexIntoView(index);
        UpdateSelectionBar();
    }

    private void OnContextOpen(object sender, RoutedEventArgs e)
    {
        if (sender is MenuFlyoutItem item && item.Tag is string path)
        {
            if (!Services.SafeOpen.TryOpenFile(path))
            {
                Services.SafeOpen.Reveal(path);
            }
        }
    }

    private void OnContextReveal(object sender, RoutedEventArgs e)
    {
        if (sender is MenuFlyoutItem item && item.Tag is string path)
        {
            Services.SafeOpen.Reveal(path);
        }
    }

    private async void OnContextFindSimilar(object sender, RoutedEventArgs e)
    {
        if (sender is not MenuFlyoutItem item || item.Tag is not long fileId) return;
        await ViewModel.FindSimilarAsync(fileId, System.Threading.CancellationToken.None);
    }

    private void OnContextCopyPath(object sender, RoutedEventArgs e)
    {
        if (sender is MenuFlyoutItem item && item.Tag is string path)
        {
            var dp = new Windows.ApplicationModel.DataTransfer.DataPackage();
            dp.SetText(path);
            Windows.ApplicationModel.DataTransfer.Clipboard.SetContent(dp);
        }
    }

    public event PropertyChangedEventHandler? PropertyChanged;
    private void OnPropertyChanged(string name)
        => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}
