using System.Text.Json;
using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class CatalogEmbeddingSpaceTests
{
    [Fact]
    public void IdentityMatchesSwiftDescriptorAndCanonicalManifestPins()
    {
        Assert.Equal("clip-vitb32-openai-v1:126bacbb4038a0820ca6582bd81f58d7892070deab8317b390e37d7632a53471", CatalogEmbeddingSpace.ModelID);
        var root = FindRepoRoot();
        using var manifest = JsonDocument.Parse(File.ReadAllText(Path.Combine(root, "shared", "models", "manifest.json")));
        foreach (var pin in CatalogEmbeddingSpace.ArtifactPins)
        {
            var artifact = manifest.RootElement.GetProperty("artifacts").EnumerateArray().Single(item => item.GetProperty("id").GetString() == pin.Key);
            Assert.Equal(artifact.GetProperty("sha256").GetString(), pin.Value);
        }
    }

    [Fact]
    public void VectorValidationRejectsMissingWrongDimensionNonFiniteAndUnnormalizedData()
    {
        Assert.False(CatalogEmbeddingSpace.IsValidVector(Array.Empty<float>()));
        Assert.False(CatalogEmbeddingSpace.IsValidVector(new float[511]));
        var vector = new float[512];
        Assert.False(CatalogEmbeddingSpace.IsValidVector(vector));
        vector[0] = float.NaN;
        Assert.False(CatalogEmbeddingSpace.IsValidVector(vector));
        vector[0] = float.PositiveInfinity;
        Assert.False(CatalogEmbeddingSpace.IsValidVector(vector));
        vector[0] = 2;
        Assert.False(CatalogEmbeddingSpace.IsValidVector(vector));
        vector[0] = 1;
        Assert.True(CatalogEmbeddingSpace.IsValidVector(vector));
    }

    [Theory]
    [InlineData("hybrid")]
    [InlineData("semantic")]
    public void SemanticRequestsUseRealValidatedVectorAndExactIdentity(string mode)
    {
        var vector = new float[512];
        vector[0] = 1;
        var request = CatalogEmbeddingSpace.SearchRequest("birthday", mode, vector);
        Assert.Equal(mode, request.SearchMode);
        Assert.Equal(CatalogEmbeddingSpace.ModelID, request.EmbeddingModel);
        Assert.Equal(vector, request.QueryVector);
        Assert.NotSame(vector, request.QueryVector);
        Assert.Throws<InvalidOperationException>(() => CatalogEmbeddingSpace.SearchRequest("birthday", mode));
        Assert.Throws<InvalidOperationException>(() => CatalogEmbeddingSpace.SearchRequest("birthday", mode, Array.Empty<float>()));
    }

    [Fact]
    public void KeywordFallbackDoesNotSendAnyModelOrSyntheticVector()
    {
        var request = CatalogEmbeddingSpace.SearchRequest("birthday", "keyword");
        Assert.Null(request.QueryVector);
        Assert.Null(request.EmbeddingModel);
        Assert.Equal("keyword", request.SearchMode);
        Assert.Equal(100, request.Limit);
    }

    [Fact]
    public async Task EmbeddingRequiresExactQueryIDQueryAndGenerationAndRejectsStaleReply()
    {
        using var requests = new WorkbenchRequests();
        var pending = requests.Register<ClipTextEmbedding>("q", 9, "birthday");
        var response = new ClipTextEmbedding("q", "birthday", Array.Empty<float>());
        Assert.False(requests.Apply("other", 9, response, "birthday"));
        Assert.False(requests.Apply("q", 8, response, "birthday"));
        Assert.False(requests.Apply("q", 9, response, "different query"));
        Assert.False(pending.IsCompleted);
        requests.FailGeneration(9);
        await Assert.ThrowsAsync<InvalidOperationException>(() => pending);
        Assert.False(requests.Apply("q", 9, response, "birthday"));
        var current = requests.Register<ClipTextEmbedding>("new", 10, "birthday");
        var actual = new ClipTextEmbedding("new", "birthday", Array.Empty<float>());
        Assert.True(requests.Apply("new", 10, actual, "birthday"));
        Assert.Same(actual, await current);
        Assert.False(CatalogEmbeddingSpace.IsValidVector(actual.Embedding));
    }

    [Fact]
    public void ExistingCatalogDTOCarriesVectorIdentityAndRetainsUnavailableStatus()
    {
        var vector = new float[512];
        vector[0] = 1;
        var request = CatalogEmbeddingSpace.SearchRequest("birthday", "hybrid", vector);
        CommandPayload command = new CatalogRequestCommand(request);
        var json = JsonSerializer.Serialize(command, IpcCoder.Options);
        var decoded = Assert.IsType<CatalogRequestCommand>(IpcCoder.Decode<CommandPayload>(json));
        Assert.Equal("hybrid", decoded.Request.SearchMode);
        Assert.Equal(CatalogEmbeddingSpace.ModelID, decoded.Request.EmbeddingModel);
        Assert.Equal(vector, decoded.Request.QueryVector);
        EventPayload error = new CatalogResponseEvent(new CatalogResponse(request.RequestID, "error", "Index unavailable",
            Array.Empty<CatalogHit>(), Array.Empty<CatalogChapter>(), Array.Empty<CatalogJob>()));
        var responseJSON = JsonSerializer.Serialize(error, IpcCoder.Options);
        var response = Assert.IsType<CatalogResponseEvent>(IpcCoder.Decode<EventPayload>(responseJSON)).Response;
        Assert.Equal("error", response.Status);
        Assert.Empty(response.Hits);
    }

    private static string FindRepoRoot()
    {
        var directory = new DirectoryInfo(AppContext.BaseDirectory);
        while (directory is not null)
        {
            if (File.Exists(Path.Combine(directory.FullName, "shared", "models", "manifest.json"))) return directory.FullName;
            directory = directory.Parent;
        }
        throw new DirectoryNotFoundException("Canonical model manifest was not found.");
    }
}
