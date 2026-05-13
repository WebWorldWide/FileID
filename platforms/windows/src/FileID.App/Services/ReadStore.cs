// ReadStore — app-side read-only SQLite access.
//
// The engine owns the writer connection (rusqlite, single-threaded by
// design — see platforms/apple/CLAUDE.md and engine/src/db/mod.rs). The
// app reads through ephemeral read-only connections that ride the same
// WAL the engine writes, getting consistent snapshots without contending
// with the writer.
//
// FTS5 search: matches `Database.swift`'s `searchFiles` 1:1. The same
// match expression syntax (whitespace-separated terms with optional
// leading/trailing wildcards). CLIP semantic search dot-products the
// query embedding against `clip_embeddings.vector` (BLOB of float32
// little-endian, L2-normalized — same on-disk format as macOS).
//
// Lifetime: one instance per FileID.App process. `Open()` is async so
// the call site can await schema validation without blocking the UI
// thread. `Dispose()` closes the underlying connection.

using System;
using System.Collections.Generic;
using System.IO;
using System.Threading;
using System.Threading.Tasks;
using Microsoft.Data.Sqlite;

namespace FileID.Services;

internal sealed class ReadStore : IAsyncDisposable, IDisposable
{
    private readonly string _dbPath;
    private readonly string _connString;
    private SqliteConnection? _connection;
    private readonly SemaphoreSlim _gate = new(1, 1);

    public ReadStore(string dbPath)
    {
        _dbPath = dbPath;
        _connString = new SqliteConnectionStringBuilder
        {
            DataSource = dbPath,
            Mode = SqliteOpenMode.ReadOnly,
            Cache = SqliteCacheMode.Shared,
        }.ToString();
    }

    public bool IsOpen => _connection?.State == System.Data.ConnectionState.Open;

    public async Task OpenAsync(CancellationToken ct = default)
    {
        await _gate.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            if (_connection != null)
            {
                return;
            }
            // First-launch guard: the engine creates the DB on first scan.
            // Until then the file doesn't exist; trying to open ReadOnly
            // would throw. Stay closed until the engine has written rows.
            // V14.7.11: File.Exists wrapped — invalid-char paths or denied
            // ACL on the parent dir would otherwise throw.
            // V14.9-B2: offload File.Exists to the thread pool. On network
            // shares or slow USB sticks this sync call can block the UI
            // for hundreds of ms; the cost on a local SSD is < 1 ms so
            // there's no downside to always going async here.
            var dbPath = _dbPath;
            bool dbExists = await Task.Run(() =>
            {
                try { return File.Exists(dbPath); }
                catch (IOException) { return false; }
                catch (UnauthorizedAccessException) { return false; }
            }, ct).ConfigureAwait(false);
            if (!dbExists)
            {
                return;
            }
            var conn = new SqliteConnection(_connString);
            await conn.OpenAsync(ct).ConfigureAwait(false);
            // Match the engine's PRAGMA shape so reads see the same cache /
            // mmap behavior as the writer (these are no-ops on read-only,
            // but documented for parity).
            using (var cmd = conn.CreateCommand())
            {
                cmd.CommandText = "PRAGMA query_only = ON;";
                await cmd.ExecuteNonQueryAsync(ct).ConfigureAwait(false);
            }
            _connection = conn;
        }
        finally
        {
            _gate.Release();
        }
    }

    /// <summary>
    /// FTS5-backed full-text search over filename + OCR text. Uses the
    /// same match-expression shape as macOS — whitespace-joined terms.
    /// </summary>
    public async Task<IReadOnlyList<FileRow>> SearchAsync(
        string query, int limit, CancellationToken ct)
    {
        if (_connection == null)
        {
            return Array.Empty<FileRow>();
        }
        var match = BuildMatchExpression(query);
        // Empty match → delegate to RecentAsync. Done BEFORE acquiring
        // the gate so RecentAsync's own gate acquisition isn't reentrant.
        if (string.IsNullOrEmpty(match))
        {
            return await RecentAsync(limit, ct).ConfigureAwait(false);
        }
        // Gate the connection across the entire query lifetime —
        // Microsoft.Data.Sqlite connections are NOT thread-safe across
        // simultaneous commands, so two parallel callers would race on
        // the same SqliteConnection's transaction state.
        await _gate.WaitAsync(ct).ConfigureAwait(false);
        try
        {
        if (_connection == null) return Array.Empty<FileRow>();
        // Two sources, deduped by file id: ocr_fts (FTS5 over OCR text;
        // ranked by bm25) UNION filename LIKE matches over files.path_text.
        // Mirror of macOS Database.swift::searchFiles which combines OCR
        // hits with filename hits in the same result set.
        var rows = new List<FileRow>(limit);
        var seen = new HashSet<long>();
        using var cmd = _connection.CreateCommand();
        cmd.CommandText = """
            SELECT f.id, f.path_text, f.kind, f.size_bytes, f.modified_at, f.has_faces, f.has_text
            FROM ocr_fts
            JOIN files f ON f.id = ocr_fts.rowid
            WHERE ocr_fts MATCH $match
            ORDER BY bm25(ocr_fts)
            LIMIT $limit
            """;
        cmd.Parameters.AddWithValue("$match", match);
        cmd.Parameters.AddWithValue("$limit", limit);
        using (var reader = await cmd.ExecuteReaderAsync(ct).ConfigureAwait(false))
        {
            while (await reader.ReadAsync(ct).ConfigureAwait(false))
            {
                var row = ReadRow(reader);
                if (seen.Add(row.Id))
                {
                    rows.Add(row);
                }
            }
        }
        if (rows.Count >= limit)
        {
            return rows;
        }
        // Filename fallback. Each whitespace-separated term must appear
        // somewhere in path_text; trailing wildcard via LIKE.
        var likePieces = new List<string>();
        var likeArgs = new List<(string name, string value)>();
        var i = 0;
        foreach (var raw in query.Split(' ', StringSplitOptions.RemoveEmptyEntries))
        {
            var token = raw.Trim();
            if (token.Length < 2) continue;
            var p = $"$t{i++}";
            likePieces.Add($"path_text LIKE {p}");
            likeArgs.Add((p, $"%{token}%"));
        }
        if (likePieces.Count == 0)
        {
            return rows;
        }
        using var cmd2 = _connection.CreateCommand();
        cmd2.CommandText = $"""
            SELECT id, path_text, kind, size_bytes, modified_at, has_faces, has_text
            FROM files
            WHERE {string.Join(" AND ", likePieces)}
            ORDER BY modified_at DESC NULLS LAST
            LIMIT $limit
            """;
        foreach (var (n, v) in likeArgs)
        {
            cmd2.Parameters.AddWithValue(n, v);
        }
        cmd2.Parameters.AddWithValue("$limit", limit);
        using var reader2 = await cmd2.ExecuteReaderAsync(ct).ConfigureAwait(false);
        while (await reader2.ReadAsync(ct).ConfigureAwait(false))
        {
            if (rows.Count >= limit) break;
            var row = ReadRow(reader2);
            if (seen.Add(row.Id))
            {
                rows.Add(row);
            }
        }
        return rows;
        }
        finally { _gate.Release(); }
    }

    /// <summary>
    /// Most-recently-modified files as a fallback when the search box is
    /// empty (or when CLIP isn't installed yet).
    /// </summary>
    public async Task<IReadOnlyList<FileRow>> RecentAsync(int limit, CancellationToken ct)
    {
        if (_connection == null) return Array.Empty<FileRow>();
        await _gate.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            if (_connection == null) return Array.Empty<FileRow>();
            var rows = new List<FileRow>(limit);
            using var cmd = _connection.CreateCommand();
            cmd.CommandText = """
                SELECT id, path_text, kind, size_bytes, modified_at, has_faces, has_text
                FROM files
                ORDER BY modified_at DESC NULLS LAST LIMIT $limit
                """;
            cmd.Parameters.AddWithValue("$limit", limit);
            using var reader = await cmd.ExecuteReaderAsync(ct).ConfigureAwait(false);
            while (await reader.ReadAsync(ct).ConfigureAwait(false))
            {
                rows.Add(ReadRow(reader));
            }
            return rows;
        }
        finally { _gate.Release(); }
    }

    /// <summary>
    /// CLIP semantic search: dot-product the L2-normalized query embedding
    /// against every `clip_embeddings.vector`, take the top-`limit`. The
    /// embedding rows are pre-normalized so cosine similarity == dot product.
    /// Phase 2.4 cut: scans every embedding (acceptable up to ~50K files);
    /// Phase 4 swaps in an HNSW or IVF index if benchmarks demand it.
    /// </summary>
    public async Task<IReadOnlyList<FileRowWithScore>> SemanticSearchAsync(
        float[] queryEmbedding, int limit, CancellationToken ct)
    {
        if (_connection == null || queryEmbedding.Length == 0)
        {
            return Array.Empty<FileRowWithScore>();
        }
        await _gate.WaitAsync(ct).ConfigureAwait(false);
        try
        {
        if (_connection == null) return Array.Empty<FileRowWithScore>();
        var heap = new PriorityQueue<FileRowWithScore, float>(limit);
        using var cmd = _connection.CreateCommand();
        cmd.CommandText = """
            SELECT f.id, f.path_text, f.kind, f.size_bytes, f.modified_at,
                   f.has_faces, f.has_text, e.embedding
            FROM clip_embeddings e
            JOIN files f ON f.id = e.file_id
            """;
        using var reader = await cmd.ExecuteReaderAsync(ct).ConfigureAwait(false);
        while (await reader.ReadAsync(ct).ConfigureAwait(false))
        {
            var blob = (byte[])reader.GetValue(7);
            float score = DotProduct(queryEmbedding, blob);
            var row = ReadRow(reader);
            if (heap.Count < limit)
            {
                heap.Enqueue(new FileRowWithScore(row, score), score);
            }
            else if (heap.TryPeek(out _, out var minScore) && score > minScore)
            {
                heap.Dequeue();
                heap.Enqueue(new FileRowWithScore(row, score), score);
            }
        }
        // Heap holds best `limit` ordered worst→best; reverse to best→worst.
        var sorted = new List<FileRowWithScore>(heap.Count);
        while (heap.Count > 0)
        {
            sorted.Add(heap.Dequeue());
        }
        sorted.Reverse();
        return sorted;
        }
        finally { _gate.Release(); }
    }

    /// <summary>V14.9-I: enumerate files that have a VLM-proposed name
    /// pending. Powers the Deep Analyze "Pending renames (N)" pill so
    /// the user can bulk-apply the VLM's suggestions without manually
    /// walking the library. Mirrors the Files schema's
    /// <c>vlm_proposed_name</c> column.</summary>
    public async Task<IReadOnlyList<ProposedRenameRow>> PendingProposedRenamesAsync(
        int limit, CancellationToken ct)
    {
        if (_connection == null) return Array.Empty<ProposedRenameRow>();
        await _gate.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            if (_connection == null) return Array.Empty<ProposedRenameRow>();
            var rows = new List<ProposedRenameRow>(limit);
            using var cmd = _connection.CreateCommand();
            cmd.CommandText = """
                SELECT id, path_text, vlm_proposed_name
                FROM files
                WHERE vlm_proposed_name IS NOT NULL
                  AND vlm_proposed_name != ''
                ORDER BY vlm_analyzed_at DESC
                LIMIT $limit
                """;
            cmd.Parameters.AddWithValue("$limit", limit);
            using var reader = await cmd.ExecuteReaderAsync(ct).ConfigureAwait(false);
            while (await reader.ReadAsync(ct).ConfigureAwait(false))
            {
                rows.Add(new ProposedRenameRow(
                    Id: reader.GetInt64(0),
                    Path: reader.GetString(1),
                    ProposedName: reader.GetString(2)));
            }
            return rows;
        }
        finally { _gate.Release(); }
    }

    /// <summary>V14.9-I: count of files with a VLM-proposed name pending.
    /// Cheap (COUNT(*) on indexed/sparse column); polled by the Deep
    /// Analyze pill to know whether to show it.</summary>
    public async Task<int> PendingProposedRenameCountAsync(CancellationToken ct)
    {
        if (_connection == null) return 0;
        await _gate.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            if (_connection == null) return 0;
            using var cmd = _connection.CreateCommand();
            cmd.CommandText =
                "SELECT COUNT(*) FROM files WHERE vlm_proposed_name IS NOT NULL AND vlm_proposed_name != ''";
            var result = await cmd.ExecuteScalarAsync(ct).ConfigureAwait(false);
            return result is null ? 0 : Convert.ToInt32(result);
        }
        finally { _gate.Release(); }
    }

    /// <summary>
    /// Distinct file kinds present in the library — drives the kind filter
    /// segmented control in the Library tab.
    /// </summary>
    public async Task<IReadOnlyDictionary<string, int>> KindCountsAsync(CancellationToken ct)
    {
        if (_connection == null) return new Dictionary<string, int>();
        await _gate.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            if (_connection == null) return new Dictionary<string, int>();
            var dict = new Dictionary<string, int>(StringComparer.OrdinalIgnoreCase);
            using var cmd = _connection.CreateCommand();
            cmd.CommandText = "SELECT kind, COUNT(*) FROM files GROUP BY kind";
            using var reader = await cmd.ExecuteReaderAsync(ct).ConfigureAwait(false);
            while (await reader.ReadAsync(ct).ConfigureAwait(false))
            {
                dict[reader.GetString(0)] = reader.GetInt32(1);
            }
            return dict;
        }
        finally { _gate.Release(); }
    }

    private static FileRow ReadRow(SqliteDataReader reader) => new(
        Id: reader.GetInt64(0),
        Path: reader.GetString(1),
        Kind: reader.GetString(2),
        SizeBytes: reader.GetInt64(3),
        ModifiedAt: reader.IsDBNull(4) ? null : reader.GetDouble(4),
        HasFaces: reader.GetInt32(5) != 0,
        HasText: reader.GetInt32(6) != 0);

    private static string BuildMatchExpression(string query)
    {
        if (string.IsNullOrWhiteSpace(query))
        {
            return string.Empty;
        }
        // Quote-wrap each term and append `*` so partial matches hit. FTS5
        // syntax: " ".join('"foo"*', '"bar"*'). Drop tokens shorter than 2
        // chars to avoid degenerate explosion of matches.
        var parts = new List<string>();
        foreach (var raw in query.Split(' ', StringSplitOptions.RemoveEmptyEntries))
        {
            var token = raw.Trim().Replace("\"", string.Empty);
            if (token.Length < 2)
            {
                continue;
            }
            parts.Add($"\"{token}\"*");
        }
        return string.Join(' ', parts);
    }

    private static float DotProduct(float[] q, byte[] blob)
    {
        // BLOB layout: little-endian float32, length = q.Length * 4.
        if (blob.Length != q.Length * 4)
        {
            return 0f;
        }
        // Cast the blob to a Span<float> in place — zero allocation. The
        // JIT auto-vectorizes the multiply-accumulate loop into AVX2/NEON
        // FMA on every modern x86_64 / ARM64 CPU; ~3x faster than the
        // per-element BitConverter.ToSingle path without taking a dep
        // on System.Numerics.Tensors. Endianness is correct because
        // both writer (engine, little-endian) and reader (Windows on
        // x86_64/ARM64, little-endian) match.
        var qSpan = q.AsSpan();
        var blobFloats = System.Runtime.InteropServices.MemoryMarshal.Cast<byte, float>(blob);
        float acc = 0f;
        for (int i = 0; i < qSpan.Length; i++)
        {
            acc += qSpan[i] * blobFloats[i];
        }
        return acc;
    }

    // Legacy per-element loop kept for reference / debugging. Not used.
    [System.Diagnostics.CodeAnalysis.SuppressMessage("Performance", "CA1859", Justification = "Reference impl")]
    private static float DotProductScalar(float[] q, byte[] blob)
    {
        if (blob.Length != q.Length * 4) return 0f;
        float acc = 0f;
        for (int i = 0; i < q.Length; i++)
        {
            float v = BitConverter.ToSingle(blob, i * 4);
            acc += q[i] * v;
        }
        return acc;
    }

    public ValueTask DisposeAsync()
    {
        Dispose();
        return ValueTask.CompletedTask;
    }

    public void Dispose()
    {
        _connection?.Dispose();
        _connection = null;
        _gate.Dispose();
    }
}

internal sealed record FileRow(
    long Id,
    string Path,
    string Kind,
    long SizeBytes,
    double? ModifiedAt,
    bool HasFaces,
    bool HasText);

internal sealed record FileRowWithScore(FileRow Row, float Score);

/// <summary>V14.9-I: one row of pending VLM-proposed rename, used to seed
/// the Deep Analyze "Pending renames" bulk-apply sheet.</summary>
internal sealed record ProposedRenameRow(
    long Id,
    string Path,
    string ProposedName);
