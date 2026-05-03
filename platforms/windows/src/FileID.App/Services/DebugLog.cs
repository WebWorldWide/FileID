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

    public static void Info(string message)  => Write("INFO ", message);
    public static void Warn(string message)  => Write("WARN ", message);
    public static void Error(string message) => Write("ERROR", message);
    public static void Debug(string message) => Write("DEBUG", message);

    private static void Write(string level, string message)
    {
        try
        {
            AppPaths.EnsureDirectories();
            var path = AppPaths.AppLogPath;
            // Bound disk usage by truncating when oversized. Crude but
            // sufficient for a desktop app log. Phase 11 polish replaces
            // with date-rolling files via Microsoft.Extensions.Logging.
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
}
