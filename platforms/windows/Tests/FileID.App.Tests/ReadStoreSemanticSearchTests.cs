// Equivalence test for ReadStore.SemanticSearchAsync's bounded top-K scan.
//
// The perf change made the loop materialize a FileRow only for rows the heap
// retains (instead of for every embedding row). This guards that the change is
// byte-identical in OUTPUT: the same set of file ids in the same best→worst
// order as a naive full scan + sort, including the dimension-mismatch skip and
// the failed=0 filter.
//
// Builds a throwaway SQLite DB with just the columns SemanticSearchAsync
// projects, then opens it through the real ReadStore (read-only).

using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using FileID.Services;
using Microsoft.Data.Sqlite;
using Xunit;

namespace FileID.App.Tests;

public sealed class ReadStoreSemanticSearchTests : IDisposable
{
    private static readonly float[] Query = { 1f, 0f, 0f };

    private readonly string _dbPath;

    public ReadStoreSemanticSearchTests()
    {
        _dbPath = Path.Combine(Path.GetTempPath(), $"fileid-readstore-test-{Guid.NewGuid():N}.sqlite");
    }

    public void Dispose()
    {
        SqliteConnection.ClearAllPools();
        try { if (File.Exists(_dbPath)) File.Delete(_dbPath); } catch { /* best effort */ }
    }

    private readonly record struct Row(long Id, string Kind, bool Failed, float[] Emb);

    private static byte[] FloatsToBlob(float[] v)
    {
        var bytes = new byte[v.Length * 4];
        Buffer.BlockCopy(v, 0, bytes, 0, bytes.Length);
        return bytes;
    }

    private void BuildDb(IReadOnlyList<Row> rows)
    {
        var cs = new SqliteConnectionStringBuilder { DataSource = _dbPath }.ToString();
        using var conn = new SqliteConnection(cs);
        conn.Open();
        using (var cmd = conn.CreateCommand())
        {
            cmd.CommandText = """
                CREATE TABLE files (
                    id INTEGER PRIMARY KEY,
                    path_text TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    size_bytes INTEGER NOT NULL,
                    modified_at REAL,
                    has_faces INTEGER NOT NULL DEFAULT 0,
                    has_text INTEGER NOT NULL DEFAULT 0,
                    failed INTEGER NOT NULL DEFAULT 0,
                    vlm_proposed_name TEXT
                );
                CREATE TABLE clip_embeddings (
                    file_id INTEGER NOT NULL,
                    embedding BLOB NOT NULL
                );
                CREATE TABLE tags (
                    file_id INTEGER NOT NULL,
                    tag TEXT NOT NULL,
                    source TEXT NOT NULL,
                    score REAL
                );
                """;
            cmd.ExecuteNonQuery();
        }
        foreach (var r in rows)
        {
            using var ins = conn.CreateCommand();
            ins.CommandText = """
                INSERT INTO files (id, path_text, kind, size_bytes, modified_at, has_faces, has_text, failed, vlm_proposed_name)
                VALUES ($id, $path, $kind, 100, NULL, 0, 0, $failed, NULL);
                INSERT INTO clip_embeddings (file_id, embedding) VALUES ($id, $emb);
                """;
            ins.Parameters.AddWithValue("$id", r.Id);
            ins.Parameters.AddWithValue("$path", $"C:/photos/{r.Id}.jpg");
            ins.Parameters.AddWithValue("$kind", r.Kind);
            ins.Parameters.AddWithValue("$failed", r.Failed ? 1 : 0);
            ins.Parameters.AddWithValue("$emb", FloatsToBlob(r.Emb));
            ins.ExecuteNonQuery();
        }
    }

    private static float Dot(float[] q, float[] e)
    {
        float acc = 0f;
        for (int i = 0; i < q.Length; i++) acc += q[i] * e[i];
        return acc;
    }

    // Naive reference: dot-product every non-failed embedding of matching dim,
    // sort best→worst, take `limit`. Mirrors the heap result's ordering.
    private static long[] NaiveTopK(IReadOnlyList<Row> rows, float[] query, int limit, string? kind)
    {
        return rows
            .Where(r => !r.Failed)
            .Where(r => kind is null || r.Kind == kind)
            .Where(r => r.Emb.Length == query.Length)
            .Select(r => (r.Id, Score: Dot(query, r.Emb)))
            .OrderByDescending(x => x.Score)
            .ThenByDescending(x => x.Id)
            .Take(limit)
            .Select(x => x.Id)
            .ToArray();
    }

    private static float[] Emb(float a, float b, float c) => new[] { a, b, c };

    private static float[] Emb2(float a, float b) => new[] { a, b };

    [Fact]
    public async Task SemanticSearch_TopK_MatchesNaiveScan()
    {
        // Scores are the first component; descending: 5 (0.9), 2 (0.8), 4 (0.5),
        // 1 (0.1), 3 (-0.2). failed row (6) must be excluded entirely.
        var rows = new List<Row>
        {
            new(1, "image", false, Emb(0.1f, 0.3f, 0.2f)),
            new(2, "image", false, Emb(0.8f, 0.1f, 0.1f)),
            new(3, "image", false, Emb(-0.2f, 0.5f, 0.1f)),
            new(4, "image", false, Emb(0.5f, 0.2f, 0.0f)),
            new(5, "image", false, Emb(0.9f, 0.0f, 0.1f)),
            new(6, "image", true, Emb(1.0f, 1.0f, 1.0f)), // failed → excluded
        };
        BuildDb(rows);

        using var store = new ReadStore(_dbPath);
        await store.OpenAsync(CancellationToken.None);

        const int limit = 3;
        var actual = await store.SemanticSearchAsync(Query, limit, CancellationToken.None);
        var actualIds = actual.Select(h => h.Row.Id).ToArray();

        var expected = NaiveTopK(rows, Query, limit, null);
        Assert.Equal(expected, actualIds);

        // Scores carried through must be descending (best→worst).
        for (int i = 1; i < actual.Count; i++)
        {
            Assert.True(actual[i - 1].Score >= actual[i].Score);
        }
    }

    [Fact]
    public async Task SemanticSearch_SkipsDimensionMismatchedEmbeddings()
    {
        var rows = new List<Row>
        {
            new(1, "image", false, Emb(0.9f, 0f, 0f)),
            new(2, "image", false, Emb2(0.5f, 0f)), // wrong dim → skipped
            new(3, "image", false, Emb(0.7f, 0f, 0f)),
        };
        BuildDb(rows);

        using var store = new ReadStore(_dbPath);
        await store.OpenAsync(CancellationToken.None);

        var actual = await store.SemanticSearchAsync(Query, 10, CancellationToken.None);
        var actualIds = actual.Select(h => h.Row.Id).ToArray();

        Assert.Equal(new long[] { 1, 3 }, actualIds);
        Assert.DoesNotContain(2L, actualIds);
    }

    [Fact]
    public async Task SemanticSearch_KindFilter_RestrictsAndStaysEquivalent()
    {
        var rows = new List<Row>
        {
            new(1, "image", false, Emb(0.9f, 0f, 0f)),
            new(2, "pdf", false, Emb(0.95f, 0f, 0f)),
            new(3, "image", false, Emb(0.7f, 0f, 0f)),
        };
        BuildDb(rows);

        using var store = new ReadStore(_dbPath);
        await store.OpenAsync(CancellationToken.None);

        var actual = await store.SemanticSearchAsync(Query, 10, CancellationToken.None, kind: "image");
        var actualIds = actual.Select(h => h.Row.Id).ToArray();

        var expected = NaiveTopK(rows, Query, 10, "image");
        Assert.Equal(expected, actualIds);
        Assert.DoesNotContain(2L, actualIds);
    }
}
