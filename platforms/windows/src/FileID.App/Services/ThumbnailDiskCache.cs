// ThumbnailDiskCache — persistent on-disk LRU cache for decoded
// thumbnails. Survives app restarts so a rescan of a 50K-tile library
// doesn't re-render every thumbnail from scratch (previously: every cold
// open of the app re-paid the shell thumbnail extraction cost for every
// visible tile).
//
// Layout: %LOCALAPPDATA%\FileID\thumbs.cache\<2hex>\<rest>.bin
//   - 2-hex fanout prevents the OS dir-entry limit at 100K+ thumbnails.
//   - SHA1(path|mtime) keying invalidates on file edit (mtime change).
//   - Stored payload is the raw bytes from the source (shell thumbnail
//     stream OR original file stream); BitmapImage's decoder handles
//     JPEG/PNG/BMP/GIF transparently.
//   - 500 MB cap (configurable later). Sweep: every 30s, drop oldest
//     files until total drops below cap.
//   - Files larger than 500 KB are NOT written to disk — the in-memory
//     LRU still covers them; persisting big originals would blow the cap
//     in dozens of entries.
//
// Concurrency: file writes go via a temp + rename so a concurrent read
// never sees a partial file. Reads are bare File.Open since we control
// the entire writing side.

using System;
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
    /// <summary>Don't write payloads above this size — the in-memory LRU
    /// still serves them, but writing a 5 MB original to disk per tile
    /// blows the 500 MB cap in ~100 tiles. Shell thumbnails are typically
    /// 5–50 KB; the fallback path's original images are kept in this
    /// budget too.</summary>
    private const int MaxBytesToCache = 500 * 1024;

    /// <summary>Total cache cap. Sweep drops oldest files until total
    /// drops below this number. Sized for ~10K avg-size thumbnails.</summary>
    private const long CacheCapBytes = 500L * 1024 * 1024;

    /// <summary>Minimum interval between sweeps. Keeps a hot-cache scan
    /// from running a directory-walk every other thumbnail write.</summary>
    private static readonly TimeSpan SweepInterval = TimeSpan.FromSeconds(30);

    private static DateTime _lastSweep = DateTime.MinValue;
    private static readonly object _sweepLock = new();
    private static long _cachedBytes;
    private static long _diskHits;
    private static long _diskWrites;
    private static long _diskSweeps;

    public static long DiskHits => Interlocked.Read(ref _diskHits);
    public static long DiskWrites => Interlocked.Read(ref _diskWrites);
    public static long DiskSweeps => Interlocked.Read(ref _diskSweeps);
    public static long CachedBytes => Interlocked.Read(ref _cachedBytes);

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
            // Touch the access time so the LRU sweep treats this as warm.
            try { File.SetLastAccessTimeUtc(cached, DateTime.UtcNow); } catch { /* swallow */ }
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"ThumbnailDiskCache read ({path}): {ex.GetType().Name}: {ex.Message}");
            try { File.Delete(cached); } catch { /* swallow */ }
            return null;
        }

        var bmp = await RenderFromBytesOnDispatcherAsync(bytes, dispatcher, ct).ConfigureAwait(false);
        if (bmp != null)
        {
            Interlocked.Increment(ref _diskHits);
        }
        else
        {
            // Cached file decoded to null — almost certainly poisoned;
            // delete so a subsequent fresh render replaces it.
            try { File.Delete(cached); } catch { /* swallow */ }
        }
        return bmp;
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
                // Skip if already cached + same size (mtime guarantees content).
                if (File.Exists(cached) && new FileInfo(cached).Length == bytes.Length)
                {
                    return;
                }
                var tmp = cached + ".tmp";
                File.WriteAllBytes(tmp, bytes);
                // Move clobbers the destination atomically on NTFS.
                File.Move(tmp, cached, overwrite: true);
                Interlocked.Add(ref _cachedBytes, bytes.Length);
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
        // Cheap probe outside the lock — only one writer thread proceeds.
        var now = DateTime.UtcNow;
        if (now - _lastSweep < SweepInterval) { return; }
        lock (_sweepLock)
        {
            if (DateTime.UtcNow - _lastSweep < SweepInterval) { return; }
            _lastSweep = DateTime.UtcNow;
        }

        try
        {
            if (!Directory.Exists(CacheRoot)) { return; }
            var files = Directory.EnumerateFiles(CacheRoot, "*.bin", SearchOption.AllDirectories)
                .Select(p =>
                {
                    var fi = new FileInfo(p);
                    return (Path: p, Size: fi.Length, LastAccess: fi.LastAccessTimeUtc);
                })
                .ToList();
            var total = files.Sum(f => f.Size);
            Interlocked.Exchange(ref _cachedBytes, total);
            if (total <= CacheCapBytes) { return; }
            // Drop oldest-access-first until under cap. The 80% headroom
            // gives the next 100MB of writes breathing room before the
            // next sweep — avoids thrashing right after eviction.
            var headroom = (long)(CacheCapBytes * 0.8);
            foreach (var f in files.OrderBy(f => f.LastAccess))
            {
                if (total <= headroom) { break; }
                try
                {
                    File.Delete(f.Path);
                    total -= f.Size;
                }
                catch { /* swallow — file may have been re-written */ }
            }
            Interlocked.Exchange(ref _cachedBytes, total);
            Interlocked.Increment(ref _diskSweeps);
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"ThumbnailDiskCache sweep: {ex.GetType().Name}: {ex.Message}");
        }
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
            try
            {
                using var stream = new InMemoryRandomAccessStream();
                using (var writer = new DataWriter(stream.GetOutputStreamAt(0)))
                {
                    writer.WriteBytes(bytes);
                    await writer.StoreAsync().AsTask(ct);
                    await writer.FlushAsync().AsTask(ct);
                    writer.DetachStream();
                }
                stream.Seek(0);
                var bmp = new BitmapImage { DecodePixelWidth = 192 };
                await bmp.SetSourceAsync(stream).AsTask(ct);
                tcs.TrySetResult(bmp);
            }
            catch (Exception ex)
            {
                DebugLog.Warn($"ThumbnailDiskCache decode: {ex.GetType().Name}: {ex.Message}");
                tcs.TrySetResult(null);
            }
        });
        if (!ok)
        {
            return null;
        }
        return await tcs.Task.ConfigureAwait(false);
    }

    /// <summary>Pre-load the cached-bytes counter so Diagnostics shows a
    /// real number on startup without waiting for the first sweep. Called
    /// once from app init; cheap (one directory walk, sized for ≤10K
    /// files).</summary>
    public static void Prime()
    {
        try
        {
            if (!Directory.Exists(CacheRoot)) { return; }
            long total = 0;
            foreach (var p in Directory.EnumerateFiles(CacheRoot, "*.bin", SearchOption.AllDirectories))
            {
                try { total += new FileInfo(p).Length; } catch { /* swallow */ }
            }
            Interlocked.Exchange(ref _cachedBytes, total);
        }
        catch { /* swallow */ }
    }
}
