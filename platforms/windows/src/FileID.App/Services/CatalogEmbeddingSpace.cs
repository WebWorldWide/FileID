using System.Security.Cryptography;
using System.Text;
using FileID.IpcSchema;

namespace FileID.Services;

internal static class CatalogEmbeddingSpace
{
    public const int Dimension = 512;
    internal static readonly KeyValuePair<string, string>[] ArtifactPins =
    {
        new("clip_vitb32_image", "fd6e1402a588279d1723c7534d4bcba5bc0b14b47dfab0e46f8c47b8270d7d40"),
        new("clip_vitb32_text", "3f6571f5bad13a97c469c1622e1cfc4d9aef78b79fdbfcff804ca357bfada8cc"),
        new("clip_bpe_vocab", "5047b556ce86ccaf6aa22b3ffccfc52d391ea4accdab9c2f2407da5b742d4363"),
        new("clip_bpe_merges", "f526393189112391ce6f9795d4695f704121ce452c3aad1f5335cc41337eba85")
    };
    public static string ModelID { get; } = BuildModelID();

    private static string BuildModelID()
    {
        var descriptor = "clip-rgb-stretch224-bpe77-l2-v1|" + string.Join("|", ArtifactPins.Select(pin => pin.Key + ":" + pin.Value));
        return "clip-vitb32-openai-v1:" + Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes(descriptor))).ToLowerInvariant();
    }

    public static bool IsValidVector(IReadOnlyList<float> vector)
    {
        if (vector.Count != Dimension) return false;
        double squaredNorm = 0;
        foreach (var value in vector)
        {
            if (!float.IsFinite(value)) return false;
            squaredNorm += (double)value * value;
        }
        return Math.Abs(squaredNorm - 1) <= 0.02;
    }

    public static CatalogRequest SearchRequest(string query, string mode, IReadOnlyList<float>? vector = null)
    {
        if (mode is not ("keyword" or "hybrid" or "semantic")) throw new ArgumentException("Unknown catalog search mode.", nameof(mode));
        if (mode != "keyword" && (vector is null || !IsValidVector(vector)))
            throw new InvalidOperationException("The installed CLIP text model did not return a finite, normalized 512-dimensional vector. Use keyword search; no semantic query was sent.");
        return new CatalogRequest(Guid.NewGuid().ToString(), "search", query, null, null, null, null, null,
            SearchMode: mode, QueryVector: mode == "keyword" ? null : vector!.ToArray(),
            EmbeddingModel: mode == "keyword" ? null : ModelID, Limit: 100, ResultScope: "all");
    }
}
