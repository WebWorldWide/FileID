// Tests for QueryEmbeddingCache — the bounded LRU that lets ClipSearchService
// skip the IPC + CLIP text-encode round-trip when an identical query string was
// already embedded. Pure logic, no engine / UI thread.

using FileID.Services;
using Xunit;

namespace FileID.App.Tests;

public class QueryEmbeddingCacheTests
{
    private static readonly float[] VecA = { 1f, 2f, 3f };
    private static readonly float[] VecB = { 4f, 5f, 6f };

    [Fact]
    public void Miss_OnEmptyCache_ReturnsFalse()
    {
        var cache = new QueryEmbeddingCache(8);
        Assert.False(cache.TryGet("dog", out var got));
        Assert.Null(got);
    }

    [Fact]
    public void StoreThenGet_ReturnsSameInstance()
    {
        var cache = new QueryEmbeddingCache(8);
        cache.Store("dog", VecA);
        Assert.True(cache.TryGet("dog", out var got));
        Assert.Same(VecA, got);
    }

    [Fact]
    public void Store_DuplicateKey_DoesNotGrowCountAndKeepsLatest()
    {
        var cache = new QueryEmbeddingCache(8);
        cache.Store("dog", VecA);
        cache.Store("dog", VecB);
        Assert.Equal(1, cache.Count);
        Assert.True(cache.TryGet("dog", out var got));
        Assert.Same(VecB, got);
    }

    [Fact]
    public void Evicts_LeastRecentlyUsed_BeyondCapacity()
    {
        var cache = new QueryEmbeddingCache(2);
        cache.Store("a", VecA);
        cache.Store("b", VecB);
        cache.Store("c", VecA); // pushes "a" out (oldest)
        Assert.Equal(2, cache.Count);
        Assert.False(cache.TryGet("a", out _));
        Assert.True(cache.TryGet("b", out _));
        Assert.True(cache.TryGet("c", out _));
    }

    [Fact]
    public void Get_RefreshesRecency_SoTouchedEntrySurvivesEviction()
    {
        var cache = new QueryEmbeddingCache(2);
        cache.Store("a", VecA);
        cache.Store("b", VecB);
        // Touch "a" so it's now most-recently-used; "b" becomes the oldest.
        Assert.True(cache.TryGet("a", out _));
        cache.Store("c", VecA); // should evict "b", not the just-touched "a"
        Assert.True(cache.TryGet("a", out _));
        Assert.False(cache.TryGet("b", out _));
        Assert.True(cache.TryGet("c", out _));
    }

    [Fact]
    public void Capacity_BelowOne_ClampsToOne()
    {
        var cache = new QueryEmbeddingCache(0);
        cache.Store("a", VecA);
        cache.Store("b", VecB);
        Assert.Equal(1, cache.Count);
        Assert.False(cache.TryGet("a", out _));
        Assert.True(cache.TryGet("b", out _));
    }
}
