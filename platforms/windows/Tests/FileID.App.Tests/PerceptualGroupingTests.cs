// Perceptual near-duplicate grouping: Hamming distance over 64-bit dHashes +
// union-find clustering. Drives the pure logic behind Cleanup's "Similar" mode
// directly, separate from the SQLite query that feeds it. Mirrors the macOS
// SharedTests/PerceptualGroupingTests.swift case-for-case.

using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading;
using FileID.Services;
using Xunit;

namespace FileID.App.Tests;

public class PerceptualGroupingTests
{
    // dHash bit-pattern helper: reinterpret a UInt64 as Int64 (the engine stores
    // the signed bit-pattern of the unsigned hash). Mirrors macOS Int64(bitPattern:).
    private static long I(ulong u) => unchecked((long)u);

    private static List<(long Id, long Phash)> Items(params (long Id, long Phash)[] xs)
        => new(xs);

    // ─── Hamming distance ───────────────────────────────────────────────────

    [Fact]
    public void HammingDistance_IsPopcountOfXorOverRaw64Bits()
    {
        Assert.Equal(0, PerceptualGrouping.HammingDistance(I(0x0), I(0x0)));   // identical
        Assert.Equal(1, PerceptualGrouping.HammingDistance(I(0x0), I(0x1)));   // one bit
        Assert.Equal(8, PerceptualGrouping.HammingDistance(I(0x0), I(0xFF)));  // low byte
        // Sign bit is just bit 63 — XOR the bit patterns, not the signed values.
        Assert.Equal(64, PerceptualGrouping.HammingDistance(I(0x0), I(ulong.MaxValue)));
        Assert.Equal(0, PerceptualGrouping.HammingDistance(I(ulong.MaxValue), I(ulong.MaxValue)));
    }

    // ─── Grouping ───────────────────────────────────────────────────────────

    [Fact]
    public void ExactMatch_GroupsAtThresholdZero()
    {
        var items = Items((1, I(0xABCD)), (2, I(0xABCD)), (3, I(0x1234)));
        var groups = PerceptualGrouping.GroupByHamming(items, maxHamming: 0);
        Assert.Single(groups);
        Assert.True(new HashSet<long> { 1, 2 }.SetEquals(groups[0]));
    }

    [Fact]
    public void WithinThreshold_Groups_FarImageStaysAlone()
    {
        // 0x0 and 0x1 are Hamming 1; 0xFFFF... is far from both.
        var items = Items((1, I(0x0)), (2, I(0x1)), (3, I(ulong.MaxValue)));
        var groups = PerceptualGrouping.GroupByHamming(items, maxHamming: 8);
        Assert.Single(groups);
        Assert.True(new HashSet<long> { 1, 2 }.SetEquals(groups[0])); // id 3 dropped (singleton)
    }

    [Fact]
    public void JustOverThreshold_DoesNotGroup_ThenGroupsWhenBumped()
    {
        // 0x1FF has 9 bits set → Hamming 9 from 0x0, just over a threshold of 8.
        var items = Items((1, I(0x0)), (2, I(0x1FF)));
        Assert.Equal(9, PerceptualGrouping.HammingDistance(I(0x0), I(0x1FF)));
        Assert.Empty(PerceptualGrouping.GroupByHamming(items, maxHamming: 8));

        var grouped = PerceptualGrouping.GroupByHamming(items, maxHamming: 9);
        Assert.Single(grouped);
        Assert.True(new HashSet<long> { 1, 2 }.SetEquals(grouped[0]));
    }

    [Fact]
    public void Transitivity_AtoB_BtoC_UnionsIntoOneGroup()
    {
        // a=0x00, b=0x0F (Ham 4 from a), c=0xFF (Ham 4 from b, but Ham 8 from a).
        long a = I(0x00), b = I(0x0F), c = I(0xFF);
        Assert.Equal(4, PerceptualGrouping.HammingDistance(a, b));
        Assert.Equal(4, PerceptualGrouping.HammingDistance(b, c));
        Assert.Equal(8, PerceptualGrouping.HammingDistance(a, c)); // not within 5 directly

        var items = Items((1, a), (2, b), (3, c));
        var groups = PerceptualGrouping.GroupByHamming(items, maxHamming: 5);
        Assert.Single(groups);
        Assert.True(new HashSet<long> { 1, 2, 3 }.SetEquals(groups[0])); // unioned transitively
    }

    [Fact]
    public void TwoIndependentClusters_FormTwoGroups()
    {
        var items = Items(
            (1, I(0x0)), (2, I(0x1)),                       // cluster A
            (3, I(ulong.MaxValue)), (4, I(ulong.MaxValue ^ 0x1)), // cluster B (Ham 1 apart)
            (5, I(0x00FF00FF)));                            // loner
        var groups = PerceptualGrouping.GroupByHamming(items, maxHamming: 4);
        Assert.Equal(2, groups.Count);
        var sets = groups.Select(g => new HashSet<long>(g)).ToList();
        Assert.Contains(sets, s => s.SetEquals(new HashSet<long> { 1, 2 }));
        Assert.Contains(sets, s => s.SetEquals(new HashSet<long> { 3, 4 }));
    }

    [Fact]
    public void GroupOrder_IsDeterministic_FirstSeenRoot()
    {
        // Cluster of ids {2,3} appears before the cluster of {1,4} by first-seen
        // index, since index 1 (id 2) precedes index 3 (id 4) — roots are the
        // smallest member index. Mirrors macOS first-seen ordering.
        var items = Items(
            (10, I(0x00FF00FF)),     // index 0 — loner
            (2, I(0x0)), (3, I(0x1)),// indices 1,2 — cluster X
            (4, I(0xF0)), (1, I(0xF1)));// indices 3,4 — cluster Y
        var groups = PerceptualGrouping.GroupByHamming(items, maxHamming: 2);
        Assert.Equal(2, groups.Count);
        Assert.True(new HashSet<long> { 2, 3 }.SetEquals(groups[0]));
        Assert.True(new HashSet<long> { 4, 1 }.SetEquals(groups[1]));
    }

    [Fact]
    public void EmptyOrSingleItem_YieldsNoGroups()
    {
        Assert.Empty(PerceptualGrouping.GroupByHamming(new List<(long, long)>(), maxHamming: 8));
        Assert.Empty(PerceptualGrouping.GroupByHamming(Items((1, I(0x0))), maxHamming: 8));
    }

    [Fact]
    public void MultiIndexMatchesBruteForceAcrossRandomizedThresholds()
    {
        var random = new Random(0xF11E1D);
        var items = Enumerable.Range(0, 600)
            .Select(i => ((long)i + 1, random.NextInt64()))
            .ToList();
        items.Add((10_001, items[7].Item2));
        items.Add((10_002, items[11].Item2 ^ 0x3F));

        foreach (var threshold in new[] { 0, 1, 4, 8, 12 })
        {
            var expected = BruteForceGroups(items, threshold);
            var actual = PerceptualGrouping.GroupByHamming(items, threshold);
            Assert.Equal(Normalize(expected), Normalize(actual));
        }
    }

    [Fact]
    public void RandomTwentyThousandSetStaysBelowPairwiseComparisonBudget()
    {
        var random = new Random(0x2060);
        var items = Enumerable.Range(0, 20_000)
            .Select(i => ((long)i + 1, random.NextInt64()))
            .ToList();

        var measured = PerceptualGrouping.GroupByHammingMeasured(items, maxHamming: 8);
        long bruteForcePairs = (long)items.Count * (items.Count - 1) / 2;
        Assert.True(measured.Comparisons < bruteForcePairs / 10,
            $"candidate comparisons {measured.Comparisons:N0} were not sufficiently subquadratic against {bruteForcePairs:N0} all-pairs");
    }

    [Fact]
    public void CancellationIsObservedBeforeCandidateSweep()
    {
        using var cts = new CancellationTokenSource();
        cts.Cancel();
        var items = Items((1, 0), (2, 1));
        Assert.Throws<OperationCanceledException>(() =>
            PerceptualGrouping.GroupByHamming(items, 8, cts.Token));
    }

    private static List<List<long>> BruteForceGroups(
        IReadOnlyList<(long Id, long Phash)> items,
        int threshold)
    {
        var parent = Enumerable.Range(0, items.Count).ToArray();
        int Find(int value)
        {
            while (parent[value] != value)
            {
                parent[value] = parent[parent[value]];
                value = parent[value];
            }
            return value;
        }
        void Union(int left, int right)
        {
            int a = Find(left);
            int b = Find(right);
            if (a != b) parent[Math.Max(a, b)] = Math.Min(a, b);
        }
        for (int i = 0; i < items.Count; i++)
        {
            for (int j = i + 1; j < items.Count; j++)
            {
                if (PerceptualGrouping.HammingDistance(items[i].Phash, items[j].Phash) <= threshold)
                {
                    Union(i, j);
                }
            }
        }
        return Enumerable.Range(0, items.Count)
            .GroupBy(Find)
            .Select(group => group.Select(i => items[i].Id).ToList())
            .Where(group => group.Count >= 2)
            .ToList();
    }

    private static string Normalize(IEnumerable<List<long>> groups)
        => string.Join("|", groups
            .Select(group => string.Join(",", group.OrderBy(id => id)))
            .OrderBy(group => group, StringComparer.Ordinal));
}
