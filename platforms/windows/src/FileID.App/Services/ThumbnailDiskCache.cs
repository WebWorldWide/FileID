// ThumbnailDiskCache — persistent on-disk LRU cache for decoded
// Survives app restarts so a rescan of a 50K-tile library doesn't re-render every thumbnail from scratch.

using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Security.Cryptography;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml.Media.Imaging;
using Windows.Storage.Streams;

namespace FileID.Services;

internal static class ThumbnailDiskCache
{
    private const int MaxBytesToCache = 500 * 1024;
    private const long CacheCapBytes = 500L * 1024 * 1024;
    // Above this the cache is far enough over the soft cap that we sweep immediately, bypassing the 30 s time throttle — otherwise a burst of writes between sweeps could overshoot the cap by hundreds of MB.
    private const long HardCeilingBytes = (long)(CacheCapBytes * 1.2);
    private static readonly TimeSpan SweepInterval = TimeSpan.FromSeconds(30);

    private static DateTime _lastSweep = DateTime.MinValue;
    private static readonly object _sweepLock = new();
    private static long _cachedBytes;
    private static long _diskHits;
    private static long _diskWrites;
    private static long _diskSweeps;

    private static readonly ConcurrentDictionary<string, CacheEntry> _index =
        new(StringComparer.OrdinalIgnoreCase);

    internal sealed class CacheEntry
    {
        public long SizeBytes;
        public long LastAccessTicks;
    }

    public static long DiskHits => Interlocked.Read(ref _diskHits);
    public static long DiskWrites => Interlocked.Read(ref _diskWrites);
    public static long DiskSweeps => Interlocked.Read(ref _diskSweeps);
    public static long CachedBytes => Interlocked.Read(ref _cachedBytes);
    public static int IndexedEntries => _index.Count;

    internal const int LegacyImagePixelSize = 192;

    private static string CacheRoot { get; } = Path.Combine(AppPaths.ThumbsDir, "v1");

    /// <summary>SHA256 over SHA1 because the CA5350 analyzer flags SHA1; not a security concern here (path keying, no secrets), but consistency wins.</summary>
    internal static string Key(string path, double? modifiedAt, int? videoPixelSize = null, int imagePixelSize = LegacyImagePixelSize)
    {
        var s = modifiedAt.HasValue
            ? $"{path}|{modifiedAt.Value:R}"
            : path;
        if (videoPixelSize is { } size) s += $"|video@{size}";
        else if (imagePixelSize != LegacyImagePixelSize) s += $"|image@{imagePixelSize}";
        Span<byte> hash = stackalloc byte[32];
        SHA256.HashData(Encoding.UTF8.GetBytes(s), hash);
        var sb = new StringBuilder(40);
        for (int i = 0; i < 20; i++) { sb.Append(hash[i].ToString("x2")); }
        return sb.ToString();
    }

    private static string CachePathFor(string key)
    {
        var bucket = key.Substring(0, 2);
        return Path.Combine(CacheRoot, bucket, key + ".bin");
    }

    /// <summary>If a cached file exists for (path, mtime), decode it on the UI dispatcher and return the BitmapImage.</summary>
    public static async Task<BitmapImage?> TryReadAsync(
        string path,
        double? modifiedAt,
        DispatcherQueue dispatcher,
        CancellationToken ct,
        int? videoPixelSize = null,
        int imagePixelSize = LegacyImagePixelSize)
    {
        var key = Key(path, modifiedAt, videoPixelSize, imagePixelSize);
        var cached = CachePathFor(key);
        if (!File.Exists(cached)) { return null; }

        byte[] bytes;
        try
        {
            bytes = await File.ReadAllBytesAsync(cached, ct).ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"ThumbnailDiskCache read ({PathRedactor.Redact(path)}): {ex.GetType().Name}: {ex.Message}");
            try { File.Delete(cached); } catch { /* swallow */ }
            ForgetIndexEntry(cached);
            return null;
        }

        var bmp = await RenderFromBytesOnDispatcherAsync(bytes, videoPixelSize is null ? imagePixelSize : null, dispatcher, ct).ConfigureAwait(false);
        if (bmp != null)
        {
            // Only a successful decode counts as an LRU hit — a poisoned file (decode null, below) is dropped, so refreshing its timestamp first would be moot and could keep a bad file alive on a transient null.
            if (_index.TryGetValue(cached, out var entry))
            {
                Interlocked.Exchange(ref entry.LastAccessTicks, DateTime.UtcNow.Ticks);
            }
            Interlocked.Increment(ref _diskHits);
        }
        else
        {
            try { File.Delete(cached); } catch { /* swallow */ }
            ForgetIndexEntry(cached);
        }
        return bmp;
    }

    private static void ForgetIndexEntry(string cached)
    {
        if (_index.TryRemove(cached, out var removed))
        {
            Interlocked.Add(ref _cachedBytes, -removed.SizeBytes);
        }
    }

    /// <summary>Validates size + writes via temp + rename so a torn write never appears as a cache hit.</summary>
    public static Task TryWriteAsync(string path, double? modifiedAt, byte[] bytes, int? videoPixelSize = null, int imagePixelSize = LegacyImagePixelSize)
    {
        if (bytes.Length == 0 || bytes.Length > MaxBytesToCache)
        {
            return Task.CompletedTask;
        }
        return Task.Run(() =>
        {
            try
            {
                var key = Key(path, modifiedAt, videoPixelSize, imagePixelSize);
                var cached = CachePathFor(key);
                var dir = Path.GetDirectoryName(cached)!;
                Directory.CreateDirectory(dir);
                if (File.Exists(cached) && new FileInfo(cached).Length == bytes.Length)
                {
                    // Identical payload already on disk — count it as a touch so the LRU sweep doesn't treat this live entry as stale.
                    if (_index.TryGetValue(cached, out var existing))
                    {
                        Interlocked.Exchange(ref existing.LastAccessTicks, DateTime.UtcNow.Ticks);
                    }
                    return;
                }
                var tmp = cached + ".tmp";
                try
                {
                    File.WriteAllBytes(tmp, bytes);
                    File.Move(tmp, cached, overwrite: true);
                }
                catch
                {
                    try { File.Delete(tmp); } catch { /* swallow */ }
                    throw;
                }

                long delta = bytes.Length;
                var now = DateTime.UtcNow.Ticks;
                _index.AddOrUpdate(
                    cached,
                    _ => new CacheEntry { SizeBytes = bytes.Length, LastAccessTicks = now },
                    (_, old) =>
                    {
                        delta = bytes.Length - old.SizeBytes;
                        return new CacheEntry { SizeBytes = bytes.Length, LastAccessTicks = now };
                    });
                Interlocked.Add(ref _cachedBytes, delta);
                Interlocked.Increment(ref _diskWrites);
                MaybeSweep();
            }
            catch (Exception ex)
            {
                DebugLog.Warn($"ThumbnailDiskCache write ({PathRedactor.Redact(path)}): {ex.GetType().Name}: {ex.Message}");
            }
        });
    }

    private static void MaybeSweep()
    {
        var cached = Interlocked.Read(ref _cachedBytes);
        if (cached <= CacheCapBytes) { return; }
        // But once the cache has blown past a hard ceiling the cap is no longer a soft target, so we sweep regardless of how recently we last swept.
        bool overHardCeiling = cached > HardCeilingBytes;
        var now = DateTime.UtcNow;
        if (!overHardCeiling && now - _lastSweep < SweepInterval) { return; }
        lock (_sweepLock)
        {
            if (!overHardCeiling && DateTime.UtcNow - _lastSweep < SweepInterval) { return; }
            _lastSweep = DateTime.UtcNow;
        }

        try
        {
            var headroom = (long)(CacheCapBytes * 0.8);
            var sweepStartTicks = DateTime.UtcNow.Ticks;
            var evicted = SelectEvictions(_index, Interlocked.Read(ref _cachedBytes), headroom);
            long freed = 0;
            foreach (var path in evicted)
            {
                if (_index.TryGetValue(path, out var current) &&
                    Interlocked.Read(ref current.LastAccessTicks) > sweepStartTicks)
                {
                    continue;
                }
                try
                {
                    File.Delete(path);
                    if (_index.TryRemove(path, out var entry)) { freed += entry.SizeBytes; }
                }
                catch { /* swallow — file may have been re-written */ }
            }
            if (freed > 0) { Interlocked.Add(ref _cachedBytes, -freed); }
            Interlocked.Increment(ref _diskSweeps);
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"ThumbnailDiskCache sweep: {ex.GetType().Name}: {ex.Message}");
        }
    }

    internal static IReadOnlyList<string> SelectEvictions(
        IEnumerable<KeyValuePair<string, CacheEntry>> entries,
        long currentBytes,
        long headroomBytes)
    {
        if (currentBytes <= headroomBytes) { return Array.Empty<string>(); }
        var sorted = entries.OrderBy(kvp => kvp.Value.LastAccessTicks);
        var picks = new List<string>();
        long total = currentBytes;
        foreach (var kvp in sorted)
        {
            if (total <= headroomBytes) { break; }
            picks.Add(kvp.Key);
            total -= kvp.Value.SizeBytes;
        }
        return picks;
    }

    /// <summary>Eager decode (SetSourceAsync from an InMemoryRandomAccessStream) avoids the lazy-UriSource bug where ImageOpened never fired.</summary>
    private static async Task<BitmapImage?> RenderFromBytesOnDispatcherAsync(
        byte[] bytes,
        int? decodePixelWidth,
        DispatcherQueue dispatcher,
        CancellationToken ct)
    {
        var tcs = new TaskCompletionSource<BitmapImage?>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        var ok = dispatcher.TryEnqueue(async () =>
        {
            InMemoryRandomAccessStream? stream = null;
            try
            {
                stream = new InMemoryRandomAccessStream();
                using (var writer = new DataWriter(stream.GetOutputStreamAt(0)))
                {
                    writer.WriteBytes(bytes);
                    await writer.StoreAsync().AsTask(ct);
                    await writer.FlushAsync().AsTask(ct);
                    writer.DetachStream();
                }
                stream.Seek(0);
                var bmp = decodePixelWidth is { } width ? new BitmapImage { DecodePixelWidth = width } : new BitmapImage();
                await bmp.SetSourceAsync(stream).AsTask(ct);
                DebugLog.Debug($"[THUMB] DECODE_OK bytes={bytes.Length} src=disk");
                tcs.TrySetResult(bmp);
            }
            catch (Exception ex)
            {
                DebugLog.Warn($"[THUMB] DECODE_FAIL ex={ex.GetType().Name} msg={ex.Message} bytes={bytes.Length} src=disk");
                tcs.TrySetResult(null);
            }
            finally
            {
                try { stream?.Dispose(); } catch { /* swallow */ }
            }
        });
        if (!ok)
        {
            return null;
        }
        return await tcs.Task.ConfigureAwait(false);
    }

    /// <summary>After Prime returns, eviction never re-walks the directory — all subsequent reads/writes update the index in place.</summary>
    public static void Prime()
    {
        try
        {
            if (!Directory.Exists(CacheRoot)) { return; }
            var dirInfo = new DirectoryInfo(CacheRoot);
            // TryAdd (not the indexer) yields to a concurrent write's newer entry, and we Add only the bytes WE inserted (never Exchange) so a write's own Interlocked.Add isn't clobbered — no double-count, no lost total.
            long added = 0;
            foreach (var fi in dirInfo.EnumerateFiles("*.bin", SearchOption.AllDirectories))
            {
                try
                {
                    var entry = new CacheEntry
                    {
                        SizeBytes = fi.Length,
                        LastAccessTicks = fi.LastAccessTimeUtc.Ticks,
                    };
                    if (_index.TryAdd(fi.FullName, entry))
                    {
                        added += fi.Length;
                    }
                }
                catch { /* swallow */ }
            }
            Interlocked.Add(ref _cachedBytes, added);
        }
        catch { /* swallow */ }
    }
}
