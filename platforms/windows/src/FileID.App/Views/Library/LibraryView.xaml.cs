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

    public LibraryView()
    {
        var paths = AppPaths.DbPath;
        var store = new ReadStore(paths);
        var clip = new ClipSearchService(store);
        ViewModel = new LibraryViewModel(store, clip, Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread());

        InitializeComponent();
        ViewModel.PropertyChanged += (_, e) =>
        {
            if (e.PropertyName is nameof(LibraryViewModel.IsLoading)
                or nameof(LibraryViewModel.ErrorMessage))
            {
                OnPropertyChanged(nameof(StatusText));
                OnPropertyChanged(nameof(FooterVisibility));
            }
        };
        ViewModel.Items.CollectionChanged += (_, _) =>
        {
            OnPropertyChanged(nameof(StatusText));
            OnPropertyChanged(nameof(FooterVisibility));
        };

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
        sheet.SetFile(tile.Path, tile.Kind, tile.SizeBytes, null);
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
        if (sender is MenuFlyoutItem item && item.Tag is string path && System.IO.File.Exists(path))
        {
            try
            {
                System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo
                {
                    FileName = path,
                    UseShellExecute = true,
                });
            }
            catch { /* user-facing toast lands when we wire a global error surface */ }
        }
    }

    private void OnContextReveal(object sender, RoutedEventArgs e)
    {
        if (sender is MenuFlyoutItem item && item.Tag is string path && System.IO.File.Exists(path))
        {
            try
            {
                System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo
                {
                    FileName = "explorer.exe",
                    Arguments = $"/select,\"{path}\"",
                    UseShellExecute = true,
                });
            }
            catch { /* swallow — non-critical */ }
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
