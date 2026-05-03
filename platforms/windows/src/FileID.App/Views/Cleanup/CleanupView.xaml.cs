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

    public CleanupView()
    {
        ViewModel = new CleanupViewModel(AppPaths.DbPath, Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread());
        InitializeComponent();
        ViewModel.PropertyChanged += (_, _) =>
        {
            OnPropertyChanged(nameof(StatusText));
            OnPropertyChanged(nameof(FooterVisibility));
        };
        ViewModel.Groups.CollectionChanged += (_, _) =>
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

    public event PropertyChangedEventHandler? PropertyChanged;
    private void OnPropertyChanged(string name)
        => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}
