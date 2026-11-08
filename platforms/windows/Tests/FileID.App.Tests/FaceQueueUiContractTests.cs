using Xunit;

namespace FileID.App.Tests;

public sealed class FaceQueueUiContractTests
{
    [Fact]
    public void QueueRequestsGoThroughTheUnlockedWorkbenchPath()
    {
        var engine = ReadSource("ViewModels/EngineClient.Workbenches.cs");
        Assert.Contains("new ListFaceReviewCommand(id, personId, offset, limit, queue)", engine, StringComparison.Ordinal);
        Assert.Contains("new ResolveFaceReviewCommand(id, faceId, personId, resolution, otherPersonId)", engine, StringComparison.Ordinal);
        Assert.Contains("ListFaceReviewAsync(null, 0, 1, FaceQueuePolicy.Refresh, ct)", engine, StringComparison.Ordinal);
    }

    [Fact]
    public void PanelResolvesAcceptAllMergesAndSplitsWithUndo()
    {
        var panel = ReadSource("Views/People/FaceReviewPanel.xaml.cs");
        Assert.Contains("ResolveFaceReviewAsync(null, personId, FaceQueuePolicy.AcceptAll)", panel, StringComparison.Ordinal);
        Assert.Contains("ResolveFaceReviewAsync(null, vm.Model.PersonId, resolution, vm.Model.OtherPersonId)", panel, StringComparison.Ordinal);
        Assert.Contains("ResolveFaceReviewAsync(null, vm.Model.PersonId, resolution)", panel, StringComparison.Ordinal);
        Assert.Contains("var message = FaceQueuePolicy.AcceptAllMessage(page);", panel, StringComparison.Ordinal);
        Assert.Contains("OfferUndo(operationId, message)", panel, StringComparison.Ordinal);
        Assert.Contains("OfferUndo(operationId, FaceQueuePolicy.MergeMessage(resolution, vm.Model))", panel, StringComparison.Ordinal);
        Assert.Contains("OfferUndo(operationId, FaceQueuePolicy.SplitMessage(resolution, vm.Model))", panel, StringComparison.Ordinal);
        Assert.Contains("UndoFaceReviewAsync(operationId)", panel, StringComparison.Ordinal);
        var xaml = ReadSource("Views/People/FaceReviewPanel.xaml");
        Assert.Contains("AutomationProperties.Name=\"{x:Bind MergeLabel}\"", xaml, StringComparison.Ordinal);
        Assert.Contains("AutomationProperties.Name=\"{x:Bind SplitLabel}\"", xaml, StringComparison.Ordinal);
        Assert.Contains("Visibility=\"{x:Bind LowQualityVisibility}\"", xaml, StringComparison.Ordinal);
    }

    [Fact]
    public void FaceRefreshSubscriptionFiltersLogsAndDispatches()
    {
        var view = ReadSource("Views/People/PeopleView.xaml.cs");
        var handler = view[view.IndexOf("private void OnEngineClientChanged", StringComparison.Ordinal)..view.IndexOf("private async void OnLoadedAsync", StringComparison.Ordinal)];
        Assert.Contains("DebugLog.SafeRun(\"PeopleView.OnEngineClientChanged\"", handler, StringComparison.Ordinal);
        Assert.Contains("OnFaceRefreshEngineChanged(e);", handler, StringComparison.Ordinal);

        var refresh = ReadSource("Views/People/PeopleView.FaceRefresh.cs");
        var push = refresh[refresh.IndexOf("private void OnFaceRefreshEngineChanged", StringComparison.Ordinal)..refresh.IndexOf("private async Task RefreshFaceRefreshStatusAsync", StringComparison.Ordinal)];
        var filter = push.IndexOf("if (!FaceQueuePolicy.IsJobUpdate(response)) return;", StringComparison.Ordinal);
        var log = push.IndexOf("[ENGINE-SUB:PeopleView] {e.PropertyName}=faceRefresh", StringComparison.Ordinal);
        var dispatch = push.IndexOf("DispatcherQueue.TryEnqueue", StringComparison.Ordinal);
        Assert.True(filter >= 0 && log > filter && dispatch > log);
        Assert.Contains("CatalogJobActions.ControlRequest(action, job)", refresh, StringComparison.Ordinal);
        Assert.Contains("FaceQueuePolicy.EnqueueRequest(Guid.NewGuid().ToString())", refresh, StringComparison.Ordinal);
        Assert.DoesNotContain("new SolidColorBrush", refresh, StringComparison.Ordinal);
    }

    private static string ReadSource(string relativePath)
    {
        for (var directory = new DirectoryInfo(AppContext.BaseDirectory); directory is not null; directory = directory.Parent)
        {
            var path = Path.Combine(directory.FullName, "platforms/windows/src/FileID.App", relativePath);
            if (File.Exists(path)) return File.ReadAllText(path);
        }
        throw new DirectoryNotFoundException("Windows app sources were not found.");
    }
}
