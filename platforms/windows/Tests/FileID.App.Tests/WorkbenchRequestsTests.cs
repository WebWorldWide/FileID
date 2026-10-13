using FileID.IpcSchema;
using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class WorkbenchRequestsTests
{
    [Fact]
    public async Task CatalogRequestsCorrelateByIDTypeAndGeneration()
    {
        using var requests = new WorkbenchRequests();
        var first = requests.Register<CatalogResponse>("first", 4);
        var second = requests.Register<CatalogResponse>("second", 4);
        var response = Catalog("second");
        Assert.False(requests.Apply("unknown", 4, response));
        Assert.False(requests.Apply("second", 3, response));
        Assert.False(requests.Apply("second", 4, "wrong response type"));
        Assert.False(first.IsCompleted);
        Assert.True(requests.Apply("second", 4, response));
        Assert.Same(response, await second);
        Assert.False(first.IsCompleted);
        Assert.False(requests.Apply("second", 4, response));
        requests.Apply("first", 4, Catalog("first"));
        await first;
    }

    [Fact]
    public async Task ChatSnapshotsRequireConversationAndOnlyTerminalSnapshotCompletes()
    {
        using var requests = new WorkbenchRequests();
        var pending = requests.Register<ChatResponse>("send", 7, "conversation");
        var busy = Chat("send", "conversation", "retrieving");
        Assert.False(requests.Apply("send", 7, busy, "other", terminal: false));
        Assert.True(requests.Apply("send", 7, busy, "conversation", terminal: false));
        Assert.False(pending.IsCompleted);
        var streaming = Chat("send", "conversation", "streaming");
        Assert.True(requests.Apply("send", 7, streaming, "conversation", terminal: false));
        Assert.False(pending.IsCompleted);
        Assert.True(requests.Apply("send", 7, streaming, "conversation", terminal: false));
        Assert.False(pending.IsCompleted);
        var completed = Chat("send", "conversation", "completed");
        Assert.True(requests.Apply("send", 7, completed, "conversation"));
        Assert.Same(completed, await pending);
        Assert.False(requests.Apply("send", 7, busy, "conversation", terminal: false));
    }

    [Fact]
    public async Task TimeoutAndUnloadedPanelRemoveOwnershipAndRejectLateReply()
    {
        using var requests = new WorkbenchRequests();
        var timedOut = requests.Register<ToolResponse>("timeout", 1);
        var unloaded = requests.Register<CatalogResponse>("unloaded", 1);
        requests.Remove<ToolResponse>("timeout", new TimeoutException("unknown outcome"));
        requests.Remove<CatalogResponse>("unloaded", new OperationCanceledException("closed"));
        await Assert.ThrowsAsync<TimeoutException>(() => timedOut);
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => unloaded);
        Assert.False(requests.Apply("unloaded", 1, Catalog("unloaded")));
        Assert.False(requests.Apply("timeout", 1, new ToolResponse("timeout", "ok", "", Array.Empty<ToolOutput>(), Array.Empty<ToolCapability>())));
    }

    [Fact]
    public async Task RestartFailsOnlyRetiredGenerationAndDisposalRejectsNewRequests()
    {
        var requests = new WorkbenchRequests();
        var retired = requests.Register<CatalogResponse>("old", 1);
        var current = requests.Register<CatalogResponse>("new", 2);
        requests.FailGeneration(1);
        await Assert.ThrowsAsync<InvalidOperationException>(() => retired);
        Assert.False(current.IsCompleted);
        Assert.False(requests.Apply("old", 1, Catalog("old")));
        requests.Dispose();
        await Assert.ThrowsAsync<ObjectDisposedException>(() => current);
        await Assert.ThrowsAsync<ObjectDisposedException>(() => requests.Register<CatalogResponse>("after-dispose", 2));
    }

    [Theory]
    [InlineData("retrieving", true)]
    [InlineData("queued", true)]
    [InlineData("streaming", true)]
    [InlineData("completed", false)]
    [InlineData("error", false)]
    [InlineData("cancelled", false)]
    public void ChatBusyReflectsBackendStatus(string status, bool busy) => Assert.Equal(busy, WorkbenchRequests.IsChatBusy(status));

    private static CatalogResponse Catalog(string id) => new(id, "ok", null, Array.Empty<CatalogHit>(), Array.Empty<CatalogChapter>(), Array.Empty<CatalogJob>());
    private static ChatResponse Chat(string id, string conversation, string status) => new(id, conversation, status, "", Array.Empty<ChatMessage>(), Array.Empty<CatalogHit>());
}
