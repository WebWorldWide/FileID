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
// Implements the cache + render orchestration. The actual interop call
// either routes through the engine's shell::thumbnail helper or uses a
// direct CsWinRT IShellItemImageFactory binding.

using System;
using System.Collections.Generic;
using System.IO;
using System.Threading;
using System.Threading.Tasks;
using System.Threading.Channels;
using Microsoft.Extensions.Caching.Memory;
using Microsoft.UI.Xaml.Media.Imaging;

namespace FileID.Services;

/// <summary>counters exposed via <see cref="ThumbnailService.Stats"/>
/// so the Settings diagnostics block can surface silent failure modes
/// (null dispatcher, dropped enqueues) the user previously couldn't see.</summary>
public readonly record struct ThumbnailDiagnostics(
    long RenderedOk,
    long RenderedFailed,
    long DroppedDispatcher,
    long FallbackUsed);

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
    private static long _renderedOk;
    private static long _renderedFailed;
    private static long _droppedDispatcher;
    private static long _fallbackUsed;

    /// <summary>snapshot of the silent-failure counters. Wire into
    /// Settings diagnostics so the user can see when thumbnails are
    /// invisibly failing.</summary>
    public static ThumbnailDiagnostics Stats => new(
        Interlocked.Read(ref _renderedOk),
        Interlocked.Read(ref _renderedFailed),
        Interlocked.Read(ref _droppedDispatcher),
        Interlocked.Read(ref _fallbackUsed));
    /// <summary>captured at construction time on the UI thread
    /// so the worker (running on a thread-pool thread) always has a
    /// reliable dispatcher to marshal BitmapImage.SetSourceAsync back to.
    /// Late-binding to <c>GetForCurrentThread()</c> in <c>RenderAsync</c>
    /// could return null on the worker thread, and the fallback
    /// (<c>HostWindow?.DispatcherQueue</c>) might also be null during
    /// startup/shutdown — causing a silent thread-affinity violation.</summary>
    private readonly Microsoft.UI.Dispatching.DispatcherQueue? _uiDispatcher;

    public ThumbnailService()
    {
        // capture the UI dispatcher at ctor time. Service is
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
        // attach a fault sink so a DrainAsync exception leaves a
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
                // drop ConfigureAwait(false). The bitmap returned
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
    /// raw, .heic, .pages all work). 192-px request, scaled by the system.
    /// dropped from 256 → 192 px to match macOS
    /// `ThumbnailService.swift:27` (size: 192). Same display target, ~44%
    /// less memory per cached tile.
    /// </summary>
    private const uint ThumbnailRequestPx = 192;

    /// <summary>extensions where we have a WIC-backed fallback if
    /// the shell provider chain returns nothing. Matches what Explorer's
    /// Photos app would render. Other extensions get null + a failure
    /// counter bump.</summary>
    private static readonly HashSet<string> ImageExtensions = new(StringComparer.OrdinalIgnoreCase)
    {
        ".jpg", ".jpeg", ".png", ".gif", ".bmp", ".webp",
    };

    private static async Task<BitmapImage?> RenderAsync(
        string path,
        Microsoft.UI.Dispatching.DispatcherQueue? uiDispatcher,
        CancellationToken ct)
    {
        if (!File.Exists(path))
        {
            Interlocked.Increment(ref _renderedFailed);
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
            // CRASH FIX: BitmapImage is a WinUI DispatcherObject.
            // Constructing it on the worker thread that resumed our await
            // is what RaiseFailFastException's the process from
            // Composition's next-frame check — bypassing every managed
            // exception handler. Construct + SetSourceAsync + Dispose all
            // happen inside the dispatcher lambda below.
            var dispatcher = uiDispatcher ?? FileID.App.HostWindow?.DispatcherQueue;
            if (dispatcher is null)
            {
                Interlocked.Increment(ref _droppedDispatcher);
                DebugLog.Warn("ThumbnailService.RenderAsync: no UI dispatcher available; skipping.");
                thumb?.Dispose();
                return null;
            }
            if (thumb == null || thumb.Size == 0)
            {
                thumb?.Dispose();
                thumb = null;
                // shell provider returned nothing. For known image
                // formats this is the most common reason the user sees
                // "blank tiles" — fall back to BitmapImage(Uri), the same
                // path Explorer's Photos uses. DecodePixelWidth caps memory.
                var ext = Path.GetExtension(path);
                if (ImageExtensions.Contains(ext))
                {
                    var fallback = await RenderImageFallbackOnDispatcherAsync(path, dispatcher, ct).ConfigureAwait(false);
                    if (fallback != null)
                    {
                        Interlocked.Increment(ref _fallbackUsed);
                        Interlocked.Increment(ref _renderedOk);
                        return fallback;
                    }
                }
                Interlocked.Increment(ref _renderedFailed);
                return null;
            }
            var capturedThumb = thumb;
            thumb = null;
            var bmp = await RenderShellThumbOnDispatcherAsync(capturedThumb, dispatcher, ct).ConfigureAwait(false);
            if (bmp != null) { Interlocked.Increment(ref _renderedOk); }
            else { Interlocked.Increment(ref _renderedFailed); }
            return bmp;
        }
        catch (Exception ex)
        {
            Interlocked.Increment(ref _renderedFailed);
            DebugLog.Warn("ThumbnailService.RenderAsync: " + ex.Message);
            try { thumb?.Dispose(); } catch { /* swallow */ }
            return null;
        }
    }

    private static async Task<BitmapImage?> RenderShellThumbOnDispatcherAsync(
        Windows.Storage.FileProperties.StorageItemThumbnail capturedThumb,
        Microsoft.UI.Dispatching.DispatcherQueue dispatcher,
        CancellationToken ct)
    {
        var tcs = new TaskCompletionSource<BitmapImage?>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        if (!TryEnqueueWithRetry(dispatcher, () => RunSetSource(capturedThumb, tcs, ct)))
        {
            Interlocked.Increment(ref _droppedDispatcher);
            DebugLog.Warn("ThumbnailService: dispatcher.TryEnqueue returned false after retry (shutdown?).");
            try { capturedThumb.Dispose(); } catch { /* swallow */ }
            return null;
        }
        return await tcs.Task.ConfigureAwait(false);
    }

    private static async void RunSetSource(
        Windows.Storage.FileProperties.StorageItemThumbnail capturedThumb,
        TaskCompletionSource<BitmapImage?> tcs,
        CancellationToken ct)
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
    }

    private static async Task<BitmapImage?> RenderImageFallbackOnDispatcherAsync(
        string path,
        Microsoft.UI.Dispatching.DispatcherQueue dispatcher,
        CancellationToken ct)
    {
        // open the file as a random-access stream on this worker
        // thread, then SetSourceAsync on the UI dispatcher. Eager-decode
        // pattern mirrors the shell path's RunSetSource (known-good).
        // The UriSource-lazy alternative leaves BitmapImages stuck
        // un-decoded for mid-scan files; this guarantees the bitmap
        // either populates or we log + return null + bump _renderedFailed.
        Windows.Storage.StorageFile file;
        Windows.Storage.Streams.IRandomAccessStream stream;
        try
        {
            file = await Windows.Storage.StorageFile.GetFileFromPathAsync(path)
                .AsTask(ct).ConfigureAwait(false);
            stream = await file.OpenAsync(Windows.Storage.FileAccessMode.Read)
                .AsTask(ct).ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"ThumbnailService image-fallback open ({path}): {ex.Message}");
            return null;
        }

        var tcs = new TaskCompletionSource<BitmapImage?>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        var enqueued = TryEnqueueWithRetry(dispatcher, () => RunFallbackSetSource(stream, path, tcs, ct));
        if (!enqueued)
        {
            Interlocked.Increment(ref _droppedDispatcher);
            try { stream.Dispose(); } catch { /* swallow */ }
            return null;
        }
        return await tcs.Task.ConfigureAwait(false);
    }

    private static async void RunFallbackSetSource(
        Windows.Storage.Streams.IRandomAccessStream stream,
        string path,
        TaskCompletionSource<BitmapImage?> tcs,
        CancellationToken ct)
    {
        try
        {
            var bmp = new BitmapImage { DecodePixelWidth = (int)ThumbnailRequestPx };
            await bmp.SetSourceAsync(stream).AsTask(ct);
            tcs.TrySetResult(bmp);
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"ThumbnailService image-fallback decode ({path}): {ex.Message}");
            tcs.TrySetResult(null);
        }
        finally
        {
            try { stream.Dispose(); } catch { /* swallow */ }
        }
    }

    /// <summary>try once, then sleep 50 ms and retry once. TryEnqueue
    /// can return false during compositor shutdown races. A single retry
    /// covers the transient case without spinning.</summary>
    private static bool TryEnqueueWithRetry(
        Microsoft.UI.Dispatching.DispatcherQueue dispatcher,
        Microsoft.UI.Dispatching.DispatcherQueueHandler action)
    {
        if (dispatcher.TryEnqueue(action)) return true;
        Thread.Sleep(50);
        return dispatcher.TryEnqueue(action);
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
