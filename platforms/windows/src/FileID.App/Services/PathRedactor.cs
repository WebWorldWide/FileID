namespace FileID.Services;

internal static class PathRedactor
{
    public static string Redact(string? path)
    {
        if (string.IsNullOrEmpty(path))
        {
            return "<null>";
        }

        var parts = path.Replace('\\', '/')
            .Split('/', StringSplitOptions.RemoveEmptyEntries);
        if (parts.Length == 0)
        {
            return "…";
        }

        var homeMarker = Array.FindIndex(parts, part =>
            part.Equals("Users", StringComparison.OrdinalIgnoreCase)
            || part.Equals("home", StringComparison.OrdinalIgnoreCase));
        var isHomeRoot = homeMarker == 0
            || (homeMarker == 1 && parts[0].EndsWith(':'));
        if (isHomeRoot && homeMarker + 2 == parts.Length)
        {
            return "…";
        }
        if (homeMarker >= 0 && homeMarker + 3 == parts.Length)
        {
            return "…/" + parts[^1];
        }

        var keep = parts.Length >= 2 ? parts[^2..] : parts;
        return "…/" + string.Join('/', keep);
    }
}
