using FileID.IpcSchema;
using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class ChatFrontendPolicyTests
{
    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public void RequestedGenerationRemainsOffForOriginalAdapter(bool requested)
    {
        Assert.False(ChatFrontendPolicy.SupportsModelGeneration);
        Assert.False(ChatFrontendPolicy.SupportsCancellation);
        Assert.False(ChatFrontendPolicy.UseModelForSend(requested));
    }

    [Fact]
    public void EmptyHistoryRetainsCitationsWhileExplicitHistoryHitsReplaceThem()
    {
        var history = Response("completed");
        Assert.False(ChatFrontendPolicy.ShouldReplaceCitations("history", history));
        var hit = new CatalogHit(1, "fixture.txt", "document", "evidence", null, null, null);
        Assert.True(ChatFrontendPolicy.ShouldReplaceCitations("history", history with { Hits = new[] { hit } }));
    }

    [Theory]
    [InlineData("send")]
    [InlineData("clear")]
    public void AcceptedFreshSendOrClearReplacesEvenEmptyCitations(string action)
    {
        Assert.True(ChatFrontendPolicy.ShouldReplaceCitations(action, Response("completed")));
    }

    [Theory]
    [InlineData("send")]
    [InlineData("history")]
    [InlineData("clear")]
    public void FailedRequestsDoNotReplaceExistingEvidence(string action)
    {
        Assert.False(ChatFrontendPolicy.ShouldReplaceCitations(action, Response("error")));
    }

    private static ChatResponse Response(string status) =>
        new("request", "conversation", status, "snapshot", Array.Empty<ChatMessage>(), Array.Empty<CatalogHit>());
}
