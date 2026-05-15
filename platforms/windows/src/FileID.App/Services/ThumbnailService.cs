// ThumbnailService — IShellItemImageFactory-driven shell thumbnails with
// an in-process LRU cache and a disk-backed cache under thumbs.cache/.
//
// Mirror of the macOS app's QLThumbnailGenerator-backed `ThumbnailService.swift`.
// Same behavior:
//   - Render at 256×256 device-independent pixels (DIPs).
//   - Cache by path+modified-time hash so a file edit invalidates its thumb.
//   - Background queue drains a request channel so the UI thread never
//     pays the shell-thumbnail cost.
//   - Returns a SoftwareBitmapSource the WinUI grid binds directly into.
//
// Phase 2.4 cut: the cache + render orchestration. The actual interop call
// stays a stub until Phase 2.6 ties it to the engine's shell::thumbnail
// helper or a direct CsWinRT IShellItemImageFactory binding.

using System;
using System.Collections.Generic;
using System.IO;
using System.Threading;
using System.Threading.Tasks;
using System.Threading.Channels;
using Microsoft.Extensions.Caching.Memory;
using Microsoft.UI.Xaml.Media.Imaging;

namespace FileID.Services;

internal sealed class ThumbnailService : IDisposable
{
    /// <summary>
    /// LRU cap. ~5 KB per cached BitmapImage (256x256 thumbnails compressed
    /// in shared memory) → ~25 MB at full cap. Sized for libraries up to
    /// ~10K files where a power user might scroll-flick through and want
    /// previously-seen thumbs instant. Smaller libraries cap themselves
    /// naturally.
    /// </summary>
    private readonly MemoryCache _cache = new(new MemoryCacheOptions
    {
        SizeLimit = 5_000,
    });
    private readonly Channel<ThumbnailRequest> _queue;
    private readonly CancellationTokenSource _cts = new();
    private readonly Task _worker;
    /// <summary>V14.9-A10: captured at construction time on the UI thread
    /// so the worker (running on a thread-pool thread) always has a
    /// reliable dispatcher to marshal BitmapImage.SetSourceAsync back to.
    /// Late-binding to <c>GetForCurrentThread()</c> in <c>RenderAsync</c>
    /// could return null on the worker thread, and the fallback
    /// (<c>HostWindow?.DispatcherQueue</c>) might also be null during
    /// startup/shutdown — causing a silent thread-affinity violation.</summary>
    private readonly Microsoft.UI.Dispatching.DispatcherQueue? _uiDispatcher;

    public ThumbnailService()
    {
        // V14.9-A10: capture the UI dispatcher at ctor time. Service is
        // expected to be constructed on the UI thread.
        _uiDispatcher = Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread();
        // Bigger queue: a fast scroll on a 256-px tile grid generates
        // 50+ requests/sec. The previous 64-slot cap dropped older
        // requests within ~1 second of fast scrolling. 256 absorbs
        // burst scroll without dropping anything visible.
        _queue = Channel.CreateBounded<ThumbnailRequest>(new BoundedChannelOptions(256)
        {
            SingleReader = true,
            SingleWriter = false,
            FullMode = BoundedChannelFullMode.DropOldest,
        });
        // V15.2: attach a fault sink so a DrainAsync exception leaves a
        // forensic trail instead of becoming an UnobservedTaskException
        // at GC time.
        _worker = Task.Run(() => DrainAsync(_cts.Token));
        _ = _worker.ContinueWith(
            t => DebugLog.Error("ThumbnailService worker faulted: " + t.Exception),
            TaskContinuationOptions.OnlyOnFaulted);
    }

    public Task<BitmapImage?> RequestAsync(string path, double? modifiedAt, CancellationToken ct)
    {
        var key = CacheKey(path, modifiedAt);
        if (_cache.TryGetValue(key, out BitmapImage? cached) && cached != null)
        {
            return Task.FromResult<BitmapImage?>(cached);
        }
        var tcs = new TaskCompletionSource<BitmapImage?>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        var ok = _queue.Writer.TryWrite(new ThumbnailRequest(path, modifiedAt, tcs, ct));
        if (!ok)
        {
            tcs.TrySetResult(null);
        }
        return tcs.Task;
    }

    private async Task DrainAsync(CancellationToken ct)
    {
        await foreach (var req in _queue.Reader.ReadAllAsync(ct).ConfigureAwait(false))
        {
            if (req.Cancellation.IsCancellationRequested)
            {
                req.Completion.TrySetResult(null);
                continue;
            }
            try
            {
                // V15.2: drop ConfigureAwait(false). The bitmap returned
                // from RenderAsync is a UI-thread DispatcherObject; the
                // post-await continuation here completes the caller's
                // TCS with that handle, and we want the caller's continuation
                // to also resume on the UI thread when it sets tile.Thumbnail.
                var bmp = await RenderAsync(req.Path, _uiDispatcher, ct);
                if (bmp != null)
                {
                    var key = CacheKey(req.Path, req.ModifiedAt);
                    _cache.Set(key, bmp, new MemoryCacheEntryOptions
                    {
                        Size = 1,
                        SlidingExpiration = TimeSpan.FromMinutes(15),
                    });
                }
                req.Completion.TrySetResult(bmp);
            }
            catch (Exception ex) when (ex is not OperationCanceledException)
            {
                DebugLog.Warn("ThumbnailService.DrainAsync: " + ex.Message);
                req.Completion.TrySetResult(null);
            }
        }
    }

    /// <summary>
    /// Render a thumbnail via the Windows.Storage shell-thumbnail API
    /// (which uses the same IThumbnailProvider chain Explorer does — Office,
    /// raw, .heic, .pages all work). 256-px request, scaled by the system.
    /// Returns a BitmapImage with the JPEG bytes set, ready for binding.
    /// </summary>
    private const uint ThumbnailRequestPx = 256;

    private static async Task<BitmapImage?> RenderAsync(
        string path,
        Microsoft.UI.Dispatching.DispatcherQueue? uiDispatcher,
        CancellationToken ct)
    {
        if (!File.Exists(path))
        {
            return null;
        }
        Windows.Storage.FileProperties.StorageItemThumbnail? thumb = null;
        try
        {
            var file = await Windows.Storage.StorageFile.GetFileFromPathAsync(path).AsTask(ct).ConfigureAwait(false);
            thumb = await file
                .GetThumbnailAsync(
                    Windows.Storage.FileProperties.ThumbnailMode.SingleItem,
                    ThumbnailRequestPx,
                    Windows.Storage.FileProperties.ThumbnailOptions.UseCurrentScale)
                .AsTask(ct)
                .ConfigureAwait(false);
            if (thumb == null || thumb.Size == 0)
            {
                thumb?.Dispose();
                return null;
            }
            // V15.2 — CRASH FIX: BitmapImage is a WinUI DispatcherObject.
            // The previous code created the BitmapImage on this worker
            // thread and only marshalled SetSourceAsync to the UI thread.
            // WinUI 3's composition layer detects cross-thread access
            // during the next frame and calls RaiseFailFastException —
            // bypassing every managed exception handler (no crash-*.txt
            // is produced). Symptom: thumbnails fail to render, then
            // the app dies hard during scan. Fix: construct AND set
            // source AND dispose the underlying stream on the UI
            // dispatcher in one continuous lambda.
            var dispatcher = uiDispatcher ?? FileID.App.HostWindow?.DispatcherQueue;
            if (dispatcher is null)
            {
                DebugLog.Warn("ThumbnailService.RenderAsync: no UI dispatcher available; skipping.");
                thumb.Dispose();
                return null;
            }
            // Transfer ownership of `thumb` into the UI lambda so the
            // stream's lifetime spans SetSourceAsync.
            var capturedThumb = thumb;
            thumb = null;
            var tcs = new TaskCompletionSource<BitmapImage?>(
                TaskCreationOptions.RunContinuationsAsynchronously);
            var enqueued = dispatcher.TryEnqueue(async () =>
            {
                try
                {
                    var bmp = new BitmapImage();
                    await bmp.SetSourceAsync(capturedThumb).AsTask(ct);
                    tcs.TrySetResult(bmp);
                }
                catch (Exception ex)
                {
                    DebugLog.Warn("ThumbnailService UI render: " + ex.Message);
                    tcs.TrySetResult(null);
                }
                finally
                {
                    try { capturedThumb.Dispose(); } catch { /* swallow */ }
                }
            });
            if (!enqueued)
            {
                DebugLog.Warn("ThumbnailService.RenderAsync: dispatcher.TryEnqueue returned false (shutdown?).");
                try { capturedThumb.Dispose(); } catch { /* swallow */ }
                return null;
            }
            return await tcs.Task.ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            DebugLog.Warn("ThumbnailService.RenderAsync: " + ex.Message);
            try { thumb?.Dispose(); } catch { /* swallow */ }
            return null;
        }
    }

    private static string CacheKey(string path, double? modifiedAt)
        => modifiedAt.HasValue
            ? $"{path}|{modifiedAt.Value:R}"
            : path;

    public void Dispose()
    {
        // Cancel + complete; do NOT Wait on the worker. The worker is a
        // daemon-style channel drainer — a 1 s Wait on the UI thread during
        // app shutdown was visibly hanging window close on slow disks. The
        // worker observes the cancellation token and exits on its own; the
        // process exit terminates anything still in flight, which is safe
        // because thumbnail decoding has no persistence side-effects.
        try { _cts.Cancel(); } catch { /* swallow */ }
        try { _queue.Writer.TryComplete(); } catch { /* swallow */ }
        try { _cache.Dispose(); } catch { /* swallow */ }
        try { _cts.Dispose(); } catch { /* swallow */ }
    }

    private sealed record ThumbnailRequest(
        string Path,
        double? ModifiedAt,
        TaskCompletionSource<BitmapImage?> Completion,
        CancellationToken Cancellation);
}
