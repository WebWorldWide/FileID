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
    long FallbackUsed,
    long DiskHits,
    long DiskWrites,
    long DiskSweeps,
    long DiskBytes);

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
        Interlocked.Read(ref _fallbackUsed),
        ThumbnailDiskCache.DiskHits,
        ThumbnailDiskCache.DiskWrites,
        ThumbnailDiskCache.DiskSweeps,
        ThumbnailDiskCache.CachedBytes);
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
        DebugLog.Debug($"[THUMB] REQUEST file={path}");
        var key = CacheKey(path, modifiedAt);
        if (_cache.TryGetValue(key, out BitmapImage? cached) && cached != null)
        {
            DebugLog.Debug($"[THUMB] L1_HIT file={path}");
            return Task.FromResult<BitmapImage?>(cached);
        }
        DebugLog.Debug($"[THUMB] L1_MISS file={path}");
        var tcs = new TaskCompletionSource<BitmapImage?>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        var ok = _queue.Writer.TryWrite(new ThumbnailRequest(path, modifiedAt, tcs, ct));
        if (!ok)
        {
            DebugLog.Debug($"[THUMB] QUEUE_FULL_DROP file={path}");
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
                var bmp = await RenderAsync(req.Path, req.ModifiedAt, _uiDispatcher, ct);
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
        double? modifiedAt,
        Microsoft.UI.Dispatching.DispatcherQueue? uiDispatcher,
        CancellationToken ct)
    {
        if (!File.Exists(path))
        {
            Interlocked.Increment(ref _renderedFailed);
            return null;
        }

        // BitmapImage is a WinUI DispatcherObject — must be constructed on
        // a UI thread. Caller captures the dispatcher at ctor time; the
        // window-hosted fallback is a last resort during startup/shutdown.
        var dispatcher = uiDispatcher ?? FileID.App.HostWindow?.DispatcherQueue;
        if (dispatcher is null)
        {
            Interlocked.Increment(ref _droppedDispatcher);
            DebugLog.Warn($"ThumbnailService.RenderAsync: no UI dispatcher available; skipping ({path}).");
            return null;
        }

        // 0) Disk cache — survives app restart.
        var diskHit = await ThumbnailDiskCache.TryReadAsync(path, modifiedAt, dispatcher, ct)
            .ConfigureAwait(false);
        if (diskHit != null)
        {
            DebugLog.Debug($"[THUMB] L2_HIT file={path}");
            Interlocked.Increment(ref _renderedOk);
            return diskHit;
        }
        DebugLog.Debug($"[THUMB] L2_MISS file={path}");

        // 1) Shell IThumbnailProvider chain — same one Explorer / Photos
        //    use. Office / RAW / HEIC / etc. all work through this path.
        var ext = Path.GetExtension(path);
        var isKnownImage = ImageExtensions.Contains(ext);
        try
        {
            var file = await Windows.Storage.StorageFile.GetFileFromPathAsync(path)
                .AsTask(ct).ConfigureAwait(false);
            using var thumb = await file
                .GetThumbnailAsync(
                    Windows.Storage.FileProperties.ThumbnailMode.SingleItem,
                    ThumbnailRequestPx,
                    Windows.Storage.FileProperties.ThumbnailOptions.UseCurrentScale)
                .AsTask(ct)
                .ConfigureAwait(false);
            if (thumb != null && thumb.Size > 0)
            {
                var bytes = await ReadAllBytesAsync(thumb, ct).ConfigureAwait(false);
                DebugLog.Debug($"[THUMB] SHELL_OK file={path} bytes={bytes.Length}");
                var bmp = await RenderFromBytesOnDispatcherAsync(bytes, dispatcher, ct).ConfigureAwait(false);
                if (bmp != null)
                {
                    DebugLog.Debug($"[THUMB] BITMAP_SET file={path} src=shell");
                    _ = ThumbnailDiskCache.TryWriteAsync(path, modifiedAt, bytes);
                    Interlocked.Increment(ref _renderedOk);
                    return bmp;
                }
            }
            else
            {
                DebugLog.Debug($"[THUMB] SHELL_NULL file={path}");
            }
        }
        catch (Exception ex)
        {
            // V15.6 fix: previously this `catch` returned null directly,
            // bypassing the image-extension fallback below. Now we log
            // the exception TYPE (was just .Message) and fall through to
            // the fallback path so a JPEG with a broken shell provider
            // still renders. Don't bump _renderedFailed here — fallback
            // gets a turn and bumps the right counter on the way out.
            DebugLog.Debug($"[THUMB] SHELL_EX file={path} ex={ex.GetType().Name}");
            DebugLog.Warn(
                $"ThumbnailService shell-path ({path}): {ex.GetType().Name}: {ex.Message}");
        }

        // 2) Image-extension fallback — eager-decode from the original
        //    file bytes. Reached when (a) shell returned null/empty, OR
        //    (b) shell threw. The V15.6 bug was that case (b) skipped
        //    this branch.
        if (isKnownImage)
        {
            try
            {
                var fileBytes = await File.ReadAllBytesAsync(path, ct).ConfigureAwait(false);
                var bmp = await RenderFromBytesOnDispatcherAsync(fileBytes, dispatcher, ct).ConfigureAwait(false);
                if (bmp != null)
                {
                    DebugLog.Debug($"[THUMB] IMG_FB_OK file={path}");
                    DebugLog.Debug($"[THUMB] BITMAP_SET file={path} src=image-fallback");
                    _ = ThumbnailDiskCache.TryWriteAsync(path, modifiedAt, fileBytes);
                    Interlocked.Increment(ref _fallbackUsed);
                    Interlocked.Increment(ref _renderedOk);
                    return bmp;
                }
                DebugLog.Debug($"[THUMB] IMG_FB_NULL file={path}");
            }
            catch (Exception ex)
            {
                DebugLog.Debug($"[THUMB] IMG_FB_EX file={path} ex={ex.GetType().Name}");
                DebugLog.Warn(
                    $"ThumbnailService image-fallback ({path}): {ex.GetType().Name}: {ex.Message}");
            }
        }

        DebugLog.Debug($"[THUMB] RENDER_FAILED file={path}");
        Interlocked.Increment(ref _renderedFailed);
        return null;
    }

    /// <summary>Drain a StorageItemThumbnail's IRandomAccessStream into a
    /// byte[] so we can both decode it and persist it to the disk cache.
    /// Shell thumbnails are typically 5–50 KB; allocating ~64 KB worth
    /// of intermediate buffer is well within budget.</summary>
    private static async Task<byte[]> ReadAllBytesAsync(
        Windows.Storage.FileProperties.StorageItemThumbnail thumb,
        CancellationToken ct)
    {
        var size = (uint)thumb.Size;
        var buffer = new Windows.Storage.Streams.Buffer(size);
        await thumb.ReadAsync(buffer, size, Windows.Storage.Streams.InputStreamOptions.None)
            .AsTask(ct).ConfigureAwait(false);
        using var reader = Windows.Storage.Streams.DataReader.FromBuffer(buffer);
        var bytes = new byte[buffer.Length];
        reader.ReadBytes(bytes);
        return bytes;
    }

    /// <summary>Decode bytes into a BitmapImage on the UI dispatcher.
    /// Eager decode (SetSourceAsync from an InMemoryRandomAccessStream)
    /// avoids the V15.5 lazy-UriSource bug where ImageOpened never fired
    /// for mid-scan files.</summary>
    private static async Task<BitmapImage?> RenderFromBytesOnDispatcherAsync(
        byte[] bytes,
        Microsoft.UI.Dispatching.DispatcherQueue dispatcher,
        CancellationToken ct)
    {
        var tcs = new TaskCompletionSource<BitmapImage?>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        var ok = TryEnqueueWithRetry(dispatcher, () => RunBytesSetSource(bytes, tcs, ct));
        if (!ok)
        {
            Interlocked.Increment(ref _droppedDispatcher);
            return null;
        }
        return await tcs.Task.ConfigureAwait(false);
    }

    private static async void RunBytesSetSource(
        byte[] bytes,
        TaskCompletionSource<BitmapImage?> tcs,
        CancellationToken ct)
    {
        try
        {
            using var stream = new Windows.Storage.Streams.InMemoryRandomAccessStream();
            using (var writer = new Windows.Storage.Streams.DataWriter(stream.GetOutputStreamAt(0)))
            {
                writer.WriteBytes(bytes);
                await writer.StoreAsync().AsTask(ct);
                await writer.FlushAsync().AsTask(ct);
                writer.DetachStream();
            }
            stream.Seek(0);
            var bmp = new BitmapImage { DecodePixelWidth = (int)ThumbnailRequestPx };
            await bmp.SetSourceAsync(stream).AsTask(ct);
            tcs.TrySetResult(bmp);
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"ThumbnailService bytes-decode: {ex.GetType().Name}: {ex.Message}");
            tcs.TrySetResult(null);
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
