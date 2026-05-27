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
            // File.Exists wrapped — invalid-char paths or denied
            // ACL on the parent dir would otherwise throw.
            // offload File.Exists to the thread pool. On network
            // shares or slow USB sticks this sync call can block the UI
            // for hundreds of ms; the cost on a local SSD is < 1 ms so
            // there's no downside to always going async here.
            var dbPath = _dbPath;
            // cap the File.Exists call. On a disconnected SMB
            // share, the system-call can stall for 30+ seconds before
            // returning. We'd rather treat it as "DB not present" after
            // a few seconds than freeze startup.
            bool dbExists;
            using (var existsCts = CancellationTokenSource.CreateLinkedTokenSource(ct))
            {
                existsCts.CancelAfter(TimeSpan.FromSeconds(5));
                try
                {
                    dbExists = await Task.Run(() =>
                    {
                        try { return File.Exists(dbPath); }
                        catch (IOException) { return false; }
                        catch (UnauthorizedAccessException) { return false; }
                    }, existsCts.Token).ConfigureAwait(false);
                }
                catch (OperationCanceledException) when (!ct.IsCancellationRequested)
                {
                    DebugLog.Warn($"ReadStore.OpenAsync: File.Exists timed out for {PathRedactor.Redact(dbPath)}; treating as missing.");
                    return;
                }
            }
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
        var trimmedSearch = query?.Trim();
        if (string.IsNullOrEmpty(trimmedSearch))
        {
            return await RecentAsync(limit, ct).ConfigureAwait(false);
        }
        await _gate.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            if (_connection == null) return Array.Empty<FileRow>();
            var rows = new List<FileRow>(limit);
            var seen = new HashSet<long>();
            using var cmd = _connection.CreateCommand();

            var escapedSearch = trimmedSearch
                .Replace("\\", "\\\\")
                .Replace("%", "\\%")
                .Replace("_", "\\_");
            var like = $"%{escapedSearch}%";
            var match = BuildMatchExpression(trimmedSearch);
            var hasMatch = !string.IsNullOrEmpty(match);

            cmd.CommandText = $"""
            SELECT f.id, f.path_text, f.kind, f.size_bytes, f.modified_at, f.has_faces, f.has_text,
                   (SELECT GROUP_CONCAT(tag, '|') FROM (SELECT tag FROM tags WHERE file_id = f.id AND source IN ('auto','user','vlm') ORDER BY CASE source WHEN 'user' THEN 0 WHEN 'vlm' THEN 1 ELSE 2 END, score DESC, rowid)) AS auto_tags,
                   f.vlm_proposed_name
            FROM files f
            WHERE f.failed = 0
              AND (
                   {(hasMatch ? "f.id IN (SELECT rowid FROM ocr_fts WHERE ocr_fts MATCH $match) OR f.id IN (SELECT rowid FROM doc_fts WHERE doc_fts MATCH $match)" : "0")}
                   OR f.path_text LIKE $like ESCAPE '\'
                   OR f.vlm_proposed_name LIKE $like ESCAPE '\'
                   OR f.vlm_description LIKE $like ESCAPE '\'
                   OR f.id IN (SELECT file_id FROM tags WHERE tag LIKE $like ESCAPE '\')
                   OR f.id IN (
                       SELECT fp.file_id FROM face_prints fp
                       INNER JOIN persons p ON p.id = fp.person_id
                       WHERE p.name LIKE $like ESCAPE '\'
                          OR p.first_name LIKE $like ESCAPE '\'
                          OR p.last_name LIKE $like ESCAPE '\'
                   )
              )
            ORDER BY f.modified_at DESC NULLS LAST
            LIMIT $limit
            """;

            if (hasMatch)
            {
                cmd.Parameters.AddWithValue("$match", match);
            }
            cmd.Parameters.AddWithValue("$like", like);
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
                SELECT id, path_text, kind, size_bytes, modified_at, has_faces, has_text,
                       (SELECT GROUP_CONCAT(tag, '|') FROM (SELECT tag FROM tags WHERE file_id = files.id AND source IN ('auto','user','vlm') ORDER BY CASE source WHEN 'user' THEN 0 WHEN 'vlm' THEN 1 ELSE 2 END, score DESC, rowid)) AS auto_tags,
                       vlm_proposed_name
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
    /// cut: scans every embedding (acceptable up to ~50K files);
    /// swaps in an HNSW or IVF index if benchmarks demand it.
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
                   f.has_faces, f.has_text,
                   (SELECT GROUP_CONCAT(tag, '|') FROM (SELECT tag FROM tags WHERE file_id = f.id AND source IN ('auto','user','vlm') ORDER BY CASE source WHEN 'user' THEN 0 WHEN 'vlm' THEN 1 ELSE 2 END, score DESC, rowid)) AS auto_tags,
                   f.vlm_proposed_name,
                   e.embedding
            FROM clip_embeddings e
            JOIN files f ON f.id = e.file_id
            """;
            using var reader = await cmd.ExecuteReaderAsync(ct).ConfigureAwait(false);
            while (await reader.ReadAsync(ct).ConfigureAwait(false))
            {
                var blob = (byte[])reader.GetValue(9);
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

    /// <summary>
    /// Fetch similar files by finding the seed file's embedding and ranking others by cosine similarity.
    /// Matches macOS similarFiles(toFileID:limit:) exactly.
    /// </summary>
    public async Task<IReadOnlyList<FileRow>> SimilarFilesAsync(long seedId, int limit, CancellationToken ct)
    {
        if (_connection == null)
        {
            return Array.Empty<FileRow>();
        }
        await _gate.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            if (_connection == null) return Array.Empty<FileRow>();

            // 1. Get seed embedding
            float[]? seedVec = null;
            using (var cmd = _connection.CreateCommand())
            {
                cmd.CommandText = "SELECT embedding FROM clip_embeddings WHERE file_id = $seedId";
                cmd.Parameters.AddWithValue("$seedId", seedId);
                using var reader = await cmd.ExecuteReaderAsync(ct).ConfigureAwait(false);
                if (await reader.ReadAsync(ct).ConfigureAwait(false))
                {
                    seedVec = BlobToFloats((byte[])reader.GetValue(0));
                }
            }

            if (seedVec == null || seedVec.Length == 0)
            {
                return Array.Empty<FileRow>();
            }

            // 2. Fetch all other embeddings and calculate cosine similarity
            var heap = new PriorityQueue<long, float>(limit);
            using (var cmd = _connection.CreateCommand())
            {
                cmd.CommandText = "SELECT file_id, embedding FROM clip_embeddings WHERE file_id != $seedId";
                cmd.Parameters.AddWithValue("$seedId", seedId);
                using var reader = await cmd.ExecuteReaderAsync(ct).ConfigureAwait(false);
                while (await reader.ReadAsync(ct).ConfigureAwait(false))
                {
                    var fid = reader.GetInt64(0);
                    var blob = (byte[])reader.GetValue(1);
                    float score = DotProduct(seedVec, blob);

                    if (heap.Count < limit)
                    {
                        heap.Enqueue(fid, score);
                    }
                    else if (heap.TryPeek(out _, out var minScore) && score > minScore)
                    {
                        heap.Dequeue();
                        heap.Enqueue(fid, score);
                    }
                }
            }

            // 3. Extract the best IDs in descending order
            var topIDs = new List<long>(heap.Count);
            while (heap.Count > 0)
            {
                topIDs.Add(heap.Dequeue());
            }
            topIDs.Reverse();

            if (topIDs.Count == 0)
            {
                return Array.Empty<FileRow>();
            }

            // 4. Fetch file rows for those IDs in the ranked order
            var rows = new List<FileRow>(topIDs.Count);
            var idPlaceholders = new List<string>();
            using (var cmd = _connection.CreateCommand())
            {
                for (int j = 0; j < topIDs.Count; j++)
                {
                    var paramName = $"$id{j}";
                    idPlaceholders.Add(paramName);
                    cmd.Parameters.AddWithValue(paramName, topIDs[j]);
                }
                cmd.CommandText = $"""
                SELECT id, path_text, kind, size_bytes, modified_at, has_faces, has_text,
                       (SELECT GROUP_CONCAT(tag, '|') FROM (SELECT tag FROM tags WHERE file_id = files.id AND source IN ('auto','user','vlm') ORDER BY CASE source WHEN 'user' THEN 0 WHEN 'vlm' THEN 1 ELSE 2 END, score DESC, rowid)) AS auto_tags,
                       vlm_proposed_name
                FROM files
                WHERE id IN ({string.Join(",", idPlaceholders)}) AND failed = 0
                """;

                var byId = new Dictionary<long, FileRow>();
                using (var reader = await cmd.ExecuteReaderAsync(ct).ConfigureAwait(false))
                {
                    while (await reader.ReadAsync(ct).ConfigureAwait(false))
                    {
                        var row = ReadRow(reader);
                        byId[row.Id] = row;
                    }
                }

                foreach (var id in topIDs)
                {
                    if (byId.TryGetValue(id, out var row))
                    {
                        rows.Add(row);
                    }
                }
            }

            return rows;
        }
        finally { _gate.Release(); }
    }

    /// <summary>enumerate files that have a VLM-proposed name
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

    /// <summary>count of files with a VLM-proposed name pending.
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

    private static FileRow ReadRow(SqliteDataReader reader)
    {
        // Optional 8th column (auto-tags, pipe-delimited via
        // GROUP_CONCAT). Queries that don't project tags get
        // FieldCount=7 and Tags stays null; the Library card binding
        // hides the chip strip when Tags is null/empty.
        System.Collections.Generic.IReadOnlyList<string>? tags = null;
        if (reader.FieldCount > 7 && !reader.IsDBNull(7))
        {
            var raw = reader.GetString(7);
            if (!string.IsNullOrEmpty(raw))
            {
                tags = raw.Split('|', StringSplitOptions.RemoveEmptyEntries);
            }
        }
        // Optional 9th column: vlm_proposed_name (smart-rename
        // proposal from Deep Analyze). When present the Library card
        // shows it in gold below the filename, matching macOS
        // LibraryView.swift's golden smartName affordance.
        string? proposedName = null;
        if (reader.FieldCount > 8 && !reader.IsDBNull(8))
        {
            var raw = reader.GetString(8);
            if (!string.IsNullOrWhiteSpace(raw)) proposedName = raw;
        }
        return new FileRow(
            Id: reader.GetInt64(0),
            Path: reader.GetString(1),
            Kind: reader.GetString(2),
            SizeBytes: reader.GetInt64(3),
            ModifiedAt: reader.IsDBNull(4) ? null : reader.GetDouble(4),
            HasFaces: reader.GetInt32(5) != 0,
            HasText: reader.GetInt32(6) != 0,
            Tags: tags,
            ProposedName: proposedName);
    }

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

    private static float[] BlobToFloats(byte[] blob)
    {
        var span = System.Runtime.InteropServices.MemoryMarshal.Cast<byte, float>(blob);
        return span.ToArray();
    }

    private static float DotProduct(float[] q, byte[] blob)
    {
        if (blob.Length != q.Length * 4) return 0f;
        var qSpan = q.AsSpan();
        var blobFloats = System.Runtime.InteropServices.MemoryMarshal.Cast<byte, float>(blob);
        float acc = 0f;
        int i = 0;
        int simdLength = System.Numerics.Vector<float>.Count;
        if (System.Numerics.Vector.IsHardwareAccelerated && qSpan.Length >= simdLength)
        {
            var sumVector = System.Numerics.Vector<float>.Zero;
            int limit = qSpan.Length - (qSpan.Length % simdLength);
            for (; i < limit; i += simdLength)
            {
                var qVec = new System.Numerics.Vector<float>(qSpan.Slice(i, simdLength));
                var bVec = new System.Numerics.Vector<float>(blobFloats.Slice(i, simdLength));
                sumVector += qVec * bVec;
            }
            acc = System.Numerics.Vector.Dot(sumVector, System.Numerics.Vector<float>.One);
        }
        for (; i < qSpan.Length; i++)
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
    bool HasText,
    System.Collections.Generic.IReadOnlyList<string>? Tags = null,
    string? ProposedName = null);

internal sealed record FileRowWithScore(FileRow Row, float Score);

/// <summary>one row of pending VLM-proposed rename, used to seed
/// the Deep Analyze "Pending renames" bulk-apply sheet.</summary>
internal sealed record ProposedRenameRow(
    long Id,
    string Path,
    string ProposedName);
