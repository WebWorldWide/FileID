using System.Text;
using FileID.IpcSchema;
using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class ChatSendStateTests
{
    [Fact]
    public async Task TimeoutThenLateSerializedTerminalUnlocksHistoryWithoutInferringSuccess()
    {
        var send = new ChatRequest("send", "conversation", "send", "generated fixture query", true);
        var client = new ChatSendState();
        var view = new ChatRequestState();
        var pending = client.Begin(send, 4);
        view.Begin(send, 4);
        using var output = new MemoryStream();
        await WorkbenchDispatch.ObserveAsync((started, ct) => CommandWriteQueue.Enqueue(Task.CompletedTask, () =>
        {
            started();
            var bytes = IpcCoder.EncodeLine(IpcCommand.New(new ChatRequestCommand(send)));
            output.Write(bytes);
        }, ct), error => client.ConfirmUnsent(send, 4, error), CancellationToken.None);
        Assert.Contains("generated fixture query", Encoding.UTF8.GetString(output.ToArray()), StringComparison.Ordinal);
        await Assert.ThrowsAsync<TimeoutException>(() => pending.WaitAsync(TimeSpan.Zero));
        Assert.True(client.IsActive);
        Assert.True(view.HasActiveSend);
        Assert.Null(client.LastResponse);
        client.GuardMutation(new ChatRequest("history", "conversation", "history"));
        Assert.Throws<InvalidOperationException>(() => client.GuardMutation(new ChatRequest("clear", "conversation", "clear")));
        await Assert.ThrowsAsync<InvalidOperationException>(() => client.Begin(send with { RequestID = "conflicting" }, 4));

        var terminalGate = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        var terminal = EmitFixtureTerminalAsync(terminalGate.Task, send);
        Assert.False(terminal.IsCompleted);
        Assert.False(pending.IsCompleted);
        terminalGate.SetResult();
        var response = await terminal;
        Assert.True(client.Apply(response, 4));
        Assert.Same(response, await pending);
        Assert.True(view.Apply(response, 4));
        Assert.False(view.HasActiveSend);
        Assert.False(client.IsActive);
        var historyRequest = new ChatRequest("history", "conversation", "history");
        view.Begin(historyRequest, 4);
        var history = DecodeFixture(response with { RequestID = historyRequest.RequestID });
        using var requests = new WorkbenchRequests();
        var refreshed = requests.Register<ChatResponse>(historyRequest.RequestID, 4, historyRequest.ConversationID);
        Assert.True(requests.Apply(history.RequestID, 4, history, history.ConversationID));
        Assert.True(view.Apply(await refreshed, 4));
        Assert.False(client.Apply(response, 4));
        Assert.False(view.Apply(response, 4));
    }

    [Fact]
    public async Task UnloadedViewDoesNotRemoveClientOwnerAndStaleStreamsCannotResurrectClearedConversation()
    {
        var client = new ChatSendState();
        var view = new ChatRequestState();
        var request = new ChatRequest("send", "conversation", "send");
        var pending = client.Begin(request, 5);
        view.Begin(request, 5);
        var busy = DecodeFixture(Response(request, "streaming"));
        Assert.True(client.Apply(busy, 5));
        Assert.False(pending.IsCompleted);
        using var lifetime = new CancellationTokenSource();
        lifetime.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => pending.WaitAsync(lifetime.Token));
        view.Reset();
        Assert.True(client.IsActive);
        Assert.False(client.Apply(busy with { ConversationID = "other" }, 5));
        Assert.False(client.Apply(busy with { RequestID = "other" }, 5));
        Assert.False(client.Apply(busy, 6));
        var terminal = await EmitFixtureTerminalAsync(Task.CompletedTask, request);
        Assert.True(client.Apply(terminal, 5));
        await pending;
        view.Begin(new ChatRequest("clear", "conversation", "clear"), 5);
        Assert.False(client.Apply(busy, 5));
        Assert.False(client.Apply(terminal, 5));
        Assert.False(view.Apply(terminal, 5));
        Assert.True(view.Apply(DecodeFixture(terminal with { RequestID = "clear", Messages = Array.Empty<ChatMessage>() }), 5));
        Assert.False(view.Apply(busy, 5));
    }

    [Fact]
    public async Task RetirementRejectsLateTerminalAndAllowsOnlyNewGenerationHistoryRecovery()
    {
        var client = new ChatSendState();
        var request = new ChatRequest("old", "conversation", "send");
        var pending = client.Begin(request, 5);
        Assert.True(client.Apply(DecodeFixture(Response(request, "streaming")), 5));
        Assert.False(client.InvalidateGeneration(4));
        Assert.True(client.InvalidateGeneration(5));
        await Assert.ThrowsAsync<InvalidOperationException>(() => pending);
        Assert.Null(client.LastResponse);
        Assert.False(client.Apply(await EmitFixtureTerminalAsync(Task.CompletedTask, request), 5));
        client.GuardMutation(new ChatRequest("history", "conversation", "history"));
        var next = request with { RequestID = "new" };
        var nextPending = client.Begin(next, 6);
        Assert.False(client.Apply(Response(request, "completed"), 6));
        Assert.True(client.Apply(await EmitFixtureTerminalAsync(Task.CompletedTask, next), 6));
        await nextPending;
    }

    private static async Task<ChatResponse> EmitFixtureTerminalAsync(Task gate, ChatRequest request)
    {
        await gate;
        return DecodeFixture(Response(request, "completed"));
    }
    private static ChatResponse Response(ChatRequest request, string status) => new(request.RequestID, request.ConversationID, status,
        "Generated protocol fixture", Array.Empty<ChatMessage>(), Array.Empty<CatalogHit>());
    private static ChatResponse DecodeFixture(ChatResponse response)
    {
        var frame = IpcCoder.Encode(IpcEvent.Now(new ChatResponseEvent(response)));
        return ((ChatResponseEvent)IpcCoder.Decode<IpcEvent>(frame).Payload!).Response;
    }
}
