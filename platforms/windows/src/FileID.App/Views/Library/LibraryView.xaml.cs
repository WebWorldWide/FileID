// LibraryView code-behind. Routes search-box + kind-filter input into the
// LibraryViewModel + drives the footer's loading/empty/error states.

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
    private readonly ThumbnailService _thumbnails = new();
    // Live-tile streaming during a scan. Mirrors macOS LibraryView's
    // .onChange(of: engine.lastBatch?.batchIndex) — refresh the grid
    // whenever a new BatchSummary lands, but throttled so a fast scan
    // doesn't issue 30+ DB reads per second.
    private long _lastSeenBatchIndex = -1;
    private DateTime _lastReloadAt = DateTime.MinValue;
    private static readonly TimeSpan LibraryReloadThrottle = TimeSpan.FromSeconds(1);
    private bool _unloaded;
    // BUG-12: ElementPrepared/ElementClearing fire on the UI thread, but
    // LoadThumbAsync's finally-block .Remove can resume on a worker thread
    // after ConfigureAwait(true) without a SyncContext. ConcurrentDictionary
    // makes the Add/Remove pair safe regardless.
    private readonly System.Collections.Concurrent.ConcurrentDictionary<FileTile, CancellationTokenSource> _inflight = new();
    private readonly ClipSearchService _clip;
    // One-shot tile-entrance gate. ItemsRepeater reuses element instances and
    // re-realizes them on every collection Reset — the throttled mid-scan
    // refresh raises a Reset ~1 Hz, so replaying the entrance on each
    // ElementPrepared made the whole grid pulse every second during a scan.
    // Track which element instances have already played their entrance so each
    // plays it at most once per element lifetime. Keyed on the element (not the
    // FileTile, which is recreated every Reset) so the mark survives rebinds.
    private static readonly System.Runtime.CompilerServices.ConditionalWeakTable<UIElement, object> _enteredTiles = new();
    private static readonly object _enteredMarker = new();

    // Keyboard-navigation cursor into ViewModel.Items. -1 until the first
    // arrow key or tile click establishes it. Drives single-select movement
    // (arrows), range-extend (Shift+arrows), Enter (open preview), Space
    // (toggle select). The selection highlight doubles as the focus cue.
    private int _focusedIndex = -1;
    private Microsoft.UI.Xaml.Input.KeyEventHandler? _gridKeyHandler;

    public LibraryView()
    {
        var paths = AppPaths.DbPath;
        var store = new ReadStore(paths);
        _clip = new ClipSearchService(store);
        ViewModel = new LibraryViewModel(store, _clip, Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread());

        InitializeComponent();
        Unloaded += OnUnloaded;
        // Named handlers (not inline lambdas) so OnUnloaded can detach
        // them. Inline lambdas leak the view + VM graph (~500 KB) every
        // time the tab is navigated away from and back to.
        ViewModel.PropertyChanged += OnViewModelPropertyChanged;
        ViewModel.Items.CollectionChanged += OnItemsCollectionChanged;
        UndoStack.Instance.PropertyChanged += OnUndoStackChanged;
        EngineClient.Instance.PropertyChanged += OnEngineChanged;

        // Arrow-key navigation for the tile grid. ItemsRepeater has NO built-in
        // keyboard nav (unlike GridView), so we drive it ourselves. Wire on the
        // tunneling Preview pass with handledEventsToo — same reason the preview
        // sheet does (9dd7785): the ScrollViewer's own OnKeyDown would otherwise
        // eat arrows as scroll before our bubbling handler sees them.
        _gridKeyHandler = new Microsoft.UI.Xaml.Input.KeyEventHandler(OnGridPreviewKeyDown);
        GridScroller.AddHandler(UIElement.PreviewKeyDownEvent, _gridKeyHandler, handledEventsToo: true);

        Loaded += async (_, _) =>
        {
            try
            {
                await store.OpenAsync(CancellationToken.None);
                await ViewModel.RefreshAsync(CancellationToken.None);
            }
            catch
            {
                // ReadStore.OpenAsync surfaces errors via ErrorMessage on
                // refresh — initial open before scan is allowed to no-op.
            }
            SyncUndoPill();
            SyncBanners();
        };
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
                        Services.DebugLog.Debug($"[ENGINE-SUB:LibraryView] {e.PropertyName}=Completed");
                        RequestLibraryRefresh(force: true);
                    }
                    break;
                case nameof(EngineClient.LastBatch):
                    var summary = EngineClient.Instance.LastBatch;
                    if (summary is null) return;
                    long batchIndex = summary.BatchIndex;
                    if (batchIndex == _lastSeenBatchIndex) return;
                    _lastSeenBatchIndex = batchIndex;
                    if (DateTime.UtcNow - _lastReloadAt < LibraryReloadThrottle) return;
                    Services.DebugLog.Debug($"[ENGINE-SUB:LibraryView] {e.PropertyName} batch={batchIndex}");
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

    // Refreshes the inline banner visibility based on EngineClient
    // observables + the user's current search query. Cheap; called
    // whenever a relevant property fires.
    private void SyncBanners()
    {
        if (_unloaded) return;
        // CLIP-missing hint: only when the user has typed ≥3 chars
        // (semantic search threshold matches macOS) and the MobileCLIP
        // slot isn't installed.
        // CLIP powers free-text semantic search; prompt to install it when the
        // user types a query (≥3 chars) and the MobileCLIP slot isn't installed.
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

        // Deep Analyze banner: visible whenever the engine is mid-caption.
        // Engine throttles Progress events to 4 Hz; once Processed == Total
        // (or DeepAnalyzeComplete arrives, clearing DeepAnalyzeProgress in
        // EngineClient.Apply), the banner collapses.
        var dap = EngineClient.Instance.DeepAnalyzeProgress;
        if (dap is not null && dap.Processed < dap.Total)
        {
            DeepAnalyzeBanner.Visibility = Visibility.Visible;
            var current = string.IsNullOrEmpty(dap.CurrentPath)
                ? string.Empty
                : System.IO.Path.GetFileName(dap.CurrentPath!);
            DeepAnalyzeBannerText.Text = string.IsNullOrEmpty(current)
                ? $"Deep Analyze running… ({dap.Processed}/{dap.Total})"
                : $"Deep Analyze: {current} ({dap.Processed}/{dap.Total})";
        }
        else
        {
            DeepAnalyzeBanner.Visibility = Visibility.Collapsed;
        }

        // Face clustering banner: simple — currently we don't get a
        // progress event during clustering, so just show it briefly when
        // the engine kicks off and clear it when LastFaceClustering lands.
        // The auto-trigger after a scan completes runs in <2s on typical
        // libraries so a short banner is enough.
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
            try { await ViewModel.RefreshAsync(CancellationToken.None); }
            catch (Exception ex) { Services.DebugLog.Warn($"Library refresh failed (force={force}): {ex.Message}"); }
        });
    }

    private void SyncUndoPill()
    {
        if (UndoButton == null) return;
        var stack = UndoStack.Instance;
        if (stack.CanUndo)
        {
            UndoButton.Visibility = Visibility.Visible;
            UndoButtonText.Text = string.IsNullOrEmpty(stack.TopLabel)
                ? "Undo"
                : $"Undo {stack.TopLabel}";
        }
        else
        {
            UndoButton.Visibility = Visibility.Collapsed;
        }
    }

    /// <summary>select-all visible toggle. If anything is
    /// currently selected, clear; otherwise select every visible tile.</summary>
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

    /// <summary>pop the top undo entry. The label update
    /// follows automatically via the UndoStack PropertyChanged handler.</summary>
    private async void OnUndoLastClicked(object sender, RoutedEventArgs e)
    {
        try
        {
            UndoButton.IsEnabled = false;
            var label = await UndoStack.Instance.UndoAsync();
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

    /// <summary>
    /// CRITICAL DISPOSAL ORDER — do not reorder without re-reading this:
    ///
    ///   1. Cancel in-flight CTSes (thumb loads).
    ///   2. Dispose ViewModel FIRST. Its Dispose cancels `_disposalCts`,
    ///      which propagates through every linked token into in-flight
    ///      RefreshAsync / SemanticSearch awaits.
    ///   3. THEN dispose `_clip` and `_thumbnails`. If those go first, a
    ///      resumed task touches a disposed service and the process dies
    ///      hard ("click sidebar mid-scan → app dies" crash class). On
    ///      Windows there's no SwiftUI lifecycle to bail us out of an
    ///      inverted order.
    ///
    /// This is one of three lifecycle-order invariants Windows has to
    /// hand-orchestrate. Do not touch.
    /// </summary>
    private void OnUnloaded(object sender, RoutedEventArgs e)
    {
        _unloaded = true;
        Unloaded -= OnUnloaded;
        // Detach VM subscriptions so the VM/View graph can be GC'd.
        try { ViewModel.PropertyChanged -= OnViewModelPropertyChanged; } catch { /* swallow */ }
        try { ViewModel.Items.CollectionChanged -= OnItemsCollectionChanged; } catch { /* swallow */ }
        try { UndoStack.Instance.PropertyChanged -= OnUndoStackChanged; } catch { /* swallow */ }
        try { EngineClient.Instance.PropertyChanged -= OnEngineChanged; } catch { /* swallow */ }
        // Step 1: cancel + dispose every in-flight thumb load so closing
        // the tab doesn't leave background tasks holding BitmapImage refs.
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
        // Step 2: dispose ViewModel FIRST. See the method-level remark
        // above for why the order is load-bearing.
        try { ViewModel.Dispose(); } catch { /* swallow */ }
        // Step 3: only AFTER ViewModel has cancelled its disposal CTS,
        // tear down the services its in-flight tasks may still touch.
        try { _clip.Dispose(); } catch { /* swallow */ }
        try { _thumbnails.Dispose(); } catch { /* swallow */ }
    }

    private void OnViewModelPropertyChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (e.PropertyName is nameof(LibraryViewModel.IsLoading)
            or nameof(LibraryViewModel.ErrorMessage))
        {
            OnPropertyChanged(nameof(StatusText));
            OnPropertyChanged(nameof(FooterVisibility));
        }
    }

    private void OnItemsCollectionChanged(object? sender, System.Collections.Specialized.NotifyCollectionChangedEventArgs e)
    {
        OnPropertyChanged(nameof(StatusText));
        OnPropertyChanged(nameof(FooterVisibility));
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
                return "No files match. Pick a folder via the sidebar to start a scan.";
            }
            return $"{ViewModel.Items.Count} files";
        }
    }

    public Visibility FooterVisibility =>
        ViewModel.IsLoading
        || !string.IsNullOrEmpty(ViewModel.ErrorMessage)
        || ViewModel.Items.Count == 0
            ? Visibility.Visible : Visibility.Collapsed;

    private void OnSearchChanged(object sender, TextChangedEventArgs e)
        => DebugLog.SafeRun(nameof(OnSearchChanged), () =>
        {
            ViewModel.Query = SearchBox.Text;
            SyncBanners();
        });

    // Ctrl+F → focus the search box. Mirrors macOS Cmd+F on LibraryView.swift.
    private void OnFocusSearchAccelerator(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
        => DebugLog.SafeRun(nameof(OnFocusSearchAccelerator), () =>
        {
            SearchBox.Focus(FocusState.Keyboard);
            SearchBox.SelectAll();
            args.Handled = true;
        });

    // Esc → if a selection is active, clear it; else if the search box has
    // text, clear it and refocus the grid. Matches macOS Library's chained
    // .keyboardShortcut(.escape).
    private void OnEscapeAccelerator(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
        => DebugLog.SafeRun(nameof(OnEscapeAccelerator), () =>
        {
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

    // Ctrl+A → select-all-visible. Identical to the toolbar "Select" button
    // so users who default to keyboard shortcuts get the same behavior.
    private void OnSelectAllAccelerator(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
        => DebugLog.SafeRun(nameof(OnSelectAllAccelerator), () =>
        {
            OnSelectAllClicked(this, new RoutedEventArgs());
            args.Handled = true;
        });

    // Ctrl+Z → undo the last destructive op (rename / trash / tag apply /
    // restructure apply). No-op when the UndoStack is empty.
    private void OnUndoAccelerator(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
        => DebugLog.SafeRun(nameof(OnUndoAccelerator), () =>
        {
            if (!UndoStack.Instance.CanUndo) return;
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

    // Right-tap on a tile lets the keyboard accelerate to the same flyout
    // (Shift+F10 / Menu key). The MenuFlyout is wired in XAML; this just
    // makes sure the right-tap event bubbles cleanly without selection
    // weirdness.
    private void OnTileRightTapped(object sender, Microsoft.UI.Xaml.Input.RightTappedRoutedEventArgs e)
    {
        e.Handled = false;
    }

    // ItemsRepeater calls ElementPrepared when a tile scrolls into view +
    // ElementClearing when it scrolls out. We fire off a thumbnail load on
    // prepare; cancel + null on clear so the LRU keeps the cache hot but
    // off-screen tiles don't hold visible BitmapImage refs that block GC.
    private void OnRepeaterElementPrepared(Microsoft.UI.Xaml.Controls.ItemsRepeater sender,
                                            Microsoft.UI.Xaml.Controls.ItemsRepeaterElementPreparedEventArgs args)
    {
        if (args.Element is not FrameworkElement el) return;
        // x:Bind in the ItemsRepeater ItemTemplate does NOT populate the
        // realized element's DataContext (compiled bindings bypass it), so
        // the old `el.DataContext is not FileTile` guard returned on every
        // tile — LoadThumbAsync never ran and not one thumbnail rendered
        // (the L2 disk cache stayed empty across every session). Resolve the
        // tile from the authoritative repeater index, then set DataContext
        // so the sibling code-behind handlers (Clearing / Tapped / Drag)
        // that read el.DataContext resolve the same tile.
        var tile = (args.Index >= 0 && args.Index < ViewModel.Items.Count)
            ? ViewModel.Items[args.Index]
            : el.DataContext as FileTile;
        Services.DebugLog.Debug(
            $"[THUMB] PREPARE idx={args.Index} dcWasNull={el.DataContext is null} resolved={tile is not null}");
        if (tile is null) return;
        el.DataContext = tile;

        // tile is back in the virtualization window — undo the
        // ElementClearing detach so a fresh thumbnail load can bind.
        tile.IsDetached = false;
        // Clear any prior failed-thumbnail state from the last attachment
        // so a retry on re-prepare gets to fall through to the shimmer
        // (and either land or surface the placeholder again).
        tile.ThumbnailFailed = false;

        // tile entry animation — scale-in pop (0.96 → 1), the scale half of
        // macOS LibraryView.swift:566-575's
        // .transition(.opacity.combined(with: .scale(scale: 0.96))). The
        // opacity half is deliberately NOT ported as a composition animation:
        // see AnimateTileEntry's remarks — an interrupted opacity spring under
        // mid-scan churn stranded whole tiles invisible. The pop plays once per
        // element; reduced-motion users get a hard snap (no animation).
        AnimateTileEntry(el);

        if (tile.Thumbnail != null)
        {
            // Already-loaded thumbnail (LRU cache hit, or a surviving tile from
            // the identity-stable merge). The Image binds Source directly at
            // Opacity 1, so it's already on screen — nothing to do.
            return;
        }

        var cts = new CancellationTokenSource();
        // BUG-12: TryAdd in case of a re-entrant prepare (rare but cheap to defend).
        if (!_inflight.TryAdd(tile, cts))
        {
            // Already loading — let the existing one finish.
            cts.Dispose();
            return;
        }
        _ = LoadThumbAsync(tile, cts.Token);
    }

    /// <summary>macOS-parity tile-entry animation — a scale-in "pop"
    /// (0.96 → 1, Tight tokens 0.35/0.78). Runs at most once per element
    /// instance (see <see cref="_enteredTiles"/>).
    ///
    /// CRITICAL — opacity is NOT animated here, and the tile root's opacity is
    /// pinned to 1 on every call. The previous version drove the root
    /// composition Opacity 0 → 1 via a spring. Under mid-scan churn the
    /// ItemsRepeater re-realizes elements ~1 Hz (each throttled refresh raises a
    /// Reset), and an interrupted opacity spring stranded the ENTIRE tile — its
    /// thumbnail, filename, and tag chips all live under this root — at
    /// Opacity 0. That single defect read to users as both "thumbnails not
    /// loading" and "tags not showing" (8611 TILE_THUMBNAIL_ASSIGNED but zero
    /// IMAGE_OPENED in app.log: bitmaps bound, tiles invisible). A scale-only
    /// entrance can never hide content, and a stranded scale at 0.96 is still
    /// fully legible.
    ///
    /// Defensive: every Composition call wrapped in try/catch because
    /// StartAnimation on a detached / mid-recycle element can throw, and a
    /// throw from a fire-and-forget animation callback is one of the
    /// fast-fail vectors. Worst case here: the tile snaps in without
    /// animation — never crashes the app, never invisible.</summary>
    private static void AnimateTileEntry(FrameworkElement el)
    {
        try
        {
            var visual = Microsoft.UI.Xaml.Hosting.ElementCompositionPreview.GetElementVisual(el);
            // Opacity is owned by the steady state and is ALWAYS 1. Stop any
            // leftover opacity spring from an older build and force-reveal.
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
                // Pre-layout: a corner-anchored scale would look wrong. Ensure
                // the tile is visible at full scale and let a later prepare
                // (post-arrange) carry the pop. Do NOT mark entered.
                visual.StopAnimation("Scale.X");
                visual.StopAnimation("Scale.Y");
                visual.Scale = new System.Numerics.Vector3(1f, 1f, 1f);
                return;
            }

            if (!firstEntry)
            {
                // Already played its entrance (recycled / re-realized on a
                // Reset) — snap to rest instead of replaying the pop every
                // refresh, which looked like the grid pulsing during a scan.
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
        // Release the bound bitmap BEFORE detaching. The Image binds
        // Source="{x:Bind Thumbnail}", so a recycled element otherwise keeps
        // the PREVIOUS file's bitmap on Source and flashes it on the next
        // reveal — the "thumbnails rendering from anything" symptom. Nulling
        // here clears Source → shimmer, and frees the BitmapImage so
        // off-screen tiles don't pin every bitmap they ever loaded (memory
        // bloat on large libraries). Must run while still attached — the
        // Thumbnail setter no-ops once IsDetached is set just below.
        tile.ClearThumbnailForRecycle();
        Services.DebugLog.Debug($"[THUMB] RECYCLE_NULLED file={tile.Path}");
        // mark detached so a late-arriving thumbnail render
        // doesn't bind into a stale tile.
        tile.IsDetached = true;
        if (_inflight.TryRemove(tile, out var cts))
        {
            try { cts.Cancel(); } catch { /* swallow */ }
            cts.Dispose();
        }
        // No opacity manipulation anywhere: the Image keeps its XAML default
        // Opacity (1) and just renders its bound Source. ClearThumbnailForRecycle
        // above already nulled the Source, so a recycled tile shows the shimmer
        // (not a stale bitmap) until its own thumbnail reloads. With the
        // identity-stable merge, on-screen tiles are no longer recycled on every
        // refresh — only on real scroll — so this fires far less often now.
    }

    private async Task LoadThumbAsync(FileTile tile, CancellationToken ct)
    {
        try
        {
            // explicit UI dispatcher post for the bind. The previous
            // ConfigureAwait(true) relied on the captured SynchronizationContext
            // (DispatcherQueueSynchronizationContext on UI thread), which the
            // thumbnail service's TCS may or may not honor depending on how
            // the result is published. Using TryEnqueue makes the
            // UI-thread assignment unambiguous.
            var bmp = await _thumbnails.RequestAsync(tile.Path, tile.ModifiedAt, ct).ConfigureAwait(false);
            if (bmp == null)
            {
                Services.DebugLog.Debug($"[THUMB] LOAD_NULL file={tile.Path}");
                // Hand off from shimmer to a broken-image placeholder so the
                // tile doesn't shimmer forever. Setter is UI-thread-affined
                // because it raises PropertyChanged; route through the
                // DispatcherQueue same as the bmp-assignment path below.
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
                Services.DebugLog.Debug($"[THUMB] LOAD_DROPPED file={tile.Path} cancelled={ct.IsCancellationRequested} detached={tile.IsDetached}");
                return;
            }
            var enqueued = DispatcherQueue.TryEnqueue(() =>
            {
                // _unloaded guard: this continuation is queued from a worker
                // thread (ConfigureAwait(false) above) and can run AFTER a tab
                // switch tore down the view. Bail before touching the tile.
                if (_unloaded || tile.IsDetached) return;
                // Assign the bitmap; the x:Bind Source updates and the Image
                // (Opacity 1) renders it. No opacity dance — the Image is never
                // hidden, so there's nothing to "reveal".
                tile.Thumbnail = bmp;
                // Pixel dims confirm the bitmap actually carries content. If a
                // tile is still blank after this, px>0 here means the problem is
                // layout (image row collapsed), not a 0-pixel decode.
                Services.DebugLog.Debug($"[THUMB] TILE_THUMBNAIL_ASSIGNED file={tile.Path} px={bmp.PixelWidth}x{bmp.PixelHeight}");
            });
            if (!enqueued)
            {
                Services.DebugLog.Debug($"[THUMB] ASSIGN_ENQUEUE_FAILED file={tile.Path}");
            }
        }
        catch (Exception ex)
        {
            Services.DebugLog.Debug($"[THUMB] LOAD_EX file={tile.Path} ex={ex.GetType().Name}");
        }
        finally
        {
            _inflight.TryRemove(tile, out _);
        }
    }

    // Tile height = width + 68 (square image area + fixed 68px caption row).
    // Set here, not via a Height self-binding to ActualWidth: ActualWidth is
    // not an observable DP, so a OneWay bind reads 0 before layout (→ height
    // 68, image row collapses to ~0) and never re-fires after arrange. This
    // was the long-standing "thumbnails decode but render blank" bug — the
    // bitmap was assigned to a zero-height image row. SizeChanged runs
    // post-arrange with the real width and again on column resize. The >0.5
    // guard breaks the height-set → SizeChanged feedback loop (width is
    // column-driven, so it doesn't change when we set Height).
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

    // Single tap toggles selection when Ctrl is held; Shift extends from
    // the last clicked tile; otherwise sets a single selection.
    // tile hover scale matching macOS LibraryView.swift:681-682
    // (scaleEffect 1.012 + 0.18s spring). Composition spring runs on the
    // GPU; visual fidelity matches SwiftUI's .spring system. CenterPoint
    // is reset on every hover so resized tiles scale around their actual
    // current center.
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
        var visual = Microsoft.UI.Xaml.Hosting.ElementCompositionPreview.GetElementVisual(el);
        visual.CenterPoint = new System.Numerics.Vector3(
            (float)(el.ActualWidth / 2), (float)(el.ActualHeight / 2), 0);
        // macOS hover uses .easeOut(0.18) — no spring overshoot.
        // SwiftUI LibraryView.swift:681-682.
        FileID.Theme.Motion.SpringEasing.AnimateScalarEaseOut(el, "Scale.X", scale, 0.18);
        FileID.Theme.Motion.SpringEasing.AnimateScalarEaseOut(el, "Scale.Y", scale, 0.18);
        ApplyTileStrokeOpacity(el, hovering: scale > 1.0f);
    }

    // macOS LibraryView.swift:676-677 ramps the tile's white stroke opacity
    // 0.08 → 0.18 alongside the scale on pointer enter. The brush is defined
    // inline in the DataTemplate so each tile owns its own instance.
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

            // Keep the keyboard cursor in sync with the click and route focus to
            // the grid so arrow keys continue from here without an extra Tab.
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
                // Plain click — only update selection if user is mid-multi-select;
                // otherwise let double-tap open the preview without setting any
                // selection.
                if (ViewModel.SelectedCount > 0)
                {
                    using (ViewModel.BulkSelectionScope())
                    {
                        foreach (var t in ViewModel.Items) t.IsSelected = false;
                        tile.IsSelected = true;
                    }
                    _lastClickedTile = tile;
                }
            }

            UpdateSelectionBar();
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
    }

    // Arrow-key navigation over the ItemsRepeater grid. Wired on GridScroller's
    // tunneling PreviewKeyDown (handledEventsToo) so it preempts the
    // ScrollViewer's built-in arrow-scroll.
    private void OnGridPreviewKeyDown(object sender, Microsoft.UI.Xaml.Input.KeyRoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnGridPreviewKeyDown), () =>
        {
            int count = ViewModel.Items.Count;
            if (count == 0) return;

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

            // First navigation key just lands the cursor on the first tile.
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
            int target;
            switch (e.Key)
            {
                case VirtualKey.Left: target = cur - 1; break;
                case VirtualKey.Right: target = cur + 1; break;
                case VirtualKey.Up: target = cur - cols; break;
                case VirtualKey.Down: target = cur + cols; break;
                case VirtualKey.Home: target = 0; break;
                case VirtualKey.End: target = last; break;
                case VirtualKey.PageUp: target = cur - page; break;
                case VirtualKey.PageDown: target = cur + page; break;
                default: return;
            }

            var shift = Microsoft.UI.Input.InputKeyboardSource
                .GetKeyStateForCurrentThread(VirtualKey.Shift)
                .HasFlag(CoreVirtualKeyStates.Down);
            MoveFocusTo(target, extend: shift);
            e.Handled = true;
        });

    // Columns the UniformGridLayout currently shows. MinItemWidth=180 +
    // MinColumnSpacing=12 (LibraryView.xaml); ItemsStretch=Fill widens cells
    // but the column COUNT is governed by the minimum width.
    private int ColumnsPerRow()
    {
        double avail = GridScroller.ViewportWidth;
        if (avail <= 0) avail = Repeater.ActualWidth;
        const double minItemWidth = 180, colSpacing = 12;
        int cols = (int)Math.Floor((avail + colSpacing) / (minItemWidth + colSpacing));
        return Math.Max(1, cols);
    }

    // Rows visible in the viewport, for PageUp/PageDown. MinItemHeight=248 +
    // MinRowSpacing=12.
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
            // The anchor follows a plain move (matches click); Shift+move keeps it.
            _lastClickedTile = tile;
        }

        _focusedIndex = target;
        BringIndexIntoView(target);
        UpdateSelectionBar();
    }

    // Realize + scroll the target tile into view. Realization can race a
    // mid-scan Reset, so it's defensive.
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
            Title = "Tag selected files",
            Content = sheet,
            PrimaryButtonText = "Apply",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
        };
        dialog.PrimaryButtonClick += async (d, args) =>
        {
            var deferral = args.GetDeferral();
            var ok = await sheet.CommitAsync();
            if (!ok) args.Cancel = true;
            deferral.Complete();
        };
        try { await dialog.ShowAsync(); } catch { /* dialog already open */ }
    }

    private async void OnRenameSelectedClicked(object sender, RoutedEventArgs e)
    {
        var selected = ViewModel.SelectedItems.ToArray();
        if (selected.Length == 0) return;

        var plan = selected.Select(t => new BulkRenameSheet.RenamePlan
        {
            FileId = t.Id,
            CurrentPath = t.Path,
            ProposedName = t.FileName, // VLM-proposed names will be wired later.
            Include = true,
        }).ToArray();

        var sheet = new BulkRenameSheet();
        sheet.SetPlan(plan);
        var dialog = new ContentDialog
        {
            XamlRoot = XamlRoot,
            Title = "Rename selected files",
            Content = sheet,
            PrimaryButtonText = "Rename",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
        };
        dialog.PrimaryButtonClick += async (d, args) =>
        {
            var deferral = args.GetDeferral();
            var ok = await sheet.CommitAsync();
            if (!ok) args.Cancel = true;
            deferral.Complete();
        };
        try { await dialog.ShowAsync(); } catch { /* dialog already open */ }
    }

    private async void OnTrashSelectedClicked(object sender, RoutedEventArgs e)
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

        try
        {
            // Listen for the engine's BulkActionResult — it tags the
            // action with "trashFiles:<batch_id>" so we can plumb undo.
            Services.UndoStack.CaptureNextBulkResult(
                "trashFiles:",
                $"trash {ids.Length} file{(ids.Length == 1 ? "" : "s")}",
                async batchId =>
                {
                    if (string.IsNullOrEmpty(batchId)) return false;
                    try
                    {
                        await EngineClient.Instance.RestoreFromTrashAsync(batchId);
                        return true;
                    }
                    catch { return false; }
                });
            await EngineClient.Instance.TrashFilesAsync(ids);
        }
        catch
        {
            // Failure surfaces through BulkActionResult event.
        }

        // Optimistic local removal from the grid; engine will catch up via
        // its DELETE in the dbwriter and a future refresh.
        foreach (var id in ids)
        {
            var match = ViewModel.Items.FirstOrDefault(t => t.Id == id);
            if (match is not null) ViewModel.Items.Remove(match);
        }
        UpdateSelectionBar();
    }

    private void OnClearSelectionClicked(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnClearSelectionClicked), () =>
        {
            ViewModel.ClearSelection();
            UpdateSelectionBar();
        });

    // Lets the user drag a tile out of FileID into Explorer / email /
    // Slack as a real file. If multiple tiles are selected, the whole
    // selection comes along.
    private async void OnTileDragStarting(UIElement sender, DragStartingEventArgs args)
    {
        var deferral = args.GetDeferral();
        try
        {
            var paths = ViewModel.SelectedCount > 0
                ? ViewModel.SelectedItems.Select(t => t.Path).ToList()
                : (sender is FrameworkElement el && el.DataContext is FileTile tile
                    ? new List<string> { tile.Path }
                    : new List<string>());
            var items = new List<Windows.Storage.IStorageItem>(paths.Count);
            foreach (var p in paths)
            {
                if (System.IO.File.Exists(p))
                {
                    try
                    {
                        items.Add(await Windows.Storage.StorageFile.GetFileFromPathAsync(p));
                    }
                    catch { /* path inaccessible — skip */ }
                }
            }
            if (items.Count > 0)
            {
                args.Data.SetStorageItems(items);
                args.Data.RequestedOperation = Windows.ApplicationModel.DataTransfer.DataPackageOperation.Copy;
            }
        }
        finally
        {
            deferral.Complete();
        }
    }

    private static string FormatSize(long bytes)
    {
        if (bytes < 1024) return $"{bytes} B";
        if (bytes < 1024 * 1024) return $"{bytes / 1024.0:0.#} KB";
        if (bytes < 1024L * 1024 * 1024) return $"{bytes / (1024.0 * 1024):0.#} MB";
        return $"{bytes / (1024.0 * 1024 * 1024):0.##} GB";
    }

    // Double-click any tile → open the FilePreviewSheet modal. The Tag
    // on the tile carries the absolute path; we look up the FileTile in
    // ViewModel.Items to get kind + size + modified for the metadata strip.
    //
    // pass the sibling list (frozen at open time so a fresh-batch
    // refresh mid-preview can't shift indices under the user) and wire
    // the sheet's own RequestClose to hide the dialog. The sheet's
    // toolbar X button + Esc key handle close inline, matching macOS's
    // self-contained preview chrome — no separate dialog CloseButton.
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

    // Shared preview-open path — used by double-tap and by keyboard Enter
    // (OnGridPreviewKeyDown). Opens the FilePreviewSheet modal for the given
    // tile, freezing the sibling list at open time.
    private async System.Threading.Tasks.Task OpenPreview(FileTile tile, int tileIndex)
    {
        // Snapshot siblings so a live BatchSummary refresh mid-preview
        // can't reorder under our feet (matches macOS frozen previewSiblings).
        var siblings = ViewModel.Items.ToList();

        var sheet = new FilePreviewSheet();
        sheet.SetSiblings(siblings, tileIndex);
        sheet.SetFile(tile.Path, tile.Kind, tile.SizeBytes, tile.ModifiedAt, tile.Id, tile.HasFaces, tile.HasText);

        var dialog = new Microsoft.UI.Xaml.Controls.ContentDialog
        {
            XamlRoot = XamlRoot,
            Content = sheet,
            // no dialog CloseButton — the sheet's own toolbar X
            // button (with media-stop + RequestClose) is the canonical
            // dismissal path. A separate ContentDialog CloseButton would
            // double-render below the preview.
        };
        // override the default ContentDialog max bounds so the
        // preview sheet actually gets the 1080×720 minimum it declares.
        // WinUI 3's default caps at ~640×480 which would crop our sidebar.
        dialog.Resources["ContentDialogMaxWidth"] = 1600.0;
        dialog.Resources["ContentDialogMaxHeight"] = 1100.0;
        sheet.RequestClose += (_, _) => { try { dialog.Hide(); } catch { /* swallow */ } };

        // The ContentDialog — not the sheet — owns keyboard focus once shown, so
        // the sheet's own PreviewKeyDown never fires (arrow keys + Space were
        // dead). Intercept on the dialog's tunneling Preview pass: handledEventsToo
        // so the XY-focus engine can't swallow the arrows first, and tunneling
        // reaches the dialog (an ancestor) BEFORE a focused Button consumes Space.
        dialog.AddHandler(
            Microsoft.UI.Xaml.UIElement.PreviewKeyDownEvent,
            new Microsoft.UI.Xaml.Input.KeyEventHandler((_, ev) => sheet.HandleKeyDown(ev)),
            handledEventsToo: true);

        try { await dialog.ShowAsync(); } catch { /* dialog already open */ }
    }

    private void OnContextOpen(object sender, RoutedEventArgs e)
    {
        // SEC-9: Open is gated by SafeOpen's extension allowlist — falls
        // back to Reveal for anything that could be executable.
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

    // Find similar — pulls the seed file's stored CLIP embedding from the
    // engine + runs the same dot-product the text-search path uses. The
    // engine emits a clipTextEmbedding event; ClipSearchService routes it
    // back to whichever EmbedQueryAsync future is awaiting on the same
    // queryId. We bypass the search-bar UI and surface results directly
    // via the existing ViewModel.Items list.
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
