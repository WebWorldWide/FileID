// DebugLog — local-only structured logging for the C# app side.
//
// Writes UTF-8 lines to %LOCALAPPDATA%\FileID\logs\app.log. Survives across
// launches (truncates only when > 10 MB to bound disk usage). NEVER
// transmits — this is enforced by the CI binary scan privacy gate.
//
// PRIVACY: every log call site that includes a user file path MUST wrap
// the path in PathRedactor.Redact first. Reviewed every PR.

using System.IO;
using System.Text;
using System.Threading;

namespace FileID.Services;

internal static class DebugLog
{
    private const long MaxLogBytes = 10 * 1024 * 1024;
    private static readonly object s_writeLock = new();
    // NOTE: writes are intentionally SYNCHRONOUS. An async/batched sink was tried
    // (audit P2) but the fix re-audit caught that it loses the last <200 ms of
    // lines on a NATIVE fast-fail — exactly the [APPLY:N]/[ENGINE-SUB] tail this
    // log exists to capture (CLAUDE.md marks it load-bearing). Any future
    // off-thread sink MUST preserve per-line durability (e.g. a persistent
    // flushed StreamWriter), verified on hardware. Do not re-batch naively.

    public static void Info(string message) => Write("INFO ", message);
    public static void Warn(string message) => Write("WARN ", message);
    public static void Error(string message) => Write("ERROR", message);
    public static void Debug(string message) => Write("DEBUG", message);

    /// <summary>Handler-wrap helper. UI click handlers that call IPC or
    /// touch the file system should route through this so a thrown
    /// exception logs + dumps + stays caught instead of escaping into the
    /// dispatcher loop. Caller supplies a short label (typically
    /// `nameof(OnFooClicked)`) for the log line.</summary>
    public static void SafeRun(string label, Action body)
    {
        try { body(); }
        catch (Exception ex)
        {
            Error($"{label} threw: {ex}");
            try { WriteCrashDump(label, ex, terminating: false); } catch { /* swallow */ }
        }
    }

    /// <summary>Async sibling of <see cref="SafeRun"/>. Use for async
    /// <c>OnXxxClicked</c> handlers so a thrown exception inside the
    /// async body lands here instead of becoming an
    /// UnobservedTaskException at GC time.</summary>
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
            AppPaths.EnsureDirectories();
            var path = AppPaths.AppLogPath;
            // Bound disk usage by truncating when oversized.
            if (File.Exists(path))
            {
                var info = new FileInfo(path);
                if (info.Length > MaxLogBytes)
                {
                    File.WriteAllText(path, "[log truncated at startup; oversized]\n");
                }
            }
            var stamp = DateTimeOffset.UtcNow.ToString("yyyy-MM-ddTHH:mm:ss.fffZ");
            var line = $"{stamp} {level} [tid {Environment.CurrentManagedThreadId}] {message}\n";
            // Synchronous + flushed: a native fast-fail must not lose the last
            // forensic line (see the field note above).
            lock (s_writeLock)
            {
                File.AppendAllText(path, line, Encoding.UTF8);
            }
        }
        catch
        {
            // Logging must never throw upstream. Disk-full / permission /
            // antivirus quarantine all silently swallowed.
        }
    }

    /// <summary>
    /// Write a dedicated crash-{timestamp}-{pid}.txt file with the
    /// exception, the source handler, and the last 50 lines of app.log
    /// for context. Designed for cases where the process is about to die
    /// (or already is) and we want a forensic artifact survivors can read
    /// even if app.log got truncated by the next launch. Best-effort —
    /// silently swallows IO errors. Returns the path for inclusion in
    /// other logs.
    /// </summary>
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
                    var lines = File.ReadAllLines(AppPaths.AppLogPath, Encoding.UTF8);
                    var start = Math.Max(0, lines.Length - 50);
                    for (int i = start; i < lines.Length; i++)
                    {
                        sb.Append(lines[i]);
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

    // Last-session breadcrumb. The managed crash sinks (Application.
    // UnhandledException / AppDomain / TaskScheduler) catch every managed
    // crash but cannot intercept a native fast-fail. WinUI 3's composition
    // layer calls RaiseFailFastException on cross-thread DispatcherObject
    // misuse, which terminates the process without unwinding the CLR — no
    // crash-*.txt is produced.
    //
    // Breadcrumb pattern: at startup, BeginSession writes last-session.txt
    // with start time + pid + a clean_exit=false marker. MarkCleanExit
    // flips the marker to true at graceful shutdown. On the NEXT launch,
    // DetectPriorAbnormalExit checks the previous file — if clean_exit
    // is missing, the previous session died without a managed handler
    // firing (most likely native fast-fail), and we emit a forensic
    // artifact session-died-without-handler-{ts}.txt.
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
            // Breadcrumb is best-effort.
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
            // best-effort
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
            // Previous session ended without flipping the marker. Surface
            // a forensic artifact so we know to look further.
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
            // best-effort
        }
    }
}
