using System.Globalization;

namespace FileID.ViewModels;

/// <summary>The capture-date window Search &amp; Moments sends as capturedFrom / capturedTo:
/// whole local days, inclusive at both ends.</summary>
internal static class SearchDateRange
{
    public static (double? From, double? To) Unix(DateTimeOffset? from, DateTimeOffset? to, TimeZoneInfo? zone = null)
    {
        zone ??= TimeZoneInfo.Local;
        double? start = from is { } f ? Math.Max(0, LocalMidnight(f, zone).ToUnixTimeMilliseconds() / 1000.0) : null;
        double? end = to is { } t ? LocalMidnight(t, zone).AddDays(1).ToUnixTimeMilliseconds() / 1000.0 - 0.001 : null;
        if (end is < 0) throw new InvalidOperationException("Pick an end date after 1 January 1970.");
        if (start is { } s && end is { } e && e < s) throw new InvalidOperationException("The end date is before the start date. Pick dates in order.");
        return (start, end);
    }

    private static DateTimeOffset LocalMidnight(DateTimeOffset picked, TimeZoneInfo zone)
    {
        var day = new DateTime(picked.Year, picked.Month, picked.Day, 0, 0, 0, DateTimeKind.Unspecified);
        return new DateTimeOffset(day, zone.GetUtcOffset(day));
    }

    public static string Describe(DateTimeOffset? from, DateTimeOffset? to)
    {
        static string Day(DateTimeOffset value) => value.ToString("d MMM yyyy", CultureInfo.CurrentCulture);
        return (from, to) switch
        {
            ({ } f, { } t) => $"Taken {Day(f)} – {Day(t)}",
            ({ } f, null) => $"Taken on or after {Day(f)}",
            (null, { } t) => $"Taken on or before {Day(t)}",
            _ => "",
        };
    }
}
