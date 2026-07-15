// Perceptual near-duplicate grouping — pure + testable.
// The engine stores a 64-bit difference-hash (dHash) per image in `files.phash`
// (the Int64 bit-pattern of a UInt64). Two images are "visually similar" when the
// Hamming distance of their dHashes is small; union-find clusters any transitive
// chain of within-threshold pairs into one group.

using System;
using System.Collections.Generic;
using System.Numerics;
using System.Threading;

namespace FileID.Services;

internal static class PerceptualGrouping
{
    internal const long MaxCandidateComparisons = 25_000_000;

    public static int HammingDistance(long a, long b)
        => BitOperations.PopCount((ulong)a ^ (ulong)b);

    public static List<List<long>> GroupByHamming(
        IReadOnlyList<(long Id, long Phash)> items,
        int maxHamming,
        CancellationToken cancellationToken = default)
        => GroupByHammingMeasured(items, maxHamming, cancellationToken).Groups;

    internal static (List<List<long>> Groups, long Comparisons) GroupByHammingMeasured(
        IReadOnlyList<(long Id, long Phash)> items,
        int maxHamming,
        CancellationToken cancellationToken = default)
    {
        ArgumentOutOfRangeException.ThrowIfNegative(maxHamming);
        ArgumentOutOfRangeException.ThrowIfGreaterThan(maxHamming, 64);

        int n = items.Count;
        var groups = new List<List<long>>();
        if (n <= 1) return (groups, 0);

        var parent = new int[n];
        for (int i = 0; i < n; i++) parent[i] = i;

        int Find(int x)
        {
            int root = x;
            while (parent[root] != root)
            {
                parent[root] = parent[parent[root]];
                root = parent[root];
            }
            return root;
        }

        void Union(int a, int b)
        {
            int rootA = Find(a);
            int rootB = Find(b);
            if (rootA == rootB) return;
            if (rootA < rootB) parent[rootB] = rootA;
            else parent[rootA] = rootB;
        }

        var unique = new List<int>(n);
        var representativeByHash = new Dictionary<long, int>(n);
        for (int i = 0; i < n; i++)
        {
            cancellationToken.ThrowIfCancellationRequested();
            if (representativeByHash.TryGetValue(items[i].Phash, out var representative))
            {
                Union(representative, i);
            }
            else
            {
                representativeByHash.Add(items[i].Phash, i);
                unique.Add(i);
            }
        }

        long comparisons = 0;
        if (maxHamming >= 64)
        {
            for (int i = 1; i < unique.Count; i++) Union(unique[0], unique[i]);
        }
        else if (maxHamming > 0 && unique.Count > 1)
        {
            int blockCount = maxHamming + 1;
            var buckets = new Dictionary<(int Block, ulong Value), List<int>>(unique.Count * 2);
            foreach (var index in unique)
            {
                cancellationToken.ThrowIfCancellationRequested();
                ulong hash = (ulong)items[index].Phash;
                for (int block = 0; block < blockCount; block++)
                {
                    ulong value = BlockValue(hash, block, blockCount);
                    var key = (block, value);
                    if (!buckets.TryGetValue(key, out var candidates))
                    {
                        candidates = new List<int>();
                        buckets.Add(key, candidates);
                    }

                    foreach (var candidate in candidates)
                    {
                        if ((comparisons & 0xFFF) == 0)
                        {
                            cancellationToken.ThrowIfCancellationRequested();
                        }
                        ulong candidateHash = (ulong)items[candidate].Phash;
                        if (SharesEarlierBlock(hash, candidateHash, block, blockCount)) continue;
                        comparisons++;
                        if (comparisons > MaxCandidateComparisons)
                        {
                            throw new InvalidOperationException(
                                $"Visually similar comparison exceeded the safety budget of {MaxCandidateComparisons:N0} candidate pairs. Reduce FILEID_NEARDUP_HAMMING or use Exact cleanup.");
                        }
                        if (BitOperations.PopCount(hash ^ candidateHash) <= maxHamming)
                        {
                            Union(candidate, index);
                        }
                    }
                    candidates.Add(index);
                }
            }
        }

        var order = new List<int>();
        var membersByRoot = new Dictionary<int, List<long>>(n);
        for (int i = 0; i < n; i++)
        {
            int root = Find(i);
            if (!membersByRoot.TryGetValue(root, out var members))
            {
                members = new List<long>();
                membersByRoot[root] = members;
                order.Add(root);
            }
            members.Add(items[i].Id);
        }

        foreach (var root in order)
        {
            var ids = membersByRoot[root];
            if (ids.Count >= 2) groups.Add(ids);
        }
        return (groups, comparisons);
    }

    private static bool SharesEarlierBlock(
        ulong left,
        ulong right,
        int currentBlock,
        int blockCount)
    {
        for (int block = 0; block < currentBlock; block++)
        {
            if (BlockValue(left, block, blockCount) == BlockValue(right, block, blockCount))
            {
                return true;
            }
        }
        return false;
    }

    private static ulong BlockValue(ulong hash, int block, int blockCount)
    {
        int start = block * 64 / blockCount;
        int end = (block + 1) * 64 / blockCount;
        int width = end - start;
        ulong mask = width == 64 ? ulong.MaxValue : (1UL << width) - 1;
        return (hash >> start) & mask;
    }
}
