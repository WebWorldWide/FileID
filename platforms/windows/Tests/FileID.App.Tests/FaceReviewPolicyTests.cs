using FileID.IpcSchema;
using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class FaceReviewPolicyTests
{
    private static readonly long[] RemainingFaces = { 1, 3 };

    private static FaceReviewItem Item(long face = 10, string reason = "suggested", double score = 0.62, string? name = "Alex",
        long? runnerId = null, string? runnerName = null, string path = "/photos/2024/IMG_0001.jpg") =>
        new(face, 42, path, "0.1,0.1,0.2,0.2", 2, name, 20, score, reason, runnerId, runnerName, runnerId is null ? null : 0.6, 100);

    private static FaceReviewPage Page(long total, params FaceReviewItem[] items) =>
        new("review-1", "list", "ok", null, null, null, null, total, 0, items);

    [Fact]
    public void ReasonSentencesMatchTheSharedWording()
    {
        Assert.Equal("Looks like Alex (62% match).", FaceReviewPolicy.Reason(Item()));
        Assert.Equal("Might be Alex, but the match is weak (47%).", FaceReviewPolicy.Reason(Item(reason: "uncertain", score: 0.465)));
        Assert.Equal("Could be Alex or Sam; both look about as close.", FaceReviewPolicy.Reason(Item(reason: "ambiguous", runnerId: 3, runnerName: "Sam")));
        Assert.Equal("Could be Alex or an unnamed person; both look about as close.", FaceReviewPolicy.Reason(Item(reason: "ambiguous", runnerId: 3)));
        Assert.Equal("Could be Alex, but it's a close call.", FaceReviewPolicy.Reason(Item(reason: "ambiguous")));
        Assert.Equal("Looks like an unnamed person (62% match).", FaceReviewPolicy.Reason(Item(name: "  ")));
    }

    [Fact]
    public void PercentRoundsAndClamps()
    {
        Assert.Equal(62, FaceReviewPolicy.Percent(0.62));
        Assert.Equal(56, FaceReviewPolicy.Percent(0.555));
        Assert.Equal(0, FaceReviewPolicy.Percent(-0.2));
        Assert.Equal(100, FaceReviewPolicy.Percent(1.3));
        Assert.Equal(0, FaceReviewPolicy.Percent(double.NaN));
    }

    [Fact]
    public void AccessibilityLabelsNameThePersonAndFile()
    {
        var item = Item(path: @"C:\Pictures\Beach.jpg");
        Assert.Equal("Face in Beach.jpg", FaceReviewPolicy.FaceLabel(item));
        Assert.Equal("Alex", FaceReviewPolicy.PersonLabel(item));
        Assert.Equal("Accept: this is Alex", FaceReviewPolicy.AcceptLabel(item));
        Assert.Equal("Not Alex", FaceReviewPolicy.RejectLabel(item));
        Assert.Equal("Ignore this face", FaceReviewPolicy.IgnoreLabel);
        Assert.Equal("Undo the last review", FaceReviewPolicy.UndoLabel);
        Assert.Equal("Looks like Alex (62% match). In Beach.jpg.", FaceReviewPolicy.RowLabel(item));
        Assert.Equal("Review suggestions, 3 waiting", FaceReviewPolicy.Header(3));
        Assert.Equal("No suggestions to review.", FaceReviewPolicy.EmptyText);
    }

    [Fact]
    public void FailureTextComesFromTheEngine()
    {
        Assert.Null(FaceReviewPolicy.Failure(Page(0)));
        var refused = new FaceReviewPage("r", "resolve", "refused", "That person is already in this photo.", null, null, null, 1, 0, Array.Empty<FaceReviewItem>());
        Assert.Equal("That person is already in this photo.", FaceReviewPolicy.Failure(refused));
        Assert.Equal("Nothing changed.", FaceReviewPolicy.Failure(refused with { Message = null }));
        Assert.Equal("Accepted a face as Alex", FaceReviewPolicy.ChangeLabel(FaceReviewPolicy.Accept, "Alex"));
        Assert.Equal("Marked a face as not an unnamed person", FaceReviewPolicy.ChangeLabel(FaceReviewPolicy.Reject, null));
        Assert.Equal("Ignored a face", FaceReviewPolicy.ChangeLabel(FaceReviewPolicy.Ignore, "Alex"));
    }

    [Fact]
    public void QueuePagesDedupesAndRemoves()
    {
        var queue = new FaceReviewQueue();
        Assert.True(queue.IsEmpty);
        Assert.Equal(2, queue.Reset(Page(3, Item(1), Item(2))).Count);
        Assert.True(queue.HasMore);
        Assert.Equal(2, queue.NextOffset);
        Assert.Single(queue.Append(Page(3, Item(2), Item(3))));
        Assert.False(queue.HasMore);
        Assert.True(queue.Remove(2));
        Assert.False(queue.Remove(2));
        Assert.Equal(2, queue.Total);
        Assert.Equal(RemainingFaces, queue.Items.Select(item => item.FaceId).ToArray());
        var refused = new FaceReviewPage("r", "list", "refused", "x", null, null, null, 0, 0, Array.Empty<FaceReviewItem>());
        Assert.Empty(queue.Append(refused));
        Assert.Equal(2, queue.Total);
        queue.Reset(Page(0));
        Assert.True(queue.IsEmpty);
    }
}
