using System.Globalization;
using FileID.IpcSchema;

namespace FileID.ViewModels;

/// <summary>Search result and Chat source wording (macOS MomentResultRow and
/// ChatWorkbench sourceMeta): file name, time or page, and what kind of
/// evidence matched.</summary>
internal static class HitPresentation
{
    private static readonly char[] Separators = { '/', '\\' };

    public static string FileName(string path)
    {
        var cut = path.LastIndexOfAny(Separators);
        return cut < 0 ? path : path[(cut + 1)..];
    }

    public static string Folder(string path)
    {
        var cut = path.LastIndexOfAny(Separators);
        return cut <= 0 ? "" : path[..cut];
    }

    public static string Clock(double seconds)
    {
        var total = double.IsFinite(seconds) && seconds > 0 ? (long)Math.Round(seconds, MidpointRounding.AwayFromZero) : 0;
        return total >= 3600
            ? string.Create(CultureInfo.InvariantCulture, $"{total / 3600}:{total / 60 % 60:00}:{total % 60:00}")
            : string.Create(CultureInfo.InvariantCulture, $"{total / 60}:{total % 60:00}");
    }

    public static string KindLabel(string kind) => kind switch
    {
        "sampledFrame" => "Video frame, not checked",
        "chapter" => "Chapter",
        "passage" => "Speech or text",
        "person" => "Person",
        "documentText" => "Text in document",
        "ocrText" => "Text in image",
        "handwrittenText" => "Handwritten text",
        _ when kind.Equals("pdf", StringComparison.OrdinalIgnoreCase) => "PDF",
        _ when kind.Length == 0 => "File",
        _ => char.ToUpperInvariant(kind[0]) + kind[1..].ToLowerInvariant(),
    };

    public static bool IsSampled(CatalogHit hit) => hit.Kind == "sampledFrame";

    /// <summary>"m:ss · Page n · Chapter" under a Search &amp; Moments result.</summary>
    public static string ResultMeta(CatalogHit hit)
    {
        var parts = TimeAndPage(hit);
        parts.Add(KindLabel(hit.Kind));
        return string.Join(" · ", parts);
    }

    /// <summary>"m:ss · Page n", plus a warning for unchecked video frames, under a Chat source.</summary>
    public static string SourceMeta(CatalogHit hit)
    {
        var parts = TimeAndPage(hit);
        if (IsSampled(hit)) parts.Add(KindLabel(hit.Kind));
        return string.Join(" · ", parts);
    }

    public static string ResultSpoken(CatalogHit hit) => $"{FileName(hit.Path)}, {ResultMeta(hit)}";

    public static string SourceSpoken(int index, CatalogHit hit)
    {
        var meta = SourceMeta(hit);
        return $"Source {index}: {FileName(hit.Path)}" + (meta.Length == 0 ? "" : ", " + meta);
    }

    public static string ResultsMeta(int count, bool searched) =>
        count == 0
            ? searched ? "No results" : "Type and press Enter"
            : string.Create(CultureInfo.CurrentCulture, $"{count:N0} result{(count == 1 ? "" : "s")}");

    private static List<string> TimeAndPage(CatalogHit hit)
    {
        var parts = new List<string>(3);
        if (hit.StartSeconds is { } seconds) parts.Add(Clock(seconds));
        if (hit.Page is { } page) parts.Add(string.Create(CultureInfo.InvariantCulture, $"Page {page}"));
        return parts;
    }
}
