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

    // V15.2: disable the button while a picker dialog is open so a
    // double-click can't spawn two concurrent pickers and race their
    // FolderPath setters.
    private int _pickInFlight; // 0 = idle, 1 = picker open

    private async void OnPickClicked(object sender, RoutedEventArgs e)
    {
        if (System.Threading.Interlocked.CompareExchange(ref _pickInFlight, 1, 0) != 0)
        {
            DebugLog.Info("OnPickClicked: picker already open; ignoring second click.");
            return;
        }
        if (sender is Button btn) btn.IsEnabled = false;
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
            try { DebugLog.WriteCrashDump("SidebarFolderHeader.OnPickClicked", ex, terminating: false); } catch { }
        }
        finally
        {
            System.Threading.Interlocked.Exchange(ref _pickInFlight, 0);
            if (sender is Button btn2) btn2.IsEnabled = true;
        }
    }

    private void OnClearClicked(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnClearClicked), () => AppViewModel.Instance.FolderPath = null);

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
            //   1. Tell the engine to shut down AND wait for the process
            //      to actually exit — without the wait, the SQLite handle
            //      is still open and the deletes below hit a sharing
            //      violation.
            //   2. Delete fileid.sqlite + the WAL/SHM sidecars, retrying
            //      a few times since the Windows kernel can keep the
            //      FILE_OBJECT alive for a few hundred ms after the
            //      process exits.
            //   3. EngineClient's auto-respawn brings up a fresh engine
            //      pointing at a fresh DB. Migrations re-apply.
            //   4. Library/People/Cleanup tabs auto-refresh on the empty
            //      DB via their existing PropertyChanged listeners.
            await EngineClient.Instance.StopAndWaitForExitAsync(TimeSpan.FromSeconds(10));

            try
            {
                foreach (var name in new[] { "fileid.sqlite", "fileid.sqlite-wal", "fileid.sqlite-shm" })
                {
                    var path = System.IO.Path.Combine(AppPaths.Root, name);
                    await TryDeleteWithRetryAsync(path);
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

    // Windows can hold a FILE_OBJECT live for a few hundred ms after the
    // owning process exits — handle-close cleanup is asynchronous. Retry
    // a couple of times before letting the final IOException propagate to
    // the caller's try/catch (which surfaces the user-visible error dialog).
    private static async Task TryDeleteWithRetryAsync(string path)
    {
        for (int attempt = 0; attempt < 3; attempt++)
        {
            try
            {
                if (System.IO.File.Exists(path)) System.IO.File.Delete(path);
                return;
            }
            catch (System.IO.IOException) when (attempt < 2)
            {
                await Task.Delay(200);
            }
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
        => DebugLog.SafeRun(nameof(OnCollapseClicked), () => AppViewModel.Instance.ToggleSidebar());

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
