using FileID.Services;
using FileID.ViewModels;
using Windows.Devices.Enumeration;

namespace FileID.Views.Library;

/// <summary>OFFLINE tiles (macOS OfflineRoots): reads the locations with listRoots on load,
/// after each scan and when a drive is plugged in or removed.</summary>
public sealed partial class LibraryView
{
    private static readonly TimeSpan VolumeSettleDelay = TimeSpan.FromSeconds(1.5);
    private CancellationTokenSource? _rootsLifetime;
    private DeviceWatcher? _volumeWatcher;
    private Microsoft.UI.Dispatching.DispatcherQueue? _rootsUi;
    private int _rootsRefreshing;
    private int _rootsRefreshQueued;
    private int _volumeEnumerated;

    private void StartOfflineRoots()
    {
        if (_unloaded || _rootsLifetime is not null) return;
        _rootsLifetime = new CancellationTokenSource();
        _rootsUi = DispatcherQueue;
        RefreshOfflineRoots();
        try
        {
            var watcher = DeviceInformation.CreateWatcher(DeviceClass.PortableStorageDevice);
            watcher.Added += OnVolumeAdded;
            watcher.Removed += OnVolumeRemoved;
            watcher.Updated += OnVolumeUpdated;
            watcher.EnumerationCompleted += OnVolumeEnumerationCompleted;
            watcher.Start();
            _volumeWatcher = watcher;
        }
        catch (Exception ex)
        {
            DebugLog.Warn("LibraryView: drive watcher unavailable: " + ex.Message);
        }
    }

    private void StopOfflineRoots()
    {
        var lifetime = _rootsLifetime;
        _rootsLifetime = null;
        lifetime?.Cancel();
        lifetime?.Dispose();
        if (_volumeWatcher is not { } watcher) return;
        _volumeWatcher = null;
        watcher.Added -= OnVolumeAdded;
        watcher.Removed -= OnVolumeRemoved;
        watcher.Updated -= OnVolumeUpdated;
        watcher.EnumerationCompleted -= OnVolumeEnumerationCompleted;
        try
        {
            if (watcher.Status is DeviceWatcherStatus.Started or DeviceWatcherStatus.EnumerationCompleted) watcher.Stop();
        }
        catch (Exception ex)
        {
            DebugLog.Warn("LibraryView: drive watcher stop failed: " + ex.Message);
        }
    }

    private void OnVolumeEnumerationCompleted(DeviceWatcher sender, object args)
        => Interlocked.Exchange(ref _volumeEnumerated, 1);

    private void OnVolumeAdded(DeviceWatcher sender, DeviceInformation args) => OnVolumeChanged();

    private void OnVolumeRemoved(DeviceWatcher sender, DeviceInformationUpdate args) => OnVolumeChanged();

    // DeviceWatcher delivers post-enumeration Added/Removed only while Updated is subscribed too.
    private void OnVolumeUpdated(DeviceWatcher sender, DeviceInformationUpdate args) { }

    private void OnVolumeChanged()
    {
        if (Volatile.Read(ref _volumeEnumerated) == 0 || _rootsLifetime is not { } lifetime || _rootsUi is not { } ui) return;
        CancellationToken ct;
        try { ct = lifetime.Token; }
        catch (ObjectDisposedException) { return; }
        _ = DebugLog.SafeRunAsync("LibraryView.VolumeChanged", async () =>
        {
            try { await Task.Delay(VolumeSettleDelay, ct).ConfigureAwait(false); }
            catch (OperationCanceledException) { return; }
            ui.TryEnqueue(RefreshOfflineRoots);
        });
    }

    private void RefreshOfflineRoots()
    {
        if (_unloaded || _rootsLifetime is not { } lifetime) return;
        if (Interlocked.Exchange(ref _rootsRefreshing, 1) != 0)
        {
            Interlocked.Exchange(ref _rootsRefreshQueued, 1);
            return;
        }
        var ct = lifetime.Token;
        var ui = DispatcherQueue;
        _ = DebugLog.SafeRunAsync("LibraryView.RefreshOfflineRoots", async () =>
        {
            try
            {
                var list = await EngineClient.Instance.ListRootsAsync(ct).ConfigureAwait(false);
                var roots = OfflineRoots.From(list.Roots);
                ui.TryEnqueue(() => DebugLog.SafeRun("LibraryView.ApplyOfflineRoots", () => ApplyOfflineRoots(roots)));
            }
            catch (OperationCanceledException) when (ct.IsCancellationRequested) { }
            catch (Exception ex)
            {
                DebugLog.Warn("LibraryView: listRoots failed: " + ex.Message);
            }
            finally
            {
                Interlocked.Exchange(ref _rootsRefreshing, 0);
                if (Interlocked.Exchange(ref _rootsRefreshQueued, 0) != 0 && !ct.IsCancellationRequested)
                {
                    ui.TryEnqueue(RefreshOfflineRoots);
                }
            }
        });
    }

    private void ApplyOfflineRoots(OfflineRoots roots)
    {
        if (_unloaded || !ViewModel.ApplyOfflineRoots(roots)) return;
        if (IsPaneOpen && SelectedPane.Tile is { } tile) SelectedPane.ApplyOffline(tile.IsOffline);
    }
}
