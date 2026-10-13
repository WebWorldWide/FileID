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
    [InlineData("unknown", "paused", false)]
    [InlineData(null, "paused", false)]
    [InlineData("catalogIndex", null, false)]
    public void OnlyPausedOrRetryableSupportedJobsEnableResume(string? kind, string? state, bool enabled)
    {
        Assert.Equal(enabled, CatalogJobActions.CanResume(kind, state));
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

    [Theory]
    [InlineData("pauseJob", "running")]
    [InlineData("resumeJob", "paused")]
    [InlineData("resumeJob", "failed")]
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
