using System.Text;
using System.Text.Json;
using FileID.IpcSchema;

namespace FileID.ViewModels;

/// <summary>One library location that Library, Search &amp; Moments and Chat are scoped to (macOS
/// LibraryScope). A file belongs to it when its path is the location's path or lies under it, never a
/// sibling that shares the prefix (/Camera never holds /Camera2), and never inside a deeper location:
/// the most specific location owns a file, as in OfflineRoots. Paths compare ordinally, like the
/// engines' rootID filter.</summary>
internal sealed class LibraryScope
{
    private readonly string[] _bounds;
    private readonly string[][] _deeper;

    private LibraryScope(LibraryRoot root, string[] bounds, string[][] deeper)
    {
        Root = root;
        _bounds = bounds;
        _deeper = deeper;
        Param = Json(bounds, deeper);
    }

    public LibraryRoot Root { get; }

    /// <summary>JSON bound to <see cref="Sql"/>'s parameter.</summary>
    public string Param { get; }

    public static LibraryScope? For(long? rootId, IReadOnlyList<LibraryRoot> roots)
    {
        if (rootId is not { } id) return null;
        var root = roots.FirstOrDefault(r => r.Id == id);
        if (root is null) return null;
        var own = SubtreeBounds(root.Path);
        var deeper = roots
            .Where(r => r.Id != id)
            .Select(r => SubtreeBounds(r.Path))
            .Where(b => !string.Equals(b[0], own[0], StringComparison.Ordinal) && Within(own, b[0]))
            .DistinctBy(b => b[0], StringComparer.Ordinal)
            .OrderBy(b => b[0], StringComparer.Ordinal)
            .ToArray();
        return new LibraryScope(root, own, deeper);
    }

    /// <summary>The stored choice once the location list is known: kept while the list is still
    /// empty (not loaded yet), dropped when its location is gone.</summary>
    public static long? Reconciled(long? stored, IReadOnlyList<LibraryRoot> roots)
    {
        if (stored is not { } id || roots.Count == 0) return stored;
        return roots.Any(r => r.Id == id) ? id : null;
    }

    /// <summary>Title-bar label: the location's name, "All locations", or the folder being worked
    /// on before anything is cataloged.</summary>
    public static string Title(LibraryScope? scope, IReadOnlyList<LibraryRoot> roots, string? folderName)
    {
        if (scope is not null) return scope.Root.Display;
        if (roots.Count == 0) return string.IsNullOrEmpty(folderName) ? "Choose a folder" : folderName;
        return "All locations";
    }

    public static string FileCountText(long count) => count == 1 ? "1 file" : $"{count:N0} files";

    public static string LocationCountText(int count) => count == 1 ? "1 location" : $"{count:N0} locations";

    /// <summary>Spoken name of one location row in the switcher.</summary>
    public static string Spoken(LibraryRoot root) =>
        $"{root.Display}, {FileCountText(root.FileCount)}" + (root.Online ? "" : ", offline");

    public bool Contains(string? path)
    {
        if (string.IsNullOrEmpty(path) || !Within(_bounds, path)) return false;
        foreach (var deeper in _deeper)
        {
            if (Within(deeper, path)) return false;
        }
        return true;
    }

    /// <summary>SQL that is true when <paramref name="column"/> lies in the scope bound to
    /// <paramref name="parameter"/>, or when that value is NULL. Binds the parameter once.</summary>
    public static string Sql(string column, string parameter)
    {
        string Inside(string bounds) =>
            $"({column}=json_extract({bounds},'$[0]') OR ({column}>=json_extract({bounds},'$[1]') AND {column}<json_extract({bounds},'$[2]')))";
        return $"(SELECT s.v IS NULL OR ({Inside("s.v")} AND NOT EXISTS (SELECT 1 FROM json_each(s.v,'$[3]') d WHERE {Inside("d.value")})) FROM (SELECT {parameter} AS v) s)";
    }

    /// <summary>Half-open range of every path under <paramref name="root"/>: <c>root/</c> up to
    /// <c>root0</c> (<c>]</c> follows <c>\</c>). Matches the engines' listRoots counts.</summary>
    internal static string[] SubtreeBounds(string root)
    {
        var windows = root.Contains('\\') && !root.Contains('/');
        var separator = windows ? '\\' : '/';
        var trimmed = root.TrimEnd(separator);
        return new[] { trimmed, trimmed + separator, trimmed + (windows ? ']' : '0') };
    }

    private static bool Within(string[] bounds, string path) =>
        string.Equals(path, bounds[0], StringComparison.Ordinal) || path.StartsWith(bounds[1], StringComparison.Ordinal);

    private static string Json(string[] bounds, string[][] deeper)
    {
        using var stream = new MemoryStream();
        using (var writer = new Utf8JsonWriter(stream))
        {
            writer.WriteStartArray();
            foreach (var value in bounds) writer.WriteStringValue(value);
            writer.WriteStartArray();
            foreach (var nested in deeper)
            {
                writer.WriteStartArray();
                foreach (var value in nested) writer.WriteStringValue(value);
                writer.WriteEndArray();
            }
            writer.WriteEndArray();
            writer.WriteEndArray();
        }
        return Encoding.UTF8.GetString(stream.ToArray());
    }
}
