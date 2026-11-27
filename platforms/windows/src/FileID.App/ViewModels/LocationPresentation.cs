using System.Globalization;
using FileID.IpcSchema;
using FileID.Services;

namespace FileID.ViewModels;

internal static class LocationPresentation
{
    public const string Empty = "No locations yet. Scan a folder or drive and it appears here.";
    public const string Footer = "Offline locations keep their tags, faces and descriptions. They come back when the drive does.";

    public static string Summary(LibraryRoot root, DateTimeOffset now) =>
        string.Create(CultureInfo.CurrentCulture, $"{root.FileCount:N0} {(root.FileCount == 1 ? "file" : "files")} · {ByteFormat.File(root.TotalBytes)} · {Scanned(root.LastScanAt, now)}");

    public static string Scanned(double? lastScanAt, DateTimeOffset now)
    {
        if (lastScanAt is not { } seconds || !double.IsFinite(seconds) || seconds <= 0) return "not scanned yet";
        var elapsed = now - DateTimeOffset.FromUnixTimeMilliseconds((long)(seconds * 1000));
        return "scanned " + Relative(elapsed);
    }

    public static string Relative(TimeSpan elapsed)
    {
        if (elapsed < TimeSpan.FromMinutes(1)) return "just now";
        if (elapsed < TimeSpan.FromHours(1)) return Ago((int)elapsed.TotalMinutes, "minute");
        if (elapsed < TimeSpan.FromDays(1)) return Ago((int)elapsed.TotalHours, "hour");
        if (elapsed < TimeSpan.FromDays(7)) return Ago((int)elapsed.TotalDays, "day");
        if (elapsed < TimeSpan.FromDays(31)) return Ago((int)(elapsed.TotalDays / 7), "week");
        if (elapsed < TimeSpan.FromDays(365)) return Ago((int)(elapsed.TotalDays / 30), "month");
        return Ago((int)(elapsed.TotalDays / 365), "year");
    }

    private static string Ago(int count, string unit) =>
        string.Create(CultureInfo.CurrentCulture, $"{Math.Max(1, count):N0} {unit}{(Math.Max(1, count) == 1 ? "" : "s")} ago");

    public static string Spoken(LibraryRoot root, DateTimeOffset now) =>
        $"{root.Display}, {(root.Online ? "online" : "offline")}{(root.ReadOnly == true ? ", read-only" : string.Empty)}, {Summary(root, now)}";

    public const string ReadOnlyTooltip = "FileID can show and search these files but can\u2019t change them.";

    public static string RescanTooltip(LibraryRoot root) => root.Online ? "Scan this location again" : "Reconnect the drive to rescan";

    public static string Glyph(string kind) => kind switch
    {
        "network" => "\uE968",
        "external" => "\uEDA2",
        _ => "\uE8B7",
    };
}
