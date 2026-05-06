// SidebarFolderHeader code-behind. Wires the picker / clear / wipe
// actions to the AppViewModel and EngineClient. The visibility of the
// "actions" vs "empty picker" sections is driven by AppViewModel.HasFolder
// changes.

using System.ComponentModel;
using System.IO;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FileID.Views.Sidebar;

public sealed partial class SidebarFolderHeader : UserControl
{
    public SidebarFolderHeader()
    {
        InitializeComponent();
        Loaded += (_, _) => Sync();
        AppViewModel.Instance.PropertyChanged += OnAppViewModelChanged;
        Unloaded += (_, _) => AppViewModel.Instance.PropertyChanged -= OnAppViewModelChanged;
    }

    private void OnAppViewModelChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName is nameof(AppViewModel.FolderPath)
                          or nameof(AppViewModel.FolderDisplay)
                          or nameof(AppViewModel.HasFolder))
        {
            DispatcherQueue.TryEnqueue(Sync);
        }
    }

    private void Sync()
    {
        var vm = AppViewModel.Instance;
        if (vm.HasFolder)
        {
            EmptyPickerPanel.Visibility = Visibility.Collapsed;
            FolderDisplayPanel.Visibility = Visibility.Visible;
            ActionsPanel.Visibility = Visibility.Visible;

            var path = vm.FolderPath ?? "";
            // Parent path muted, leaf gold (matches macOS Sidebar.swift:164).
            var parent = Path.GetDirectoryName(path);
            ParentPathText.Text = string.IsNullOrEmpty(parent) ? "" : parent;
            LeafNameText.Text = vm.FolderDisplay ?? path;
        }
        else
        {
            EmptyPickerPanel.Visibility = Visibility.Visible;
            FolderDisplayPanel.Visibility = Visibility.Collapsed;
            ActionsPanel.Visibility = Visibility.Collapsed;
        }
    }

    private async void OnPickClicked(object sender, RoutedEventArgs e)
    {
        try
        {
            var hwnd = (this.XamlRoot?.ContentIslandEnvironment?.AppWindowId is { Value: not 0 })
                ? GetParentHwnd()
                : GetParentHwnd();
            var result = await FolderPickerService.PickFolderAsync(hwnd);

            if (result.FailureReason is not null)
            {
                await ShowAlertAsync("Couldn't open folder", result.FailureReason);
                return;
            }
            if (result.Path is null)
            {
                // User cancelled — no-op.
                return;
            }

            AppViewModel.Instance.FolderPath = result.Path;
            DebugLog.Info($"FolderPicker: chose {PathRedactor.Redact(result.Path)}");
        }
        catch (Exception ex)
        {
            DebugLog.Warn("OnPickClicked threw: " + ex);
        }
    }

    private void OnClearClicked(object sender, RoutedEventArgs e)
    {
        AppViewModel.Instance.FolderPath = null;
    }

    private async void OnWipeClicked(object sender, RoutedEventArgs e)
    {
        try
        {
            var dialog = new ContentDialog
            {
                XamlRoot = this.XamlRoot,
                Title = "Wipe library + rescan?",
                Content = "This deletes everything FileID has learned about this folder — tags, face clusters, captions, smart names. The folder itself isn't touched. After wiping, FileID will rescan from scratch.\n\nThis can't be undone.",
                PrimaryButtonText = "Wipe and rescan",
                CloseButtonText = "Cancel",
                DefaultButton = ContentDialogButton.Close,
            };
            var result = await dialog.ShowAsync();
            if (result != ContentDialogResult.Primary)
            {
                return;
            }

            // Real wipe flow:
            //   1. Tell the engine to shut down (releases the SQLite WAL).
            //   2. Wait briefly for the process to actually exit so the file
            //      lock is released — without this the deletes below race.
            //   3. Delete fileid.sqlite + the WAL/SHM sidecars.
            //   4. EngineClient's auto-respawn (1s/4s/16s backoff) brings up
            //      a fresh engine pointing at a fresh DB. Migrations re-apply.
            //   5. Library/People/Cleanup tabs auto-refresh on the empty DB
            //      via their existing PropertyChanged listeners.
            await EngineClient.Instance.ShutdownAsync();
            await Task.Delay(800);

            try
            {
                foreach (var name in new[] { "fileid.sqlite", "fileid.sqlite-wal", "fileid.sqlite-shm" })
                {
                    var path = System.IO.Path.Combine(AppPaths.Root, name);
                    if (System.IO.File.Exists(path))
                    {
                        System.IO.File.Delete(path);
                    }
                }
                DebugLog.Info("Wipe-and-rescan: DB files deleted.");
            }
            catch (Exception ex)
            {
                await ShowAlertAsync(
                    "Wipe partially failed",
                    $"Couldn't delete DB files: {ex.Message}\n\n" +
                    "Close FileID, delete %LOCALAPPDATA%\\FileID\\fileid.sqlite manually, " +
                    "and relaunch — the engine will rebuild on next start.");
                return;
            }

            // Force the engine to come back up now (don't wait for the
            // backoff window; user explicitly asked for a rescan).
            await EngineClient.Instance.StartAsync();
        }
        catch (Exception ex)
        {
            DebugLog.Warn("OnWipeClicked threw: " + ex);
        }
    }

    private async Task ShowAlertAsync(string title, string body)
    {
        var dialog = new ContentDialog
        {
            XamlRoot = this.XamlRoot,
            Title = title,
            Content = body,
            CloseButtonText = "OK",
            DefaultButton = ContentDialogButton.Close,
        };
        await dialog.ShowAsync();
    }

    private void OnCollapseClicked(object sender, RoutedEventArgs e)
    {
        AppViewModel.Instance.ToggleSidebar();
    }

    private IntPtr GetParentHwnd()
    {
        // Walk up the visual tree to the host Window's HWND.
        if (App.HostWindow is { } window)
        {
            return WinRT.Interop.WindowNative.GetWindowHandle(window);
        }
        return IntPtr.Zero;
    }
}
