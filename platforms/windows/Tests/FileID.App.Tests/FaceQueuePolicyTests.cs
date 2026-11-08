using FileID.IpcSchema;
using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class FaceQueuePolicyTests
{
    private static readonly long[] Accepted = { 1, 2, 3 };
    private static readonly long[] Skipped = { 4 };
    private static readonly long[] StripIds = { 5, 0, 5, 6, 7, 8, 9, 10, 11 };
    private static readonly long[] StripExpected = { 5, 6, 7, 8, 9, 10 };
    private static readonly long[] KeepIds = { 1, 2 };
    private static readonly long[] MoveIds = { 3 };

    private static FaceReviewItem Item(long face, string reason = "suggested", bool? lowQuality = null) =>
        new(face, 42, "/photos/IMG.jpg", "0.1,0.1,0.2,0.2", 2, "Alex", 20, 0.6, reason, null, null, null, 100, lowQuality);

    private static FaceReviewPage Reply(string status = "ok", IReadOnlyList<long>? accepted = null, IReadOnlyList<long>? skipped = null) =>
        new("r", "resolve", status, null, "op-1", null, 2, 0, 0, Array.Empty<FaceReviewItem>(), AcceptedFaceIds: accepted, SkippedFaceIds: skipped);

    private static FaceMergeSuggestion MergeOf(string? name = "Alex", string? other = null, long faces = 12, long otherFaces = 1) =>
        new(2, name, 20, faces, 3, other, 30, otherFaces, 0.81, 100);

    private static FaceSplitSuggestion SplitOf(string? name = "Kim") =>
        new(4, name, 40, 0.7, 9, 3, KeepIds, MoveIds, 100);

    private static FaceRefreshStatus Status(long outdated = 0, string? state = null, double? progress = null, long missing = 0, long offline = 0, long failed = 0) =>
        new("sface", "v2", outdated, 10, 4, 6, missing, offline, failed, state is null ? null : FaceQueuePolicy.JobId, state, progress);

    private static CatalogJob Job(string state, double progress = 0.5, string? error = null) =>
        new(FaceQueuePolicy.JobId, FaceQueuePolicy.JobKind, Array.Empty<long>(), state, progress, error, 0, 1);

    [Fact]
    public void AcceptAllCountsOnlySuggestedItemsForAPerson()
    {
        var items = new[] { Item(1), Item(2, "uncertain"), Item(3), Item(4, "ambiguous"), Item(5) };
        Assert.Equal(3, FaceQueuePolicy.SuggestedCount(items));
        Assert.True(FaceQueuePolicy.ShowAcceptAll(2, 3));
        Assert.False(FaceQueuePolicy.ShowAcceptAll(2, 0));
        Assert.False(FaceQueuePolicy.ShowAcceptAll(null, 3));
        Assert.Equal("Accept all suggested (3)", FaceQueuePolicy.AcceptAllText(3));
        Assert.Equal("Accept all 5 suggested faces for Alex", FaceQueuePolicy.AcceptAllLabel(5, "Alex"));
        Assert.Equal("Accept all 1 suggested face for an unnamed person", FaceQueuePolicy.AcceptAllLabel(1, null));
    }

    [Fact]
    public void AcceptAllMessageMentionsSkippedFacesOnlyWhenThereAreSome()
    {
        Assert.Equal("Accepted 3 faces", FaceQueuePolicy.AcceptAllMessage(Reply(accepted: Accepted)));
        Assert.Equal("Accepted 3 faces · 1 skipped, already in their photo", FaceQueuePolicy.AcceptAllMessage(Reply(accepted: Accepted, skipped: Skipped)));
        Assert.Equal("Accepted 1 face", FaceQueuePolicy.AcceptAllMessage(Reply(accepted: Skipped)));
        Assert.Equal("Accepted 0 faces", FaceQueuePolicy.AcceptAllMessage(Reply()));
    }

    [Fact]
    public void LowQualityFacesGetAShortNote()
    {
        Assert.Equal("Small or blurry face", FaceQueuePolicy.LowQualityNote(Item(1, lowQuality: true)));
        Assert.True(FaceQueuePolicy.IsLowQuality(Item(1, lowQuality: true)));
        Assert.Equal("", FaceQueuePolicy.LowQualityNote(Item(1, lowQuality: false)));
        Assert.False(FaceQueuePolicy.IsLowQuality(Item(1)));
    }

    [Fact]
    public void QueueSwitcherShowsCountsAndPlainEmptyStates()
    {
        Assert.Equal("Faces (4)", FaceQueuePolicy.QueueText(FaceQueuePolicy.Faces, 4));
        Assert.Equal("Merges (0)", FaceQueuePolicy.QueueText(FaceQueuePolicy.Merges, -2));
        Assert.Equal("Splits (1)", FaceQueuePolicy.QueueText(FaceQueuePolicy.Splits, 1));
        Assert.Equal("Merges: 1 pair of people who may be the same person", FaceQueuePolicy.QueueLabel(FaceQueuePolicy.Merges, 1));
        Assert.Equal("Splits: 2 people who may be two people", FaceQueuePolicy.QueueLabel(FaceQueuePolicy.Splits, 2));
        Assert.Equal("Faces: 1 face to review", FaceQueuePolicy.QueueLabel(FaceQueuePolicy.Faces, 1));
        Assert.Equal("No suggestions to review.", FaceQueuePolicy.EmptyText(FaceQueuePolicy.Faces));
        Assert.Equal("No people look like the same person.", FaceQueuePolicy.EmptyText(FaceQueuePolicy.Merges));
        Assert.Equal("No one looks like two different people.", FaceQueuePolicy.EmptyText(FaceQueuePolicy.Splits));
        Assert.Null(FaceQueuePolicy.ListQueue(FaceQueuePolicy.Faces));
        Assert.Equal("merges", FaceQueuePolicy.ListQueue(FaceQueuePolicy.Merges));
        Assert.Equal(FaceQueuePolicy.Faces, FaceQueuePolicy.Normalize(null));
        Assert.Equal(FaceQueuePolicy.Faces, FaceQueuePolicy.Normalize("refresh"));
    }

    [Fact]
    public void MergeRowsNameBothPeopleAndTheDirection()
    {
        var merge = MergeOf();
        Assert.Equal("Merge Unnamed into Alex", FaceQueuePolicy.MergeLabel(merge));
        Assert.Equal("Unnamed and Alex are not the same person", FaceQueuePolicy.NotSameLabel(merge));
        Assert.Equal("Dismiss merging Unnamed into Alex", FaceQueuePolicy.DismissMergeLabel(merge));
        Assert.Equal("Unnamed (1 face) may be the same person as Alex (12 faces), 81% match.", FaceQueuePolicy.MergeRowLabel(merge));
        Assert.Equal("Merged Unnamed into Alex", FaceQueuePolicy.MergeMessage(FaceQueuePolicy.Merge, merge));
        Assert.Equal("Kept Unnamed and Alex apart", FaceQueuePolicy.MergeMessage(FaceQueuePolicy.NotSamePerson, merge));
        Assert.Equal("Dismissed the suggestion", FaceQueuePolicy.MergeMessage(FaceQueuePolicy.Dismiss, merge));
        Assert.Equal("Unnamed", FaceQueuePolicy.Name("  "));
        Assert.Equal("Sam", FaceQueuePolicy.Name(" Sam "));
    }

    [Fact]
    public void SplitRowsDescribeBothGroups()
    {
        var split = SplitOf();
        Assert.Equal("Stays (9)", FaceQueuePolicy.KeepCaption(split));
        Assert.Equal("Moves to a new person (3)", FaceQueuePolicy.MoveCaption(split));
        Assert.Equal("Split Kim into two people", FaceQueuePolicy.SplitLabel(split));
        Assert.Equal("Dismiss splitting Kim", FaceQueuePolicy.DismissSplitLabel(split));
        Assert.Equal("Kim may be two people: 9 faces stay, 3 faces move to a new person.", FaceQueuePolicy.SplitRowLabel(split));
        Assert.Equal("Split Kim into two people", FaceQueuePolicy.SplitMessage(FaceQueuePolicy.Split, split));
        Assert.Equal("Dismissed the suggestion", FaceQueuePolicy.SplitMessage(FaceQueuePolicy.Dismiss, split));
        Assert.Equal("Split Unnamed into two people", FaceQueuePolicy.SplitLabel(SplitOf(null)));
        Assert.Equal(StripExpected, FaceQueuePolicy.Strip(StripIds));
        Assert.Empty(FaceQueuePolicy.Strip(null));
        Assert.Equal("Face 2 of Kim", FaceQueuePolicy.StripFaceLabel("Kim", 1));
    }

    [Fact]
    public void RefreshBannerHidesWhenNothingIsOutdatedAndNoJobRuns()
    {
        Assert.Same(FaceRefreshView.Hidden, FaceQueuePolicy.RefreshView(null, null));
        Assert.False(FaceQueuePolicy.RefreshView(Status(), null).Visible);
        Assert.False(FaceQueuePolicy.RefreshView(Status(state: "completed", progress: 1), Job("completed", 1)).Visible);
    }

    [Fact]
    public void RefreshBannerOffersAnUpdateWhenFacesAreOutdated()
    {
        var view = FaceQueuePolicy.RefreshView(Status(outdated: 120), null);
        Assert.True(view.Visible);
        Assert.Equal("Faces need updating (120)", view.Text);
        Assert.True(view.CanUpdate);
        Assert.False(view.CanPause || view.CanResume || view.CanCancel);
        Assert.Equal("Update faces, 120 faces need updating", FaceQueuePolicy.UpdateLabel(120));
        Assert.Equal("Update faces, 1 face needs updating", FaceQueuePolicy.UpdateLabel(1));
        var cancelled = FaceQueuePolicy.RefreshView(Status(outdated: 5, state: "cancelled"), Job("cancelled"));
        Assert.True(cancelled.CanUpdate);
        Assert.Equal("Faces need updating (5)", cancelled.Text);
    }

    [Fact]
    public void RunningAndPausedJobsShowProgressTallyAndControls()
    {
        var running = FaceQueuePolicy.RefreshView(Status(outdated: 50, missing: 2, failed: 1), Job("running", 0.426));
        Assert.Equal("Updating faces: 42% · 2 missing · 1 failed", running.Text);
        Assert.False(running.CanUpdate);
        Assert.True(running.CanPause);
        Assert.False(running.CanResume);
        Assert.True(running.CanCancel);

        var plain = FaceQueuePolicy.RefreshView(Status(state: "queued", progress: 0), null);
        Assert.Equal("Updating faces: 0%", plain.Text);
        Assert.True(plain.CanPause);

        var paused = FaceQueuePolicy.RefreshView(Status(offline: 3), Job("paused", 0.5, "Interrupted. Resume to continue; faces already updated are kept."));
        Assert.Equal("Updating faces: 50% · 3 offline · paused", paused.Text);
        Assert.False(paused.CanPause);
        Assert.True(paused.CanResume);
        Assert.True(paused.CanCancel);
        Assert.Equal("Interrupted. Resume to continue; faces already updated are kept.", paused.Note);
    }

    [Fact]
    public void PercentFloorsAndClamps()
    {
        Assert.Equal(99, FaceQueuePolicy.Percent(0.999));
        Assert.Equal(100, FaceQueuePolicy.Percent(1.4));
        Assert.Equal(0, FaceQueuePolicy.Percent(-1));
        Assert.Equal(0, FaceQueuePolicy.Percent(null));
        Assert.Equal(0, FaceQueuePolicy.Percent(double.NaN));
    }

    [Fact]
    public void JobUpdatesComeFromCatalogResponsesCarryingTheFaceRefreshJob()
    {
        var other = new CatalogJob("take-grouping", "takeGrouping", Array.Empty<long>(), "running", 0.1, null, 0, 1);
        Assert.Null(FaceQueuePolicy.Job(null));
        Assert.Null(FaceQueuePolicy.Job(new[] { other }));
        Assert.Equal(FaceQueuePolicy.JobKind, FaceQueuePolicy.Job(new[] { other, Job("running") })!.Kind);
        Assert.False(FaceQueuePolicy.IsJobUpdate(null));
        var request = FaceQueuePolicy.EnqueueRequest("req-1");
        Assert.Equal("enqueueFaceRefresh", request.Action);
        Assert.Equal("req-1", request.RequestID);
        Assert.Null(request.JobID);
    }

    [Fact]
    public void MergeJobPrefersTheNewerStatusAndBuildsAJobFromStatusAlone()
    {
        Assert.Null(FaceQueuePolicy.MergeJob(null, Status()));
        var built = FaceQueuePolicy.MergeJob(null, Status(state: "paused", progress: 0.3))!;
        Assert.Equal(FaceQueuePolicy.JobId, built.Id);
        Assert.Equal(FaceQueuePolicy.JobKind, built.Kind);
        Assert.Equal("paused", built.State);
        Assert.Equal(0.3, built.Progress);
        var pushed = Job("paused", 0.3, "Interrupted.");
        Assert.Equal("Interrupted.", FaceQueuePolicy.MergeJob(pushed, Status(state: "paused", progress: 0.3))!.Error);
        var resumed = FaceQueuePolicy.MergeJob(pushed, Status(state: "running", progress: 0.4))!;
        Assert.Equal("running", resumed.State);
        Assert.Equal(0.4, resumed.Progress);
        Assert.Null(resumed.Error);
        Assert.Same(pushed, FaceQueuePolicy.MergeJob(pushed, null));
    }

    [Fact]
    public void KeyedQueueDedupesPagesAndRemovesByKey()
    {
        var queue = new KeyedReviewQueue<FaceMergeSuggestion, (long, long)>(m => (m.PersonId, m.OtherPersonId));
        Assert.True(queue.IsEmpty);
        var first = MergeOf();
        var second = first with { OtherPersonId = 9 };
        Assert.Equal(2, queue.Reset(new[] { first, second }, 3).Count);
        Assert.True(queue.HasMore);
        Assert.Equal(2, queue.NextOffset);
        Assert.Empty(queue.Append(new[] { first }, 3));
        Assert.Single(queue.Append(new[] { first with { OtherPersonId = 11 } }, 3));
        Assert.False(queue.HasMore);
        Assert.True(queue.Remove((2, 9)));
        Assert.False(queue.Remove((2, 9)));
        Assert.Equal(2, queue.Total);
        queue.Reset(null, 0);
        Assert.True(queue.IsEmpty);
    }

    [Fact]
    public void TotalsSumEveryQueue()
    {
        var totals = new FaceQueueTotals();
        Assert.Equal(0, totals.Sum);
        totals.Set(null, 4);
        totals.Set(FaceQueuePolicy.Merges, 2);
        totals.Set(FaceQueuePolicy.Splits, -1);
        Assert.Equal(4, totals[FaceQueuePolicy.Faces]);
        Assert.Equal(2, totals[FaceQueuePolicy.Merges]);
        Assert.Equal(0, totals[FaceQueuePolicy.Splits]);
        Assert.Equal(6, totals.Sum);
    }

    [Theory]
    [InlineData("failed")]
    [InlineData("cancelled")]
    public void ResumingAStoppedFaceRefreshQueuesItAgain(string state)
    {
        var request = CatalogJobActions.ControlRequest("resumeJob", Job(state));
        Assert.Equal("enqueueFaceRefresh", request.Action);
        Assert.Null(request.JobID);
    }

    [Fact]
    public void PausingAndCancellingFaceRefreshTargetsItsJob()
    {
        Assert.Equal(FaceQueuePolicy.JobId, CatalogJobActions.ControlRequest("pauseJob", Job("running")).JobID);
        Assert.Equal(FaceQueuePolicy.JobId, CatalogJobActions.ControlRequest("resumeJob", Job("paused")).JobID);
        Assert.Equal("cancelJob", CatalogJobActions.ControlRequest("cancelJob", Job("paused")).Action);
        Assert.Throws<InvalidOperationException>(() => CatalogJobActions.ControlRequest("pauseJob", Job("completed")));
    }
}
