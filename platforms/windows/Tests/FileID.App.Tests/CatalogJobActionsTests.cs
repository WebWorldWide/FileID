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
    [InlineData("timelineSample", "paused", false)]
    [InlineData("timelineSample", "failed", false)]
    [InlineData("timelineSample", "cancelled", false)]
    [InlineData(null, "paused", false)]
    [InlineData("catalogIndex", null, false)]
    public void OnlyPausedOrRetryableCatalogIndexJobsEnableResume(string? kind, string? state, bool enabled)
    {
        Assert.Equal(enabled, CatalogJobActions.CanResume(kind, state));
    }

    [Fact]
    public async Task BaselineUnsupportedResumeResponseIsRetainedAsErrorNotSuccess()
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
