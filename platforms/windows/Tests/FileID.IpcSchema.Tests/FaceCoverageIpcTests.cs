using System.Text.Json;
using Xunit;

namespace FileID.IpcSchema.Tests;

public class FaceCoverageIpcTests
{
    [Fact]
    public void LegacyResultRetainsCoverageOmission()
    {
        const string json = "{\"personCount\":1,\"faceCount\":2,\"unmatchedFaces\":0,\"durationSeconds\":1}";
        var result = JsonSerializer.Deserialize<FaceClusteringResult>(json, IpcCoder.Options)!;
        Assert.Null(result.DeferredFaces);
        using var encoded = JsonDocument.Parse(JsonSerializer.Serialize(result, IpcCoder.Options));
        Assert.False(encoded.RootElement.TryGetProperty("deferredFaces", out _));
    }

    [Fact]
    public void CurrentResultRetainsDeferredCount()
    {
        const string json = "{\"personCount\":1,\"faceCount\":2,\"unmatchedFaces\":0,\"durationSeconds\":1,\"deferredFaces\":4}";
        var result = JsonSerializer.Deserialize<FaceClusteringResult>(json, IpcCoder.Options)!;
        Assert.Equal(4UL, result.DeferredFaces);
        using var encoded = JsonDocument.Parse(JsonSerializer.Serialize(result, IpcCoder.Options));
        Assert.Equal(4UL, encoded.RootElement.GetProperty("deferredFaces").GetUInt64());
    }
}
