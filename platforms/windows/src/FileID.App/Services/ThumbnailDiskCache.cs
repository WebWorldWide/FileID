// ThumbnailDiskCache — persistent on-disk LRU cache for decoded
// thumbnails. Survives app restarts so a rescan of a 50K-tile library
// doesn't re-render every thumbnail from scratch.
//
// Layout: %LOCALAPPDATA%\FileID\thumbs.cache\v1\<2hex>\<key>.bin
//   - 2-hex fanout prevents the OS dir-entry limit at 100K+ thumbnails.
//   - SHA256(path|mtime) keying invalidates on file edit.
//   - Stored payload is raw bytes (shell thumbnail or original file);
//     BitmapImage decodes JPEG/PNG/BMP/GIF transparently.
//   - 500 MB cap. Eviction is by in-memory LRU; no recurring disk walk.
//   - Files > 500 KB skip disk; in-memory LRU still serves them.
//
// Concurrency: writes go temp+rename; reads are bare File.Open. The
// in-memory index keeps (path → size, lastAccessTicks) so eviction
// doesn't need to re-walk the directory on every cap trip.

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
    private static readonly TimeSpan SweepInterval = TimeSpan.FromSeconds(30);

    private static DateTime _lastSweep = DateTime.MinValue;
    private static readonly object _sweepLock = new();
    private static long _cachedBytes;
    private static long _diskHits;
    private static long _diskWrites;
    private static long _diskSweeps;

    // path → (sizeBytes, lastAccessTicks). Populated by Prime(), kept
    // current by reads/writes. Eviction iterates this dictionary instead
    // of re-walking the filesystem.
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

    private static string CacheRoot { get; } = Path.Combine(AppPaths.ThumbsDir, "v1");

    /// <summary>SHA256(path|mtime), truncated to 40 hex chars for a
    /// compact filename. mtime null → key is just path (so files without
    /// an mtime get a stable key, but won't naturally invalidate on edit
    /// — these are the discovered-during-scan transient case). SHA256 over
    /// SHA1 because the CA5350 analyzer flags SHA1; not a security
    /// concern here (path keying, no secrets), but consistency wins.</summary>
    private static string Key(string path, double? modifiedAt)
    {
        var s = modifiedAt.HasValue
            ? $"{path}|{modifiedAt.Value:R}"
            : path;
        Span<byte> hash = stackalloc byte[32];
        SHA256.HashData(Encoding.UTF8.GetBytes(s), hash);
        // Hex-encode first 20 bytes (40 hex chars) — plenty of distinct
        // keys for ≤100K cached thumbnails.
        var sb = new StringBuilder(40);
        for (int i = 0; i < 20; i++) { sb.Append(hash[i].ToString("x2")); }
        return sb.ToString();
    }

    private static string CachePathFor(string key)
    {
        var bucket = key.Substring(0, 2);
        return Path.Combine(CacheRoot, bucket, key + ".bin");
    }

    /// <summary>If a cached file exists for (path, mtime), decode it on
    /// the UI dispatcher and return the BitmapImage. Bumps the file's
    /// last-access timestamp so the LRU sweep keeps it alive. Returns
    /// null on miss OR on any decode error (treats the cached file as
    /// poisoned and deletes it).</summary>
    public static async Task<BitmapImage?> TryReadAsync(
        string path,
        double? modifiedAt,
        DispatcherQueue dispatcher,
        CancellationToken ct)
    {
        var key = Key(path, modifiedAt);
        var cached = CachePathFor(key);
        if (!File.Exists(cached)) { return null; }

        byte[] bytes;
        try
        {
            bytes = await File.ReadAllBytesAsync(cached, ct).ConfigureAwait(false);
            if (_index.TryGetValue(cached, out var entry))
            {
                Interlocked.Exchange(ref entry.LastAccessTicks, DateTime.UtcNow.Ticks);
            }
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"ThumbnailDiskCache read ({path}): {ex.GetType().Name}: {ex.Message}");
            try { File.Delete(cached); } catch { /* swallow */ }
            ForgetIndexEntry(cached);
            return null;
        }

        var bmp = await RenderFromBytesOnDispatcherAsync(bytes, dispatcher, ct).ConfigureAwait(false);
        if (bmp != null)
        {
            Interlocked.Increment(ref _diskHits);
        }
        else
        {
            // Decoded to null — treat as poisoned, drop the file + index entry.
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

    /// <summary>Fire-and-forget write. Validates size + writes via temp + rename
    /// so a torn write never appears as a cache hit.</summary>
    public static Task TryWriteAsync(string path, double? modifiedAt, byte[] bytes)
    {
        if (bytes.Length == 0 || bytes.Length > MaxBytesToCache)
        {
            return Task.CompletedTask;
        }
        return Task.Run(() =>
        {
            try
            {
                var key = Key(path, modifiedAt);
                var cached = CachePathFor(key);
                var dir = Path.GetDirectoryName(cached)!;
                Directory.CreateDirectory(dir);
                if (File.Exists(cached) && new FileInfo(cached).Length == bytes.Length)
                {
                    return;
                }
                var tmp = cached + ".tmp";
                File.WriteAllBytes(tmp, bytes);
                File.Move(tmp, cached, overwrite: true);

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
                DebugLog.Warn($"ThumbnailDiskCache write ({path}): {ex.GetType().Name}: {ex.Message}");
            }
        });
    }

    private static void MaybeSweep()
    {
        if (Interlocked.Read(ref _cachedBytes) <= CacheCapBytes) { return; }
        var now = DateTime.UtcNow;
        if (now - _lastSweep < SweepInterval) { return; }
        lock (_sweepLock)
        {
            if (DateTime.UtcNow - _lastSweep < SweepInterval) { return; }
            _lastSweep = DateTime.UtcNow;
        }

        try
        {
            var headroom = (long)(CacheCapBytes * 0.8);
            var evicted = SelectEvictions(_index, Interlocked.Read(ref _cachedBytes), headroom);
            long freed = 0;
            foreach (var path in evicted)
            {
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

    // Pure eviction policy — exposed internal for unit-test coverage.
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

    /// <summary>Decode raw bytes into a BitmapImage on the UI dispatcher.
    /// Eager decode (SetSourceAsync from an InMemoryRandomAccessStream)
    /// avoids the V15.5 lazy-UriSource bug where ImageOpened never
    /// fired.</summary>
    private static async Task<BitmapImage?> RenderFromBytesOnDispatcherAsync(
        byte[] bytes,
        DispatcherQueue dispatcher,
        CancellationToken ct)
    {
        var tcs = new TaskCompletionSource<BitmapImage?>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        var ok = dispatcher.TryEnqueue(async () =>
        {
            // Stream lifetime — see ThumbnailService.RunBytesSetSource:
            // the `using var stream` form disposes too early under some
            // SetSourceAsync timings. Pull out, dispose in finally AFTER
            // the await completes.
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
                // Match ThumbnailService.ThumbnailRequestPx (192). DPI-aware
                // sampling was reverted in V16.1 — see the comment in
                // ThumbnailService.cs for the rationale.
                var bmp = new BitmapImage { DecodePixelWidth = 192 };
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

    /// <summary>One-time startup walk that seeds the in-memory LRU index
    /// (path → size, lastAccessTicks). After Prime returns, eviction never
    /// re-walks the directory — all subsequent reads/writes update the
    /// index in place. Called once from app init.</summary>
    public static void Prime()
    {
        try
        {
            if (!Directory.Exists(CacheRoot)) { return; }
            var dirInfo = new DirectoryInfo(CacheRoot);
            long total = 0;
            foreach (var fi in dirInfo.EnumerateFiles("*.bin", SearchOption.AllDirectories))
            {
                try
                {
                    _index[fi.FullName] = new CacheEntry
                    {
                        SizeBytes = fi.Length,
                        LastAccessTicks = fi.LastAccessTimeUtc.Ticks,
                    };
                    total += fi.Length;
                }
                catch { /* swallow */ }
            }
            Interlocked.Exchange(ref _cachedBytes, total);
        }
        catch { /* swallow */ }
    }
}
