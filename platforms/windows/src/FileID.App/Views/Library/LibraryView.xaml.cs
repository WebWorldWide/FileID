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
            }
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
                foreach (var t in ViewModel.Items) t.IsSelected = true;
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
        => DebugLog.SafeRun(nameof(OnSearchChanged), () => ViewModel.Query = SearchBox.Text);

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
        if (args.Element is not FrameworkElement el || el.DataContext is not FileTile tile) return;
        // tile is back in the virtualization window — undo the
        // ElementClearing detach so a fresh thumbnail load can bind.
        tile.IsDetached = false;

        // tile entry animation — fade + scale-in, matches macOS
        // LibraryView.swift:566-575 (.transition(.opacity.combined(with:
        // .scale(scale: 0.96))) with .easeOut(0.30)). Each tile springs
        // in from opacity 0 + scale 0.96 to 1/1 on prepare. Recycled
        // virtualized elements get the same treatment so a scroll-back
        // looks identical to the first reveal. Reduced-motion users
        // get a hard snap (no animation).
        AnimateTileEntry(el);

        if (tile.Thumbnail != null)
        {
            // Already-loaded thumbnail (LRU cache hit on re-virtualization).
            // The XAML Opacity="0" + ImageOpened="OnTileImageOpened" would
            // wait for a decode event that won't fire if the BitmapImage is
            // already decoded. Make the thumbnail visible immediately and
            // let the parent tile-entry spring carry the visual reveal.
            EnsureThumbnailVisible(el);
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

    /// <summary>macOS-parity tile-entry animation. Snap to opacity=0
    /// + scale=0.96 first, then spring to 1 using Tight tokens (0.35/0.78,
    /// matches macOS scale-in feel). Operates on the Composition visual
    /// directly so XAML Opacity bindings don't fight the animation.
    ///
    /// Defensive: every Composition call wrapped in try/catch because
    /// StartAnimation on a detached / mid-recycle element can throw, and a
    /// throw from a fire-and-forget animation callback is one of the
    /// fast-fail vectors. Worst case here: the tile snaps in without
    /// animation — never crashes the app.</summary>
    private static void AnimateTileEntry(FrameworkElement el)
    {
        try
        {
            var visual = Microsoft.UI.Xaml.Hosting.ElementCompositionPreview.GetElementVisual(el);
            // Always stop in-flight animations on this visual (recycled
            // tiles can have leftover springs from a previous prepare).
            visual.StopAnimation("Opacity");
            visual.StopAnimation("Scale.X");
            visual.StopAnimation("Scale.Y");

            if (FileID.Theme.Motion.ReducedMotion.Instance.IsReduced)
            {
                visual.Opacity = 1f;
                visual.Scale = new System.Numerics.Vector3(1f, 1f, 1f);
                return;
            }

            var size = visual.Size;
            if (size.X <= 0 || size.Y <= 0)
            {
                // Pre-layout: scale animation from corner would look wrong.
                // Skip the scale; do opacity-only fade-in instead. This path
                // hits during the very first prepare before measure/arrange.
                visual.Opacity = 0f;
                visual.Scale = new System.Numerics.Vector3(1f, 1f, 1f);
                var t1 = FileID.Theme.Motion.SpringEasing.Tokens.Tight;
                FileID.Theme.Motion.SpringEasing.AnimateOpacity(el, 1f, t1.Response, t1.DampingFraction);
                return;
            }

            visual.CenterPoint = new System.Numerics.Vector3(size.X / 2, size.Y / 2, 0);
            visual.Opacity = 0f;
            visual.Scale = new System.Numerics.Vector3(0.96f, 0.96f, 1f);
            var t = FileID.Theme.Motion.SpringEasing.Tokens.Tight;
            FileID.Theme.Motion.SpringEasing.AnimateOpacity(el, 1f, t.Response, t.DampingFraction);
            FileID.Theme.Motion.SpringEasing.AnimateScale(el, 1f, t.Response, t.DampingFraction);
        }
        catch (Exception ex)
        {
            Services.DebugLog.Warn("AnimateTileEntry threw: " + ex.Message);
            // Snap to final state so the tile is at least visible.
            try { el.Opacity = 1; } catch { /* swallow */ }
        }
    }

    /// <summary>snap a recycled tile's thumbnail Image to opacity=1
    /// when the BitmapImage is already decoded (ImageOpened won't fire on a
    /// re-attached, already-decoded source). Operates on the composition
    /// visual so animation state from a previous tile lifecycle is also
    /// cleared.</summary>
    private static void EnsureThumbnailVisible(FrameworkElement tileRoot)
    {
        var img = FindBoundImage(tileRoot);
        if (img is null) return;
        try
        {
            var visual = Microsoft.UI.Xaml.Hosting.ElementCompositionPreview.GetElementVisual(img);
            visual.StopAnimation("Opacity");
            visual.Opacity = 1f;
        }
        catch (Exception ex)
        {
            Services.DebugLog.Warn("EnsureThumbnailVisible: " + ex.Message);
        }
    }

    private static Microsoft.UI.Xaml.Controls.Image? FindBoundImage(DependencyObject root)
    {
        var count = Microsoft.UI.Xaml.Media.VisualTreeHelper.GetChildrenCount(root);
        for (int i = 0; i < count; i++)
        {
            var child = Microsoft.UI.Xaml.Media.VisualTreeHelper.GetChild(root, i);
            if (child is Microsoft.UI.Xaml.Controls.Image img && img.Source is not null)
            {
                return img;
            }
            var nested = FindBoundImage(child);
            if (nested is not null) return nested;
        }
        return null;
    }

    /// <summary>thumbnail crossfade. Fires once per BitmapImage
    /// decode. The XAML <c>Opacity="0"</c> initial state is the static
    /// fallback; this snaps composition Opacity to 0 (override any leftover
    /// animation state) and springs to 1.
    ///
    /// Defensive try/catch — this is a XAML event callback that runs
    /// outside any SafeRun scope, and a throw escapes to the dispatcher.</summary>
    private void OnTileImageOpened(object sender, RoutedEventArgs e)
    {
        if (sender is not Microsoft.UI.Xaml.Controls.Image img) return;
        // [THUMB] tracing — confirms the Image control actually decoded its
        // assigned BitmapImage. If this never fires for a tile whose
        // BITMAP_SET log line was emitted, the chain breaks between
        // ThumbnailService and the Image.Source binding.
        var path = (img.DataContext as ViewModels.FileTile)?.Path
                  ?? (img.GetValue(FrameworkElement.TagProperty) as string)
                  ?? "?";
        Services.DebugLog.Debug($"[THUMB] IMAGE_OPENED file={path}");
        // XAML default is Opacity 1, but OnRepeaterElementClearing may have
        // forced the composition visual to 0 to reset for recycle. Snap
        // back to 1 here so the tile is visible. (Earlier code did a
        // spring 0→1, which flickered for already-decoded BitmapImages
        // arriving via the LRU; the parent tile-entry spring in
        // OnRepeaterElementPrepared carries the visual reveal.)
        try
        {
            var visual = Microsoft.UI.Xaml.Hosting.ElementCompositionPreview.GetElementVisual(img);
            visual.StopAnimation("Opacity");
            visual.Opacity = 1f;
            Services.DebugLog.Debug($"[THUMB] OPACITY_SET file={path} value=1");
        }
        catch (Exception ex)
        {
            Services.DebugLog.Warn("OnTileImageOpened threw: " + ex.Message);
            try { img.Opacity = 1; } catch { /* swallow */ }
        }
    }

    private void OnRepeaterElementClearing(Microsoft.UI.Xaml.Controls.ItemsRepeater sender,
                                           Microsoft.UI.Xaml.Controls.ItemsRepeaterElementClearingEventArgs args)
    {
        if (args.Element is not FrameworkElement el || el.DataContext is not FileTile tile) return;
        // mark detached so a late-arriving thumbnail render
        // doesn't bind into a stale tile.
        tile.IsDetached = true;
        if (_inflight.TryRemove(tile, out var cts))
        {
            try { cts.Cancel(); } catch { /* swallow */ }
            cts.Dispose();
        }
        // reset thumbnail Image composition opacity so the next
        // recycle gets a fresh crossfade. Setting XAML Opacity alone
        // isn't enough — a finished StartAnimation leaves the visual
        // pinned at its final value, decoupled from the XAML property.
        // Stop the animation explicitly + snap visual.Opacity = 0.
        var img = FindBoundImage(el);
        if (img is not null)
        {
            try
            {
                var visual = Microsoft.UI.Xaml.Hosting.ElementCompositionPreview.GetElementVisual(img);
                visual.StopAnimation("Opacity");
                visual.Opacity = 0f;
            }
            catch { /* swallow — animation reset is best-effort */ }
        }
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
                return;
            }
            if (ct.IsCancellationRequested || tile.IsDetached)
            {
                Services.DebugLog.Debug($"[THUMB] LOAD_DROPPED file={tile.Path} cancelled={ct.IsCancellationRequested} detached={tile.IsDetached}");
                return;
            }
            var enqueued = DispatcherQueue.TryEnqueue(() =>
            {
                if (tile.IsDetached) return;
                tile.Thumbnail = bmp;
                Services.DebugLog.Debug($"[THUMB] TILE_THUMBNAIL_ASSIGNED file={tile.Path}");
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
        FileID.Theme.Motion.SpringEasing.AnimateScalar(el, "Scale.X", scale, 0.18, 0.8);
        FileID.Theme.Motion.SpringEasing.AnimateScalar(el, "Scale.Y", scale, 0.18, 0.8);
    }

    private void OnTileTapped(object sender, TappedRoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnTileTapped), () =>
        {
            if (sender is not FrameworkElement el || el.DataContext is not FileTile tile) return;

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
                    if (!ctrl) foreach (var t in ViewModel.Items) t.IsSelected = false;
                    for (int i = lo; i <= hi; i++) ViewModel.Items[i].IsSelected = true;
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
                    foreach (var t in ViewModel.Items) t.IsSelected = false;
                    tile.IsSelected = true;
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
        var queryId = Guid.NewGuid().ToString("N");
        var tcs = new System.Threading.Tasks.TaskCompletionSource<float[]?>();
        void OnceHandler(object? _, System.ComponentModel.PropertyChangedEventArgs ev)
        {
            if (ev.PropertyName != nameof(EngineClient.LastClipTextEmbedding)) return;
            var emb = EngineClient.Instance.LastClipTextEmbedding;
            if (emb is null || emb.QueryId != queryId) return;
            EngineClient.Instance.PropertyChanged -= OnceHandler;
            tcs.TrySetResult(emb.Embedding?.ToArray());
        }
        // subscribe + try/finally so any throw between subscribe
        // and the final unsubscribe still cleans up. -= is idempotent, so
        // double-removal (handler self-removed + finally) is safe.
        EngineClient.Instance.PropertyChanged += OnceHandler;
        try
        {
            try
            {
                await EngineClient.Instance.EmbedImageQueryAsync(fileId, queryId);
            }
            catch (Exception ex)
            {
                DebugLog.Warn("OnContextFindSimilar: EmbedImageQueryAsync threw: " + ex.Message);
                return;
            }
            // 5-second timeout.
            var timeoutTask = System.Threading.Tasks.Task.Delay(TimeSpan.FromSeconds(5));
            var done = await System.Threading.Tasks.Task.WhenAny(tcs.Task, timeoutTask);
            if (done != tcs.Task) return;
            var seed = await tcs.Task;
            if (seed == null) return;
            // Run a semantic search against the existing ReadStore via the
            // ViewModel's path. We don't need a separate sheet — the Library
            // grid itself is the result list.
            await ViewModel.SemanticSearchWithSeedAsync(seed, System.Threading.CancellationToken.None);
        }
        finally
        {
            EngineClient.Instance.PropertyChanged -= OnceHandler;
        }
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
