using FileID.IpcSchema;
using FileID.Services;
using Xunit;

namespace FileID.App.Tests;

public sealed class EngineSimilarityTests
{
    private static float[] UnitVector()
    {
        var vector = new float[CatalogEmbeddingSpace.Dimension];
        vector[0] = 1f;
        return vector;
    }

    private static CatalogHit Hit(long id) => new(id, $"C:/{id}.jpg", "image", "", null, null, null);

    private static CatalogResponse Response(string status, string? message, params CatalogHit[] hits) =>
        new("r", status, message, hits, Array.Empty<CatalogChapter>(), Array.Empty<CatalogJob>());

    [Fact]
    public void SeedRequestAsksForFilesLikeOneFileInTheChosenLocation()
    {
        var request = EngineSimilarity.SeedRequest(42, 7, 300);

        Assert.Equal("search", request.Action);
        Assert.Equal("semantic", request.SearchMode);
        Assert.Equal(42, request.FileID);
        Assert.Equal(7, request.RootID);
        Assert.Equal("files", request.ResultScope);
        Assert.Equal(EngineSimilarity.MaxResults, request.Limit);
        Assert.Null(request.QueryVector);
        Assert.Null(request.EmbeddingModel);
        Assert.Null(request.Query);
    }

    [Fact]
    public void QueryRequestCarriesTheVectorAndItsModel()
    {
        var request = EngineSimilarity.QueryRequest("  beach  ", UnitVector(), null, 60);

        Assert.NotNull(request);
        Assert.Equal("beach", request!.Query);
        Assert.Equal("semantic", request.SearchMode);
        Assert.Equal(CatalogEmbeddingSpace.ModelID, request.EmbeddingModel);
        Assert.Equal(CatalogEmbeddingSpace.Dimension, request.QueryVector!.Count);
        Assert.Equal(60, request.Limit);
        Assert.Null(request.FileID);
        Assert.Null(request.RootID);
    }

    [Fact]
    public void QueryRequestIsRefusedForAVectorTheHelperWouldReject()
    {
        Assert.Null(EngineSimilarity.QueryRequest("beach", new float[CatalogEmbeddingSpace.Dimension], null, 60));
        Assert.Null(EngineSimilarity.QueryRequest("beach", new float[3], null, 60));
        Assert.Null(EngineSimilarity.QueryRequest("   ", UnitVector(), null, 60));
        Assert.Null(EngineSimilarity.QueryRequest(new string('a', EngineSimilarity.MaxQueryLength + 1), UnitVector(), null, 60));
    }

    [Fact]
    public void RankedIdsKeepTheHelperOrderAndDropRepeats()
    {
        var ids = EngineSimilarity.RankedIds(Response("ok", "Visual similarity results.", Hit(5), Hit(2), Hit(5), Hit(9)));

        Assert.Equal(new long[] { 5, 2, 9 }, ids);
    }

    [Theory]
    [InlineData("error", "Preparing the local visual search index. Similarity results are not available yet.", true)]
    [InlineData("indexing", null, true)]
    [InlineData("error", "Invalid visual query vector or model", false)]
    [InlineData("ok", null, false)]
    public void AnythingButARankingMeansRankInTheApp(string status, string? message, bool pending)
    {
        var response = Response(status, message);

        Assert.Null(EngineSimilarity.RankedIds(response));
        Assert.Equal(pending, EngineSimilarity.IsPending(response));
    }
}
