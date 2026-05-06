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

    public ThumbnailService()
    {
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
        _worker = Task.Run(() => DrainAsync(_cts.Token));
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
                var bmp = await RenderAsync(req.Path, ct).ConfigureAwait(false);
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

    private static async Task<BitmapImage?> RenderAsync(string path, CancellationToken ct)
    {
        if (!File.Exists(path))
        {
            return null;
        }
        try
        {
            var file = await Windows.Storage.StorageFile.GetFileFromPathAsync(path).AsTask(ct).ConfigureAwait(false);
            using var thumb = await file
                .GetThumbnailAsync(
                    Windows.Storage.FileProperties.ThumbnailMode.SingleItem,
                    ThumbnailRequestPx,
                    Windows.Storage.FileProperties.ThumbnailOptions.UseCurrentScale)
                .AsTask(ct)
                .ConfigureAwait(false);
            if (thumb == null || thumb.Size == 0)
            {
                return null;
            }
            // BitmapImage.SetSourceAsync must run on the UI thread because
            // BitmapImage is a DispatcherObject. Marshal back via the
            // current dispatcher; the caller awaits us on the UI thread
            // typically (FileTile binding) but the worker drains on a
            // background thread, so we hop explicitly.
            var bmp = new BitmapImage();
            var dispatcher = Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread()
                ?? FileID.App.HostWindow?.DispatcherQueue;
            if (dispatcher != null)
            {
                var tcs = new TaskCompletionSource<BitmapImage?>();
                dispatcher.TryEnqueue(async () =>
                {
                    try
                    {
                        await bmp.SetSourceAsync(thumb).AsTask(ct);
                        tcs.TrySetResult(bmp);
                    }
                    catch
                    {
                        tcs.TrySetResult(null);
                    }
                });
                return await tcs.Task.ConfigureAwait(false);
            }
            await bmp.SetSourceAsync(thumb).AsTask(ct);
            return bmp;
        }
        catch
        {
            return null;
        }
    }

    private static string CacheKey(string path, double? modifiedAt)
        => modifiedAt.HasValue
            ? $"{path}|{modifiedAt.Value:R}"
            : path;

    public void Dispose()
    {
        _cts.Cancel();
        _queue.Writer.TryComplete();
        try { _worker.Wait(TimeSpan.FromSeconds(1)); } catch { /* swallow */ }
        _cache.Dispose();
        _cts.Dispose();
    }

    private sealed record ThumbnailRequest(
        string Path,
        double? ModifiedAt,
        TaskCompletionSource<BitmapImage?> Completion,
        CancellationToken Cancellation);
}
