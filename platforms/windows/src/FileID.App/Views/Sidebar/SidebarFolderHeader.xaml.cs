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

    // disable the button while a picker dialog is open so a
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
            var hwnd = (XamlRoot?.ContentIslandEnvironment?.AppWindowId is { Value: not 0 })
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

    // re-entrancy guard. The wipe flow is multi-second (shutdown +
    // wait + file delete + respawn). A second click that lands while the
    // first is in flight would race the deletes against the new engine's
    // open + likely corrupt the fresh DB. Set true at the top of the
    // confirmed wipe, cleared in finally.
    private int _wipeInFlight; // 0 = idle, 1 = wipe running

    private async void OnWipeClicked(object sender, RoutedEventArgs e)
    {
        // gate BEFORE the confirmation dialog. The dialog itself
        // is modal but WinUI 3 still dispatches click events even while a
        // ContentDialog is open; a fast double-click could open two
        // sequential dialogs and run two wipes back-to-back if the gate
        // were placed after confirmation. Moving the gate here also means
        // the button doesn't appear to respond to a rapid second click,
        // matching the "first wipe wins" expectation.
        if (System.Threading.Interlocked.CompareExchange(ref _wipeInFlight, 1, 0) != 0)
        {
            DebugLog.Info("[WIPE] already in flight; ignoring second click.");
            return;
        }
        try
        {
            DebugLog.Info("[WIPE] OnWipeClicked entered");
            var dialog = new ContentDialog
            {
                XamlRoot = XamlRoot,
                Title = "Wipe everything?",
                Content = "This deletes everything FileID has learned — tags, face clusters, captions, smart names — and returns the app to a clean slate. Your actual files are never touched, and your downloaded AI models are kept.\n\nThis can't be undone.",
                PrimaryButtonText = "Wipe",
                CloseButtonText = "Cancel",
                DefaultButton = ContentDialogButton.Close,
            };
            var result = await dialog.ShowAsync();
            if (result != ContentDialogResult.Primary)
            {
                DebugLog.Info("[WIPE] cancelled at confirm dialog");
                return;
            }

            await RunWipeAsync();
        }
        catch (Exception ex)
        {
            DebugLog.Warn("[WIPE] OnWipeClicked threw: " + ex);
        }
        finally
        {
            System.Threading.Interlocked.Exchange(ref _wipeInFlight, 0);
        }
    }

    /// <summary>extracted from OnWipeClicked so the engine ALWAYS
    /// comes back up — even when file delete fails. Previously every
    /// failure path early-returned without calling StartAsync, leaving
    /// the engine permanently Crashed and the entire app unusable. The
    /// finally block now restarts the engine unconditionally; the user
    /// sees "Wipe partially failed" alert but still has a working app.
    ///
    /// Stages (each logged so a wipe hang or silent failure is
    /// localizable from app.log alone — the original wipe code logged
    /// only the IPC OUT line and went silent for 27 s on the user's
    /// first repro):
    ///   1. ClearPhaseAndError + reset LastProgress/LastBatch — sidebar
    ///      stops showing the prior scan's "Completed" panel during the
    ///      shutdown window.
    ///   2. StopAndWaitForExitAsync — releases the SQLite handle.
    ///   3. Delete fileid.sqlite + WAL/SHM with retry (kernel FILE_OBJECT
    ///      can stay live ~200 ms past process exit).
    ///   4. StartAsync — fresh engine respawns, migrations re-apply on the
    ///      empty DB. ALWAYS attempted in the finally so a delete failure
    ///      doesn't strand the user.</summary>
    private async Task RunWipeAsync()
    {
        DebugLog.Info("[WIPE] stage 1: clear stale UI state");
        try
        {
            EngineClient.Instance.ResetForWipe();
        }
        catch (Exception ex)
        {
            DebugLog.Warn("[WIPE] ResetForWipe threw (non-fatal): " + ex.Message);
        }

        // Preferred path: ask the running engine to wipe in-process. It owns
        // the only SQLite handle, so truncating tables there can't hit the
        // "file in use by another process" race the app-side delete did — no
        // shutdown/restart. On success we go straight to a fresh rescan.
        if (EngineClient.Instance.State == EngineClient.LifecycleState.Ready)
        {
            try
            {
                DebugLog.Info("[WIPE] engine-side wipeLibrary");
                var wipeResult = await EngineClient.Instance.WipeLibraryAndWaitAsync(TimeSpan.FromSeconds(30));
                if (wipeResult.Ok)
                {
                    DebugLog.Info("[WIPE] engine confirmed libraryWiped");
                    await FinishWipeAsync(success: true);
                    return;
                }
                DebugLog.Warn("[WIPE] engine wipe ok=false: " + (wipeResult.Message ?? "(no message)") + " — using fallback");
            }
            catch (Exception ex)
            {
                DebugLog.Warn("[WIPE] engine-side wipe failed; using fallback: " + ex.Message);
            }
        }
        else
        {
            DebugLog.Info($"[WIPE] engine not Ready (state={EngineClient.Instance.State}); using fallback wipe");
        }

        DebugLog.Info("[WIPE] stage 2: shutdown engine");
        try
        {
            await EngineClient.Instance.StopAndWaitForExitAsync(TimeSpan.FromSeconds(10));
            DebugLog.Info("[WIPE] stage 2 complete");
        }
        catch (Exception ex)
        {
            DebugLog.Warn("[WIPE] stage 2 (shutdown) threw: " + ex.Message);
            // Continue anyway — engine may already be dead.
        }

        DebugLog.Info("[WIPE] stage 3: delete DB files");
        string? deleteError = null;
        try
        {
            foreach (var name in new[] { "fileid.sqlite", "fileid.sqlite-wal", "fileid.sqlite-shm" })
            {
                var path = System.IO.Path.Combine(AppPaths.Root, name);
                DebugLog.Info($"[WIPE] deleting {PathRedactor.Redact(path)}");
                await TryDeleteWithRetryAsync(path);
            }
            DebugLog.Info("[WIPE] stage 3 complete (DB files deleted)");
        }
        catch (Exception ex)
        {
            deleteError = ex.Message;
            DebugLog.Warn("[WIPE] stage 3 (delete) threw: " + ex);
        }

        // stage 3b: face_crops/ and thumbs.cache/. Earlier wipes
        // only deleted the SQLite trio, leaving stale face crops and
        // thumbnails on disk that the fresh DB would re-reference (face
        // crops live alongside face_print rows; thumbnails are content-
        // hashed but a wipe shouldn't accumulate orphaned files). Best-
        // effort + non-fatal — a partial dir delete is degraded but
        // recoverable, and gating the engine restart on it would strand
        // the user the same way pre-3b DB-only failures did.
        try
        {
            foreach (var dir in new[] { AppPaths.FacesDir, AppPaths.ThumbsDir })
            {
                if (System.IO.Directory.Exists(dir))
                {
                    DebugLog.Info($"[WIPE] deleting dir {PathRedactor.Redact(dir)}");
                    await TryDeleteDirWithRetryAsync(dir);
                }
            }
            DebugLog.Info("[WIPE] stage 3b complete (face_crops + thumbs.cache cleared)");
        }
        catch (Exception ex)
        {
            // Don't promote to deleteError — face crops and thumbnails
            // surviving a wipe is degraded but not user-visible until
            // they re-appear in People / Library, by which point the
            // new scan will have regenerated them anyway.
            DebugLog.Warn("[WIPE] stage 3b (face_crops/thumbs) threw (non-fatal): " + ex.Message);
        }

        // ALWAYS attempt restart, even when the delete failed.
        // Without this, a single locked WAL leaves the user with a dead
        // engine and no recovery path short of relaunching the app.
        DebugLog.Info("[WIPE] stage 4: restart engine");
        try
        {
            await EngineClient.Instance.StartAsync();
            DebugLog.Info("[WIPE] stage 4 complete (StartAsync returned)");
        }
        catch (Exception ex)
        {
            DebugLog.Warn("[WIPE] stage 4 (StartAsync) threw: " + ex);
        }

        await FinishWipeAsync(success: deleteError is null, failureDetail: deleteError);
    }

    /// <summary>Final step of a wipe: reset the app to its first-run clean
    /// state and tell the user. We deliberately do NOT rescan — "Wipe" leaves
    /// the library empty until the user picks a folder again. Clearing
    /// FolderPath nulls LastFolderPath/LastFolderDisplay in settings and raises
    /// HasFolder, which drives the sidebar back to the empty picker (the same
    /// path "Clear folder" takes). Downloaded models under %LOCALAPPDATA%\FileID\
    /// Models are intentionally kept — they're not library state and are
    /// expensive to re-download.</summary>
    private async Task FinishWipeAsync(bool success, string? failureDetail = null)
    {
        try { AppViewModel.Instance.FolderPath = null; }
        catch (Exception ex) { DebugLog.Warn("[WIPE] clearing folder threw (non-fatal): " + ex.Message); }

        if (success)
        {
            DebugLog.Info("[WIPE] complete — reset to clean state");
            await ShowAlertAsync(
                "Library wiped",
                "FileID is back to a clean slate. Your files and downloaded models are untouched — pick a folder whenever you want to start a fresh scan.");
        }
        else
        {
            await ShowAlertAsync(
                "Wipe partially failed",
                $"Couldn't delete some database files: {failureDetail}\n\n" +
                "The engine has been restarted and the folder cleared, but old library data may still be present. " +
                "If a later scan surfaces old data, close FileID, delete " +
                "%LOCALAPPDATA%\\FileID\\fileid.sqlite manually, and relaunch.");
        }
    }

    // Windows can hold a FILE_OBJECT live for a few hundred ms after the
    // owning process exits — handle-close cleanup is asynchronous. Retry
    // a couple of times before letting the final IOException propagate to
    // the caller's try/catch (which surfaces the user-visible error dialog).
    private static async Task TryDeleteWithRetryAsync(string path)
    {
        // The engine-side wipeLibrary path avoids the post-exit FILE_OBJECT
        // race entirely; this retry only backstops the fallback delete.
        // Exponential backoff (~3 s total) gives the kernel ample time to
        // release the handle before the IOException propagates to the caller.
        const int maxAttempts = 6;
        for (int attempt = 0; attempt < maxAttempts; attempt++)
        {
            try
            {
                if (System.IO.File.Exists(path)) System.IO.File.Delete(path);
                return;
            }
            catch (System.IO.IOException) when (attempt < maxAttempts - 1)
            {
                await Task.Delay(100 * (1 << attempt));
            }
            catch (UnauthorizedAccessException) when (attempt < maxAttempts - 1)
            {
                await Task.Delay(100 * (1 << attempt));
            }
        }
    }

    /// <summary>Recursive directory delete with the same FILE_OBJECT
    /// retry profile as the per-file delete above. The thumbs cache
    /// can hold thousands of small files — recreating the empty
    /// directory after delete keeps the next scan's first thumbnail
    /// write from racing against EnsureDirectories on engine startup.</summary>
    private static async Task TryDeleteDirWithRetryAsync(string dir)
    {
        for (int attempt = 0; attempt < 3; attempt++)
        {
            try
            {
                if (System.IO.Directory.Exists(dir))
                {
                    System.IO.Directory.Delete(dir, recursive: true);
                }
                System.IO.Directory.CreateDirectory(dir);
                return;
            }
            catch (System.IO.IOException) when (attempt < 2)
            {
                await Task.Delay(200);
            }
            catch (UnauthorizedAccessException) when (attempt < 2)
            {
                await Task.Delay(200);
            }
        }
    }

    private async Task ShowAlertAsync(string title, string body)
    {
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
