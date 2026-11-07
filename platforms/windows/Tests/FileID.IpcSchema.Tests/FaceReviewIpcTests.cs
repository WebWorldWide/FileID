using System.Text.Json;
using Xunit;

namespace FileID.IpcSchema.Tests;

public class FaceReviewIpcTests
{
    [Fact]
    public void ListOmitsUnsetFilters()
    {
        var json = IpcCoder.Encode(new IpcCommand("c", new ListFaceReviewCommand("review-1")));
        using var doc = JsonDocument.Parse(json);
        var body = doc.RootElement.GetProperty("payload").GetProperty("listFaceReview");
        Assert.Equal("review-1", body.GetProperty("requestID").GetString());
        Assert.False(body.TryGetProperty("personID", out _));
        Assert.False(body.TryGetProperty("offset", out _));
        Assert.False(body.TryGetProperty("limit", out _));
    }

    [Fact]
    public void ResolveAndUndoUseSchemaKeys()
    {
        using var resolve = JsonDocument.Parse(IpcCoder.Encode(new IpcCommand("c", new ResolveFaceReviewCommand("review-2", 10, 2, "reject"))));
        var body = resolve.RootElement.GetProperty("payload").GetProperty("resolveFaceReview");
        Assert.Equal(10, body.GetProperty("faceID").GetInt64());
        Assert.Equal(2, body.GetProperty("personID").GetInt64());
        Assert.Equal("reject", body.GetProperty("resolution").GetString());
        using var undo = JsonDocument.Parse(IpcCoder.Encode(new IpcCommand("c", new UndoFaceReviewCommand("review-3", "op-1"))));
        Assert.Equal("op-1", undo.RootElement.GetProperty("payload").GetProperty("undoFaceReview").GetProperty("operationID").GetString());
    }

    [Fact]
    public void ReplyDecodesFromEngineWire()
    {
        const string line = "{\"t\":\"2026-11-07T12:00:00Z\",\"payload\":{\"faceReview\":{\"_0\":{\"requestID\":\"review-1\",\"action\":\"list\",\"status\":\"ok\",\"total\":3,\"offset\":0,\"items\":[{\"faceID\":10,\"fileID\":42,\"path\":\"/p/a.jpg\",\"bbox\":\"0.1,0.1,0.2,0.2\",\"personID\":2,\"personName\":\"Alex\",\"representativeFaceID\":20,\"score\":0.62,\"reason\":\"suggested\",\"runnerUpPersonID\":3,\"runnerUpScore\":0.41,\"createdAt\":100}]}}}}";
        var ev = IpcCoder.Decode<IpcEvent>(line);
        var page = Assert.IsType<FaceReviewEvent>(ev.Payload).Page;
        Assert.Equal("review-1", page.RequestId);
        Assert.Equal(3, page.Total);
        Assert.Null(page.OperationId);
        var item = Assert.Single(page.Items);
        Assert.Equal(10, item.FaceId);
        Assert.Equal("Alex", item.PersonName);
        Assert.Null(item.RunnerUpName);
        Assert.Equal(0.41, item.RunnerUpScore);
    }

    [Fact]
    public void ResolveReplyCarriesOperation()
    {
        const string line = "{\"t\":\"2026-11-07T12:00:00Z\",\"payload\":{\"faceReview\":{\"_0\":{\"requestID\":\"review-2\",\"action\":\"resolve\",\"status\":\"ok\",\"operationID\":\"op-1\",\"faceID\":10,\"personID\":2,\"total\":2,\"offset\":0,\"items\":[]}}}}";
        var page = Assert.IsType<FaceReviewEvent>(IpcCoder.Decode<IpcEvent>(line).Payload).Page;
        Assert.Equal("op-1", page.OperationId);
        Assert.Equal(2, page.PersonId);
        Assert.Empty(page.Items);
    }

    [Fact]
    public void ClusteringResultCarriesReviewQueued()
    {
        const string withCount = "{\"t\":\"2026-11-07T12:00:00Z\",\"payload\":{\"faceClusteringComplete\":{\"_0\":{\"personCount\":3,\"faceCount\":10,\"unmatchedFaces\":1,\"durationSeconds\":0.5,\"reviewQueued\":4}}}}";
        Assert.Equal(4, Assert.IsType<FaceClusteringCompleteEvent>(IpcCoder.Decode<IpcEvent>(withCount).Payload).Result.ReviewQueued);
        using var encoded = JsonDocument.Parse(IpcCoder.Encode(IpcEvent.Now(new FaceClusteringCompleteEvent(new FaceClusteringResult(3, 10, 1, 0.5)))));
        Assert.False(encoded.RootElement.GetProperty("payload").GetProperty("faceClusteringComplete").GetProperty("_0").TryGetProperty("reviewQueued", out _));
    }
}
