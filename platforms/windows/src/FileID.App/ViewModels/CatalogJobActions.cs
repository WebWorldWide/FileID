using FileID.IpcSchema;

namespace FileID.ViewModels;

internal static class CatalogJobActions
{
    public static bool CanPause(string? state) => state is "running" or "queued";
    public static bool CanCancel(string? state) => state is "running" or "queued" or "paused";
    public static bool CanResume(string? kind, string? state) => kind is "catalogIndex" or "timelineSample" && state is "paused" or "failed" or "cancelled";

    public static IReadOnlyList<long> TimelineFileIDs(IEnumerable<CatalogHit> hits) => hits.Select(hit => hit.FileID).Distinct().Order().ToArray();
    public static CatalogRequest TimelineRequest(IEnumerable<CatalogHit> hits)
    {
        var ids = TimelineFileIDs(hits);
        if (ids.Count is < 1 or > 10000) throw new InvalidOperationException("Select 1–10000 catalog files for timeline analysis.");
        return new CatalogRequest(Guid.NewGuid().ToString(), "enqueueTimeline", null, null, null, null, null, ids);
    }
    public static CatalogRequest ControlRequest(string action, CatalogJob job)
    {
        var allowed = action switch
        {
            "pauseJob" => CanPause(job.State),
            "resumeJob" => CanResume(job.Kind, job.State),
            "cancelJob" => CanCancel(job.State),
            _ => false
        };
        if (!allowed) throw new InvalidOperationException("This action is unavailable for the selected job state.");
        return new CatalogRequest(Guid.NewGuid().ToString(), action, null, null, null, null, job.Id, null);
    }
}
