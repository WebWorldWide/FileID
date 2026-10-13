using FileID.IpcSchema;
using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class ChatPresentationRevisionTests
{
    [Theory]
    [InlineData("completed")]
    [InlineData("streaming")]
    [InlineData("error")]
    [InlineData("reset")]
    [InlineData("generation")]
    public void HistoryCompletionThenNewerAcceptedUpdateRejectsDelayedRedraw(string update)
    {
        var revision = new ChatPresentationRevision();
        var send = new ChatRequest("send", "conversation", "send", "generated fixture");
        var historyRequest = new ChatRequest("history", "conversation", "history");
        var client = new ChatSendState();
        var pending = client.Begin(send, 7);
        var request = new ChatRequestState();
        request.Begin(send, 7);
        var display = new Display();
        var previousStream = Snapshot(send.RequestID, "streaming", "previous stream", 2);
        Assert.True(client.Apply(previousStream, 7));
        Assert.True(request.Apply(previousStream, 7));
        display.Show(previousStream);
        var cancel = request.Cancel(7);
        var stamp = revision.CaptureHistory(historyRequest, 7);
        var history = Snapshot(historyRequest.RequestID, "completed", "older history", 1);
        var historyResponse = new TaskCompletionSource<ChatResponse>();
        historyResponse.SetResult(history);
        var currentGeneration = 7;
        var rendered = true;
        var dispatcher = new Queue<Action>();
        dispatcher.Enqueue(() => rendered = revision.TryRenderHistory(stamp, historyResponse.Task.GetAwaiter().GetResult(),
            "conversation", currentGeneration, display.Show));

        if (update is "reset" or "generation")
        {
            revision.Invalidate();
            request.Reset();
            display.Status = "presentation reset";
            if (update == "generation") currentGeneration = 8;
        }
        else
        {
            var newer = Snapshot(send.RequestID, update, "newer send answer", 3);
            Assert.True(client.Apply(newer, 7));
            Assert.True(request.Apply(newer, 7));
            revision.Invalidate();
            display.Show(newer);
        }
        var expectedMessages = display.Messages;
        var expectedHits = display.Hits;
        var expectedStatus = display.Status;
        dispatcher.Dequeue()();
        Assert.False(rendered);
        Assert.Same(expectedMessages, display.Messages);
        Assert.Same(expectedHits, display.Hits);
        Assert.Equal(expectedStatus, display.Status);
        Assert.NotEmpty(display.Messages);
        Assert.NotEmpty(display.Hits);
        Assert.NotEqual(history.Messages[0].Text, display.Messages[0].Text);
        Assert.NotEqual(history.Hits[0].EvidenceID, display.Hits[0].EvidenceID);
        if (update is "streaming" or "reset" or "generation")
        {
            Assert.True(client.IsActive);
            Assert.False(pending.IsCompleted);
        }
        if (update == "streaming")
        {
            Assert.True(request.HasActiveSend);
            Assert.False(request.CanStop);
            Assert.Throws<InvalidOperationException>(() => request.Cancel(7));
            Assert.NotEqual(send.RequestID, cancel.RequestID);
        }
        if (client.IsActive) Assert.True(client.Apply(Snapshot(send.RequestID, "completed", "fixture terminal", 4), 7));
        Assert.True(pending.IsCompletedSuccessfully);
    }

    [Fact]
    public void UnchangedHistoryRendersNonemptySnapshotWithoutRetiringSendOrStopLatch()
    {
        var revision = new ChatPresentationRevision();
        var send = new ChatRequest("send", "conversation", "send");
        var client = new ChatSendState();
        var pending = client.Begin(send, 7);
        var request = new ChatRequestState();
        request.Begin(send, 7);
        request.Cancel(7);
        var historyRequest = new ChatRequest("history", "conversation", "history");
        var stamp = revision.CaptureHistory(historyRequest, 7);
        var history = Snapshot(historyRequest.RequestID, "completed", "unchanged history", 1);
        var display = new Display();
        Assert.True(revision.TryRenderHistory(stamp, history, "conversation", 7, display.Show));
        Assert.Equal("unchanged history", display.Messages[0].Text);
        Assert.Equal("evidence-1", display.Hits[0].EvidenceID);
        Assert.Equal("completed · unchanged history", display.Status);
        Assert.True(client.IsActive);
        Assert.False(pending.IsCompleted);
        Assert.True(request.HasActiveSend);
        Assert.False(request.CanStop);
        Assert.True(client.Apply(Snapshot(send.RequestID, "completed", "fixture terminal", 4), 7));
    }

    [Theory]
    [InlineData("request")]
    [InlineData("response-conversation")]
    [InlineData("current-conversation")]
    [InlineData("generation")]
    public void StaleIdentityNeverInvokesProductionRedraw(string stale)
    {
        var revision = new ChatPresentationRevision();
        var historyRequest = new ChatRequest("history", "conversation", "history");
        var stamp = revision.CaptureHistory(historyRequest, 7);
        var history = Snapshot(historyRequest.RequestID, "completed", "older history", 1);
        if (stale == "request") history = history with { RequestID = "other" };
        if (stale == "response-conversation") history = history with { ConversationID = "other" };
        var currentConversation = stale == "current-conversation" ? "other" : "conversation";
        var generation = stale == "generation" ? 8 : 7;
        var invoked = false;
        Assert.False(revision.TryRenderHistory(stamp, history, currentConversation, generation, _ => invoked = true));
        Assert.False(invoked);
    }

    [Theory]
    [InlineData("capture")]
    [InlineData("invalidate")]
    [InlineData("render")]
    public void RevisionOperationsRejectNonPresentationThread(string operation)
    {
        var revision = new ChatPresentationRevision();
        var request = new ChatRequest("history", "conversation", "history");
        var stamp = revision.CaptureHistory(request, 7);
        var response = Snapshot(request.RequestID, "completed", "history", 1);
        Exception? failure = null;
        var invoked = false;
        var thread = new Thread(() =>
        {
            try
            {
                if (operation == "capture") revision.CaptureHistory(request, 7);
                else if (operation == "invalidate") revision.Invalidate();
                else revision.TryRenderHistory(stamp, response, "conversation", 7, _ => invoked = true);
            }
            catch (Exception error) { failure = error; }
        });
        thread.Start();
        Assert.True(thread.Join(TimeSpan.FromSeconds(10)));
        Assert.IsType<InvalidOperationException>(failure);
        Assert.False(invoked);
    }

    private static ChatResponse Snapshot(string requestID, string status, string text, long fileID) => new(requestID, "conversation", status, text,
        new[] { new ChatMessage($"message-{fileID}", "assistant", text, 0) },
        new[] { new CatalogHit(fileID, $"generated-fixture-{fileID}.mp4", "sampledFrame", text, $"evidence-{fileID}", fileID, null) });

    private sealed class Display
    {
        public IReadOnlyList<ChatMessage> Messages { get; private set; } = Array.Empty<ChatMessage>();
        public IReadOnlyList<CatalogHit> Hits { get; private set; } = Array.Empty<CatalogHit>();
        public string Status { get; set; } = "initial";
        public void Show(ChatResponse response)
        {
            Status = $"{response.Status} · {response.Message}";
            if (response.Status == "error") return;
            Messages = response.Messages;
            Hits = response.Hits;
        }
    }
}
