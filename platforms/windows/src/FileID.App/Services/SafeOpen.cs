// SEC-9: centralized "Open file in default handler" helper with an
// extension allowlist. The library DB stores paths; if a row's underlying
// file is swapped on disk for a `.exe` / `.lnk` / `.bat` between scan + open,
// a naive ShellExecute would execute it. This helper restricts "Open" to
// known media extensions; everything else falls back to "Reveal in Explorer"
// (which can't execute).
//
// "Reveal" stays universal — Explorer just selects the file in its parent
// folder. That's a UI surface, not an execution path.

using System;
using System.Diagnostics;
using System.IO;

namespace FileID.Services;

internal static class SafeOpen
{
    // Conservative allowlist. Extend only if a user reports a missing
    // legitimate format. NEVER include .exe / .lnk / .bat / .cmd / .ps1 /
    // .vbs / .msi / .scr / .com / .pif / .reg / .url / .website / .hta.
    private static readonly HashSet<string> AllowedExts = new(StringComparer.OrdinalIgnoreCase)
    {
        // Images
        ".jpg", ".jpeg", ".png", ".gif", ".bmp", ".webp", ".tiff", ".tif",
        ".heic", ".heif", ".raw", ".cr2", ".nef", ".arw", ".dng", ".orf",
        ".rw2", ".pef", ".srw", ".raf", ".x3f",
        // Video
        ".mp4", ".mov", ".m4v", ".avi", ".mkv", ".wmv", ".flv", ".webm",
        ".mpg", ".mpeg", ".3gp", ".3g2", ".mts", ".m2ts",
        // Audio
        ".mp3", ".m4a", ".aac", ".flac", ".wav", ".ogg", ".opus", ".wma",
        ".aiff", ".aif",
        // Documents
        ".pdf", ".txt", ".md", ".rtf", ".doc", ".docx", ".odt",
        ".xls", ".xlsx", ".ods", ".csv", ".tsv",
        ".ppt", ".pptx", ".odp", ".key",
        ".epub", ".mobi", ".azw", ".azw3",
        // Web / data (read-only viewers)
        ".html", ".htm", ".xml", ".json", ".yaml", ".yml",
    };

    /// <summary>
    /// Open the given file via ShellExecute IF its extension is in the
    /// allowlist. Returns false (without launching) for anything else.
    /// On false, callers should fall back to Reveal.
    /// </summary>
    public static bool TryOpenFile(string path)
    {
        if (string.IsNullOrWhiteSpace(path)) return false;
        if (!SafeFileExists(path)) return false;
        var ext = Path.GetExtension(path);
        if (string.IsNullOrEmpty(ext) || !AllowedExts.Contains(ext))
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

    /// <summary>
    /// Open a folder (Explorer). Folder paths are inherently safe — no
    /// execution surface — so no allowlist applies. Uses
    /// UseShellExecute=false + ArgumentList so quotes/backslashes/
    /// shell metachars in the path can't be interpreted by cmd.
    /// </summary>
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

    /// <summary>
    /// Reveal-in-Explorer. Selects the file in its parent folder. Universal:
    /// works for any file extension, can't execute the target. Uses
    /// UseShellExecute=false + ArgumentList for the same reason as
    /// OpenFolder above.
    /// </summary>
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
            // explorer.exe expects "/select," and the path as ONE argument
            // (no space between comma and path). ArgumentList quotes each
            // entry independently, so we still need to glue them ourselves
            // for the /select, syntax.
            psi.ArgumentList.Add("/select," + path);
            Process.Start(psi);
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"Reveal failed for '{PathRedactor.Redact(path)}': {ex.Message}");
        }
    }

    /// <summary>
    /// File.Exists wrapper that swallows IOException / UnauthorizedAccessException.
    /// an unguarded File.Exists can throw on paths with invalid
    /// characters or denied access — defensive wrap returns false in
    /// those cases so callers don't crash.
    /// </summary>
    private static bool SafeFileExists(string path)
    {
        try { return File.Exists(path); }
        catch (IOException) { return false; }
        catch (UnauthorizedAccessException) { return false; }
    }
}
