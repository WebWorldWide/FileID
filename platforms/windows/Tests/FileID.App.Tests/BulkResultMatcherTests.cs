using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class BulkResultMatcherTests
{
    [Fact]
    public void ResultWithTheSentIdIsAccepted()
    {
        var matcher = new BulkResultMatcher("renameFiles");
        matcher.NoteSent("a");
        Assert.True(matcher.Accepts("renameFiles", "a"));
    }

    [Fact]
    public void ResultWithAnotherIdIsRejectedEvenWhenTheActionMatches()
    {
        var matcher = new BulkResultMatcher("renameFiles");
        matcher.NoteSent("a");
        Assert.False(matcher.Accepts("renameFiles", "b"));
    }

    [Fact]
    public void ResultWithoutAnIdFallsBackToThePrefix()
    {
        var matcher = new BulkResultMatcher("trashFiles");
        matcher.NoteSent("a");
        Assert.True(matcher.Accepts("trashFiles:batch-7", null));
        Assert.True(matcher.Accepts("trashFiles", ""));
        Assert.False(matcher.Accepts("renameFiles", null));
        Assert.False(matcher.Accepts(null, null));
    }

    [Fact]
    public void WaiterThatNeverSawItsIdStillMatchesByPrefix()
    {
        var matcher = new BulkResultMatcher("applyTags");
        Assert.True(matcher.Accepts("applyTags", "x"));
        Assert.False(matcher.Accepts("renameFiles", "x"));
    }

    [Fact]
    public void TwoSameActionRequestsInFlightGetTheirOwnResults()
    {
        var first = new BulkResultMatcher("renameFiles");
        var second = new BulkResultMatcher("renameFiles");
        first.NoteSent("one");
        second.NoteSent("two");

        Assert.True(first.Accepts("renameFiles", "one"));
        Assert.False(second.Accepts("renameFiles", "one"));
        Assert.True(second.Accepts("renameFiles", "two"));
        Assert.False(first.Accepts("renameFiles", "two"));
    }

    [Fact]
    public void ACommandThatSendsSeveralFramesAcceptsAnyOfThem()
    {
        var matcher = new BulkResultMatcher("applyTags");
        matcher.NoteSent("a");
        matcher.NoteSent("b");
        Assert.True(matcher.Accepts("applyTags", "a"));
        Assert.True(matcher.Accepts("applyTags", "b"));
        Assert.False(matcher.Accepts("applyTags", "c"));
    }
}
