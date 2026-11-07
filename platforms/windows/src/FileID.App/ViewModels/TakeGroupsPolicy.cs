using FileID.IpcSchema;

namespace FileID.ViewModels;

/// <summary>Manual Best Takes groups without any UI: the clip pool, when a group
/// can be saved or scored, take card wording and the feedback each card button
/// sends. Mirrors the Linux takes_groups_model.rs and the macOS TakeWorkbench /
/// TakeCard (DESIGN_V3_PORT "Best Takes", manual groups).</summary>
internal static class TakeGroupsPolicy
{
    public enum Tone { Gold, Ai, Muted, Done, Danger, Warning, Text2 }

    public enum Outcome { Met, Missed, Unclear }

    public static readonly IReadOnlyList<Outcome> Outcomes = new[] { Outcome.Met, Outcome.Missed, Outcome.Unclear };

    public const double CardMinWidth = 260;
    public const double CardMaxWidth = 420;
    public const double CardGap = 12;

    private static readonly HashSet<string> VideoExtensions = new(StringComparer.OrdinalIgnoreCase)
    {
        "mp4", "mov", "m4v", "avi", "mkv", "webm", "mts", "m2ts", "wmv", "mxf", "3gp", "3g2",
        "flv", "ogv", "mpeg", "mpg", "vob", "f4v", "asf", "divx",
    };

    /// <summary>Same extension list as the shared engine's FileKind::Video.</summary>
    public static bool IsVideoPath(string path)
    {
        var name = HitPresentation.FileName(path);
        var dot = name.LastIndexOf('.');
        return dot >= 0 && dot < name.Length - 1 && VideoExtensions.Contains(name[(dot + 1)..]);
    }

    /// <summary>Search candidates first, then clips found here, each file once.</summary>
    public static IReadOnlyList<CatalogHit> Pool(IReadOnlyList<CatalogHit> candidates, IReadOnlyList<CatalogHit> found)
    {
        var seen = new HashSet<long>();
        return candidates.Concat(found).Where(hit => seen.Add(hit.FileID)).ToArray();
    }

    /// <summary>Video hits from a clip search that are not already candidates.</summary>
    public static IReadOnlyList<CatalogHit> FoundVideos(IReadOnlyList<CatalogHit> hits, IReadOnlyList<CatalogHit> candidates)
    {
        var seen = candidates.Select(hit => hit.FileID).ToHashSet();
        return hits.Where(hit => IsVideoPath(hit.Path) && seen.Add(hit.FileID)).ToArray();
    }

    /// <summary>Clips of the open group that are not in the pool yet, so its
    /// checklist lists every file the group holds.</summary>
    public static IReadOnlyList<CatalogHit> MissingTakes(IReadOnlyList<CatalogTake> takes, IReadOnlyList<CatalogHit> pool)
    {
        var seen = pool.Select(hit => hit.FileID).ToHashSet();
        return takes.Where(take => seen.Add(take.FileID))
            .Select(take => new CatalogHit(take.FileID, take.Path, "video", "", null, null, null))
            .ToArray();
    }

    /// <summary>The Search &amp; Moments "Best takes" button needs two different files to compare.</summary>
    public static bool CanOpenFromSearch(IEnumerable<CatalogHit> hits) => hits.Select(hit => hit.FileID).Distinct().Take(2).Count() >= 2;

    public static string SearchTooltip(IEnumerable<CatalogHit> hits) =>
        CanOpenFromSearch(hits)
            ? "Compare these clips as tries of the same shot and pick the best one"
            : "Find at least two clips first";

    public static string FindRelatedTooltip(int pool) =>
        pool < 2
            ? "Search for at least two videos first"
            : "Suggest groups of clips that look alike and were filmed close together";

    public static bool CanSave(string title, string goal, int chosen) =>
        !string.IsNullOrWhiteSpace(title) && !string.IsNullOrWhiteSpace(goal) && chosen >= 2;

    public static string SaveTooltip(bool canSave) =>
        canSave ? "Save (Ctrl+S)" : "Needs a name, a goal and at least two clips";

    /// <summary>The event to save: the open group's id, or a new one.</summary>
    public static CatalogEvent EventToSave(CatalogEvent? existing, string newID, string title, string goal, IEnumerable<long> chosen) =>
        new(existing?.Id ?? newID, title, goal, chosen.Distinct().Order().ToArray(), false);

    public static string ClipsHeader(int chosen, int pool) => $"CLIPS · {chosen} OF {pool} CHOSEN";

    public static bool AllChosen(IReadOnlySet<long> chosen, IReadOnlyList<CatalogHit> pool) =>
        pool.Count > 0 && pool.All(hit => chosen.Contains(hit.FileID));

    public static string ChooseLabel(IReadOnlySet<long> chosen, IReadOnlyList<CatalogHit> pool) =>
        AllChosen(chosen, pool) ? "Choose none" : "Choose all";

    public static HashSet<long> ToggleAll(IReadOnlySet<long> chosen, IReadOnlyList<CatalogHit> pool) =>
        AllChosen(chosen, pool) ? new HashSet<long>() : pool.Select(hit => hit.FileID).ToHashSet();

    public static string EditorNote(bool poolEmpty) =>
        poolEmpty
            ? "Search on the left to add clips."
            : "Scores are suggestions from a few frames. Your videos are never changed.";

    public static bool JobActive(CatalogJob? job) => job?.State is "queued" or "running" or "paused";

    /// <summary>Score takes runs only on a saved, unchanged group of videos
    /// with no scoring job already going.</summary>
    public static bool CanScore(CatalogEvent saved, string goalText, IReadOnlySet<long> chosen, IReadOnlyList<CatalogTake> takes, bool busy, CatalogJob? job) =>
        !busy
        && goalText.Trim() == saved.Goal
        && chosen.Count == saved.FileIDs.Count && chosen.SetEquals(saved.FileIDs)
        && takes.Count == saved.FileIDs.Count
        && takes.All(take => IsVideoPath(take.Path))
        && !JobActive(job);

    /// <summary>The timeline job a Score takes request just started: a new
    /// sampling job over exactly the group's files.</summary>
    public static CatalogJob? NewGoalJob(IReadOnlyList<CatalogJob> jobs, IReadOnlySet<long> requested, IReadOnlySet<string> previous) =>
        jobs.FirstOrDefault(job => job.Kind == "timelineSample" && !previous.Contains(job.Id) && job.FileIDs.ToHashSet().SetEquals(requested));

    public static string JobTitle(CatalogJob job) =>
        job.Kind == "catalogIndex" ? "Search index" : job.Kind == "takeGrouping" ? "Related tries" : $"{job.FileIDs.Count} video{(job.FileIDs.Count == 1 ? "" : "s")} · finding moments";

    public static (string Text, Tone Tone) JobState(CatalogJob job) => job.State switch
    {
        "running" => ("Running", Tone.Gold),
        "paused" => ("Paused", Tone.Muted),
        "completed" => ("Completed", Tone.Done),
        "failed" => ("Failed", Tone.Danger),
        "cancelled" => ("Cancelled", Tone.Danger),
        "queued" => ("Queued", Tone.Muted),
        _ => (job.State.Length == 0 ? "" : char.ToUpperInvariant(job.State[0]) + job.State[1..], Tone.Muted),
    };

    public static string EventMeta(CatalogEvent saved) => $"{saved.FileIDs.Count} clips · {saved.Goal}";

    public static string EventSpoken(CatalogEvent saved) => $"{saved.Title}, {saved.FileIDs.Count} clips, goal: {saved.Goal}";

    public static IReadOnlyList<string> SuggestionNames(CatalogTakeGroupSuggestion suggestion) =>
        suggestion.Members.Select(member => HitPresentation.FileName(member.Path)).ToArray();

    public static string SuggestionMeta(CatalogTakeGroupSuggestion suggestion) =>
        $"{suggestion.Members.Count} clips · {Percent(suggestion.Similarity)}% alike";

    public static IReadOnlyList<CatalogHit> SuggestionHits(CatalogTakeGroupSuggestion suggestion) =>
        suggestion.Members.Select(member => new CatalogHit(member.FileID, member.Path, "video", "", null, null, null)).ToArray();

    public static Tone RecommendationTone(CatalogTakeRecommendation recommendation) =>
        recommendation.Status.ToLowerInvariant() switch
        {
            "ranked" or "preferred" => Tone.Gold,
            "tie" => Tone.Ai,
            _ => Tone.Muted,
        };

    public static string RecommendationHeader(CatalogTakeRecommendation recommendation) =>
        $"RECOMMENDATION · {recommendation.Status.ToUpperInvariant()}";

    public static bool IsRecommended(CatalogTakeRecommendation? recommendation, CatalogTake take) =>
        recommendation?.FileIDs.Contains(take.FileID) == true;

    private static bool ReviewedByUser(CatalogTake take) => take.ModelVersion == "user";

    private static bool IsDraft(CatalogTake take) =>
        take.ModelVersion?.StartsWith("take-outcome-sequence-v1/", StringComparison.Ordinal) == true;

    /// <summary>The card's headline and its tone.</summary>
    public static (string Text, Tone Tone) Headline(CatalogTake take)
    {
        if (take.Preferred) return ("Preferred by you", Tone.Gold);
        if (take.Stale) return ("Needs review: the file changed", Tone.Warning);
        if (take.OutcomeScore is not { } score)
        {
            if (ReviewedByUser(take)) return ("Unclear", Tone.Text2);
            return (IsDraft(take) ? "Not enough evidence" : "Not reviewed", Tone.Muted);
        }
        var prefix = IsDraft(take) ? "Draft · " : "";
        return score >= 0.5 ? (prefix + "Goal met", Tone.Done) : (prefix + "Goal missed", Tone.Danger);
    }

    /// <summary>The 7 px dot: tone, tooltip, spoken label.</summary>
    public static (Tone Tone, string Tooltip, string Spoken) ReviewDot(CatalogTake take) =>
        ReviewedByUser(take) ? (Tone.Gold, "Reviewed by you", "Reviewed by you")
        : IsDraft(take) ? (Tone.Ai, "Suggested by FileID from a few frames", "Model draft")
        : (Tone.Muted, "No score yet", "No score");

    public static string Label(Outcome outcome) => outcome switch
    {
        Outcome.Met => "Met",
        Outcome.Missed => "Missed",
        _ => "Unclear",
    };

    public static string Spoken(Outcome outcome) => $"Goal {Label(outcome).ToLowerInvariant()}";

    private static double? Score(Outcome outcome) => outcome switch
    {
        Outcome.Met => 1.0,
        Outcome.Missed => 0.0,
        _ => null,
    };

    /// <summary>Which outcome button is on: only the user's own review lights one.</summary>
    public static Outcome? SelectedOutcome(CatalogTake take)
    {
        if (!ReviewedByUser(take)) return null;
        return take.OutcomeScore switch
        {
            null => Outcome.Unclear,
            >= 0.5 => Outcome.Met,
            _ => Outcome.Missed,
        };
    }

    public static CatalogTakeFeedback OutcomeFeedback(string eventID, CatalogTake take, Outcome outcome) =>
        new(eventID, take.FileID, Score(outcome), take.Preferred);

    public static CatalogTakeFeedback PreferFeedback(string eventID, CatalogTake take) =>
        new(eventID, take.FileID, take.OutcomeScore, !take.Preferred);

    public static string CardSpoken(int index, CatalogTake take, bool recommended) =>
        $"Take {index}, {HitPresentation.FileName(take.Path)}{(recommended ? ", recommended" : "")}";

    public static int Percent(double value) => (int)Math.Round(Math.Clamp(value, 0, 1) * 100, MidpointRounding.AwayFromZero);

    /// <summary>Actions whose reply carries the one open group rather than the list.</summary>
    public static bool RepliesWithOpenGroup(string action) =>
        action is "takeGroup" or "saveEvent" or "setTakeFeedback" or "undoTakeFeedback";

    /// <summary>Actions after which the YOUR GROUPS list is read again.</summary>
    public static bool Relists(string action) => action is "saveEvent" or "deleteEvent" or "undoEventEdit";

    /// <summary>Adaptive card grid (macOS GridItem.adaptive(260...420), gap 12):
    /// as many columns as fit at the minimum width, each at most 420 wide.</summary>
    public static (int Columns, double Width) CardColumns(double available)
    {
        if (!double.IsFinite(available) || available <= 0) return (1, CardMaxWidth);
        var columns = Math.Max(1, (int)((available + CardGap) / (CardMinWidth + CardGap)));
        var width = (available - CardGap * (columns - 1)) / columns;
        return (columns, Math.Min(CardMaxWidth, Math.Max(0, width)));
    }
}
