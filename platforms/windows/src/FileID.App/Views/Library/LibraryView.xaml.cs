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
        };
    }

    private void OnUnloaded(object sender, RoutedEventArgs e)
    {
        Unloaded -= OnUnloaded;
        // Detach VM subscriptions so the VM/View graph can be GC'd.
        try { ViewModel.PropertyChanged -= OnViewModelPropertyChanged; } catch { /* swallow */ }
        try { ViewModel.Items.CollectionChanged -= OnItemsCollectionChanged; } catch { /* swallow */ }
        // Cancel + dispose every in-flight thumb load so closing the tab
        // doesn't leave background tasks holding BitmapImage refs.
        foreach (var (_, cts) in _inflight)
        {
            try { cts.Cancel(); } catch { /* swallow */ }
            cts.Dispose();
        }
        _inflight.Clear();
        try { _thumbnails.Dispose(); } catch { /* swallow */ }
        try { _clip.Dispose(); } catch { /* swallow */ }
        try { ViewModel.Dispose(); } catch { /* swallow */ }
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
    {
        ViewModel.Query = SearchBox.Text;
    }

    private void OnKindChanged(object sender, SelectionChangedEventArgs e)
    {
        if (KindFilter.SelectedItem is ComboBoxItem item && item.Tag is string tag)
        {
            ViewModel.KindFilter = tag;
        }
    }

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
        if (tile.Thumbnail != null) return; // cached on previous prepare

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

    private void OnRepeaterElementClearing(Microsoft.UI.Xaml.Controls.ItemsRepeater sender,
                                           Microsoft.UI.Xaml.Controls.ItemsRepeaterElementClearingEventArgs args)
    {
        if (args.Element is not FrameworkElement el || el.DataContext is not FileTile tile) return;
        if (_inflight.TryRemove(tile, out var cts))
        {
            try { cts.Cancel(); } catch { /* swallow */ }
            cts.Dispose();
        }
    }

    private async Task LoadThumbAsync(FileTile tile, CancellationToken ct)
    {
        try
        {
            var bmp = await _thumbnails.RequestAsync(tile.Path, tile.ModifiedAt, ct).ConfigureAwait(true);
            if (bmp != null && !ct.IsCancellationRequested)
            {
                tile.Thumbnail = bmp;
            }
        }
        catch { /* swallow -- placeholder stays */ }
        finally
        {
            _inflight.TryRemove(tile, out _);
        }
    }

    // Single tap toggles selection when Ctrl is held; Shift extends from
    // the last clicked tile; otherwise sets a single selection.
    private void OnTileTapped(object sender, TappedRoutedEventArgs e)
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
    }

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
            XamlRoot = this.XamlRoot,
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
            ProposedName = t.FileName, // Phase 6 will seed VLM-proposed names.
            Include = true,
        }).ToArray();

        var sheet = new BulkRenameSheet();
        sheet.SetPlan(plan);
        var dialog = new ContentDialog
        {
            XamlRoot = this.XamlRoot,
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
            XamlRoot = this.XamlRoot,
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
    {
        ViewModel.ClearSelection();
        UpdateSelectionBar();
    }

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
    private async void OnTileDoubleTapped(object sender, Microsoft.UI.Xaml.Input.DoubleTappedRoutedEventArgs e)
    {
        if (sender is not FrameworkElement el || el.Tag is not string path) return;
        FileTile? tile = null;
        foreach (var t in ViewModel.Items)
        {
            if (t.Path == path) { tile = t; break; }
        }
        if (tile is null) return;

        var sheet = new FilePreviewSheet();
        sheet.SetFile(tile.Path, tile.Kind, tile.SizeBytes, tile.ModifiedAt, tile.Id);
        var dialog = new Microsoft.UI.Xaml.Controls.ContentDialog
        {
            XamlRoot = this.XamlRoot,
            Content = sheet,
            CloseButtonText = "Close",
            DefaultButton = Microsoft.UI.Xaml.Controls.ContentDialogButton.Close,
        };
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
        EngineClient.Instance.PropertyChanged += OnceHandler;
        void OnceHandler(object? _, System.ComponentModel.PropertyChangedEventArgs ev)
        {
            if (ev.PropertyName != nameof(EngineClient.LastClipTextEmbedding)) return;
            var emb = EngineClient.Instance.LastClipTextEmbedding;
            if (emb is null || emb.QueryId != queryId) return;
            EngineClient.Instance.PropertyChanged -= OnceHandler;
            tcs.TrySetResult(emb.Embedding?.ToArray());
        }
        try
        {
            await EngineClient.Instance.EmbedImageQueryAsync(fileId, queryId);
        }
        catch
        {
            EngineClient.Instance.PropertyChanged -= OnceHandler;
            return;
        }
        // 5-second timeout.
        var timeoutTask = System.Threading.Tasks.Task.Delay(TimeSpan.FromSeconds(5));
        var done = await System.Threading.Tasks.Task.WhenAny(tcs.Task, timeoutTask);
        EngineClient.Instance.PropertyChanged -= OnceHandler;
        if (done != tcs.Task) return;
        var seed = await tcs.Task;
        if (seed == null) return;
        // Run a semantic search against the existing ReadStore via the
        // ViewModel's path. We don't need a separate sheet — the Library
        // grid itself is the result list.
        await ViewModel.SemanticSearchWithSeedAsync(seed, System.Threading.CancellationToken.None);
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
