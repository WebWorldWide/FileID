using FileID.IpcSchema;

namespace FileID.Services;

/// <summary>Requests and reply handling for letting FileID's helper rank
/// look-alike files from its own index, so the app does not read every
/// stored picture fingerprint itself.</summary>
internal static class EngineSimilarity
{
    /// <summary>The helper accepts at most this many results per request.</summary>
    public const int MaxResults = 100;
    public const int MaxQueryLength = 2000;
    public static readonly TimeSpan EngineWait = TimeSpan.FromSeconds(6);
    public static readonly TimeSpan RetryAfterMiss = TimeSpan.FromSeconds(5);
    private const string PendingPrefix = "Preparing the local visual search index";

    public static CatalogRequest SeedRequest(long fileId, long? rootId, int limit) =>
        new(Guid.NewGuid().ToString(), "search", null, fileId, null, null, null, null,
            SearchMode: "semantic", Limit: Math.Clamp(limit, 1, MaxResults), ResultScope: "files", RootID: rootId);

    /// <summary>Null when the vector is not one the helper would accept; the
    /// caller then ranks in the app as before.</summary>
    public static CatalogRequest? QueryRequest(string query, IReadOnlyList<float> vector, long? rootId, int limit)
    {
        var text = query.Trim();
        if (text.Length == 0 || text.Length > MaxQueryLength || !CatalogEmbeddingSpace.IsValidVector(vector)) return null;
        return new CatalogRequest(Guid.NewGuid().ToString(), "search", text, null, null, null, null, null,
            SearchMode: "semantic", QueryVector: vector.ToArray(), EmbeddingModel: CatalogEmbeddingSpace.ModelID,
            Limit: Math.Clamp(limit, 1, MaxResults), ResultScope: "files", RootID: rootId);
    }

    /// <summary>True while the helper is still building its index.</summary>
    public static bool IsPending(CatalogResponse response) =>
        response.Status == "indexing"
        || (response.Status == "error"
            && response.Message is { } message
            && message.StartsWith(PendingPrefix, StringComparison.Ordinal));

    /// <summary>File ids in the helper's ranked order, each once. Null when
    /// the reply is not a usable ranking (index not ready, an error, or no
    /// results), which means: rank in the app instead.</summary>
    public static IReadOnlyList<long>? RankedIds(CatalogResponse response)
    {
        if (response.Status != "ok" || response.Hits.Count == 0) return null;
        var seen = new HashSet<long>(response.Hits.Count);
        var ids = new List<long>(response.Hits.Count);
        foreach (var hit in response.Hits)
        {
            if (seen.Add(hit.FileID)) ids.Add(hit.FileID);
        }
        return ids;
    }
}
