using FileID.IpcSchema;

namespace FileID.ViewModels;

internal static class CatalogJobActions
{
    public static bool CanPause(string? state) => state is "running" or "queued";
    public static bool CanCancel(string? state) => state is "running" or "queued" or "paused";
    public static bool CanResume(string? kind, string? state) =>
        kind is "catalogIndex" or "timelineSample" or TakeProposalPolicy.JobKind or FaceQueuePolicy.JobKind && state is "paused" or "failed" or "cancelled";

    /// <summary>What a job is doing, in plain words.</summary>
    public static string StateWords(string? state, double progress) => state switch
    {
        "queued" => "Waiting",
        "running" => $"Working {Percent(progress)}%",
        "paused" => "Paused",
        "completed" => "Done",
        "failed" => "Didn't finish",
        "cancelled" => "Stopped",
        _ => "Waiting"
    };

    private static int Percent(double progress) =>
        double.IsFinite(progress) ? (int)Math.Round(Math.Clamp(progress, 0, 1) * 100) : 0;

    public static string KindWords(string? kind) => kind switch
    {
        "catalogIndex" => "Getting search ready",
        "takeGrouping" => "Finding related tries",
        "faceRefresh" => "Sorting faces",
        _ => "Looking through videos"
    };

    /// <summary>One list row for a job: what it is, how it is going and how
    /// many files it covers. No ids, internal states or raw error text.</summary>
    public static string RowText(CatalogJob job)
    {
        var files = job.FileIDs.Count;
        var text = $"{KindWords(job.Kind)} · {StateWords(job.State, job.Progress)}";
        if (files > 0) text += $" · {files:N0} file{(files == 1 ? "" : "s")}";
        if (job.State == "failed") text += "\nPress Resume to try again.";
        return text;
    }

    /// <summary>True when two job lists would show the same rows and enable
    /// the same buttons, so the list on screen can be left alone.</summary>
    public static bool SameJobs(IReadOnlyList<CatalogJob> shown, IReadOnlyList<CatalogJob> next)
    {
        if (shown.Count != next.Count) return false;
        for (var i = 0; i < shown.Count; i++)
        {
            var a = shown[i];
            var b = next[i];
            if (a.Id != b.Id || a.Kind != b.Kind || a.State != b.State || a.FileIDs.Count != b.FileIDs.Count
                || StateWords(a.State, a.Progress) != StateWords(b.State, b.Progress))
            {
                return false;
            }
        }
        return true;
    }

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
        if (action == "resumeJob" && job.Kind == TakeProposalPolicy.JobKind && job.State is "failed" or "cancelled")
            return TakeProposalPolicy.EnqueueRequest(Guid.NewGuid().ToString());
        if (action == "resumeJob" && job.Kind == FaceQueuePolicy.JobKind && job.State is "failed" or "cancelled")
            return FaceQueuePolicy.EnqueueRequest(Guid.NewGuid().ToString());
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
