// Opens files in the default handler only for an allowlist of media extensions, so a library row whose file was swapped for a `.exe`/`.lnk`/`.bat` after scanning is never executed. Everything else falls back to Reveal in Explorer, which cannot execute.

using System;
using System.Diagnostics;
using System.IO;

namespace FileID.Services;

internal static class SafeOpen
{
    // Never add .exe / .lnk / .bat / .cmd / .ps1 / .vbs / .msi / .scr / .com / .pif / .reg / .url / .website / .hta.
    private static readonly HashSet<string> AllowedExts = new(StringComparer.OrdinalIgnoreCase)
    {
        ".jpg", ".jpeg", ".png", ".gif", ".bmp", ".webp", ".tiff", ".tif",
        ".heic", ".heif", ".raw", ".cr2", ".nef", ".arw", ".dng", ".orf",
        ".rw2", ".pef", ".srw", ".raf", ".x3f",
        ".mp4", ".mov", ".m4v", ".avi", ".mkv", ".wmv", ".flv", ".webm",
        ".mpg", ".mpeg", ".3gp", ".3g2", ".mts", ".m2ts",
        ".mp3", ".m4a", ".aac", ".flac", ".wav", ".ogg", ".opus", ".wma",
        ".aiff", ".aif",
        ".pdf", ".txt", ".md", ".rtf", ".doc", ".docx", ".odt",
        ".xls", ".xlsx", ".ods", ".csv", ".tsv",
        ".ppt", ".pptx", ".odp", ".key",
        ".epub", ".mobi", ".azw", ".azw3",
        ".html", ".htm", ".xml", ".json", ".yaml", ".yml",
    };

    /// <summary>Opens the file via ShellExecute only if its extension is allowlisted; returns false otherwise so callers fall back to Reveal.</summary>
    public static bool TryOpenFile(string path)
    {
        if (string.IsNullOrWhiteSpace(path)) return false;
        if (!SafeFileExists(path)) return false;
        var ext = Path.GetExtension(path);
        if (!IsAllowedFileExtension(path))
        {
            DebugLog.Warn($"SafeOpen blocked '{ext}' — falling back to reveal.");
            return false;
        }
        try
        {
            Process.Start(new ProcessStartInfo
            {
                FileName = path,
                UseShellExecute = true,
            });
            return true;
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"SafeOpen failed for '{PathRedactor.Redact(path)}': {ex.Message}");
            return false;
        }
    }

    internal static bool IsAllowedFileExtension(string? path) =>
        !string.IsNullOrWhiteSpace(path) && AllowedExts.Contains(Path.GetExtension(path));

    /// <summary>Opens a folder in Explorer. UseShellExecute=false with ArgumentList keeps shell metacharacters in the path inert.</summary>
    public static void OpenFolder(string path)
    {
        if (string.IsNullOrWhiteSpace(path) || !Directory.Exists(path)) return;
        try
        {
            var psi = new ProcessStartInfo
            {
                FileName = "explorer.exe",
                UseShellExecute = false,
            };
            psi.ArgumentList.Add(path);
            Process.Start(psi);
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"OpenFolder failed for '{PathRedactor.Redact(path)}': {ex.Message}");
        }
    }

    /// <summary>Selects the file in its parent folder in Explorer; works for any extension and cannot execute the target.</summary>
    public static void Reveal(string path)
    {
        if (string.IsNullOrWhiteSpace(path)) return;
        try
        {
            var psi = new ProcessStartInfo
            {
                FileName = "explorer.exe",
                UseShellExecute = false,
            };
            // explorer.exe needs "/select," glued to the path as one argument; ArgumentList would split them.
            psi.ArgumentList.Add("/select," + path);
            Process.Start(psi);
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"Reveal failed for '{PathRedactor.Redact(path)}': {ex.Message}");
        }
    }

    /// <summary>File.Exists that returns false instead of throwing on invalid characters or denied access.</summary>
    private static bool SafeFileExists(string path)
    {
        try { return File.Exists(path); }
        catch (IOException) { return false; }
        catch (UnauthorizedAccessException) { return false; }
    }
}
