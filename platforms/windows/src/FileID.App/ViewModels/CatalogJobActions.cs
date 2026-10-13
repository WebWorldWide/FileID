namespace FileID.ViewModels;

internal static class CatalogJobActions
{
    public static bool CanPause(string? state) => state is "running" or "queued";
    public static bool CanCancel(string? state) => state is "running" or "queued" or "paused";
    public static bool CanResume(string? kind, string? state) => kind == "catalogIndex" && state is "paused" or "failed" or "cancelled";
}
