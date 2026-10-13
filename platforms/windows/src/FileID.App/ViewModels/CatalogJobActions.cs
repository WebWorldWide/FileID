using FileID.IpcSchema;

namespace FileID.ViewModels;

internal static class CatalogJobActions
{
    public static bool CanPause(string? state) => state is "running" or "queued";
    public static bool CanCancel(string? state) => state is "running" or "queued" or "paused";
    public static bool CanResume(string? kind, string? state) => kind is "catalogIndex" or "timelineSample" && state is "paused" or "failed" or "cancelled";

    public static IReadOnlyList<long> TimelineFileIDs(IEnumerable<CatalogHit> hits) => hits.Select(hit => hit.FileID).Distinct().Order().ToArray();
    public static CatalogRequest TimelineRequest(IEnumerable<CatalogHit> hits, string? timelineMode = null) =>
        TimelineRequest(TimelineFileIDs(hits), timelineMode);
    private static CatalogRequest TimelineRequest(IReadOnlyList<long> ids, string? timelineMode)
    {
        if (ids.Count is < 1 or > 1000) throw new InvalidOperationException("Select 1–1000 catalog videos for timeline analysis.");
        if (timelineMode is not (null or "sampled" or "moments")) throw new InvalidOperationException("Unknown timeline analysis mode.");
        return new CatalogRequest(Guid.NewGuid().ToString(), "enqueueTimeline", null, null, null, null, null, ids, TimelineMode: timelineMode);
    }
    public static CatalogRequest ControlRequest(string action, CatalogJob job)
    {
        // The engine resumes only paused timeline jobs; a failed or cancelled
        // one is retried as a fresh job over the same files, as on macOS.
        if (action == "resumeJob" && job.Kind == "timelineSample" && job.State is "failed" or "cancelled")
            return TimelineRequest(job.FileIDs.Distinct().Order().ToArray(), null);
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
