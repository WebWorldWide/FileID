using FileID.IpcSchema;

namespace FileID.ViewModels;

/// <summary>Which cataloged paths sit under a location that is offline right now (macOS
/// OfflineRoots). The most specific location wins, so an online folder inside an offline
/// drive's path stays online. Paths compare case-insensitively with either separator.</summary>
internal sealed class OfflineRoots : IEquatable<OfflineRoots>
{
    private readonly (string Path, bool Online)[] _entries;

    public static OfflineRoots None { get; } = new(Array.Empty<(string, bool)>());

    private OfflineRoots((string Path, bool Online)[] entries) => _entries = entries;

    public static OfflineRoots From(IEnumerable<LibraryRoot> roots)
    {
        var list = roots.ToArray();
        if (!list.Any(r => !r.Online)) return None;
        var entries = list
            .Select(r => (Path: Normalize(r.Path), r.Online))
            .Where(e => e.Path.Length > 0)
            .OrderByDescending(e => e.Path.Length)
            .ThenBy(e => e.Path, StringComparer.OrdinalIgnoreCase)
            .ToArray();
        return entries.Length == 0 ? None : new OfflineRoots(entries);
    }

    public bool IsEmpty => _entries.Length == 0;

    public bool IsOffline(string? path)
    {
        if (_entries.Length == 0 || string.IsNullOrEmpty(path)) return false;
        var normalized = path.Replace('\\', '/');
        foreach (var (root, online) in _entries)
        {
            if (Contains(root, normalized)) return !online;
        }
        return false;
    }

    private static string Normalize(string path)
    {
        var normalized = path.Replace('\\', '/');
        var end = normalized.Length;
        while (end > 1 && normalized[end - 1] == '/') end--;
        return normalized[..end];
    }

    private static bool Contains(string root, string path)
    {
        if (path.Length < root.Length || !path.StartsWith(root, StringComparison.OrdinalIgnoreCase)) return false;
        return path.Length == root.Length || root == "/" || path[root.Length] == '/';
    }

    public bool Equals(OfflineRoots? other)
    {
        if (other is null) return false;
        if (ReferenceEquals(this, other)) return true;
        if (_entries.Length != other._entries.Length) return false;
        for (var i = 0; i < _entries.Length; i++)
        {
            var same = _entries[i].Online == other._entries[i].Online
                && string.Equals(_entries[i].Path, other._entries[i].Path, StringComparison.OrdinalIgnoreCase);
            if (!same) return false;
        }
        return true;
    }

    public override bool Equals(object? obj) => Equals(obj as OfflineRoots);

    public override int GetHashCode()
    {
        var hash = new HashCode();
        foreach (var (path, online) in _entries)
        {
            hash.Add(path, StringComparer.OrdinalIgnoreCase);
            hash.Add(online);
        }
        return hash.ToHashCode();
    }
}
