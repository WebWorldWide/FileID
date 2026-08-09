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
    /// <summary>Decoded-BitmapImage L1 cache, bounded by a real BYTE budget
    /// (not an entry count). Each cached BitmapImage holds a DECODED bitmap
    /// (~ThumbnailRequestPx² × 4 bytes), so the old 5000-entry cap was really
    /// ~550 MB of live bitmaps on a large library — not the "~25 MB" the entry
    /// count implied (that estimate was the ENCODED ~5 KB/thumb, not the decoded
    /// surface). Sizing each entry by its decoded cost and capping the total at
    /// ~128 MB holds the working set bounded across a 50K-file scroll; LRU evicts
    /// the coldest thumbnails and a miss just re-decodes (no correctness hit).</summary>
    private const long DecodedBytesPerEntry = (long)ThumbnailRequestPx * ThumbnailRequestPx * 4;
    private const long L1CacheByteBudget = 128L * 1024 * 1024;
    private readonly MemoryCache _cache = new(new MemoryCacheOptions
    {
        SizeLimit = L1CacheByteBudget,
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

    /// <summary>VIDEO thumbnails are NOT produced in-process (the shell /
    /// Media Foundation chain native-fast-fails the whole app — see
    /// <see cref="VideoExtensions"/>). On an L1+L2 miss for a video we ask the
    /// engine to extract a keyframe out-of-process and complete the tile when
    /// the <c>thumbnailGenerated</c> event arrives. Three pieces of instance
    /// state coordinate that round-trip:
    ///   • <see cref="_pendingVideo"/> — CacheKey → the request's TCS, completed
    ///     by the engine event (or the timeout) instead of being completed null
    ///     in <see cref="DrainAsync"/>.
    ///   • <see cref="_requestedVideo"/> — CacheKeys already asked-for this
    ///     session, so a re-scroll over the same tile never re-sends the command
    ///     (the engine extract is expensive). Cleared per key once the event
    ///     lands so a later file-edit (new modifiedAt → new key) can re-request.
    /// Both are guarded by <see cref="_videoLock"/> (the worker thread enqueues,
    /// the engine-event handler on the UI thread completes — two threads).</summary>
    private readonly Dictionary<string, TaskCompletionSource<BitmapImage?>> _pendingVideo = new();
    private readonly HashSet<string> _requestedVideo = new();
    private readonly object _videoLock = new();
    private const int VideoThumbTimeoutMs = 20_000;
    private volatile bool _disposed;

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
        // Out-of-process video keyframes: the engine replies with a
        // `thumbnailGenerated` event (surfaced as LastThumbnailGenerated). We
        // L2-write it, decode it on the UI thread, and complete the tile's
        // pending TCS. Detached in Dispose.
        EngineClient.Instance.PropertyChanged += OnEngineClientChanged;
    }

    public Task<BitmapImage?> RequestAsync(string path, double? modifiedAt, CancellationToken ct)
    {
        DebugLog.Debug($"[THUMB] REQUEST file={PathRedactor.Redact(path)}");
        var key = CacheKey(path, modifiedAt);
        if (_cache.TryGetValue(key, out BitmapImage? cached) && cached != null)
        {
            DebugLog.Debug($"[THUMB] L1_HIT file={PathRedactor.Redact(path)}");
            return Task.FromResult<BitmapImage?>(cached);
        }
        DebugLog.Debug($"[THUMB] L1_MISS file={PathRedactor.Redact(path)}");
        var tcs = new TaskCompletionSource<BitmapImage?>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        var ok = _queue.Writer.TryWrite(new ThumbnailRequest(path, modifiedAt, tcs, ct));
        if (!ok)
        {
            DebugLog.Debug($"[THUMB] QUEUE_FULL_DROP file={PathRedactor.Redact(path)}");
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
                // Honor the PER-REQUEST token (a scrolled-away tile cancels its own
                // request), linked with the service token, so the full shell+disk+
                // decode doesn't run for a tile the user already scrolled past. (audit A6)
                using var linked = CancellationTokenSource.CreateLinkedTokenSource(ct, req.Cancellation);
                // No ConfigureAwait(false): the returned BitmapImage is a
                // UI-thread DispatcherObject, so keep the continuation (and the
                // caller's, which sets tile.Thumbnail) on the UI thread.
                var bmp = await RenderAsync(req.Path, req.ModifiedAt, _uiDispatcher, linked.Token);
                if (bmp != null)
                {
                    var key = CacheKey(req.Path, req.ModifiedAt);
                    _cache.Set(key, bmp, new MemoryCacheEntryOptions
                    {
                        Size = DecodedBytesPerEntry,
                        SlidingExpiration = TimeSpan.FromMinutes(15),
                    });
                    req.Completion.TrySetResult(bmp);
                }
                else if (TryBeginVideoThumbnailRequest(req))
                {
                    // Video L1+L2 miss: RenderAsync returned null (we never invoke
                    // the in-process shell chain for video — native fast-fail
                    // hazard). The request is now PENDING on the engine's
                    // out-of-process keyframe; do NOT complete it null here.
                    // OnEngineClientChanged completes req.Completion when the
                    // `thumbnailGenerated` event arrives, or the armed timeout
                    // completes it null (placeholder) and unregisters it.
                }
                else
                {
                    req.Completion.TrySetResult(bmp);
                }
            }
            catch (OperationCanceledException)
            {
                // Per-request cancel (tile scrolled away): complete null and keep
                // draining. A service-token (shutdown) cancel re-throws so the
                // loop exits as before. (audit A6)
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

    /// <summary>For a VIDEO file that missed L1+L2: register the request's TCS
    /// as PENDING (so it stays unresolved until the engine event or the
    /// timeout), dedupe per (path,modifiedAt) so the expensive out-of-process
    /// extract is requested at most once per session, fire the
    /// <c>generateVideoThumbnail</c> command fire-and-forget, and arm a timeout
    /// that completes the tile null (placeholder) if no keyframe arrives.
    /// Returns true when the request was taken over (caller must NOT complete
    /// it); false for non-video, or when the service is disposed.</summary>
    private bool TryBeginVideoThumbnailRequest(ThumbnailRequest req)
    {
        if (_disposed) return false;
        if (!VideoExtensions.Contains(Path.GetExtension(req.Path))) return false;

        var key = CacheKey(req.Path, req.ModifiedAt);
        lock (_videoLock)
        {
            if (_disposed) return false;
            // Already requested this session: a prior tile owns the engine
            // round-trip. We still take over THIS request — park its TCS under
            // the key. The event handler completes whichever TCS is registered;
            // if one is already parked, this later request just gets null after
            // its own timeout (the first tile's keyframe filled the caches, so a
            // future RequestAsync L1-hits). Keep the most recent pending TCS.
            _pendingVideo[key] = req.Completion;
            bool firstRequest = _requestedVideo.Add(key);
            if (firstRequest)
            {
                DebugLog.Debug($"[THUMB] VIDEO_ENGINE_REQUEST file={PathRedactor.Redact(req.Path)}");
                _ = EngineClient.Instance.GenerateVideoThumbnailAsync(req.Path, req.ModifiedAt);
            }
            else
            {
                DebugLog.Debug($"[THUMB] VIDEO_ENGINE_DEDUP file={PathRedactor.Redact(req.Path)}");
            }
        }

        // Timeout: if no keyframe lands in VideoThumbTimeoutMs, complete the
        // tile null (placeholder) and unregister so the dictionary doesn't grow
        // unbounded. The dedupe set intentionally KEEPS the key so we don't
        // re-hammer the engine for a video it couldn't extract this session.
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
            // Service disposed mid-wait — release the pending tile so callers
            // (and the awaiting RequestAsync) don't hang.
            tcs.TrySetResult(null);
            return;
        }
        lock (_videoLock)
        {
            // Only clear the dictionary entry if it's still THIS request's TCS;
            // a newer request for the same key may have replaced it.
            if (_pendingVideo.TryGetValue(key, out var current) && ReferenceEquals(current, tcs))
            {
                _pendingVideo.Remove(key);
            }
        }
        if (tcs.TrySetResult(null))
        {
            DebugLog.Debug($"[THUMB] VIDEO_ENGINE_TIMEOUT key={PathRedactor.Redact(key)}");
        }
    }

    private void OnEngineClientChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("ThumbnailService.OnEngineClientChanged", () =>
        {
            if (_disposed) return;
            if (e.PropertyName != nameof(EngineClient.LastThumbnailGenerated)) return;
            DebugLog.Debug($"[ENGINE-SUB:ThumbnailService] {e.PropertyName}");
            var evt = EngineClient.Instance.LastThumbnailGenerated;
            if (evt is null) return;

            // Decode + cache + complete the tile, all on the UI thread:
            // BitmapImage is a DispatcherObject (must be built on a thread we
            // captured), and the byte-budgeted L1 _cache.Set lives next to it.
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

            // Persist to L2 first so a later session / re-scroll L2-hits even if
            // no tile is currently pending (another instance, or post-timeout).
            _ = ThumbnailDiskCache.TryWriteAsync(evt.Path, evt.ModifiedAt, bytes);

            // We're already on the UI dispatcher (TryEnqueue above), so the
            // BitmapImage decode + SetSourceAsync is thread-affinity-safe.
            var bmp = await DecodeOnThisThreadAsync(bytes, _cts.Token).ConfigureAwait(true);
            if (bmp is null)
            {
                CompletePendingVideoNull(key);
                return;
            }

            _cache.Set(key, bmp, new MemoryCacheEntryOptions
            {
                Size = DecodedBytesPerEntry,
                SlidingExpiration = TimeSpan.FromMinutes(15),
            });
            DebugLog.Debug($"[THUMB] BITMAP_SET file={PathRedactor.Redact(evt.Path)} src=engine-video");

            TaskCompletionSource<BitmapImage?>? pending;
            lock (_videoLock)
            {
                // Robust: if no tile is pending (timed out, or another instance
                // requested it) we still wrote the caches above — just return.
                if (_pendingVideo.TryGetValue(key, out pending))
                {
                    _pendingVideo.Remove(key);
                }
            }
            pending?.TrySetResult(bmp);
        }).ConfigureAwait(true);
    }

    /// <summary>Complete + unregister the pending tile for a key with null
    /// (placeholder). No-op when nothing is pending. Guarded by the lock.</summary>
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

    /// <summary>Decode bytes into a BitmapImage on the CURRENT (UI) thread.
    /// Caller MUST already be on the dispatcher (the engine-event handler
    /// enqueues this). Mirrors ThumbnailDiskCache's eager InMemoryRandomAccessStream
    /// + SetSourceAsync decode. Returns null on any decode failure.</summary>
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
            var bmp = new BitmapImage { DecodePixelWidth = (int)ThumbnailRequestPx };
            await bmp.SetSourceAsync(stream).AsTask(ct);
            DebugLog.Debug($"[THUMB] DECODE_OK bytes={bytes.Length} src=engine-video");
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

    /// <summary>
    /// Shell-thumbnail request size. 192-px matches macOS ThumbnailService.swift
    /// (the shell uses the same IThumbnailProvider chain as Explorer — Office /
    /// raw / .heic all work). Do NOT add per-window DPI scaling here: the WinRT
    /// `GetWindowHandle` interop is UI-thread-affined and, called from this
    /// worker-thread drain, poisoned later `SetSourceAsync` calls (V16.1 revert).
    /// </summary>
    private const uint ThumbnailRequestPx = 192;

    /// <summary>extensions where we have a WIC-backed fallback if
    /// the shell provider chain returns nothing. Matches what Explorer's
    /// Photos app would render. Other extensions get null + a failure
    /// counter bump.</summary>
    private static readonly HashSet<string> ImageExtensions = new(StringComparer.OrdinalIgnoreCase)
    {
        ".jpg", ".jpeg", ".png", ".gif", ".bmp", ".webp",
        ".tif", ".tiff", ".heic", ".heif", ".avif", ".ico", ".jfif",
    };

    /// <summary>Audio extensions whose album art comes ONLY from the in-process
    /// shell IThumbnailProvider chain. That native code runs inside our process
    /// (no DllHost/COM-surrogate isolation like Explorer), so a flaky audio
    /// codec/art handler that fast-fails takes the whole app down with NO
    /// managed exception. The 2026-05-30 ~2h-scan crash died exactly here:
    /// app.log stops mid-burst extracting .mp3 album art (clean_exit=false,
    /// native RaiseFailFastException). We skip the shell call for these and
    /// render the placeholder instead — a previously disk-cached cover still
    /// shows (the L2 read runs first). Diverges from macOS (QLThumbnailGenerator
    /// runs OUT of process there, so it's safe). Revisit once a WER LocalDump
    /// confirms the faulting provider.</summary>
    private static readonly HashSet<string> AudioExtensions = new(StringComparer.OrdinalIgnoreCase)
    {
        ".mp3", ".m4a", ".aac", ".flac", ".wav", ".ogg", ".oga", ".opus",
        ".wma", ".aiff", ".aif", ".alac", ".ape", ".mka", ".m4b",
    };

    /// <summary>Video extensions whose thumbnail (a decoded keyframe) would come
    /// from the SAME in-process shell IThumbnailProvider / Media Foundation chain
    /// as audio art — and the same hazard: a flaky codec/handler fast-fails the
    /// whole app with NO managed exception. The 2026-06-02 scan crash died EXACTLY
    /// here, mid-burst thumbnailing .mov files (app.log stops with no exception;
    /// the engine then saw stdin EOF and exited cleanly — innocent). We skip the
    /// in-proc shell call for video and render the placeholder; a previously
    /// disk-cached keyframe still shows via the L2 read above. Mirrors the
    /// AudioExtensions guard. Restoring LIVE video thumbnails safely needs an
    /// OUT-OF-PROCESS extractor (shell IThumbnailCache, or reusing the engine's
    /// scan-time keyframe) — tracked in NEXT.md. Arm build/enable-crash-dumps.ps1
    /// to capture the native faulting stack if a provider ever faults again.</summary>
    private static readonly HashSet<string> VideoExtensions = new(StringComparer.OrdinalIgnoreCase)
    {
        ".mov", ".mp4", ".m4v", ".avi", ".mkv", ".wmv", ".webm", ".mpg",
        ".mpeg", ".mpe", ".3gp", ".3g2", ".mts", ".m2ts", ".ts", ".flv",
        ".ogv", ".vob", ".qt",
    };

    /// <summary>True when the file's extension is audio or video — kinds whose
    /// shell IThumbnailProvider runs IN-PROCESS (Media Foundation) and can
    /// native-fast-fail the whole app (an unpackaged WinUI app has no
    /// DllHost/COM-surrogate isolation). EVERY direct GetThumbnailAsync callsite
    /// must consult this before invoking the shell chain (this service's
    /// RenderAsync, DeepAnalyzeView, DrillDownSheet). NOT images / HEIC / HEIF /
    /// AVIF — those render fine through the shell provider, so they must NOT be
    /// skipped. Single source of truth so a newly-added extension covers all
    /// callsites at once.</summary>
    public static bool SkipShellThumbnailForExtension(string path)
    {
        var ext = Path.GetExtension(path);
        return AudioExtensions.Contains(ext) || VideoExtensions.Contains(ext);
    }

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
        var dispatcher = uiDispatcher
            ?? FileID.App.HostWindow?.DispatcherQueue
            ?? Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread();
        if (dispatcher is null)
        {
            Interlocked.Increment(ref _droppedDispatcher);
            DebugLog.Warn($"ThumbnailService.RenderAsync: no UI dispatcher available; skipping ({PathRedactor.Redact(path)}).");
            return null;
        }

        // 0) Disk cache — survives app restart.
        var diskHit = await ThumbnailDiskCache.TryReadAsync(path, modifiedAt, dispatcher, ct)
            .ConfigureAwait(false);
        if (diskHit != null)
        {
            DebugLog.Debug($"[THUMB] L2_HIT file={PathRedactor.Redact(path)}");
            Interlocked.Increment(ref _renderedOk);
            return diskHit;
        }
        DebugLog.Debug($"[THUMB] L2_MISS file={PathRedactor.Redact(path)}");

        var ext = Path.GetExtension(path);

        // Audio: do NOT invoke the in-process shell provider (see
        // AudioExtensions). A previously-cached cover already returned above via
        // the L2 disk read; with no cache we render the placeholder rather than
        // risk a native fast-fail in an audio art handler.
        if (AudioExtensions.Contains(ext))
        {
            DebugLog.Debug($"[THUMB] AUDIO_SHELL_SKIP file={PathRedactor.Redact(path)} ext={ext}");
            Interlocked.Increment(ref _renderedFailed);
            return null;
        }

        // Video: same in-process shell hazard as audio — a flaky codec / Media
        // Foundation thumbnail handler fast-fails the whole app with no managed
        // exception. The 2026-06-02 scan crash died here on .mov. Skip the shell
        // call; a previously-cached keyframe already returned via the L2 read, else
        // the placeholder renders. (See VideoExtensions; out-of-proc keyframe is
        // the follow-up to restore live video thumbnails — NEXT.md.)
        if (VideoExtensions.Contains(ext))
        {
            DebugLog.Debug($"[THUMB] VIDEO_SHELL_SKIP file={PathRedactor.Redact(path)} ext={ext}");
            Interlocked.Increment(ref _renderedFailed);
            return null;
        }

        // 1) Shell IThumbnailProvider chain — same one Explorer / Photos
        //    use. Office / RAW / HEIC / etc. all work through this path.
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
                DebugLog.Debug($"[THUMB] SHELL_OK file={PathRedactor.Redact(path)} bytes={bytes.Length}");
                var bmp = await RenderFromBytesOnDispatcherAsync(bytes, dispatcher, ct).ConfigureAwait(false);
                if (bmp != null)
                {
                    DebugLog.Debug($"[THUMB] BITMAP_SET file={PathRedactor.Redact(path)} src=shell");
                    _ = ThumbnailDiskCache.TryWriteAsync(path, modifiedAt, bytes);
                    Interlocked.Increment(ref _renderedOk);
                    return bmp;
                }
            }
            else
            {
                DebugLog.Debug($"[THUMB] SHELL_NULL file={PathRedactor.Redact(path)}");
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
            DebugLog.Debug($"[THUMB] SHELL_EX file={PathRedactor.Redact(path)} ex={ex.GetType().Name}");
            DebugLog.Warn(
                $"ThumbnailService shell-path ({PathRedactor.Redact(path)}): {ex.GetType().Name}: {ex.Message}");
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
                    DebugLog.Debug($"[THUMB] IMG_FB_OK file={PathRedactor.Redact(path)}");
                    DebugLog.Debug($"[THUMB] BITMAP_SET file={PathRedactor.Redact(path)} src=image-fallback");
                    _ = ThumbnailDiskCache.TryWriteAsync(path, modifiedAt, fileBytes);
                    Interlocked.Increment(ref _fallbackUsed);
                    Interlocked.Increment(ref _renderedOk);
                    return bmp;
                }
                DebugLog.Debug($"[THUMB] IMG_FB_NULL file={PathRedactor.Redact(path)}");
            }
            catch (Exception ex)
            {
                DebugLog.Debug($"[THUMB] IMG_FB_EX file={PathRedactor.Redact(path)} ex={ex.GetType().Name}");
                DebugLog.Warn(
                    $"ThumbnailService image-fallback ({PathRedactor.Redact(path)}): {ex.GetType().Name}: {ex.Message}");
                Interlocked.Increment(ref _renderedFailed);
                return null;
            }
        }
        else
        {
            DebugLog.Debug($"[THUMB] NO_PROVIDER file={PathRedactor.Redact(path)} ext={ext}");
        }

        DebugLog.Debug($"[THUMB] RENDER_FAILED file={PathRedactor.Redact(path)}");
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
        // Stream lifetime: must outlive SetSourceAsync. The prior `using var
        // stream` form disposed the stream as soon as RunBytesSetSource
        // returned, even if SetSourceAsync was still walking the bytes. Pull
        // out of `using` and Dispose explicitly in finally AFTER the await.
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
            var bmp = new BitmapImage { DecodePixelWidth = (int)ThumbnailRequestPx };
            await bmp.SetSourceAsync(stream).AsTask(ct);
            DebugLog.Debug($"[THUMB] DECODE_OK bytes={bytes.Length} px={ThumbnailRequestPx}");
            tcs.TrySetResult(bmp);
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"[THUMB] DECODE_FAIL ex={ex.GetType().Name} msg={ex.Message} bytes={bytes.Length} px={ThumbnailRequestPx}");
            tcs.TrySetResult(null);
        }
        finally
        {
            try { stream?.Dispose(); } catch { /* swallow */ }
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
        _disposed = true;
        try { EngineClient.Instance.PropertyChanged -= OnEngineClientChanged; } catch { /* swallow */ }
        // Release any tiles still waiting on an engine video keyframe so the
        // awaiting RequestAsync callers don't hang on shutdown. (_cts.Cancel
        // below also faults the armed timeouts, which release their own TCS.)
        List<TaskCompletionSource<BitmapImage?>> stranded;
        lock (_videoLock)
        {
            stranded = new List<TaskCompletionSource<BitmapImage?>>(_pendingVideo.Values);
            _pendingVideo.Clear();
            _requestedVideo.Clear();
        }
        foreach (var tcs in stranded) { tcs.TrySetResult(null); }

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
