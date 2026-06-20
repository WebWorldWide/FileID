// Perceptual near-duplicate grouping — pure + testable. Mirrors the macOS
// FileIDShared.PerceptualGrouping 1:1 (platforms/apple/.../PerceptualGrouping.swift).
// The engine stores a 64-bit difference-hash (dHash) per image in `files.phash`
// (the Int64 bit-pattern of a UInt64). Two images are "visually similar" when the
// Hamming distance of their dHashes is small; union-find clusters any transitive
// chain of within-threshold pairs into one group. Kept dependency-free so the
// xUnit suite can drive it directly, separate from the SQLite query that feeds it.

using System;
using System.Collections.Generic;
using System.Numerics;

namespace FileID.Services;

internal static class PerceptualGrouping
{
    /// <summary>Hamming distance of two 64-bit dHashes: popcount(a XOR b) over the
    /// raw 64 bits. We XOR the *bit patterns*, not the signed Int64 values — the
    /// sign bit is just bit 63 of the hash. (mirrors macOS hammingDistance)</summary>
    public static int HammingDistance(long a, long b)
        => BitOperations.PopCount((ulong)a ^ (ulong)b);

    /// <summary>Union-find clustering: items whose dHashes are within
    /// <paramref name="maxHamming"/> of one another — transitively (A~B, B~C ⇒
    /// {A,B,C}) — form a group. Returns groups of size &gt;= 2, each as its member
    /// ids in first-seen order, with the groups themselves in first-seen order.
    /// O(N²) pairwise; callers guard input size. (mirrors macOS groupByHamming)</summary>
    public static List<List<long>> GroupByHamming(
        IReadOnlyList<(long Id, long Phash)> items, int maxHamming)
    {
        int n = items.Count;
        var groups = new List<List<long>>();
        if (n <= 1) return groups;

        var parent = new int[n];
        for (int i = 0; i < n; i++) parent[i] = i;

        int Find(int x)
        {
            int r = x;
            while (parent[r] != r) { parent[r] = parent[parent[r]]; r = parent[r]; }
            return r;
        }

        void Union(int a, int b)
        {
            int ra = Find(a), rb = Find(b);
            // Point the higher index at the lower one so every component's root is
            // its smallest member index — keeps group order deterministic.
            if (ra != rb)
            {
                if (ra < rb) parent[rb] = ra;
                else parent[ra] = rb;
            }
        }

        for (int i = 0; i < n; i++)
        {
            for (int j = i + 1; j < n; j++)
            {
                if (HammingDistance(items[i].Phash, items[j].Phash) <= maxHamming)
                {
                    Union(i, j);
                }
            }
        }

        var order = new List<int>();
        var membersByRoot = new Dictionary<int, List<long>>(n);
        for (int i = 0; i < n; i++)
        {
            int r = Find(i);
            if (!membersByRoot.TryGetValue(r, out var list))
            {
                list = new List<long>();
                membersByRoot[r] = list;
                order.Add(r);
            }
            list.Add(items[i].Id);
        }

        foreach (var root in order)
        {
            var ids = membersByRoot[root];
            if (ids.Count >= 2) groups.Add(ids);
        }
        return groups;
    }
}
