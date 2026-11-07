using System.Text.Json;
using Xunit;

namespace FileID.IpcSchema.Tests;

public class TakeGroupingIpcTests
{
    private static readonly string[] ExpectedEvidence = ["captureSession", "visualSimilarity", "simultaneousCameras"];

    [Fact]
    public void ZeroSetupGroupsOmitRelationAndEvidence()
    {
        const string json = "{\"groupKey\":\"takes-1\",\"kind\":\"photos\",\"status\":\"open\",\"capturedAt\":10,\"similarity\":0.9,\"shots\":[{\"primaryFileID\":1,\"fileIDs\":[1],\"path\":\"/a.jpg\"},{\"primaryFileID\":2,\"fileIDs\":[2],\"path\":\"/b.jpg\"}]}";
        var group = JsonSerializer.Deserialize<CatalogTakeAutoGroup>(json, IpcCoder.Options)!;
        Assert.Null(group.Relation);
        Assert.Null(group.Evidence);
        using var encoded = JsonDocument.Parse(JsonSerializer.Serialize(group, IpcCoder.Options));
        Assert.False(encoded.RootElement.TryGetProperty("relation", out _));
        Assert.False(encoded.RootElement.TryGetProperty("evidence", out _));
    }

    [Fact]
    public void ProposalsCarryRelationAndEvidence()
    {
        const string json = "{\"groupKey\":\"takes-2\",\"kind\":\"videos\",\"status\":\"skipped\",\"capturedAt\":10,\"similarity\":0.9,\"shots\":[{\"primaryFileID\":1,\"fileIDs\":[1],\"path\":\"/a.mov\"},{\"primaryFileID\":2,\"fileIDs\":[2],\"path\":\"/b.mov\"}],\"relation\":\"alternateViews\",\"evidence\":[\"captureSession\",\"visualSimilarity\",\"simultaneousCameras\"]}";
        var group = JsonSerializer.Deserialize<CatalogTakeAutoGroup>(json, IpcCoder.Options)!;
        Assert.Equal("alternateViews", group.Relation);
        Assert.Equal(ExpectedEvidence, group.Evidence);
        using var encoded = JsonDocument.Parse(JsonSerializer.Serialize(group, IpcCoder.Options));
        Assert.Equal("alternateViews", encoded.RootElement.GetProperty("relation").GetString());
        Assert.Equal(3, encoded.RootElement.GetProperty("evidence").GetArrayLength());
    }

    [Fact]
    public void GroupingActionsRoundTrip()
    {
        var request = new CatalogRequest("r", "enqueueTakeGrouping", null, null, null, null, null, null);
        using var encoded = JsonDocument.Parse(JsonSerializer.Serialize(request, IpcCoder.Options));
        Assert.Equal("enqueueTakeGrouping", encoded.RootElement.GetProperty("action").GetString());
        var read = JsonSerializer.Deserialize<CatalogRequest>("{\"requestID\":\"r\",\"action\":\"takeGroupProposals\",\"limit\":5}", IpcCoder.Options)!;
        Assert.Equal("takeGroupProposals", read.Action);
        Assert.Equal(5, read.Limit);
    }
}
