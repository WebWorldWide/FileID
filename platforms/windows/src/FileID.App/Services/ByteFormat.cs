namespace FileID.Services;

/// <summary>File and download sizes in Finder's decimal units (macOS
/// ByteCountFormatter .file): KB whole numbers, MB up to one decimal, GB and TB
/// up to two decimals, trailing zeros dropped. Memory sizes stay binary.</summary>
internal static class ByteFormat
{
    private static readonly (double Size, string Unit, int Decimals)[] Units =
    {
        (1e3, "KB", 0),
        (1e6, "MB", 1),
        (1e9, "GB", 2),
        (1e12, "TB", 2),
    };

    public static string File(ulong bytes) => File(bytes > long.MaxValue ? long.MaxValue : (long)bytes);

    public static string File(long bytes)
    {
        if (bytes <= 0) return "Zero KB";
        if (bytes == 1) return "1 byte";
        if (bytes < 1000) return $"{bytes} bytes";
        for (int i = 0; i < Units.Length; i++)
        {
            var (size, unit, decimals) = Units[i];
            var rounded = Math.Round(bytes / size, decimals, MidpointRounding.AwayFromZero);
            if (rounded < 1000 || i == Units.Length - 1)
            {
                return rounded.ToString(decimals == 0 ? "0" : "0." + new string('#', decimals)) + " " + unit;
            }
        }
        return $"{bytes} bytes";
    }
}
