using FileID.IpcSchema;

namespace FileID.ViewModels;

/// <summary>Related-take proposals from the library-wide takeGrouping job: the relation chip,
/// plain-language evidence and the job requests. Lockstep with macOS TakeProposals and GTK takes_model.</summary>
internal static class TakeProposalPolicy
{
    public const string JobKind = "takeGrouping";
    public const string JobId = "take-grouping";

    public enum RelationTone { Gold, Info, Ai }

    public static string? RelationLabel(string? relation) => relation switch
    {
        "repeatedAttempts" => "Repeated tries",
        "alternateViews" => "Other angles",
        "uncertain" => "Not sure",
        _ => null,
    };

    public static RelationTone Tone(string? relation) => relation switch
    {
        "repeatedAttempts" => RelationTone.Gold,
        "alternateViews" => RelationTone.Info,
        _ => RelationTone.Ai,
    };

    public static string? RelationHelp(string? relation) => relation switch
    {
        "repeatedAttempts" => "One camera took these one after another, so they are separate tries of the same shot.",
        "alternateViews" => "Different cameras caught the same moment, so these are other angles or copies rather than separate tries.",
        "uncertain" => "Camera details are missing or mixed, so FileID can't tell separate tries from other angles.",
        _ => null,
    };

    public static string? EvidenceSentence(string code) => code switch
    {
        "captureSession" => "Taken close together.",
        "visualSimilarity" => "They look alike.",
        "sharedSubject" => "The same people are in every try.",
        "sameCamera" => "One camera took them all.",
        "simultaneousCameras" => "Different cameras took them at the same moment.",
        "mixedCameras" => "They come from a mix of cameras.",
        "cameraDetailsMissing" => "Some have no camera details.",
        _ => null,
    };

    public static string EvidenceText(IEnumerable<string>? codes) =>
        string.Join(" ", (codes ?? Array.Empty<string>()).Select(EvidenceSentence).OfType<string>());

    /// <summary>The review page's intro: the evidence and what the relation means, then how to pick.</summary>
    public static string Summary(CatalogTakeAutoGroup group)
    {
        const string pick = "FileID starred the one that looks best. Check it, then keep it and clear out the rest.";
        if (group.Relation is null) return "Tries of the same shot, taken moments apart. " + pick;
        return string.Join(" ", new[] { EvidenceText(group.Evidence), RelationHelp(group.Relation) ?? "", pick }.Where(s => s.Length > 0));
    }

    public static CatalogJob? Job(IEnumerable<CatalogJob>? jobs) => jobs?.FirstOrDefault(job => job.Kind == JobKind);

    /// <summary>Find related tries is offered unless a run is already queued or running.</summary>
    public static bool CanFind(CatalogJob? job) => job?.State is not ("queued" or "running");

    /// <summary>True when the job has just finished a run, so the proposals should be read again.</summary>
    public static bool Finished(CatalogJob? previous, CatalogJob? current) =>
        current?.State == "completed" && (previous?.State != "completed" || previous.UpdatedAt != current.UpdatedAt);

    /// <summary>The job row shows while a run is pending, paused or stopped; a finished run hides it.</summary>
    public static bool ShowJob(CatalogJob? job) => job is not null && job.State != "completed";

    /// <summary>A catalog response the engine pushed while the job runs (its request ID is the job's).</summary>
    public static bool IsJobPush(CatalogResponse? response) => response is not null && response.RequestID == JobId && Job(response.Jobs) is not null;

    public static CatalogRequest EnqueueRequest(string requestId) =>
        new(requestId, "enqueueTakeGrouping", null, null, null, null, null, null);

    public static CatalogRequest ProposalsRequest(string requestId) =>
        new(requestId, "takeGroupProposals", null, null, null, null, null, null, Limit: 100);

    public static string Header(IReadOnlyList<CatalogTakeAutoGroup> proposals) =>
        $"RELATED TRIES · {TakeReviewPolicy.OpenCount(proposals)}";

    public static string EmptyText(CatalogJob? job) =>
        job is null ? "Not looked yet. Find related tries to search the library." : "No related tries found yet.";

    public static string Spoken(CatalogTakeAutoGroup group, string date)
    {
        var relation = RelationLabel(group.Relation) is { } label ? $", {label}" : "";
        var skipped = TakeReviewPolicy.IsOpen(group) ? "" : ", skipped";
        var evidence = EvidenceText(group.Evidence);
        return $"{TakeReviewPolicy.GroupTitle(group)}{relation}, {date}{skipped}" + (evidence.Length == 0 ? "" : ". " + evidence);
    }

    /// <summary>The members to put in a new group when the person saves a proposal: one per try.</summary>
    public static IReadOnlyList<CatalogHit> SaveHits(CatalogTakeAutoGroup group)
    {
        var kind = group.Kind == "videos" ? "video" : "image";
        return group.Shots.Select(shot => new CatalogHit(shot.PrimaryFileID, shot.Path, kind, "", null, null, null)).ToArray();
    }

    /// <summary>The next proposal to open after one is resolved, keeping the reviewer's place.</summary>
    public static int? NextAfter(IReadOnlyList<CatalogTakeAutoGroup> before, IReadOnlyList<CatalogTakeAutoGroup> after, string resolved)
    {
        var position = before.Select(g => g.GroupKey).ToList().IndexOf(resolved);
        var open = after.Select((g, i) => (g, i)).Where(x => TakeReviewPolicy.IsOpen(x.g) && x.g.GroupKey != resolved).Select(x => x.i).ToArray();
        if (open.Length == 0) return null;
        return open[Math.Min(Math.Max(position, 0), open.Length - 1)];
    }
}
