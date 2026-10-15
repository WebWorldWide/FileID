using FileID.IpcSchema;
using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class ChatRequestStateTests
{
    [Fact]
    public void StreamToFinalRequiresExactRequestConversationAndGeneration()
    {
        var state = new ChatRequestState();
        state.Begin(new ChatRequest("send", "conversation", "send", "query", true), 4);
        Assert.False(state.Apply(Response("other", "conversation", "streaming"), 4));
        Assert.False(state.Apply(Response("send", "other", "streaming"), 4));
        Assert.False(state.Apply(Response("send", "conversation", "streaming"), 3));
        Assert.True(state.Apply(Response("send", "conversation", "streaming"), 4));
        Assert.True(state.HasActiveSend);
        Assert.True(state.Apply(Response("send", "conversation", "completed"), 4));
        Assert.False(state.HasActiveSend);
        Assert.False(state.Apply(Response("send", "conversation", "streaming"), 4));
    }

    [Fact]
    public void StopAcknowledgementWaitsForOwnTerminalAndRejectsLateStreamAfterClear()
    {
        var state = new ChatRequestState();
        state.Begin(new ChatRequest("send", "conversation", "send"), 4);
        var cancel = state.Cancel(4);
        Assert.Equal("cancel", cancel.Action);
        Assert.Equal("conversation", cancel.ConversationID);
        Assert.NotEqual("send", cancel.RequestID);
        Assert.True(state.HasActiveSend);
        Assert.False(state.CanStop);
        Assert.True(state.Apply(Response("send", "conversation", "streaming"), 4));
        Assert.True(state.Apply(Response(cancel.RequestID, "conversation", "cancelled"), 4));
        Assert.True(state.HasActiveSend);
        Assert.True(state.CanStop);
        Assert.Equal("send", state.RequestID);
        Assert.Throws<InvalidOperationException>(() => state.Begin(new ChatRequest("clear", "conversation", "clear"), 4));
        Assert.True(state.Apply(Response("send", "conversation", "cancelled"), 4));
        Assert.False(state.HasActiveSend);
        state.Begin(new ChatRequest("clear", "conversation", "clear"), 4);
        Assert.False(state.Apply(Response("send", "conversation", "completed"), 4));
        Assert.True(state.Apply(Response("clear", "conversation", "completed"), 4));
        Assert.False(state.Apply(Response("send", "conversation", "streaming"), 4));
    }

    [Fact]
    public void RestartConversationChangeAndStopFailureNeverTransferOwnership()
    {
        var state = new ChatRequestState();
        state.Begin(new ChatRequest("old", "first", "send"), 4);
        Assert.Throws<InvalidOperationException>(() => state.Cancel(5));
        Assert.Throws<InvalidOperationException>(() => state.Begin(new ChatRequest("clear", "first", "clear"), 4));
        var cancel = state.Cancel(4);
        Assert.True(state.Apply(Response(cancel.RequestID, "first", "error"), 4));
        Assert.True(state.CanStop);
        state.Reset();
        state.Begin(new ChatRequest("new", "second", "send"), 5);
        Assert.False(state.Apply(Response("old", "first", "completed"), 4));
        Assert.False(state.Apply(Response(cancel.RequestID, "first", "cancelled"), 5));
        Assert.True(state.HasActiveSend);
    }

    [Theory]
    [InlineData("completed")]
    [InlineData("error")]
    [InlineData("cancelled")]
    public void AckBeforeOwnTerminalKeepsExactOwnerAndClassifiesTerminal(string terminalStatus)
    {
        var state = new ChatRequestState();
        state.Begin(new ChatRequest("send", "conversation", "send"), 4);
        var cancel = state.Cancel(4);
        var acknowledgement = Response(cancel.RequestID, "conversation", "cancelled");
        Assert.Equal(ChatResponseOwner.CancellationAcknowledgement, state.GetResponseOwner(acknowledgement, 4));
        Assert.True(state.Apply(acknowledgement, 4));
        Assert.True(state.HasActiveSend);
        Assert.Equal("send", state.RequestID);
        Assert.Equal("send", state.Action);
        Assert.Equal(4, state.Generation);
        Assert.False(state.Apply(Response("other", "conversation", terminalStatus), 4));
        Assert.False(state.Apply(Response("send", "other", terminalStatus), 4));
        Assert.False(state.Apply(Response("send", "conversation", terminalStatus), 5));
        Assert.True(state.HasActiveSend);
        Assert.Equal(ChatResponseOwner.None, state.GetResponseOwner(acknowledgement, 4));
        Assert.False(state.Apply(acknowledgement, 4));
        var terminal = Response("send", "conversation", terminalStatus);
        Assert.Equal(ChatResponseOwner.Request, state.GetResponseOwner(terminal, 4));
        Assert.True(state.Apply(terminal, 4));
        Assert.False(state.HasActiveSend);
        Assert.False(state.Apply(terminal, 4));
    }

    [Theory]
    [InlineData("retrieving")]
    [InlineData("queued")]
    [InlineData("streaming")]
    public void OwnBusyAfterAckKeepsSendIdentityUntilOwnTerminal(string busyStatus)
    {
        var state = new ChatRequestState();
        state.Begin(new ChatRequest("send", "conversation", "send"), 4);
        var cancel = state.Cancel(4);
        Assert.True(state.Apply(Response(cancel.RequestID, "conversation", "cancelled"), 4));
        var busy = Response("send", "conversation", busyStatus);
        Assert.Equal(ChatResponseOwner.Request, state.GetResponseOwner(busy, 4));
        Assert.True(state.Apply(busy, 4));
        Assert.True(state.HasActiveSend);
        Assert.Equal("send", state.RequestID);
        Assert.True(state.Apply(Response("send", "conversation", "completed"), 4));
        Assert.False(state.HasActiveSend);
    }

    [Fact]
    public void CancellationAcknowledgementRequiresExactRequestConversationAndGeneration()
    {
        var state = new ChatRequestState();
        state.Begin(new ChatRequest("send", "conversation", "send"), 4);
        var cancel = state.Cancel(4);
        foreach (var response in new[]
        {
            Response("wrong-request", "conversation", "cancelled"),
            Response(cancel.RequestID, "wrong-conversation", "cancelled")
        })
        {
            Assert.Equal(ChatResponseOwner.None, state.GetResponseOwner(response, 4));
            Assert.False(state.Apply(response, 4));
        }
        var acknowledgement = Response(cancel.RequestID, "conversation", "cancelled");
        Assert.Equal(ChatResponseOwner.None, state.GetResponseOwner(acknowledgement, 5));
        Assert.False(state.Apply(acknowledgement, 5));
        Assert.False(state.Apply(Response(cancel.RequestID, "conversation", "streaming"), 4));
        Assert.True(state.HasActiveSend);
        Assert.False(state.CanStop);
        Assert.Equal(ChatResponseOwner.CancellationAcknowledgement, state.GetResponseOwner(acknowledgement, 4));
        Assert.True(state.Apply(acknowledgement, 4));
        Assert.True(state.HasActiveSend);
        Assert.True(state.CanStop);
    }

    [Fact]
    public void HistoryCannotUnlockPendingSendAfterAck()
    {
        var state = new ChatRequestState();
        state.Begin(new ChatRequest("send", "conversation", "send"), 4);
        var cancel = state.Cancel(4);
        Assert.True(state.Apply(Response(cancel.RequestID, "conversation", "cancelled"), 4));
        var history = Response("history", "conversation", "completed");
        Assert.Equal(ChatResponseOwner.None, state.GetResponseOwner(history, 4));
        Assert.False(state.Apply(history, 4));
        Assert.True(state.HasActiveSend);
        Assert.Throws<InvalidOperationException>(() => state.Begin(new ChatRequest("clear", "conversation", "clear"), 4));
        Assert.Throws<InvalidOperationException>(() => state.Begin(new ChatRequest("next", "conversation", "send"), 4));
        Assert.True(state.Apply(Response("send", "conversation", "error"), 4));
        state.Begin(new ChatRequest("history", "conversation", "history"), 4);
        Assert.True(state.Apply(history, 4));
    }

    [Fact]
    public void UnloadResetRejectsLateAcknowledgementAndOwnTerminal()
    {
        var state = new ChatRequestState();
        state.Begin(new ChatRequest("send", "conversation", "send"), 4);
        var cancel = state.Cancel(4);
        state.Reset();
        foreach (var response in new[]
        {
            Response(cancel.RequestID, "conversation", "cancelled"),
            Response("send", "conversation", "completed")
        })
        {
            Assert.Equal(ChatResponseOwner.None, state.GetResponseOwner(response, 4));
            Assert.False(state.Apply(response, 4));
        }
        Assert.False(state.HasActiveSend);
        Assert.False(state.CanStop);
    }

    private static ChatResponse Response(string id, string conversation, string status) =>
        new(id, conversation, status, "snapshot", Array.Empty<ChatMessage>(), Array.Empty<CatalogHit>());
}
