using FileID.IpcSchema;
using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class CatalogJobActionsTests
{
    [Theory]
    [InlineData("catalogIndex", "paused", true)]
    [InlineData("catalogIndex", "failed", true)]
    [InlineData("catalogIndex", "cancelled", true)]
    [InlineData("catalogIndex", "queued", false)]
    [InlineData("catalogIndex", "running", false)]
    [InlineData("catalogIndex", "completed", false)]
    [InlineData("timelineSample", "paused", true)]
    [InlineData("timelineSample", "failed", true)]
    [InlineData("timelineSample", "cancelled", true)]
    [InlineData("timelineSample", "running", false)]
    [InlineData("timelineSample", "completed", false)]
    [InlineData("takeGrouping", "paused", true)]
    [InlineData("takeGrouping", "failed", true)]
    [InlineData("takeGrouping", "running", false)]
    [InlineData("takeGrouping", "completed", false)]
    [InlineData("faceRefresh", "paused", true)]
    [InlineData("faceRefresh", "failed", true)]
    [InlineData("faceRefresh", "cancelled", true)]
    [InlineData("faceRefresh", "running", false)]
    [InlineData("faceRefresh", "completed", false)]
    [InlineData("unknown", "paused", false)]
    [InlineData(null, "paused", false)]
    [InlineData("catalogIndex", null, false)]
    public void OnlyPausedOrRetryableSupportedJobsEnableResume(string? kind, string? state, bool enabled)
    {
        Assert.Equal(enabled, CatalogJobActions.CanResume(kind, state));
    }

    private static CatalogJob Job(string id, string state, double progress, string? error = null, params long[] files) =>
        new(id, "timelineSample", files, state, progress, error, 1, 2);

    [Theory]
    [InlineData("queued", 0.0, "Waiting")]
    [InlineData("running", 0.45, "Working 45%")]
    [InlineData("running", 7.0, "Working 100%")]
    [InlineData("running", double.NaN, "Working 0%")]
    [InlineData("paused", 0.3, "Paused")]
    [InlineData("failed", 0.3, "Didn't finish")]
    [InlineData("cancelled", 0.3, "Stopped")]
    [InlineData("completed", 1.0, "Done")]
    [InlineData(null, 0.0, "Waiting")]
    public void JobStatesReadAsPlainWords(string? state, double progress, string expected)
    {
        Assert.Equal(expected, CatalogJobActions.StateWords(state, progress));
    }

    [Fact]
    public void JobRowsShowNoIdsStatesOrRawErrors()
    {
        var row = CatalogJobActions.RowText(Job("job-7f3a", "failed", 0.5, "decode error 0x80070005 at C:\\Users\\a\\clip.mp4", 11, 12));

        Assert.Equal("Looking through videos · Didn't finish · 2 files\nPress Resume to try again.", row);
        Assert.DoesNotContain("job-7f3a", row, StringComparison.Ordinal);
        Assert.DoesNotContain("0x8007", row, StringComparison.Ordinal);
        Assert.Equal("Looking through videos · Working 45% · 1 file", CatalogJobActions.RowText(Job("a", "running", 0.45, null, 11)));
    }

    [Fact]
    public void AnUnchangedJobListIsLeftAlone()
    {
        var shown = new[] { Job("a", "running", 0.451, null, 1), Job("b", "queued", 0, null, 2) };

        Assert.True(CatalogJobActions.SameJobs(shown, new[] { Job("a", "running", 0.454, null, 1), Job("b", "queued", 0, null, 2) }));
        Assert.False(CatalogJobActions.SameJobs(shown, new[] { Job("a", "running", 0.47, null, 1), Job("b", "queued", 0, null, 2) }));
        Assert.False(CatalogJobActions.SameJobs(shown, new[] { Job("a", "paused", 0.451, null, 1), Job("b", "queued", 0, null, 2) }));
        Assert.False(CatalogJobActions.SameJobs(shown, new[] { Job("a", "running", 0.451, null, 1) }));
        Assert.False(CatalogJobActions.SameJobs(shown, new[] { Job("b", "queued", 0, null, 2), Job("a", "running", 0.451, null, 1) }));
    }

    [Fact]
    public void TimelineEnqueueUsesUniqueCatalogFileIDsNotEvidenceIDs()
    {
        var hit = new CatalogHit(42, "generated.mp4", "sampledFrame", "", "frame", 1, null);
        var request = CatalogJobActions.TimelineRequest(new[] { hit, hit with { EvidenceID = "other" }, hit with { FileID = 7 } });
        Assert.Equal("enqueueTimeline", request.Action);
        Assert.Equal(new long[] { 7, 42 }, request.FileIDs);
        Assert.Null(request.FileID);
        Assert.Null(request.JobID);
        Assert.Throws<InvalidOperationException>(() => CatalogJobActions.TimelineRequest(Array.Empty<CatalogHit>()));
    }

    [Fact]
    public void TimelineEnqueueCarriesTheSelectedModeAndRejectsUnknownModes()
    {
        var hit = new CatalogHit(42, "generated.mp4", "sampledFrame", "", "frame", 1, null);
        Assert.Equal("moments", CatalogJobActions.TimelineRequest(new[] { hit }, "moments").TimelineMode);
        Assert.Null(CatalogJobActions.TimelineRequest(new[] { hit }).TimelineMode);
        Assert.Throws<InvalidOperationException>(() => CatalogJobActions.TimelineRequest(new[] { hit }, "dense"));
    }

    [Theory]
    [InlineData("failed")]
    [InlineData("cancelled")]
    public void RetryingAStoppedTimelineEnqueuesItsFilesAgain(string state)
    {
        var job = new CatalogJob("durable-timeline", "timelineSample", new long[] { 42, 7, 42 }, state, 0.5, "error", 0, 1);
        var request = CatalogJobActions.ControlRequest("resumeJob", job);
        Assert.Equal("enqueueTimeline", request.Action);
        Assert.Equal(new long[] { 7, 42 }, request.FileIDs);
        Assert.Null(request.JobID);
    }

    [Theory]
    [InlineData("failed")]
    [InlineData("cancelled")]
    public void RetryingAStoppedTakeGroupingQueuesItAgain(string state)
    {
        var job = new CatalogJob("take-grouping", "takeGrouping", Array.Empty<long>(), state, 0.5, null, 0, 1);
        var request = CatalogJobActions.ControlRequest("resumeJob", job);
        Assert.Equal("enqueueTakeGrouping", request.Action);
        Assert.Null(request.JobID);
        var paused = CatalogJobActions.ControlRequest("resumeJob", job with { State = "paused" });
        Assert.Equal(("resumeJob", "take-grouping"), (paused.Action, paused.JobID));
    }

    [Theory]
    [InlineData("pauseJob", "running")]
    [InlineData("resumeJob", "paused")]
    [InlineData("cancelJob", "queued")]
    public void TimelineControlsKeepTheDurableJobID(string action, string state)
    {
        var job = new CatalogJob("durable-timeline", "timelineSample", new long[] { 42, 7 }, state, 0.5, null, 0, 1);
        var request = CatalogJobActions.ControlRequest(action, job);
        Assert.Equal(action, request.Action);
        Assert.Equal(job.Id, request.JobID);
        Assert.Null(request.FileIDs);
    }

    [Fact]
    public async Task UnsupportedResumeResponseIsRetainedAsErrorNotSuccess()
    {
        using var requests = new WorkbenchRequests();
        var request = new CatalogRequest("resume", "resumeJob", null, null, null, null, "catalog-index", null);
        var pending = requests.Register<CatalogResponse>(request.RequestID, 1);
        var backend = new CatalogResponse(request.RequestID, "error", "The worker is not yet available in this engine",
            Array.Empty<CatalogHit>(), Array.Empty<CatalogChapter>(), Array.Empty<CatalogJob>());
        Assert.True(requests.Apply(request.RequestID, 1, backend));
        var result = await pending;
        Assert.Equal("resumeJob", request.Action);
        Assert.Equal("catalog-index", request.JobID);
        Assert.Equal("error", result.Status);
        Assert.Equal(backend.Message, result.Message);
        Assert.Empty(result.Jobs);
    }

    [Theory]
    [InlineData("running", true, true)]
    [InlineData("queued", true, true)]
    [InlineData("paused", false, true)]
    [InlineData("failed", false, false)]
    [InlineData("cancelled", false, false)]
    [InlineData("completed", false, false)]
    [InlineData("unknown", false, false)]
    [InlineData(null, false, false)]
    public void ControlsReflectRealDurableJobStates(string? state, bool pause, bool cancel)
    {
        Assert.Equal(pause, CatalogJobActions.CanPause(state));
        Assert.Equal(cancel, CatalogJobActions.CanCancel(state));
    }
}
