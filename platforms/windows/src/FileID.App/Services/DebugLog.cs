// Local-only log at %LOCALAPPDATA%\FileID\logs\app.log, truncated past 10 MB. Debug lines need FILEID_DEBUG_LOG=1; Trace lines need TraceEnabled.
// Nothing is ever transmitted. Call sites that log a user file path must wrap it in PathRedactor.Redact.

using System.IO;
using System.Text;
using System.Threading;

namespace FileID.Services;

internal static class DebugLog
{
    private const long MaxLogBytes = 10 * 1024 * 1024;
    private const string TruncationMarker = "[log truncated; oversized]\n";
    private static readonly object s_writeLock = new();
    private static readonly UTF8Encoding s_utf8 = new(encoderShouldEmitUTF8Identifier: false);
    private static FileStream? s_stream;
    private static string? s_streamPath;
    private static long s_streamBytes;
    private static long s_reopenAfterTick;
    private static bool s_traceEnabled;
    private static bool s_debugEnabled = ReadDebugSwitch();
    // One persistent unbuffered handle: each line is a single synchronous write, so it survives a native fast-fail. A queued background writer would lose the last [APPLY:N]/[ENGINE-SUB] lines this log exists to capture.

    public static void Info(string message) => Write("INFO ", message);
    public static void Warn(string message) => Write("WARN ", message);
    public static void Error(string message) => Write("ERROR", message);
    public static void Debug(string message)
    {
        if (DebugEnabled) Write("DEBUG", message);
    }
    /// <summary>Debug lines are skipped unless FILEID_DEBUG_LOG=1 (or "true") is set for the app process. Info and above always write, so the [APPLY:N]/[ENGINE-SUB] forensic lines must stay at Info.</summary>
    public static bool DebugEnabled
    {
        get => Volatile.Read(ref s_debugEnabled);
        set => Volatile.Write(ref s_debugEnabled, value);
    }
    public static bool TraceEnabled
    {
        get => Volatile.Read(ref s_traceEnabled);
        set => Volatile.Write(ref s_traceEnabled, value);
    }
    public static void Trace(string message)
    {
        if (TraceEnabled) Write("TRACE", message);
    }

    private static bool ReadDebugSwitch()
    {
        try
        {
            var value = Environment.GetEnvironmentVariable("FILEID_DEBUG_LOG");
            return string.Equals(value, "1", StringComparison.Ordinal)
                || string.Equals(value, "true", StringComparison.OrdinalIgnoreCase);
        }
        catch
        {
            return false;
        }
    }

    /// <summary>UI click handlers that call IPC or touch the file system should route through this so a thrown exception logs + dumps + stays caught instead of escaping into the dispatcher loop.</summary>
    public static void SafeRun(string label, Action body)
    {
        try { body(); }
        catch (Exception ex)
        {
            Error($"{label} threw: {ex}");
            try { WriteCrashDump(label, ex, terminating: false); } catch { /* swallow */ }
        }
    }

    /// <summary>Use for async OnXxxClicked handlers so a thrown exception inside the async body lands here instead of becoming an UnobservedTaskException at GC time.</summary>
    public static async System.Threading.Tasks.Task SafeRunAsync(string label, System.Func<System.Threading.Tasks.Task> body)
    {
        try { await body().ConfigureAwait(true); }
        catch (Exception ex)
        {
            Error($"{label} threw: {ex}");
            try { WriteCrashDump(label, ex, terminating: false); } catch { /* swallow */ }
        }
    }

    private static void Write(string level, string message)
    {
        try
        {
            var stamp = DateTimeOffset.UtcNow.ToString("yyyy-MM-ddTHH:mm:ss.fffZ");
            var line = $"{stamp} {level} [tid {Environment.CurrentManagedThreadId}] {message}\n";
            var bytes = s_utf8.GetBytes(line);
            // Truncation and append share s_writeLock so a concurrent append can't land between the truncate and the marker.
            lock (s_writeLock)
            {
                var stream = EnsureStreamLocked();
                if (stream is null)
                {
                    return;
                }
                try
                {
                    if (s_streamBytes > MaxLogBytes)
                    {
                        stream.SetLength(0);
                        stream.Position = 0;
                        var marker = s_utf8.GetBytes(TruncationMarker);
                        stream.Write(marker, 0, marker.Length);
                        s_streamBytes = marker.Length;
                    }
                    stream.Write(bytes, 0, bytes.Length);
                    s_streamBytes += bytes.Length;
                }
                catch
                {
                    CloseStreamLocked();
                }
            }
        }
        catch
        {
            // Logging must never throw upstream.
        }
    }

    private static FileStream? EnsureStreamLocked()
    {
        var path = AppPaths.AppLogPath;
        if (s_stream is not null && string.Equals(s_streamPath, path, StringComparison.Ordinal))
        {
            return s_stream;
        }
        // After a failed open (folder locked, disk full) wait before retrying so a broken log doesn't cost a syscall storm per line.
        var now = Environment.TickCount64;
        if (s_stream is null && now < Volatile.Read(ref s_reopenAfterTick))
        {
            return null;
        }
        CloseStreamLocked();
        try
        {
            AppPaths.EnsureDirectories();
            // bufferSize 1 disables FileStream's own buffer: every Write is handed straight to the OS.
            var stream = new FileStream(
                path,
                FileMode.OpenOrCreate,
                FileAccess.Write,
                FileShare.ReadWrite,
                bufferSize: 1,
                FileOptions.None);
            stream.Seek(0, SeekOrigin.End);
            s_stream = stream;
            s_streamPath = path;
            s_streamBytes = stream.Length;
            return stream;
        }
        catch
        {
            Volatile.Write(ref s_reopenAfterTick, now + 2000);
            return null;
        }
    }

    private static void CloseStreamLocked()
    {
        var stream = s_stream;
        s_stream = null;
        s_streamPath = null;
        s_streamBytes = 0;
        if (stream is null)
        {
            return;
        }
        try { stream.Dispose(); } catch { /* best-effort */ }
    }

    public static void Close()
    {
        try
        {
            lock (s_writeLock)
            {
                CloseStreamLocked();
            }
        }
        catch
        {
        }
    }

    private static string[] ReadLogTail(int maxLines)
    {
        const int TailBytes = 256 * 1024;
        using var fs = new FileStream(
            AppPaths.AppLogPath,
            FileMode.Open,
            FileAccess.Read,
            FileShare.ReadWrite | FileShare.Delete);
        if (fs.Length > TailBytes)
        {
            fs.Seek(-TailBytes, SeekOrigin.End);
        }
        using var reader = new StreamReader(fs, Encoding.UTF8);
        var lines = reader.ReadToEnd().Split('\n', StringSplitOptions.RemoveEmptyEntries);
        return lines.Length <= maxLines ? lines : lines[^maxLines..];
    }

    public static string WriteCrashDump(string source, Exception? exception, bool terminating)
    {
        try
        {
            AppPaths.EnsureDirectories();
            var logsDir = Path.GetDirectoryName(AppPaths.AppLogPath) ?? AppPaths.Root;
            var fileName = $"crash-{DateTimeOffset.UtcNow:yyyyMMdd-HHmmss}-{Environment.ProcessId}.txt";
            var fullPath = Path.Combine(logsDir, fileName);
            var sb = new StringBuilder();
            sb.Append("FileID crash dump\n");
            sb.Append("=================\n");
            sb.Append($"timestamp_utc:    {DateTimeOffset.UtcNow:O}\n");
            sb.Append($"source:           {source}\n");
            sb.Append($"terminating:      {terminating}\n");
            sb.Append($"pid:              {Environment.ProcessId}\n");
            sb.Append($"managed_threadid: {Environment.CurrentManagedThreadId}\n");
            sb.Append($"clr_version:      {Environment.Version}\n");
            sb.Append($"os_version:       {Environment.OSVersion}\n");
            sb.Append("\n--- Exception ---\n");
            if (exception is null)
            {
                sb.Append("(no exception object)\n");
            }
            else
            {
                sb.Append($"type:    {exception.GetType().FullName}\n");
                sb.Append($"message: {exception.Message}\n\n");
                sb.Append("stack:\n");
                sb.Append(exception.ToString());
                sb.Append('\n');
            }
            sb.Append("\n--- Last 50 lines of app.log ---\n");
            try
            {
                if (File.Exists(AppPaths.AppLogPath))
                {
                    foreach (var tailLine in ReadLogTail(50))
                    {
                        sb.Append(tailLine.TrimEnd('\r'));
                        sb.Append('\n');
                    }
                }
                else
                {
                    sb.Append("(app.log not present)\n");
                }
            }
            catch (Exception readEx)
            {
                sb.Append($"(failed to read app.log: {readEx.Message})\n");
            }
            File.WriteAllText(fullPath, sb.ToString(), Encoding.UTF8);
            return fullPath;
        }
        catch
        {
            return string.Empty;
        }
    }

    // WinUI's composition layer can end the process with a native fast-fail that skips every managed crash sink. BeginSession writes last-session.txt with clean_exit=false and MarkCleanExit flips it; a launch that finds it unflipped records the prior death.
    private const string LastSessionFileName = "last-session.txt";
    private const string PriorDeathFilePrefix = "session-died-without-handler-";

    public static void BeginSession()
    {
        try
        {
            DetectPriorAbnormalExit();
            AppPaths.EnsureDirectories();
            var logsDir = Path.GetDirectoryName(AppPaths.AppLogPath) ?? AppPaths.Root;
            var path = Path.Combine(logsDir, LastSessionFileName);
            var sb = new StringBuilder();
            sb.Append("started_utc=").Append(DateTimeOffset.UtcNow.ToString("O")).Append('\n');
            sb.Append("pid=").Append(Environment.ProcessId).Append('\n');
            sb.Append("clr_version=").Append(Environment.Version).Append('\n');
            sb.Append("os_version=").Append(Environment.OSVersion).Append('\n');
            sb.Append("clean_exit=false\n");
            File.WriteAllText(path, sb.ToString(), Encoding.UTF8);
        }
        catch
        {
        }
    }

    public static void MarkCleanExit()
    {
        try
        {
            var logsDir = Path.GetDirectoryName(AppPaths.AppLogPath) ?? AppPaths.Root;
            var path = Path.Combine(logsDir, LastSessionFileName);
            if (!File.Exists(path)) return;
            var existing = File.ReadAllText(path, Encoding.UTF8);
            var updated = existing.Replace("clean_exit=false", "clean_exit=true");
            updated += $"ended_utc={DateTimeOffset.UtcNow:O}\n";
            File.WriteAllText(path, updated, Encoding.UTF8);
        }
        catch
        {
        }
    }

    private static void DetectPriorAbnormalExit()
    {
        try
        {
            AppPaths.EnsureDirectories();
            var logsDir = Path.GetDirectoryName(AppPaths.AppLogPath) ?? AppPaths.Root;
            var path = Path.Combine(logsDir, LastSessionFileName);
            if (!File.Exists(path)) return;
            var prior = File.ReadAllText(path, Encoding.UTF8);
            if (prior.Contains("clean_exit=true", StringComparison.Ordinal))
            {
                return; // healthy previous session
            }
            var fileName = $"{PriorDeathFilePrefix}{DateTimeOffset.UtcNow:yyyyMMdd-HHmmss}.txt";
            var fullPath = Path.Combine(logsDir, fileName);
            var sb = new StringBuilder();
            sb.Append("FileID prior session ended without flipping clean_exit marker.\n");
            sb.Append("This usually indicates a NATIVE fast-fail (RaiseFailFastException)\n");
            sb.Append("from the WinUI composition or COM layer — which bypasses every\n");
            sb.Append("managed exception handler. Check the last entries of app.log for\n");
            sb.Append("the last action before death.\n\n");
            sb.Append("--- previous last-session.txt ---\n");
            sb.Append(prior);
            sb.Append('\n');
            File.WriteAllText(fullPath, sb.ToString(), Encoding.UTF8);
            Write("WARN ",
                "[STARTUP] previous session ended without handler (likely native fast-fail) — see " + fullPath);
        }
        catch
        {
        }
    }
}
