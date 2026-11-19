// ThumbnailService — IShellItemImageFactory-driven shell thumbnails with an in-process LRU cache and a disk-backed cache under thumbs.cache/. Cache keys hash path+modified-time so a file edit invalidates its thumb; a background queue drains requests so the UI thread never pays the shell-thumbnail cost.

using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.IO;
using System.Threading;
using System.Threading.Tasks;
using System.Threading.Channels;
using FileID.IpcSchema;
using FileID.ViewModels;
using Microsoft.Extensions.Caching.Memory;
using Microsoft.UI.Xaml.Media.Imaging;

namespace FileID.Services;

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
    internal const int QueueCapacity = 256;
    internal const long MaxFallbackEncodedBytes = 32L * 1024 * 1024;

    internal static Channel<ThumbnailRequest> CreateRequestChannel() =>
        Channel.CreateBounded<ThumbnailRequest>(new BoundedChannelOptions(QueueCapacity)
        {
            SingleReader = false,
            SingleWriter = false,
            FullMode = BoundedChannelFullMode.DropOldest,
        }, dropped => dropped.Completion.TrySetResult(null));

    internal static async Task<byte[]> ReadFallbackFileBytesAsync(string path, CancellationToken ct)
    {
        await using var stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read,
            bufferSize: 64 * 1024, options: FileOptions.Asynchronous | FileOptions.SequentialScan);
        if (stream.Length > MaxFallbackEncodedBytes)
        {
            throw new InvalidDataException("Image is too large for thumbnail fallback decoding.");
        }

        using var output = new MemoryStream((int)stream.Length);
        var buffer = new byte[64 * 1024];
        int read;
        while ((read = await stream.ReadAsync(buffer, ct).ConfigureAwait(false)) != 0)
        {
            if (output.Length + read > MaxFallbackEncodedBytes)
            {
                throw new InvalidDataException("Image grew beyond the thumbnail fallback limit.");
            }
            output.Write(buffer, 0, read);
        }
        return output.ToArray();
    }

    // Decoded-BitmapImage L1 cache, bounded by a real BYTE budget (not an entry count): each cached BitmapImage holds a DECODED bitmap (~ThumbnailRequestPx² × 4 bytes), so an entry-count cap would really be hundreds of MB of live bitmaps on a large library. Sizing each entry by its decoded cost and capping the total holds the working set bounded across a 50K-file scroll; LRU evicts the coldest thumbnails and a miss just re-decodes (no correctness hit).
    private const long L1CacheByteBudget = 128L * 1024 * 1024;
    private const long L1CacheByteBudgetHighDpi = 256L * 1024 * 1024;
    /// <summary>Shell and disk work runs on this many drains at once; every BitmapImage is still built on the UI dispatcher.</summary>
    internal const int WorkerCount = 3;
    private readonly long _decodedBytesPerEntry;
    private readonly int _requestPx;
    private readonly MemoryCache _cache;
    private readonly Channel<ThumbnailRequest> _queue;
    private readonly CancellationTokenSource _cts = new();
    private static ThumbnailService? s_shared;

    /// <summary>The one app-lifetime service every page uses, so the decoded cache survives tab switches.</summary>
    internal static ThumbnailService Shared => s_shared ??= new ThumbnailService();
    private static long _renderedOk;
    private static long _renderedFailed;
    private static long _droppedDispatcher;
    private static long _fallbackUsed;

    /// <summary>Wire into Settings diagnostics so the user can see when thumbnails are invisibly failing.</summary>
    public static ThumbnailDiagnostics Stats => new(
        Interlocked.Read(ref _renderedOk),
        Interlocked.Read(ref _renderedFailed),
        Interlocked.Read(ref _droppedDispatcher),
        Interlocked.Read(ref _fallbackUsed),
        ThumbnailDiskCache.DiskHits,
        ThumbnailDiskCache.DiskWrites,
        ThumbnailDiskCache.DiskSweeps,
        ThumbnailDiskCache.CachedBytes);
    /// <summary>captured at construction time on the UI thread so the worker (running on a thread-pool thread) always has a reliable dispatcher to marshal BitmapImage.SetSourceAsync back to.</summary>
    private readonly Microsoft.UI.Dispatching.DispatcherQueue? _uiDispatcher;

    /// <summary>VIDEO thumbnails are NOT produced in-process (the shell / Media Foundation chain native-fast-fails the whole app — see <see cref="VideoExtensions"/>). On an L1+L2 miss for a video we ask the engine to extract a keyframe out-of-process and complete the tile when the <c>thumbnailGenerated</c> event arrives. <see cref="_pendingVideo"/> maps CacheKey to the request's TCS, completed by the engine event (or the timeout) instead of null in <see cref="DrainAsync"/>; <see cref="_requestedVideo"/> holds CacheKeys already asked for this session so a re-scroll never re-sends the (expensive) command, cleared per key once the event lands so a later file edit (new modifiedAt → new key) can re-request. Both are guarded by <see cref="_videoLock"/> (the worker thread enqueues, the engine-event handler on the UI thread completes — two threads).</summary>
    private readonly Dictionary<string, TaskCompletionSource<BitmapImage?>> _pendingVideo = new();
    private readonly HashSet<string> _requestedVideo = new();
    private readonly object _videoLock = new();
    private const int VideoThumbTimeoutMs = 20_000;
    private readonly int _videoPixelSize;
    private volatile bool _disposed;

    public ThumbnailService()
    {
        // capture the UI dispatcher at ctor time.
        _uiDispatcher = Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread();
        var scale = _uiDispatcher?.HasThreadAccess == true
            ? FileID.App.HostWindow?.Content?.XamlRoot?.RasterizationScale ?? 1
            : 1;
        _videoPixelSize = VideoPixelSize(scale);
        _requestPx = ImagePixelSize(scale);
        _decodedBytesPerEntry = (long)_requestPx * _requestPx * 4;
        _cache = new MemoryCache(new MemoryCacheOptions
        {
            SizeLimit = _requestPx > TilePx ? L1CacheByteBudgetHighDpi : L1CacheByteBudget,
        });
        _queue = CreateRequestChannel();
        // attach a fault sink so a DrainAsync exception leaves a forensic trail instead of becoming an UnobservedTaskException at GC time.
        for (var i = 0; i < WorkerCount; i++)
        {
            var worker = Task.Run(() => DrainAsync(_cts.Token));
            _ = worker.ContinueWith(
                t => DebugLog.Error("ThumbnailService worker faulted: " + t.Exception),
                TaskContinuationOptions.OnlyOnFaulted);
        }
        // We L2-write it, decode it on the UI thread, and complete the tile's pending TCS.
        EngineClient.Instance.PropertyChanged += OnEngineClientChanged;
    }

    public Task<BitmapImage?> RequestAsync(string path, double? modifiedAt, CancellationToken ct)
    {
        if (DebugLog.TraceEnabled) DebugLog.Trace($"[THUMB] REQUEST file={PathRedactor.Redact(path)}");
        var key = CacheKey(path, modifiedAt);
        if (_cache.TryGetValue(key, out BitmapImage? cached) && cached != null)
        {
            if (DebugLog.TraceEnabled) DebugLog.Trace($"[THUMB] L1_HIT file={PathRedactor.Redact(path)}");
            return Task.FromResult<BitmapImage?>(cached);
        }
        if (DebugLog.TraceEnabled) DebugLog.Trace($"[THUMB] L1_MISS file={PathRedactor.Redact(path)}");
        var tcs = new TaskCompletionSource<BitmapImage?>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        var ok = _queue.Writer.TryWrite(new ThumbnailRequest(path, modifiedAt, tcs, ct));
        if (!ok)
        {
            if (DebugLog.TraceEnabled) DebugLog.Trace($"[THUMB] QUEUE_FULL_DROP file={PathRedactor.Redact(path)}");
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
                // Honor the PER-REQUEST token (a scrolled-away tile cancels its own request), linked with the service token, so the full shell+disk+ decode doesn't run for a tile the user already scrolled past.
                using var linked = CancellationTokenSource.CreateLinkedTokenSource(ct, req.Cancellation);
                // No ConfigureAwait(false): the returned BitmapImage is a UI-thread DispatcherObject, so keep the continuation (and the caller's, which sets tile.Thumbnail) on the UI thread.
                var bmp = await RenderAsync(req.Path, req.ModifiedAt, _videoPixelSize, _requestPx, _uiDispatcher, linked.Token);
                if (bmp != null)
                {
                    var key = CacheKey(req.Path, req.ModifiedAt);
                    _cache.Set(key, bmp, new MemoryCacheEntryOptions
                    {
                        Size = VideoExtensions.Contains(Path.GetExtension(req.Path))
                            ? (long)_videoPixelSize * _videoPixelSize * 4
                            : _decodedBytesPerEntry,
                        SlidingExpiration = TimeSpan.FromMinutes(15),
                    });
                    req.Completion.TrySetResult(bmp);
                }
                else if (TryBeginVideoThumbnailRequest(req))
                {
                    // Video L1+L2 miss: RenderAsync returned null (we never invoke the in-process shell chain for video — native fast-fail hazard). The request is now PENDING on the engine's out-of-process keyframe; do NOT complete it null here. OnEngineClientChanged completes req.Completion when the `thumbnailGenerated` event arrives, or the armed timeout completes it null (placeholder) and unregisters it.
                }
                else
                {
                    req.Completion.TrySetResult(bmp);
                }
            }
            catch (OperationCanceledException)
            {
                // Per-request cancel (tile scrolled away): complete null and keep draining.
                req.Completion.TrySetResult(null);
                if (ct.IsCancellationRequested) throw;
            }
            catch (Exception ex) when (ex is not OperationCanceledException)
            {
                DebugLog.Warn("ThumbnailService.DrainAsync: " + ex.Message);
                req.Completion.TrySetResult(null);
            }
        }
    }

    /// <summary>For a VIDEO file that missed L1+L2: registers the request's TCS as PENDING (unresolved until the engine event or the timeout), dedupes per (path,modifiedAt) so the expensive out-of-process extract is requested at most once per session, fires the <c>generateVideoThumbnail</c> command fire-and-forget, and arms a timeout that completes the tile null (placeholder) if no keyframe arrives. Returns true when the request was taken over (caller must NOT complete it); false for non-video, or when the service is disposed.</summary>
    private bool TryBeginVideoThumbnailRequest(ThumbnailRequest req)
    {
        if (_disposed) return false;
        if (!VideoExtensions.Contains(Path.GetExtension(req.Path))) return false;

        var key = CacheKey(req.Path, req.ModifiedAt);
        lock (_videoLock)
        {
            if (_disposed) return false;
            // The event handler completes whichever TCS is registered; if one is already parked, this later request just gets null after its own timeout (the first tile's keyframe filled the caches, so a future RequestAsync L1-hits).
            _pendingVideo[key] = req.Completion;
            bool firstRequest = _requestedVideo.Add(key);
            if (firstRequest)
            {
                if (DebugLog.TraceEnabled) DebugLog.Trace($"[THUMB] VIDEO_ENGINE_REQUEST file={PathRedactor.Redact(req.Path)}");
                _ = EngineClient.Instance.SendCommandAsync(
                    new GenerateVideoThumbnailCommand(req.Path, req.ModifiedAt, _videoPixelSize));
            }
            else
            {
                if (DebugLog.TraceEnabled) DebugLog.Trace($"[THUMB] VIDEO_ENGINE_DEDUP file={PathRedactor.Redact(req.Path)}");
            }
        }

        // Timeout: if no keyframe lands in VideoThumbTimeoutMs, complete the tile null (placeholder) and unregister so the dictionary doesn't grow unbounded. The dedupe set intentionally KEEPS the key so we don't re-hammer the engine for a video it couldn't extract this session.
        _ = ArmVideoTimeoutAsync(key, req.Completion);
        return true;
    }

    private async Task ArmVideoTimeoutAsync(string key, TaskCompletionSource<BitmapImage?> tcs)
    {
        try
        {
            await Task.Delay(VideoThumbTimeoutMs, _cts.Token).ConfigureAwait(false);
        }
        catch (OperationCanceledException)
        {
            // Service disposed mid-wait — release the pending tile so callers (and the awaiting RequestAsync) don't hang.
            tcs.TrySetResult(null);
            return;
        }
        lock (_videoLock)
        {
            // Only clear the dictionary entry if it's still THIS request's TCS; a newer request for the same key may have replaced it.
            if (_pendingVideo.TryGetValue(key, out var current) && ReferenceEquals(current, tcs))
            {
                _pendingVideo.Remove(key);
            }
        }
        if (tcs.TrySetResult(null))
        {
            if (DebugLog.TraceEnabled) DebugLog.Trace($"[THUMB] VIDEO_ENGINE_TIMEOUT key={PathRedactor.Redact(key)}");
        }
    }

    private void OnEngineClientChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("ThumbnailService.OnEngineClientChanged", () =>
        {
            if (_disposed) return;
            if (e.PropertyName != nameof(EngineClient.LastThumbnailGenerated)) return;
            DebugLog.Info($"[ENGINE-SUB:ThumbnailService] {e.PropertyName}");
            var evt = EngineClient.Instance.LastThumbnailGenerated;
            if (evt is null) return;

            // Decode + cache + complete the tile, all on the UI thread: BitmapImage is a DispatcherObject (must be built on a thread we captured), and the byte-budgeted L1 _cache.Set lives next to it.
            var dispatcher = _uiDispatcher
                ?? FileID.App.HostWindow?.DispatcherQueue;
            if (dispatcher is null)
            {
                DebugLog.Warn("ThumbnailService.OnEngineClientChanged: no UI dispatcher; dropping video keyframe.");
                CompletePendingVideoNull(CacheKey(evt.Path, evt.ModifiedAt));
                return;
            }
            if (!dispatcher.TryEnqueue(() => _ = ApplyVideoKeyframeAsync(evt)))
            {
                CompletePendingVideoNull(CacheKey(evt.Path, evt.ModifiedAt));
            }
        });

    private async Task ApplyVideoKeyframeAsync(ThumbnailGenerated evt)
    {
        var key = CacheKey(evt.Path, evt.ModifiedAt);
        await DebugLog.SafeRunAsync("ThumbnailService.ApplyVideoKeyframe", async () =>
        {
            if (_disposed)
            {
                CompletePendingVideoNull(key);
                return;
            }

            byte[] bytes;
            try
            {
                bytes = Convert.FromBase64String(evt.Bytes);
            }
            catch (FormatException ex)
            {
                DebugLog.Warn($"ThumbnailService video keyframe base64 decode ({PathRedactor.Redact(evt.Path)}): {ex.Message}");
                CompletePendingVideoNull(key);
                return;
            }
            if (bytes.Length == 0)
            {
                CompletePendingVideoNull(key);
                return;
            }

            _ = ThumbnailDiskCache.TryWriteAsync(evt.Path, evt.ModifiedAt, bytes, _videoPixelSize);

            // We're already on the UI dispatcher (TryEnqueue above), so the BitmapImage decode + SetSourceAsync is thread-affinity-safe.
            var bmp = await DecodeOnThisThreadAsync(bytes, _cts.Token).ConfigureAwait(true);
            if (bmp is null)
            {
                CompletePendingVideoNull(key);
                return;
            }

            _cache.Set(key, bmp, new MemoryCacheEntryOptions
            {
                Size = (long)_videoPixelSize * _videoPixelSize * 4,
                SlidingExpiration = TimeSpan.FromMinutes(15),
            });
            if (DebugLog.TraceEnabled) DebugLog.Trace($"[THUMB] BITMAP_SET file={PathRedactor.Redact(evt.Path)} src=engine-video");

            TaskCompletionSource<BitmapImage?>? pending;
            lock (_videoLock)
            {
                if (_pendingVideo.TryGetValue(key, out pending))
                {
                    _pendingVideo.Remove(key);
                }
            }
            pending?.TrySetResult(bmp);
        }).ConfigureAwait(true);
    }

    private void CompletePendingVideoNull(string key)
    {
        TaskCompletionSource<BitmapImage?>? pending;
        lock (_videoLock)
        {
            if (!_pendingVideo.TryGetValue(key, out pending)) return;
            _pendingVideo.Remove(key);
        }
        pending?.TrySetResult(null);
    }

    /// <summary>Decode bytes into a BitmapImage on the CURRENT (UI) thread; the caller MUST already be on the dispatcher (the engine-event handler enqueues this). Returns null on any decode failure.</summary>
    private static async Task<BitmapImage?> DecodeOnThisThreadAsync(byte[] bytes, CancellationToken ct)
    {
        Windows.Storage.Streams.InMemoryRandomAccessStream? stream = null;
        try
        {
            stream = new Windows.Storage.Streams.InMemoryRandomAccessStream();
            using (var writer = new Windows.Storage.Streams.DataWriter(stream.GetOutputStreamAt(0)))
            {
                writer.WriteBytes(bytes);
                await writer.StoreAsync().AsTask(ct);
                await writer.FlushAsync().AsTask(ct);
                writer.DetachStream();
            }
            stream.Seek(0);
            var bmp = new BitmapImage();
            await bmp.SetSourceAsync(stream).AsTask(ct);
            if (DebugLog.TraceEnabled) DebugLog.Trace($"[THUMB] DECODE_OK bytes={bytes.Length} src=engine-video");
            return bmp;
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"[THUMB] DECODE_FAIL ex={ex.GetType().Name} msg={ex.Message} bytes={bytes.Length} src=engine-video");
            return null;
        }
        finally
        {
            try { stream?.Dispose(); } catch { /* swallow */ }
        }
    }

    private const uint ThumbnailRequestPx = 192;

    internal const int TilePx = 256;
    private static readonly int[] s_imagePixelBuckets = { 256, 384, 512, 768 };

    /// <summary>Physical pixels asked of the shell and decoded for an image tile: the tile size times the display scale, rounded up to a bucket so the disk cache holds a handful of sizes, not one per scale. The scale is read once on the UI thread at construction: reading it from a worker drain (the WinRT window interop is UI-thread-affined) poisoned later `SetSourceAsync` calls.</summary>
    internal static int ImagePixelSize(double scale)
    {
        var factor = double.IsFinite(scale) && scale > 0 ? scale : 1;
        var wanted = TilePx * factor;
        foreach (var bucket in s_imagePixelBuckets)
        {
            if (wanted <= bucket) return bucket;
        }
        return s_imagePixelBuckets[^1];
    }

    /// <summary>The scale is read once on the UI thread at construction, never from the worker drain.</summary>
    internal static int VideoPixelSize(double scale)
    {
        var factor = double.IsFinite(scale) && scale > 0 ? scale : 1;
        return (int)Math.Clamp(Math.Ceiling(ThumbnailRequestPx * factor), 64, 2048);
    }

    private static readonly HashSet<string> ImageExtensions = new(StringComparer.OrdinalIgnoreCase)
    {
        ".jpg", ".jpeg", ".png", ".gif", ".bmp", ".webp",
        ".tif", ".tiff", ".heic", ".heif", ".avif", ".ico", ".jfif",
    };

    /// <summary>Audio extensions whose album art comes ONLY from the in-process shell IThumbnailProvider chain.</summary>
    private static readonly HashSet<string> AudioExtensions = new(StringComparer.OrdinalIgnoreCase)
    {
        ".mp3", ".m4a", ".aac", ".flac", ".wav", ".ogg", ".oga", ".opus",
        ".wma", ".aiff", ".aif", ".alac", ".ape", ".mka", ".m4b",
    };

    private static readonly HashSet<string> VideoExtensions = new(StringComparer.OrdinalIgnoreCase)
    {
        ".mov", ".mp4", ".m4v", ".avi", ".mkv", ".wmv", ".webm", ".mpg",
        ".mpeg", ".mpe", ".3gp", ".3g2", ".mts", ".m2ts", ".ts", ".flv",
        ".ogv", ".vob", ".qt",
    };

    /// <summary>EVERY direct GetThumbnailAsync callsite must consult this before invoking the shell chain (this service's RenderAsync, DeepAnalyzeView, DrillDownSheet).</summary>
    public static bool SkipShellThumbnailForExtension(string path)
    {
        var ext = Path.GetExtension(path);
        return AudioExtensions.Contains(ext) || VideoExtensions.Contains(ext);
    }

    private static async Task<BitmapImage?> RenderAsync(
        string path,
        double? modifiedAt,
        int videoPixelSize,
        int requestPx,
        Microsoft.UI.Dispatching.DispatcherQueue? uiDispatcher,
        CancellationToken ct)
    {
        if (!File.Exists(path))
        {
            Interlocked.Increment(ref _renderedFailed);
            return null;
        }

        // BitmapImage is a WinUI DispatcherObject — must be constructed on a UI thread.
        var dispatcher = uiDispatcher
            ?? FileID.App.HostWindow?.DispatcherQueue
            ?? Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread();
        if (dispatcher is null)
        {
            Interlocked.Increment(ref _droppedDispatcher);
            DebugLog.Warn($"ThumbnailService.RenderAsync: no UI dispatcher available; skipping ({PathRedactor.Redact(path)}).");
            return null;
        }

        var ext = Path.GetExtension(path);

        int? diskVideoSize = VideoExtensions.Contains(ext) ? videoPixelSize : null;
        var diskHit = await ThumbnailDiskCache.TryReadAsync(path, modifiedAt, dispatcher, ct, diskVideoSize, requestPx)
            .ConfigureAwait(false);
        if (diskHit != null)
        {
            if (DebugLog.TraceEnabled) DebugLog.Trace($"[THUMB] L2_HIT file={PathRedactor.Redact(path)}");
            Interlocked.Increment(ref _renderedOk);
            return diskHit;
        }
        if (DebugLog.TraceEnabled) DebugLog.Trace($"[THUMB] L2_MISS file={PathRedactor.Redact(path)}");

        if (AudioExtensions.Contains(ext))
        {
            if (DebugLog.TraceEnabled) DebugLog.Trace($"[THUMB] AUDIO_SHELL_SKIP file={PathRedactor.Redact(path)} ext={ext}");
            Interlocked.Increment(ref _renderedFailed);
            return null;
        }

        if (VideoExtensions.Contains(ext))
        {
            if (DebugLog.TraceEnabled) DebugLog.Trace($"[THUMB] VIDEO_SHELL_SKIP file={PathRedactor.Redact(path)} ext={ext}");
            Interlocked.Increment(ref _renderedFailed);
            return null;
        }

        var isKnownImage = ImageExtensions.Contains(ext);
        try
        {
            var file = await Windows.Storage.StorageFile.GetFileFromPathAsync(path)
                .AsTask(ct).ConfigureAwait(false);
            using var thumb = await file
                .GetThumbnailAsync(
                    Windows.Storage.FileProperties.ThumbnailMode.SingleItem,
                    (uint)requestPx,
                    Windows.Storage.FileProperties.ThumbnailOptions.ResizeThumbnail)
                .AsTask(ct)
                .ConfigureAwait(false);
            if (thumb != null && thumb.Size > 0)
            {
                var bytes = await ReadAllBytesAsync(thumb, ct).ConfigureAwait(false);
                if (DebugLog.TraceEnabled) DebugLog.Trace($"[THUMB] SHELL_OK file={PathRedactor.Redact(path)} bytes={bytes.Length}");
                var bmp = await RenderFromBytesOnDispatcherAsync(bytes, requestPx, dispatcher, ct).ConfigureAwait(false);
                if (bmp != null)
                {
                    if (DebugLog.TraceEnabled) DebugLog.Trace($"[THUMB] BITMAP_SET file={PathRedactor.Redact(path)} src=shell");
                    _ = ThumbnailDiskCache.TryWriteAsync(path, modifiedAt, bytes, null, requestPx);
                    Interlocked.Increment(ref _renderedOk);
                    return bmp;
                }
            }
            else
            {
                if (DebugLog.TraceEnabled) DebugLog.Trace($"[THUMB] SHELL_NULL file={PathRedactor.Redact(path)}");
            }
        }
        catch (Exception ex)
        {
            if (DebugLog.TraceEnabled) DebugLog.Trace($"[THUMB] SHELL_EX file={PathRedactor.Redact(path)} ex={ex.GetType().Name}");
            DebugLog.Warn(
                $"ThumbnailService shell-path ({PathRedactor.Redact(path)}): {ex.GetType().Name}: {ex.Message}");
        }

        if (isKnownImage)
        {
            try
            {
                var fileBytes = await ReadFallbackFileBytesAsync(path, ct).ConfigureAwait(false);
                var bmp = await RenderFromBytesOnDispatcherAsync(fileBytes, requestPx, dispatcher, ct).ConfigureAwait(false);
                if (bmp != null)
                {
                    if (DebugLog.TraceEnabled) DebugLog.Trace($"[THUMB] IMG_FB_OK file={PathRedactor.Redact(path)}");
                    if (DebugLog.TraceEnabled) DebugLog.Trace($"[THUMB] BITMAP_SET file={PathRedactor.Redact(path)} src=image-fallback");
                    _ = ThumbnailDiskCache.TryWriteAsync(path, modifiedAt, fileBytes, null, requestPx);
                    Interlocked.Increment(ref _fallbackUsed);
                    Interlocked.Increment(ref _renderedOk);
                    return bmp;
                }
                if (DebugLog.TraceEnabled) DebugLog.Trace($"[THUMB] IMG_FB_NULL file={PathRedactor.Redact(path)}");
            }
            catch (Exception ex)
            {
                if (DebugLog.TraceEnabled) DebugLog.Trace($"[THUMB] IMG_FB_EX file={PathRedactor.Redact(path)} ex={ex.GetType().Name}");
                DebugLog.Warn(
                    $"ThumbnailService image-fallback ({PathRedactor.Redact(path)}): {ex.GetType().Name}: {ex.Message}");
                Interlocked.Increment(ref _renderedFailed);
                return null;
            }
        }
        else
        {
            if (DebugLog.TraceEnabled) DebugLog.Trace($"[THUMB] NO_PROVIDER file={PathRedactor.Redact(path)} ext={ext}");
        }

        if (DebugLog.TraceEnabled) DebugLog.Trace($"[THUMB] RENDER_FAILED file={PathRedactor.Redact(path)}");
        Interlocked.Increment(ref _renderedFailed);
        return null;
    }

    /// <summary>Drain a StorageItemThumbnail's IRandomAccessStream into a byte[] so we can both decode it and persist it to the disk cache.</summary>
    private static async Task<byte[]> ReadAllBytesAsync(
        Windows.Storage.FileProperties.StorageItemThumbnail thumb,
        CancellationToken ct)
    {
        if (thumb.Size > MaxFallbackEncodedBytes)
        {
            throw new InvalidDataException("Shell thumbnail exceeds the encoded image limit.");
        }
        var size = (uint)thumb.Size;
        var buffer = new Windows.Storage.Streams.Buffer(size);
        await thumb.ReadAsync(buffer, size, Windows.Storage.Streams.InputStreamOptions.None)
            .AsTask(ct).ConfigureAwait(false);
        using var reader = Windows.Storage.Streams.DataReader.FromBuffer(buffer);
        var bytes = new byte[buffer.Length];
        reader.ReadBytes(bytes);
        return bytes;
    }

    /// <summary>Eager decode (SetSourceAsync from an InMemoryRandomAccessStream) avoids the lazy-UriSource bug where ImageOpened never fired for mid-scan files.</summary>
    private static async Task<BitmapImage?> RenderFromBytesOnDispatcherAsync(
        byte[] bytes,
        int decodePx,
        Microsoft.UI.Dispatching.DispatcherQueue dispatcher,
        CancellationToken ct)
    {
        var tcs = new TaskCompletionSource<BitmapImage?>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        var ok = TryEnqueueWithRetry(dispatcher, () => RunBytesSetSource(bytes, decodePx, tcs, ct));
        if (!ok)
        {
            Interlocked.Increment(ref _droppedDispatcher);
            return null;
        }
        return await tcs.Task.ConfigureAwait(false);
    }

    private static async void RunBytesSetSource(
        byte[] bytes,
        int decodePx,
        TaskCompletionSource<BitmapImage?> tcs,
        CancellationToken ct)
    {
        // Stream lifetime: must outlive SetSourceAsync.
        Windows.Storage.Streams.InMemoryRandomAccessStream? stream = null;
        try
        {
            stream = new Windows.Storage.Streams.InMemoryRandomAccessStream();
            using (var writer = new Windows.Storage.Streams.DataWriter(stream.GetOutputStreamAt(0)))
            {
                writer.WriteBytes(bytes);
                await writer.StoreAsync().AsTask(ct);
                await writer.FlushAsync().AsTask(ct);
                writer.DetachStream();
            }
            stream.Seek(0);
            var bmp = new BitmapImage { DecodePixelWidth = decodePx };
            await bmp.SetSourceAsync(stream).AsTask(ct);
            if (DebugLog.TraceEnabled) DebugLog.Trace($"[THUMB] DECODE_OK bytes={bytes.Length} px={decodePx}");
            tcs.TrySetResult(bmp);
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"[THUMB] DECODE_FAIL ex={ex.GetType().Name} msg={ex.Message} bytes={bytes.Length} px={decodePx}");
            tcs.TrySetResult(null);
        }
        finally
        {
            try { stream?.Dispose(); } catch { /* swallow */ }
        }
    }

    /// <summary>TryEnqueue can return false during compositor shutdown races.</summary>
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
        _disposed = true;
        if (ReferenceEquals(s_shared, this)) s_shared = null;
        try { EngineClient.Instance.PropertyChanged -= OnEngineClientChanged; } catch { /* swallow */ }
        // Release any tiles still waiting on an engine video keyframe so the awaiting RequestAsync callers don't hang on shutdown.
        List<TaskCompletionSource<BitmapImage?>> stranded;
        lock (_videoLock)
        {
            stranded = new List<TaskCompletionSource<BitmapImage?>>(_pendingVideo.Values);
            _pendingVideo.Clear();
            _requestedVideo.Clear();
        }
        foreach (var tcs in stranded) { tcs.TrySetResult(null); }

        // Cancel + complete; do NOT Wait on the worker.
        try { _cts.Cancel(); } catch { /* swallow */ }
        try { _queue.Writer.TryComplete(); } catch { /* swallow */ }
        try { _cache.Dispose(); } catch { /* swallow */ }
        try { _cts.Dispose(); } catch { /* swallow */ }
    }

    internal sealed record ThumbnailRequest(
        string Path,
        double? ModifiedAt,
        TaskCompletionSource<BitmapImage?> Completion,
        CancellationToken Cancellation);
}
