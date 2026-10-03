using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text;
using System.Text.Json;

namespace FileID.ViewModels;

internal static class EngineLifecyclePolicy
{
    internal static bool IsSafeToFinalizeApplicationClose(
        bool terminalStopActive,
        bool startInFlight,
        bool processAlive)
        => terminalStopActive && !startInFlight && !processAlive;
}

internal static class PersistedUndoReader
{
    internal readonly record struct PersistedRestructureUndo(string LibraryRoot, DateTime UpdatedUtc);

    internal readonly record struct PersistedShortcutUndo(string LibraryRoot, string Token);

    private sealed record UndoJournalCandidate(string LibraryRoot, DateTime UpdatedUtc);
    private sealed record ShortcutUndoCandidate(string LibraryRoot, string Token, DateTime UpdatedUtc);

    private const int MaxPersistedUndoFiles = 1024;
    private const int MaxPersistedUndoLineBytes = 64 * 1024;
    private const long MaxPersistedRestructureJournalBytes = 64 * 1024 * 1024;
    private const long MaxPersistedShortcutManifestBytes = 2 * 1024 * 1024;

    private static readonly UTF8Encoding s_strictUtf8 = new(false, true);

    internal static string? ReadPersistedRestructureUndoRoot(string path)
        => ReadPersistedRestructureUndo(path)?.LibraryRoot;

    internal static PersistedRestructureUndo? ReadPersistedRestructureUndo(string path)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(path);
        var current = TryReadRestructureUndoJournal(path);
        if (current is not null)
        {
            return new(current.LibraryRoot, current.UpdatedUtc);
        }

        var directory = Path.GetDirectoryName(Path.GetFullPath(path));
        if (directory is null || !Directory.Exists(directory)) return null;
        var priors = EnumerateBoundedFiles(
            directory,
            ".restructure_undo.ndjson.prior-*",
            MaxPersistedUndoFiles);
        if (priors is null) return null;

        var valid = priors
            .Where(IsOwnedPriorJournalName)
            .Select(TryReadRestructureUndoJournal)
            .Where(candidate => candidate is not null)
            .Cast<UndoJournalCandidate>()
            .ToArray();
        if (valid.Length != 1) return null;
        return new(valid[0].LibraryRoot, valid[0].UpdatedUtc);
    }

    internal static bool IsRegularPersistedUndoFileAttributes(FileAttributes attributes)
        => (attributes & (FileAttributes.Directory | FileAttributes.ReparsePoint | FileAttributes.Device)) == 0;

    internal static PersistedShortcutUndo? ReadPersistedShortcutUndo(
        string directory,
        string? excludedToken = null)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(directory);
        if (!TryGetRegularDirectory(directory, out var fullDirectory)) return null;
        var files = EnumerateBoundedFiles(fullDirectory, "*.ndjson", MaxPersistedUndoFiles);
        if (files is null) return null;

        Guid? excluded = Guid.TryParseExact(excludedToken, "D", out var excludedGuid)
            ? excludedGuid
            : null;
        var valid = files
            .Where(path => IsRegularFile(path))
            .Select(TryReadShortcutManifest)
            .Where(candidate => candidate is not null &&
                (!excluded.HasValue || !Guid.TryParseExact(candidate.Token, "D", out var token) || token != excluded.Value))
            .Cast<ShortcutUndoCandidate>()
            .ToArray();
        if (valid.Length == 0) return null;

        var latest = valid.Max(candidate => candidate.UpdatedUtc);
        var newest = valid.Where(candidate => candidate.UpdatedUtc == latest).ToArray();
        return newest.Length == 1
            ? new(newest[0].LibraryRoot, newest[0].Token)
            : null;
    }

    private static UndoJournalCandidate? TryReadRestructureUndoJournal(string path)
    {
        if (!IsRegularFile(path)) return null;
        if (!TryReadBoundedLines(path, MaxPersistedRestructureJournalBytes, out var lines)
            || lines.Count < 2)
        {
            return null;
        }

        try
        {
            using var header = JsonDocument.Parse(lines[0]);
            if (!TryGetInt32(header.RootElement, "version", out var version)
                || version is not (2 or 3)
                || !TryGetCanonicalRoot(header.RootElement, out var libraryRoot))
            {
                return null;
            }

            for (var index = 1; index < lines.Count; index++)
            {
                using var item = JsonDocument.Parse(lines[index]);
                var entry = item.RootElement;
                if (!TryGetPositiveInt64(entry, "file_id", out _)
                    || !TryGetString(entry, "from", out var from)
                    || !TryGetString(entry, "to", out var to)
                    || !IsWithinLibraryRoot(libraryRoot, from)
                    || !IsWithinLibraryRoot(libraryRoot, to)
                    || (version == 3 && !HasFileIdentity(entry, "source_identity")))
                {
                    return null;
                }
            }

            return new(libraryRoot, File.GetLastWriteTimeUtc(path));
        }
        catch (JsonException)
        {
            return null;
        }
        catch (IOException)
        {
            return null;
        }
        catch (UnauthorizedAccessException)
        {
            return null;
        }
        catch (ArgumentException)
        {
            return null;
        }
    }

    private static ShortcutUndoCandidate? TryReadShortcutManifest(string path)
    {
        if (!TryReadBoundedLines(path, MaxPersistedShortcutManifestBytes, out var lines)
            || lines.Count == 0)
        {
            return null;
        }

        try
        {
            using var header = JsonDocument.Parse(lines[0]);
            var root = header.RootElement;
            if (!TryGetInt32(root, "version", out var version)
                || version is not (2 or 3)
                || !TryGetCanonicalRoot(root, out var libraryRoot)
                || !TryGetCanonicalGuid(root, "token", out var token)
                || !string.Equals(
                    Path.GetFileNameWithoutExtension(path),
                    token,
                    StringComparison.OrdinalIgnoreCase))
            {
                return null;
            }

            string? stagingDirectory = null;
            if (version == 3)
            {
                if (!TryGetString(root, "staging_dir", out stagingDirectory)
                    || !IsExpectedStagingDirectory(libraryRoot, token, stagingDirectory)
                    || !HasFileIdentity(root, "staging_dir_identity"))
                {
                    return null;
                }
            }

            var validEntries = 0;
            for (var index = 1; index < lines.Count; index++)
            {
                using var item = JsonDocument.Parse(lines[index]);
                var entry = item.RootElement;
                if (entry.TryGetProperty("operation_id", out _))
                {
                    if (version != 1 || stagingDirectory is null
                        || !IsValidShortcutIntent(entry, token, libraryRoot, stagingDirectory))
                    {
                        return null;
                    }
                    continue;
                }

                if (!TryGetPositiveInt64(entry, "file_id", out _)
                    || !TryGetString(entry, "source", out var source)
                    || !TryGetString(entry, "link", out var link)
                    || !IsWithinLibraryRoot(libraryRoot, source)
                    || !IsWithinLibraryRoot(libraryRoot, link)
                    || !HasFileIdentity(entry, "source_identity")
                    || !HasFileIdentity(entry, "link_identity"))
                {
                    return null;
                }

                if (version == 3
                    && (!TryGetString(entry, "staging_link", out var stagingLink)
                        || !IsCanonicalStagingLink(stagingDirectory!, stagingLink)))
                {
                    return null;
                }
                validEntries++;
            }

            if (validEntries == 0 && stagingDirectory is null)
            {
                return null;
            }

            if (validEntries == 0 && !HasValidShortcutIntent(libraryRoot, token, stagingDirectory!))
            {
                return null;
            }

            return new(libraryRoot, token, File.GetLastWriteTimeUtc(path));
        }
        catch (JsonException)
        {
            return null;
        }
        catch (IOException)
        {
            return null;
        }
        catch (UnauthorizedAccessException)
        {
            return null;
        }
        catch (ArgumentException)
        {
            return null;
        }
    }

    private static bool HasValidShortcutIntent(string libraryRoot, string token, string stagingDirectory)
    {
        if (!TryGetRegularDirectory(stagingDirectory, out var fullStagingDirectory)) return false;
        var files = EnumerateBoundedFiles(fullStagingDirectory, "*.intent.json", MaxPersistedUndoFiles);
        if (files is null) return false;
        foreach (var path in files)
        {
            if (!IsRegularFile(path)) continue;
            if (!TryReadBoundedLines(path, MaxPersistedShortcutManifestBytes, out var lines)
                || lines.Count != 1)
            {
                continue;
            }
            try
            {
                using var intent = JsonDocument.Parse(lines[0]);
                if (IsValidShortcutIntent(intent.RootElement, token, libraryRoot, stagingDirectory))
                {
                    return true;
                }
            }
            catch (JsonException)
            {
            }
        }
        return false;
    }

    private static bool IsValidShortcutIntent(
        JsonElement intent,
        string token,
        string libraryRoot,
        string stagingDirectory)
        => TryGetInt32(intent, "version", out var version)
            && version == 1
            && TryGetString(intent, "token", out var intentToken)
            && string.Equals(intentToken, token, StringComparison.OrdinalIgnoreCase)
            && TryGetCanonicalGuid(intent, "operation_id", out var operationId)
            && TryGetPositiveInt64(intent, "file_id", out _)
            && TryGetString(intent, "source", out var source)
            && IsWithinLibraryRoot(libraryRoot, source)
            && TryGetString(intent, "link", out var link)
            && IsWithinLibraryRoot(libraryRoot, link)
            && TryGetString(intent, "staging_link", out var stagingLink)
            && IsCanonicalStagingLink(stagingDirectory, stagingLink)
            && string.Equals(
                Path.GetFileName(stagingLink),
                operationId + ".link",
                StringComparison.OrdinalIgnoreCase)
            && HasFileIdentity(intent, "source_identity");

    private static bool IsExpectedStagingDirectory(string libraryRoot, string token, string stagingDirectory)
    {
        if (!TryCanonicalPath(stagingDirectory, out var fullStagingDirectory)) return false;
        var expected = Path.GetFullPath(Path.Combine(
            libraryRoot,
            ".fileid-restructure-shortcut-staging",
            token));
        return string.Equals(fullStagingDirectory, expected, StringComparison.OrdinalIgnoreCase);
    }

    private static bool IsCanonicalStagingLink(string stagingDirectory, string stagingLink)
    {
        if (!TryCanonicalPath(stagingLink, out var fullStagingLink)) return false;
        var expectedDirectory = Path.GetFullPath(stagingDirectory);
        if (!string.Equals(
            Path.GetDirectoryName(fullStagingLink),
            expectedDirectory,
            StringComparison.OrdinalIgnoreCase))
        {
            return false;
        }
        var filename = Path.GetFileName(fullStagingLink);
        return filename.EndsWith(".link", StringComparison.OrdinalIgnoreCase)
            && Guid.TryParseExact(filename[..^5], "D", out var guid)
            && string.Equals(filename[..^5], guid.ToString("D"), StringComparison.OrdinalIgnoreCase);
    }

    private static bool HasFileIdentity(JsonElement element, string propertyName)
        => element.TryGetProperty(propertyName, out var identity)
            && identity.ValueKind == JsonValueKind.Object
            && identity.TryGetProperty("volume", out var volume)
            && volume.TryGetUInt64(out _)
            && identity.TryGetProperty("file", out var file)
            && file.TryGetUInt64(out var fileId)
            && fileId > 0;

    private static bool TryGetCanonicalRoot(JsonElement element, out string root)
    {
        root = string.Empty;
        if (!TryGetString(element, "library_root", out var value)
            || !TryCanonicalPath(value, out var fullPath))
        {
            return false;
        }
        root = fullPath.TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);
        if (root.Length == 0) root = Path.GetPathRoot(fullPath) ?? fullPath;
        return root.Length > 0;
    }

    private static bool TryCanonicalPath(string value, out string fullPath)
    {
        fullPath = string.Empty;
        if (string.IsNullOrWhiteSpace(value) || !Path.IsPathFullyQualified(value)) return false;
        try
        {
            fullPath = Path.GetFullPath(value);
            return string.Equals(value, fullPath, StringComparison.OrdinalIgnoreCase);
        }
        catch (ArgumentException)
        {
            return false;
        }
        catch (NotSupportedException)
        {
            return false;
        }
        catch (PathTooLongException)
        {
            return false;
        }
    }

    private static bool IsWithinLibraryRoot(string root, string path)
    {
        try
        {
            var fullPath = Path.IsPathFullyQualified(path)
                ? Path.GetFullPath(path)
                : Path.GetFullPath(path, root);
            var prefix = Path.EndsInDirectorySeparator(root)
                ? root
                : root + Path.DirectorySeparatorChar;
            return fullPath.StartsWith(prefix, StringComparison.OrdinalIgnoreCase)
                && !string.Equals(fullPath, root, StringComparison.OrdinalIgnoreCase);
        }
        catch (ArgumentException)
        {
            return false;
        }
        catch (NotSupportedException)
        {
            return false;
        }
        catch (PathTooLongException)
        {
            return false;
        }
    }

    private static bool TryGetString(JsonElement element, string name, out string value)
    {
        value = string.Empty;
        if (!element.TryGetProperty(name, out var property)
            || property.ValueKind != JsonValueKind.String)
        {
            return false;
        }
        value = property.GetString() ?? string.Empty;
        return value.Length > 0 && value.Length <= 32 * 1024;
    }

    private static bool TryGetInt32(JsonElement element, string name, out int value)
    {
        value = 0;
        return element.TryGetProperty(name, out var property)
            && property.TryGetInt32(out value);
    }

    private static bool TryGetPositiveInt64(JsonElement element, string name, out long value)
    {
        value = 0;
        return element.TryGetProperty(name, out var property)
            && property.TryGetInt64(out value)
            && value > 0;
    }

    private static bool TryGetCanonicalGuid(JsonElement element, string name, out string value)
    {
        value = string.Empty;
        if (!TryGetString(element, name, out var raw)
            || !Guid.TryParseExact(raw, "D", out var guid)
            || !string.Equals(raw, guid.ToString("D"), StringComparison.OrdinalIgnoreCase))
        {
            return false;
        }
        value = guid.ToString("D");
        return true;
    }

    private static bool TryReadBoundedLines(string path, long maxBytes, out List<string> lines)
    {
        lines = [];
        try
        {
            var info = new FileInfo(path);
            if (info.Length is <= 0 || info.Length > maxBytes) return false;
            var bytes = File.ReadAllBytes(path);
            if (bytes.Length != info.Length || bytes[^1] != (byte)'\n') return false;
            var text = s_strictUtf8.GetString(bytes);
            foreach (var line in text.Split('\n'))
            {
                if (line.Length == 0) continue;
                var lineBytes = s_strictUtf8.GetByteCount(line);
                if (lineBytes > MaxPersistedUndoLineBytes) return false;
                lines.Add(line.EndsWith('\r') ? line[..^1] : line);
            }
            return lines.Count > 0;
        }
        catch (DecoderFallbackException)
        {
            return false;
        }
        catch (IOException)
        {
            return false;
        }
        catch (UnauthorizedAccessException)
        {
            return false;
        }
    }

    private static string[]? EnumerateBoundedFiles(string directory, string pattern, int maxFiles)
    {
        try
        {
            var files = Directory.EnumerateFiles(directory, pattern, SearchOption.TopDirectoryOnly)
                .Take(maxFiles + 1)
                .ToArray();
            return files.Length <= maxFiles ? files : null;
        }
        catch (IOException)
        {
            return null;
        }
        catch (UnauthorizedAccessException)
        {
            return null;
        }
        catch (ArgumentException)
        {
            return null;
        }
    }

    private static bool IsOwnedPriorJournalName(string path)
    {
        var name = Path.GetFileName(path);
        const string prefix = ".restructure_undo.ndjson.prior-";
        return name.StartsWith(prefix, StringComparison.OrdinalIgnoreCase)
            && Guid.TryParseExact(name[prefix.Length..], "D", out var id)
            && string.Equals(name[prefix.Length..], id.ToString("D"), StringComparison.OrdinalIgnoreCase);
    }

    private static bool IsRegularFile(string path)
    {
        try
        {
            return IsRegularPersistedUndoFileAttributes(File.GetAttributes(path));
        }
        catch (IOException)
        {
            return false;
        }
        catch (UnauthorizedAccessException)
        {
            return false;
        }
    }

    private static bool TryGetRegularDirectory(string path, out string fullDirectory)
    {
        fullDirectory = string.Empty;
        try
        {
            var fullPath = Path.GetFullPath(path);
            var attributes = File.GetAttributes(fullPath);
            if ((attributes & FileAttributes.Directory) == 0
                || (attributes & FileAttributes.ReparsePoint) != 0)
            {
                return false;
            }
            fullDirectory = fullPath;
            return true;
        }
        catch (IOException)
        {
            return false;
        }
        catch (UnauthorizedAccessException)
        {
            return false;
        }
        catch (ArgumentException)
        {
            return false;
        }
        catch (NotSupportedException)
        {
            return false;
        }
    }
}
